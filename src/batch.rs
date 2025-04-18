use std::error::Error;
use std::sync::Arc;

use bitcoin::psbt::{Input, Output};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Amount, Psbt, TxIn, TxOut};
use rand::seq::SliceRandom;
use rand::thread_rng;

use crate::broker::Broker;
use crate::messages::BatchMessage;
use crate::types::PeerManager;

pub(crate) fn process_batch_messages(
	node_id: &PublicKey, broker: &Broker, peer_manager: Arc<PeerManager>, msg: BatchMessage,
) -> Result<(), Box<dyn Error>> {
	if let BatchMessage::BatchPsbt {
		sender_node_id,
		receiver_node_id,
		uniform_amount,
		fee_per_participant,
		max_participants,
		participants,
		hops,
		psbt_hex,
		sign,
	} = msg
	{
		println!(
                "[{}] BatchPsbt: sender={} | uni_amount={} | fee={} | max_p={} | participants={} | hops={} | len={} | sign={}",
                node_id,
                sender_node_id,
                uniform_amount,
                fee_per_participant,
                max_participants,
                participants.len(),
                hops.len(),
                psbt_hex.len(),
                sign,
            );

		let mut psbt = Psbt::deserialize(&hex::decode(psbt_hex).unwrap()).unwrap();

		let mut hops = hops;
		let mut participants = participants;

		// Not a participant yet
		if !sign && !participants.contains(&receiver_node_id) {
			participants.push(receiver_node_id);
			// Add node's inputs/outputs and route it to the next node
			let fee = Amount::from_sat(fee_per_participant);

			let uniform_amount_opt =
				if uniform_amount > 0 { Some(Amount::from_sat(uniform_amount)) } else { None };
			broker.add_utxos_to_psbt(&mut psbt, 2, uniform_amount_opt, fee, false).unwrap();
		}

		let mut sign = sign;
		if (participants.len() as u8) >= max_participants {
			sign = true;

			// Shuffling inputs/outputs
			println!("\n[{}] BatchPsbt: Shuffling inputs/outputs before starting the Signing workflow...", node_id);
			let mut rng = thread_rng();
			let mut paired_inputs: Vec<(Input, TxIn)> =
				psbt.inputs.iter().cloned().zip(psbt.unsigned_tx.input.iter().cloned()).collect();
			paired_inputs.shuffle(&mut rng);

			// Unzip the shuffled pairs back into psbt
			let (shuffled_psbt_inputs, shuffled_tx_inputs): (Vec<_>, Vec<_>) =
				paired_inputs.into_iter().unzip();
			psbt.inputs = shuffled_psbt_inputs;
			psbt.unsigned_tx.input = shuffled_tx_inputs;

			// Step 2: Shuffle outputs while keeping psbt.outputs and psbt.unsigned_tx.output aligned
			let mut paired_outputs: Vec<(Output, TxOut)> =
				psbt.outputs.iter().cloned().zip(psbt.unsigned_tx.output.iter().cloned()).collect();
			paired_outputs.shuffle(&mut rng);

			// Unzip the shuffled pairs back into psbt
			let (shuffled_psbt_outputs, shuffled_tx_outputs): (Vec<_>, Vec<_>) =
				paired_outputs.into_iter().unzip();
			psbt.outputs = shuffled_psbt_outputs;
			psbt.unsigned_tx.output = shuffled_tx_outputs;

			println!("\n[{}] BatchPsbt: Starting the Signing workflow (send final PSBT back to initial node)...\n", node_id);
		}

		let peers = peer_manager.list_peers();

		if !sign {
			let mut next_node_id = None;
			for peer in &peers {
				if participants.contains(&peer.counterparty_node_id) {
					continue;
				}
				if hops.last().unwrap() == &peer.counterparty_node_id {
					continue;
				}
				next_node_id = Some(peer.counterparty_node_id);
				break;
			}

			// We are already a participant/hop and all our peers are too, so we need to route the PSBT back to someone else
			if next_node_id.is_none() {
				if peers.len() == 1 {
					next_node_id = Some(peers[0].counterparty_node_id);
				}
				for peer in peers {
					if peer.counterparty_node_id == sender_node_id {
						continue;
					}
					next_node_id = Some(peer.counterparty_node_id);
					break;
				}
			}

			hops.push(receiver_node_id);
			let psbt_hex = psbt.serialize_hex();

			let next_node_id = next_node_id.unwrap();
			let msg = BatchMessage::BatchPsbt {
				sender_node_id: receiver_node_id,
				receiver_node_id: next_node_id,
				uniform_amount,
				fee_per_participant,
				max_participants,
				participants: participants.clone(),
				hops: hops.clone(),
				psbt_hex,
				sign: false,
			};

			broker.send(next_node_id, msg)?;
		} else {
			// Check if we need to sign or just route the PSBT to someone else
			if participants.contains(&receiver_node_id) {
				println!("[{}] BatchPsbt: Signing...", node_id);
				broker.sign_psbt(&mut psbt).unwrap();
				participants.retain(|key| *key != receiver_node_id);
			}

			let psbt_hex = psbt.serialize_hex();

			// Do we need more signatures?
			if !hops.is_empty() {
				let next_signer_node_id = hops.pop().unwrap();
				if peer_manager.peer_by_node_id(&next_signer_node_id).is_some() {
					let msg = BatchMessage::BatchPsbt {
						sender_node_id: receiver_node_id,
						receiver_node_id: next_signer_node_id,
						uniform_amount,
						fee_per_participant,
						max_participants,
						participants: participants.clone(),
						hops: hops.clone(),
						psbt_hex,
						sign: true,
					};
					broker.send(next_signer_node_id, msg)?;
				} else {
					let mut inner_participants = participants.clone();
					for node_id in participants.iter().rev() {
						if peer_manager.peer_by_node_id(node_id).is_some() {
							// We need to add back the next_signer_node_id to participants
							if !inner_participants.contains(&next_signer_node_id) {
								inner_participants.push(next_signer_node_id);
							}
							let msg = BatchMessage::BatchPsbt {
								sender_node_id: receiver_node_id,
								receiver_node_id: *node_id,
								uniform_amount,
								fee_per_participant,
								max_participants,
								participants: inner_participants,
								hops: hops.clone(),
								psbt_hex,
								sign: true,
							};
							broker.send(next_signer_node_id, msg)?;
							break;
						}
					}
				}
			} else {
				println!(
					"[{}] BatchPsbt: PSBT was signed by all participants! (len={})",
					node_id,
					psbt_hex.len()
				);
				broker.push_to_batch_psbts(psbt_hex).unwrap();
			}
		}
	}
	Ok(())
}
