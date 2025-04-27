mod common;

use batcher::bitcoind;
use batcher::bitcoind::BitcoindConfig;
use batcher::config::BrokerConfig;
use batcher::wallet;

use bdk_wallet::KeychainKind;
use bitcoin::{absolute::LockTime, policy::DEFAULT_MIN_RELAY_TX_FEE, Amount, FeeRate, Network};

use bitcoind::{bitcoind_client, fund_address, wait_for_block};
use common::{broadcast_tx, setup_nodes};
use wallet::create_wallet;

use std::error::Error;
use std::thread::sleep;
use tokio::time::Duration;

#[test]
fn batcher_as_node() -> Result<(), Box<dyn Error>> {
	let network = Network::Signet;

	let bitcoind_config = BitcoindConfig::new("http://0.0.0.0:38332", "local", "local");

	let broker_config = BrokerConfig::new(vec![], 25_000, 2);
	let nodes = setup_nodes(8, 7777, network, bitcoind_config.clone(), broker_config)?;

	let BitcoindConfig { rpc_address, rpc_user, rpc_pass } = bitcoind_config;
	let bitcoind = bitcoind_client(rpc_address, rpc_user, rpc_pass, Some("miner"))?;

	for node in &nodes {
		fund_address(&bitcoind, node.wallet_new_address()?, Amount::from_sat(1_000_000), 10)?;
	}

	wait_for_block(&bitcoind, 2)?;

	for node in &nodes {
		node.sync_wallet(true)?;
	}

	for node in &nodes {
		println!("[{}][{}] Balance: {}", node.node_id(), node.alias(), node.balance().total());
	}

	//             N2      N7 (Sender)
	//           /    \   /
	//   N0 -- N1       N4 -- N5 -- N6
	//           \    /
	//             N3
	nodes[0].connect(&nodes[1])?;
	nodes[1].connect(&nodes[2])?;
	nodes[1].connect(&nodes[3])?;
	nodes[2].connect(&nodes[4])?;
	nodes[3].connect(&nodes[4])?;
	nodes[4].connect(&nodes[5])?;
	nodes[5].connect(&nodes[6])?;

	sleep(Duration::from_millis(1_000));

	// Sender's node index
	let starting_node_idx = 7;
	// Sender must start the Batch workflow by selecting an initial Node
	let initial_node_idx = 4;

	// Starting Batch workflow
	let mut receiver =
		create_wallet(&[255u8; 64], network, "/tmp/batcher/receiver.db".to_string())?;

	let amount = Amount::from_sat(777_777);
	let script_pubkey =
		receiver.reveal_next_address(KeychainKind::External).address.script_pubkey();
	let uniform_amount = true;
	let fee_rate = FeeRate::from_sat_per_vb(DEFAULT_MIN_RELAY_TX_FEE as u64).unwrap();
	let locktime: LockTime = LockTime::ZERO;
	let max_utxo_count = 4;
	let fee_per_participant = Amount::from_sat(99_999);
	let max_participants = 7;

	// Sender must connect to an initial Node
	nodes[starting_node_idx].connect(&nodes[initial_node_idx])?;
	while !nodes[starting_node_idx].is_peer_connected(&nodes[initial_node_idx].node_id()) {
		sleep(Duration::from_millis(250));
		println!(
			"Connecting to {} -> {} ...",
			nodes[starting_node_idx].alias(),
			nodes[initial_node_idx].alias()
		);
	}

	nodes[starting_node_idx].init_psbt_batch(
		nodes[initial_node_idx].node_id(),
		script_pubkey,
		amount,
		fee_rate,
		locktime,
		uniform_amount,
		fee_per_participant,
		max_participants,
		max_utxo_count,
	)?;

	broadcast_tx(&bitcoind, &nodes[starting_node_idx], &nodes, &mut receiver, None)?;

	for node in &nodes {
		println!("[{}][{}] Stopping...", node.node_id(), node.alias());
		node.stop()?;
	}

	Ok(())
}
