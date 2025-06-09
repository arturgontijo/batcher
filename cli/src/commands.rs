use batcher::{
	bitcoind::bitcoind_client, config::NodeConfig, node::Node, storage::BatchPsbtStatus,
	types::BoxError,
};
use bitcoin::{absolute::LockTime, secp256k1::PublicKey, Address, Amount, FeeRate};
use bitcoincore_rpc::{Client, RpcApi};
use chrono::{DateTime, Utc};
use std::{
	sync::{Arc, RwLock},
	thread::sleep,
	time::Duration,
};

use std::str::FromStr;

pub fn handle_command(
	input: &str, node: &Arc<RwLock<Option<Arc<Node>>>>, client: &Arc<RwLock<Option<Arc<Client>>>>,
) -> Result<(), BoxError> {
	let mut parts = input.split_whitespace();
	let cmd = match parts.next() {
		Some(c) => c,
		None => return Ok(()),
	};

	match cmd {
		"start" => {
			let config = parts.next();
			match config {
				Some(config) => {
					let mut unlocked = node.write().unwrap();
					if let Some(node) = unlocked.clone() {
						println!("[{}][{}] Node is already running.", node.node_id(), node.alias());
					} else {
						let config = NodeConfig::new(config)?;
						let _node = Node::new_from_config(config.clone())?;
						println!("[{}][{}] Starting Node...", _node.node_id(), _node.alias());
						_node.start()?;
						*unlocked = Some(Arc::new(_node));
					}
				},
				_ => {
					println!("Usage: start <config.toml>");
				},
			}
			Ok(())
		},
		"stats" | "s" => {
			let unlocked = node.read().unwrap();
			if let Some(node) = unlocked.clone() {
				let utxos = node.broker.list_unspent()?;
				let mut value = Amount::ZERO;
				for utxo in &utxos {
					value += utxo.txout.value;
				}
				let psbts = node.broker.next_id()?;
				println!(
					"[{}][{}] Stats: peers={} | utxos={} | value={} | psbts={}",
					node.node_id(),
					node.alias(),
					node.peer_manager.list_peers().len(),
					utxos.len(),
					value,
					psbts,
				);
			} else {
				println!("Node is not running.");
			}
			Ok(())
		},
		"peers" | "lp" => {
			let unlocked = node.read().unwrap();
			if let Some(node) = unlocked.clone() {
				let peers_info = node.list_peers()?;
				if peers_info.is_empty() {
					println!("[{}][{}] No connected peers!", node.node_id(), node.alias());
				} else {
					for peer in peers_info {
						println!(
							"[{}][{}] ({}, {}) | {} | {}",
							node.node_id(),
							node.alias(),
							peer.0,
							peer.1,
							if node.is_peer_connected(&peer.0) {
								"CONNECTED"
							} else {
								"DISCONNECTED"
							},
							format_timestamp(peer.2),
						);
					}
				}
			} else {
				println!("Node is not running.");
			}
			Ok(())
		},
		"connect" | "c" => {
			let node_id = parts.next();
			let address = parts.next();
			match (node_id, address) {
				(Some(node_id), Some(address)) => {
					let unlocked = node.read().unwrap();
					if let Some(node) = unlocked.clone() {
						let pubkey = PublicKey::from_str(node_id).unwrap();
						node.connect(pubkey, address.to_string())?;
						let mut checks = 0;
						while !node.is_peer_connected(&pubkey) {
							sleep(Duration::from_millis(300));
							checks += 1;
							if checks == 50 || checks == 100 {
								println!(
									"[{}][{}] Connection is taking too long ({} @ {})",
									node.node_id(),
									node.alias(),
									node_id,
									address
								);
							}
							if checks >= 200 {
								break;
							}
						}
						if checks < 5 {
							println!(
								"[{}][{}] Connected to peer {} @ {}",
								node.node_id(),
								node.alias(),
								node_id,
								address
							);
						} else {
							println!(
								"[{}][{}] Unable to connect to peer {} @ {}",
								node.node_id(),
								node.alias(),
								node_id,
								address
							);
						}
					} else {
						println!("Node is not running.");
					}
				},
				_ => {
					println!("Usage: connect <NODE_ID> <HOST:PORT>");
				},
			}
			Ok(())
		},
		"addr" | "na" => {
			let unlocked = node.read().unwrap();
			if let Some(node) = unlocked.clone() {
				let addr = node.wallet_new_address()?;
				println!("[{}][{}] Address: {}", node.node_id(), node.alias(), addr);
			} else {
				println!("Node is not running.");
			}
			Ok(())
		},
		// b tb1qw6w293n3pxnydktdlsngtjfcek4stsl5tfyvcm 777777 99999
		"batch" | "b" => {
			let addr_str = parts.next();
			let amount = parts.next();
			let fee_per_participant = parts.next();
			let max_participants = parts.next().unwrap_or("2");
			let max_utxo_per_participant = parts.next().unwrap_or("2");
			let max_hops = parts.next().unwrap_or("255");
			let unlocked = node.read().unwrap();
			if let Some(node) = unlocked.clone() {
				match (addr_str, amount, fee_per_participant) {
					(Some(addr_str), Some(amount), Some(fee_per_participant)) => {
						match node.peer_manager.list_peers().first() {
							Some(pd) => {
								let address = Address::from_str(addr_str)?.assume_checked();
								let output_script = address.script_pubkey();
								node.init_psbt_batch(
									pd.counterparty_node_id,
									output_script,
									Amount::from_sat(amount.parse()?),
									FeeRate::from_sat_per_vb_unchecked(50),
									LockTime::ZERO,
									true,
									Amount::from_sat(fee_per_participant.parse()?),
									max_participants.parse()?,
									max_utxo_per_participant.parse()?,
									max_hops.parse()?,
								)?;
								println!("Batch initialized, check 'stats' for ready PSBTs.")
							},
							None => println!("No connected peers!"),
						}
					},
					_ => {
						println!(
							"Usage: batch <RECEIVER_ADDR> <AMOUNT_SATS> <FEE_PER_PARTICIPANT_SATS>"
						);
					},
				}
			} else {
				println!("Node is not running.");
			}
			Ok(())
		},
		"broadcast" | "bc" => {
			let unlocked = node.read().unwrap();
			if let Some(node) = unlocked.clone() {
				for batch_psbt in node.broker.stored_psbts(BatchPsbtStatus::Ready)? {
					let psbt = batch_psbt.psbt.clone();
					let tx = psbt.clone().extract_tx()?;
					println!("\nTx Inputs/Outputs:\n");
					for input in tx.input.iter() {
						let tx_info = node
							.broker
							.bitcoind_client
							.get_raw_transaction_info(&input.previous_output.txid, None)?;
						let value = tx_info.vout[input.previous_output.vout as usize].value;
						println!("====> In  ({})", value);
					}

					for output in tx.output.iter() {
						println!("====> Out ({})", output.value);
					}

					println!("\nSending Tx (id={})...\n", tx.compute_txid());

					let tx_id = node.broadcast_transactions(&tx)?;
					println!("Tx Sent (id={})\n", tx_id);

					node.broker.upsert_psbt(
						Some(batch_psbt.id),
						BatchPsbtStatus::Completed,
						&psbt,
					)?;
				}
			} else {
				println!("Node is not running.");
			}
			Ok(())
		},
		"client" | "cs" => {
			let rpc_address = parts.next();
			let rpc_user = parts.next();
			let rpc_pass = parts.next();
			let wallet = parts.next();
			let mut unlocked = client.write().unwrap();
			if unlocked.is_some() {
				println!("Client is already set up.");
			} else {
				match (rpc_address, rpc_user, rpc_pass, wallet) {
					(Some(rpc_address), Some(rpc_user), Some(rpc_pass), Some(wallet)) => {
						let _client = bitcoind_client(
							rpc_address.to_string(),
							rpc_user.to_string(),
							rpc_pass.to_string(),
							Some(wallet),
						)?;
						*unlocked = Some(Arc::new(_client));
						println!("Client is set up.");
					},
					_ => {
						println!("Usage: client <RPC_ADDR> <USER> <PASS> <WALLET>");
					},
				}
			}
			Ok(())
		},
		"fund" | "f" => {
			let unlocked = client.read().unwrap();
			if let Some(client) = unlocked.clone() {
				let address = parts.next();
				let amount = parts.next();
				match (address, amount) {
					(Some(address), Some(amount)) => {
						let amt = Amount::from_btc(amount.parse()?)?;
						client.send_to_address(
							&Address::from_str(address)?.assume_checked(),
							amt,
							None,
							None,
							None,
							None,
							None,
							None,
						)?;
						println!("Client sent {} to {}.", amt, address);
					},
					_ => {
						println!("Usage: fund <ADDR> <AMOUNT_BTC>");
					},
				}
			} else {
				println!("Client is not running.");
			}
			Ok(())
		},
		"mine" | "m" => {
			let unlocked = client.read().unwrap();
			if let Some(client) = unlocked.clone() {
				let block_num = parts.next();
				match block_num {
					Some(block_num) => {
						let block_num: u64 = block_num.parse()?;
						let address = client.get_new_address(None, None)?.assume_checked();
						client.generate_to_address(block_num, &address)?;
						println!("Client mined {} block to {}.", block_num, address);
					},
					_ => {
						println!("Usage: mine <BLOCK_NUM>");
					},
				}
			} else {
				println!("Client is not running.");
			}
			Ok(())
		},
		"help" | "h" => {
			print_subcommands_help();
			Ok(())
		},

		unknown => {
			println!("Unknown command: '{}'", unknown);
			println!("Type 'help' to see available commands.");
			Ok(())
		},
	}
}

pub fn print_subcommands_help() {
	println!("\nBatcher CLI:");
	println!("  start                                                           - Start a Node");
	println!(
		"  s  | stats                                                      - Show Nonde stats"
	);
	println!(
		"  lp | peers                                                      - List connected peers"
	);
	println!("  c  | connect <ID> <ADDR>                                        - Connect to a peer (eg. connect 03ef...c8a7 0.0.0.0:7000)");
	println!("  b  | batch <RCVR_ADDR> <AMT_SATS> <FEE_PER_PARTICIPANT_SATS>    - Init a batch");
	println!("  bc | broadcast                                                  - Broadcast a Tx from a BatchPsbt");
	println!("  h  | help                                                       - Show this help message");
	println!("  q  | exit | quit                                                - Exit the node\n");
}

fn format_timestamp(timestamp: u64) -> String {
	let datetime = DateTime::<Utc>::from_timestamp(timestamp as i64, 0).expect("Invalid timestamp");
	datetime.format("%m/%d/%Y %H:%M:%S").to_string()
}
