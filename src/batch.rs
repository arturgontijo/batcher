use std::error::Error;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use bitcoin::psbt::{Input, Output};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Amount, Psbt, TxIn, TxOut};
use lightning_net_tokio::setup_outbound;
use rand::seq::SliceRandom;
use rand::thread_rng;
use tokio::net::TcpStream;

use crate::broker::Broker;
use crate::logger::print_pubkey;
use crate::messages::BatchMessage;
use crate::types::PeerManager;

pub(crate) fn process_batch_messages(
	node_alias: &String, node_id: &PublicKey, node_endpoint: String, broker: &Broker,
	peer_manager: Arc<PeerManager>, msg: BatchMessage,
) -> Result<(), Box<dyn Error>> {
	if let BatchMessage::BatchPsbt {
		sender_node_id,
		receiver_node_id: _,
		uniform_amount,
		fee_per_participant,
		max_utxo_per_participant,
		max_participants,
		participants,
		endpoints,
		not_participants,
		psbt,
		sign,
	} = msg
	{
		println!(
			"[{}][{}] BatchPsbt: sender={} | uni_amt={} | fee={} | utxos={} | max_p={} | pts={} | ep={} | n_pts={} | len={} | sign={}",
			node_id,
			node_alias,
			print_pubkey(&sender_node_id),
			uniform_amount,
			fee_per_participant,
			max_utxo_per_participant,
			max_participants,
			participants.len(),
			endpoints.len(),
			not_participants.len(),
			psbt.len(),
			sign,
		);

		let mut psbt = Psbt::deserialize(&psbt).unwrap();

		let mut participants = participants.clone();
		let mut endpoints = endpoints.clone();
		let mut not_participants = not_participants.clone();

		// Can we participate?
		let mut can_participate = false;
		let mut utxos_count = 0;
		let mut utxos_value = Amount::ZERO;
		for utxo in broker.list_unspent()? {
			utxos_value += utxo.txout.value;
			utxos_count += 1;
			if utxos_count > 1 && utxos_value.to_sat() > uniform_amount {
				can_participate = true;
				break;
			}
		}
		// ------

		if can_participate {
			// Not a participant yet
			if !sign && !participants.contains(node_id) {
				participants.push(*node_id);
				endpoints.push(node_endpoint.clone());
				// Add node's inputs/outputs and route it to the next node
				let fee = Amount::from_sat(fee_per_participant);

				let uniform_amount_opt =
					if uniform_amount > 0 { Some(Amount::from_sat(uniform_amount)) } else { None };
				broker.add_utxos_to_psbt(
					&mut psbt,
					max_utxo_per_participant,
					uniform_amount_opt,
					fee,
					false,
				)?;
			}
		} else if !participants.contains(node_id) && !not_participants.contains(node_id) {
			not_participants.push(*node_id);
		}

		let mut sign = sign;
		if (participants.len() as u8) >= max_participants {
			sign = true;

			// Shuffling inputs/outputs
			println!("\n[{}][{}] BatchPsbt: Shuffling inputs/outputs before starting the Signing workflow...", node_id, node_alias);
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

			println!("\n[{}][{}] BatchPsbt: Starting the Signing workflow (send final PSBT back to initial node)...\n", node_id, node_alias);
		}

		let mut peers = peer_manager.list_peers();
		peers.shuffle(&mut thread_rng());

		if !sign {
			let mut next_node_id = None;
			for pd in &peers {
				if participants.contains(&pd.counterparty_node_id) {
					continue;
				}
				if not_participants.contains(&pd.counterparty_node_id) {
					continue;
				}
				next_node_id = Some(pd.counterparty_node_id);
				break;
			}

			// We are already a participant/hop and all our peers are too, so we need to route the PSBT back to someone else
			if next_node_id.is_none() {
				if peers.len() == 1 {
					next_node_id = Some(peers[0].counterparty_node_id);
				}
				for pd in peers {
					if pd.counterparty_node_id == sender_node_id {
						continue;
					}
					next_node_id = Some(pd.counterparty_node_id);
					break;
				}
			}

			let psbt = psbt.serialize();

			let next_node_id = next_node_id.unwrap();
			let msg = BatchMessage::BatchPsbt {
				sender_node_id: *node_id,
				receiver_node_id: next_node_id,
				uniform_amount,
				fee_per_participant,
				max_utxo_per_participant,
				max_participants,
				participants,
				endpoints,
				not_participants,
				psbt,
				sign: false,
			};

			broker.send(next_node_id, msg)?;
		} else {
			// Check if we need to sign or just route the PSBT to someone else
			if participants.contains(node_id) {
				println!("[{}][{}] BatchPsbt: Signing...", node_id, node_alias);
				broker.sign_psbt(&mut psbt).unwrap();
				participants.retain(|key| key != node_id);
				endpoints.retain(|ep| ep != &node_endpoint.clone());
			}

			let psbt = psbt.serialize();

			// Do we need more signatures?
			if !participants.is_empty() {
				let next_signer_node_id = *participants.last().unwrap();
				let next_signer_endpoint = endpoints.last().unwrap().clone();
				// Wait for handshake to finish
				while peer_manager.peer_by_node_id(&next_signer_node_id).is_none() {
					let next_signer_endpoint = next_signer_endpoint.clone();
					sleep(Duration::from_millis(500));
					if peer_manager.peer_by_node_id(&next_signer_node_id).is_none() {
						let pm_clone = peer_manager.clone();
						tokio::spawn(async move {
							let stream = TcpStream::connect(&next_signer_endpoint)
								.await
								.expect("Failed to connect");
							match stream.into_std() {
								Ok(std_stream) => {
									setup_outbound(
										pm_clone.clone(),
										next_signer_node_id,
										std_stream,
									)
									.await
								},
								Err(e) => eprintln!("❌ Failed to convert stream: {e}"),
							}
						});
						sleep(Duration::from_millis(500));
					}
				}
				if peer_manager.peer_by_node_id(&next_signer_node_id).is_some() {
					let msg = BatchMessage::BatchPsbt {
						sender_node_id: *node_id,
						receiver_node_id: next_signer_node_id,
						uniform_amount,
						fee_per_participant,
						max_utxo_per_participant,
						max_participants,
						participants,
						endpoints,
						not_participants,
						psbt,
						sign: true,
					};
					broker.send(next_signer_node_id, msg)?;
				} else {
					println!(
						"[{}][{}] BatchPsbt: Woooooops (not connected to: {})!",
						node_id, node_alias, next_signer_node_id
					);
				}
			} else {
				println!(
					"[{}][{}] BatchPsbt: PSBT was signed by all participants! (len={})",
					node_id,
					node_alias,
					psbt.len()
				);
				broker.push_to_batch_psbts(psbt).unwrap();
			}
		}
	}
	Ok(())
}
