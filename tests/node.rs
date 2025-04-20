mod common;

use batcher::bitcoind;
use batcher::wallet;

use bdk_wallet::KeychainKind;
use bitcoin::{
	absolute::LockTime, policy::DEFAULT_MIN_RELAY_TX_FEE, Amount, FeeRate, Network, Psbt,
};
use bitcoincore_rpc::RpcApi;
use bitcoind::{bitcoind_client, fund_address, wait_for_block};
use common::setup_nodes;
use wallet::{create_wallet, wallet_total_balance};

use std::error::Error;
use tokio::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn batcher_as_node() -> Result<(), Box<dyn Error>> {
	let network = Network::Signet;

	let nodes = setup_nodes(8, 7777, network).await?;

	let bitcoind = bitcoind_client("miner")?;
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

	//                    (500k:0)- N2 -(500k:0)    N7 (Sender)
	//                   /                      \  /
	//   N0 -(500k:0)- N1                        N4 -(500k:0)- N5 -(500k:0)- N6
	//                   \                      /
	//                    (500k:0)- N3 -(500k:0)
	nodes[0].connect(&nodes[1]).await;
	nodes[1].connect(&nodes[2]).await;
	nodes[1].connect(&nodes[3]).await;
	nodes[2].connect(&nodes[4]).await;
	nodes[3].connect(&nodes[4]).await;
	nodes[4].connect(&nodes[5]).await;
	nodes[5].connect(&nodes[6]).await;

	tokio::time::sleep(Duration::from_millis(1_000)).await;

	// Sender's node index
	let starting_node_idx = 7;
	// Sender must start the Batch workflow by selecting an initial Node
	let initial_node_idx = 4;

	// Starting Batch workflow
	let mut receiver = create_wallet(&[255u8; 64], network, "data/receiver.db".to_string())?;

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
	nodes[starting_node_idx].connect(&nodes[initial_node_idx]).await;
	while !nodes[starting_node_idx].is_peer_connected(&nodes[initial_node_idx].node_id()) {
		tokio::time::sleep(Duration::from_millis(250)).await;
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

	let mut batch_psbts = nodes[starting_node_idx].broker.get_batch_psbts()?;
	while batch_psbts.is_empty() {
		wait_for_block(&bitcoind, 2)?;
		batch_psbts = nodes[starting_node_idx].broker.get_batch_psbts()?;
	}

	// Sender has the final PSBT (signed by all participants) now
	println!("\nSender Node has the fully signed PSBT.\n");
	assert!(batch_psbts.len() == 1);
	let psbt_hex = batch_psbts.first().unwrap();

	let psbt = Psbt::deserialize(&hex::decode(psbt_hex).unwrap()).unwrap();

	println!("Extracting Tx...\n");
	let tx = psbt.clone().extract_tx()?;

	for node in &nodes {
		node.sync_wallet(false)?;
	}

	let receiver_initial_balance = wallet_total_balance(&bitcoind, &mut receiver)?;

	let mut nodes_balance = vec![];
	for node in &nodes {
		nodes_balance.push(node.balance().confirmed);
	}

	println!("\nTx Inputs/Outputs:\n");
	for input in tx.input.iter() {
		let tx_info = bitcoind.get_raw_transaction_info(&input.previous_output.txid, None)?;
		let value = tx_info.vout[input.previous_output.vout as usize].value;
		println!("====> Inputs  ({})", value);
	}

	for output in tx.output.iter() {
		println!("====> Outputs ({})", output.value);
	}

	println!("\nSending Tx (id={})...\n", tx.compute_txid());

	nodes[starting_node_idx].broadcast_transactions(&[&tx])?;
	let tx_id = bitcoind.send_raw_transaction(&tx)?;
	println!("Tx Sent (id={})\n", tx_id);

	wait_for_block(&bitcoind, 3)?;

	for node in &nodes {
		node.sync_wallet(false)?;
	}

	let balance = wallet_total_balance(&bitcoind, &mut receiver)?;
	println!(
		"\n[{}][{}] Receiver Balances (b/a/delta): {} | {} | {}\n",
		nodes[starting_node_idx].node_id(),
		nodes[starting_node_idx].alias(),
		receiver_initial_balance,
		balance,
		balance - receiver_initial_balance,
	);

	for (idx, node) in nodes.iter().enumerate() {
		let before = nodes_balance[idx];
		let balance = node.balance().confirmed;
		if idx == starting_node_idx {
			println!(
				"[{}][{}] Balances (b/a/delta)         : {} | {} | {}",
				node.node_id(),
				node.alias(),
				before,
				balance,
				before - balance,
			);
		} else {
			println!(
				"[{}][{}] Balances (b/a/delta)         : {} | {} | {}",
				node.node_id(),
				node.alias(),
				before,
				balance,
				balance - before,
			);
		}
	}

	for node in &nodes {
		println!("[{}][{}] Stopping...", node.node_id(), node.alias());
		node.stop()?;
	}

	Ok(())
}
