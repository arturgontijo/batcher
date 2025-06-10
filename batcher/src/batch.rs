use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use bitcoin::psbt::{Input, Output};
use bitcoin::secp256k1::PublicKey;
use bitcoin::{Amount, Psbt, TxIn, TxOut};
use lightning::util::logger::Logger;
use lightning::{log_debug, log_error, log_info};
use lightning_net_tokio::setup_outbound;
use rand::seq::SliceRandom;
use rand::thread_rng;
use tokio::net::TcpStream;

use crate::broker::Broker;
use crate::logger::{print_pubkey, SimpleLogger};
use crate::messages::BatchMessage;
use crate::storage::{BatchPsbtKind, BatchPsbtStatus, PeerStorage};
use crate::types::{BoxError, PeerManager};

pub(crate) fn process_batch_messages(
	node_alias: &String, node_id: &PublicKey, node_endpoint: String, broker: &Broker,
	peer_manager: Arc<PeerManager>, peer_storage: Arc<Mutex<PeerStorage>>, logger: &SimpleLogger,
	msg: BatchMessage,
) -> Result<(), BoxError> {
	log_debug!(logger, "{:?}", msg);
	if let BatchMessage::BatchPsbt {
		id,
		sender_node_id,
		receiver_node_id: _,
		uniform_amount,
		fee_per_participant,
		max_utxo_per_participant,
		max_participants,
		max_hops,
		participants,
		endpoints,
		not_participants,
		hops,
		psbt,
		sign,
	} = msg
	{
		log_info!(
			logger,
			"[{}][{}] BatchPsbt(id={}): snd={} | uamt={} | fee={} | utxos={} | max_p={} | pts={} | ep={} | n_pts={} | hops={}/{} | len={} | sign={}",
			node_id,
			node_alias,
			id,
			print_pubkey(&sender_node_id),
			uniform_amount,
			fee_per_participant,
			max_utxo_per_participant,
			max_participants,
			participants.len(),
			endpoints.len(),
			not_participants.len(),
			hops,
			max_hops,
			psbt.len(),
			sign,
		);

		// Check hops counter to see if we can process the BatchPsbt or just route it back to last participant
		// This can be triggered if msg finds itself into an infinite loop of nodes or if there is no more node to be a participant
		let mut hops = hops;
		if !sign && (hops >= max_hops || hops == u8::MAX) {
			let last_participant_id = *participants.last().unwrap();
			let last_participant_endpoint = endpoints.last().unwrap().clone();
			connect_peer(
				peer_manager,
				peer_storage,
				last_participant_id,
				last_participant_endpoint,
			)?;
			let msg = BatchMessage::BatchPsbt {
				id,
				sender_node_id: *node_id,
				receiver_node_id: last_participant_id,
				uniform_amount,
				fee_per_participant,
				max_utxo_per_participant,
				// We must change this in order to force the signing phase
				max_participants: participants.len() as u8,
				max_hops,
				participants,
				endpoints,
				not_participants,
				hops,
				psbt,
				sign: true,
			};
			log_info!(
				logger,
				"[{}][{}] BatchPsbt: Too many hops [{}/{}], routing it back to last participant...",
				node_id,
				node_alias,
				hops,
				max_hops
			);
			return broker.send(&last_participant_id, msg);
		}

		let mut psbt = Psbt::deserialize(&psbt).unwrap();

		let mut participants = participants.clone();
		let mut endpoints = endpoints.clone();
		let mut not_participants = not_participants.clone();

		// Can we participate?
		// Check fee value and
		// We must have at least one UTXO and it must cover the uniform_amount value
		let mut can_participate = false;
		if fee_per_participant >= broker.config.minimum_fee {
			let mut utxos_value = Amount::ZERO;
			for utxo in broker.list_unspent()? {
				utxos_value += utxo.txout.value;
				if utxos_value.to_sat() > uniform_amount {
					can_participate = true;
					break;
				}
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

				let max_count = broker.config.max_utxo_count.min(max_utxo_per_participant);

				broker.add_utxos_to_psbt(&mut psbt, max_count, uniform_amount_opt, fee, false)?;
			}
		} else if !participants.contains(node_id) && !not_participants.contains(node_id) {
			not_participants.push(*node_id);
		}

		let mut sign = sign;
		if (participants.len() as u8) >= max_participants {
			sign = true;

			// Shuffling inputs/outputs
			log_info!(logger, "[{}][{}] BatchPsbt: Shuffling inputs/outputs before starting the Signing workflow...", node_id, node_alias);
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

			log_info!(logger, "[{}][{}] BatchPsbt: Starting the Signing workflow (send final PSBT back to initial node)...\n", node_id, node_alias);
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

			hops = hops.saturating_add(1);

			let psbt = psbt.serialize();

			let next_node_id = next_node_id.unwrap();
			let msg = BatchMessage::BatchPsbt {
				id,
				sender_node_id: *node_id,
				receiver_node_id: next_node_id,
				uniform_amount,
				fee_per_participant,
				max_utxo_per_participant,
				max_participants,
				max_hops,
				participants,
				endpoints,
				not_participants,
				hops,
				psbt,
				sign: false,
			};

			broker.send(&next_node_id, msg)?;
		} else {
			// Check if we need to sign or just route the PSBT to someone else
			if participants.contains(node_id) {
				// We are the initial/last node, check if we are the batch sender or it is a foreign PSBT
				let is_sender = if participants.len() == 1 {
					if let Some(stored_psbt) = broker.psbt_by_id(id)? {
						matches!(stored_psbt.kind, BatchPsbtKind::Node | BatchPsbtKind::Multisig)
					} else {
						false
					}
				} else {
					false
				};

				if is_sender || broker.check_psbt(&psbt, Amount::from_sat(fee_per_participant))? {
					log_info!(logger, "[{}][{}] BatchPsbt: Signing...", node_id, node_alias);
					broker.sign_psbt(&mut psbt).unwrap();
				} else {
					log_error!(
						logger,
						"[{}][{}] BatchPsbt: NOT Signing as inputs or outputs were tampered...",
						node_id,
						node_alias
					);
				}
				participants.retain(|key| key != node_id);
				endpoints.retain(|ep| ep != &node_endpoint.clone());
			}

			let psbt = psbt.serialize();

			// Do we need more signatures?
			if !participants.is_empty() {
				let next_signer_node_id = *participants.last().unwrap();
				let next_signer_endpoint = endpoints.last().unwrap().clone();
				connect_peer(
					peer_manager.clone(),
					peer_storage,
					next_signer_node_id,
					next_signer_endpoint,
				)?;
				if peer_manager.peer_by_node_id(&next_signer_node_id).is_some() {
					let msg = BatchMessage::BatchPsbt {
						id,
						sender_node_id: *node_id,
						receiver_node_id: next_signer_node_id,
						uniform_amount,
						fee_per_participant,
						max_utxo_per_participant,
						max_participants,
						max_hops,
						participants,
						endpoints,
						not_participants,
						hops,
						psbt,
						sign: true,
					};
					broker.send(&next_signer_node_id, msg)?;
				} else {
					log_info!(
						logger,
						"[{}][{}] BatchPsbt: Woooooops (not connected to: {})!",
						node_id,
						node_alias,
						next_signer_node_id
					);
				}
			} else {
				log_info!(
					logger,
					"[{}][{}] BatchPsbt: PSBT was signed by all participants! (len={})",
					node_id,
					node_alias,
					psbt.len()
				);
				broker.update_psbt(id, BatchPsbtStatus::Ready, &Psbt::deserialize(&psbt)?)?;
			}
		}
	} else if let BatchMessage::Announcement { sender_node_id, receiver_node_id, endpoint } = msg {
		log_info!(
			logger,
			"[{}][{}] Announcement: snd={} | rcv={} | ep={}",
			node_id,
			node_alias,
			print_pubkey(&sender_node_id),
			print_pubkey(&receiver_node_id),
			endpoint,
		);
		let storage = peer_storage.lock().unwrap();
		storage.upsert_peer(&sender_node_id, endpoint).unwrap();
	}
	Ok(())
}

fn connect_peer(
	peer_manager: Arc<PeerManager>, peer_storage: Arc<Mutex<PeerStorage>>,
	other_node_id: PublicKey, other_endpoint: String,
) -> Result<(), BoxError> {
	let mut counter = 0;
	// Wait for handshake to finish
	while peer_manager.peer_by_node_id(&other_node_id).is_none() {
		let next_node_endpoint = other_endpoint.clone();
		sleep(Duration::from_millis(300));
		if peer_manager.peer_by_node_id(&other_node_id).is_none() {
			let storage = peer_storage.lock().unwrap();
			storage.upsert_peer(&other_node_id, next_node_endpoint.clone()).unwrap();

			let pm_clone = peer_manager.clone();
			tokio::spawn(async move {
				let stream =
					TcpStream::connect(&next_node_endpoint).await.expect("Failed to connect");
				match stream.into_std() {
					Ok(std_stream) => {
						setup_outbound(pm_clone.clone(), other_node_id, std_stream).await
					},
					Err(e) => eprintln!("❌ Failed to convert stream: {e}"),
				}
			});

			sleep(Duration::from_millis(300));
		}
		counter += 1;
		if counter > 1_000 {
			return Err(format!(
				"Can not connect to peer: {} at {}",
				other_node_id, other_endpoint
			)
			.into());
		}
	}
	Ok(())
}
