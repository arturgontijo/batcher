mod common;

use batcher::bitcoind::BitcoindConfig;
use batcher::bitcoind::{self, setup_bitcoind};
use batcher::config::BrokerConfig;
use batcher::node::Node;
use batcher::wallet;

use bdk_wallet::KeychainKind;
use bitcoin::{absolute::LockTime, Amount, FeeRate, Network};

use bitcoincore_rpc::Client;
use bitcoind::{fund_address, wait_for_block};
use common::{broadcast_tx, connect, create_temp_dir, setup_nodes};
use wallet::create_wallet;

use std::error::Error;
use std::sync::Arc;
use std::thread::sleep;
use tokio::time::Duration;

fn setup_network(
	client: &Client, network: Network, bitcoind_config: BitcoindConfig,
) -> Result<Vec<Arc<Node>>, Box<dyn Error>> {
	let broker_config = BrokerConfig::new(vec![], 25_000, 2, 60);
	let nodes = setup_nodes(8, 7777, network, bitcoind_config.clone(), broker_config)?;

	for node in &nodes {
		fund_address(&client, node.wallet_new_address()?, Amount::from_sat(1_000_000), 10)?;
	}

	wait_for_block(&client, 2)?;

	for node in &nodes {
		node.sync_wallet()?;
	}

	for node in &nodes {
		println!("[{}][{}] Balance: {}", node.node_id(), node.alias(), node.balance().total());
	}

	//             N2      N7 (Sender)
	//           /    \   /
	//   N0 -- N1       N4 -- N5 -- N6
	//           \    /
	//             N3
	connect(&nodes[0], &nodes[1])?;
	connect(&nodes[1], &nodes[2])?;
	connect(&nodes[1], &nodes[3])?;
	connect(&nodes[2], &nodes[4])?;
	connect(&nodes[3], &nodes[4])?;
	connect(&nodes[4], &nodes[5])?;
	connect(&nodes[5], &nodes[6])?;

	sleep(Duration::from_millis(1_000));

	Ok(nodes)
}

#[test]
fn batcher_as_node() -> Result<(), Box<dyn Error>> {
	let network = Network::Regtest;

	let bitcoind = setup_bitcoind()?;

	let temp_dir = create_temp_dir("batcher_as_node")?;

	let bitcoind_config = BitcoindConfig::new(&bitcoind.rpc_url(), "bitcoind", "bitcoind");

	let nodes = setup_network(&bitcoind.client, network, bitcoind_config)?;

	// Sender's node index
	let starting_node_idx = 7;
	// Sender must start the Batch workflow by selecting an initial Node
	let initial_node_idx = 4;

	// Starting Batch workflow
	let mut receiver =
		create_wallet(&[255u8; 64], network, format!("{}/receiver.db", temp_dir.display()))?;

	let amount = Amount::from_sat(777_777);
	let script_pubkey =
		receiver.reveal_next_address(KeychainKind::External).address.script_pubkey();
	let uniform_amount = true;
	let fee_rate = FeeRate::from_sat_per_vb(50).unwrap();
	let locktime: LockTime = LockTime::ZERO;
	let max_utxo_count = 4;
	let fee_per_participant = Amount::from_sat(99_999);
	let max_participants = 7;
	let max_hops = 128;

	// Sender must connect to an initial Node
	connect(&nodes[starting_node_idx], &nodes[initial_node_idx])?;
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
		max_hops,
	)?;

	broadcast_tx(&bitcoind.client, &nodes[starting_node_idx], &nodes, &mut receiver, None)?;

	let max_hops = 3;

	let script_pubkey =
		receiver.reveal_next_address(KeychainKind::External).address.script_pubkey();

	println!("\nInitializing BatchPSBT with max_hops={}\n", max_hops);
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
		max_hops,
	)?;

	broadcast_tx(&bitcoind.client, &nodes[starting_node_idx], &nodes, &mut receiver, None)?;

	for node in &nodes {
		println!("[{}][{}] Stopping...", node.node_id(), node.alias());
		node.stop()?;
	}

	Ok(())
}
