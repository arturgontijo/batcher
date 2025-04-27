use bdk_wallet::SignOptions;
use bitcoin::{secp256k1::PublicKey, Network, Psbt};
use bitcoincore_rpc::{Client, RpcApi};
use node::Node;

use std::{error::Error, sync::Arc, thread::sleep};
use tokio::time::Duration;

use batcher::{
	bitcoind::{wait_for_block, BitcoindConfig},
	config::BrokerConfig,
	node,
	types::PersistedWallet,
	wallet::wallet_total_balance,
};

pub fn setup_nodes(
	count: u8, mut port: u16, network: Network, bitcoind_config: BitcoindConfig,
	broker_config: BrokerConfig,
) -> Result<Vec<Arc<Node>>, Box<dyn Error>> {
	let mut nodes = vec![];
	for i in 0..count {
		let node = Arc::new(Node::new(
			format!("node-{}", i),
			&[42 + i; 32],
			"0.0.0.0".to_string(),
			port,
			network,
			bitcoind_config.clone(),
			&[42 + i; 32],
			format!("/tmp/batcher/wallet_{}.db", i),
			broker_config.clone(),
		)?);
		let node_clone = node.clone();
		node_clone.start()?;
		nodes.push(node);
		port += 1;
	}
	sleep(Duration::from_secs(1));
	Ok(nodes)
}

pub fn broadcast_tx(
	bitcoind: &Client, starting_node: &Node, nodes: &Vec<Arc<Node>>,
	receiver: &mut PersistedWallet,
	multisig_signers: Option<(&Node, &PublicKey, &PersistedWallet)>,
) -> Result<(), Box<dyn Error>> {
	let mut batch_psbts = starting_node.broker.get_batch_psbts()?;
	while batch_psbts.is_empty() {
		wait_for_block(bitcoind, 2)?;
		batch_psbts = starting_node.broker.get_batch_psbts()?;
	}

	// Sender has the final PSBT (signed by all participants) now
	println!("\nSender Node has the fully signed PSBT.\n");
	assert!(batch_psbts.len() == 1);
	let psbt_hex = batch_psbts.first().unwrap();

	let mut psbt = Psbt::deserialize(&psbt_hex).unwrap();

	if let Some((node, sender_pubkey, sender)) = multisig_signers {
		// Multisig signatures
		node.multisig_sign_psbt(&sender_pubkey, &mut psbt)?;
		sender.sign(&mut psbt, SignOptions::default())?;
	}

	println!("Extracting Tx...\n");
	let tx = psbt.clone().extract_tx()?;

	for node in nodes {
		node.sync_wallet(false)?;
	}

	let receiver_initial_balance = wallet_total_balance(bitcoind, receiver)?;

	let mut nodes_balance = vec![];
	for node in nodes {
		nodes_balance.push(node.balance().confirmed);
	}

	println!("\nTx Inputs/Outputs:\n");
	for input in tx.input.iter() {
		let tx_info = bitcoind.get_raw_transaction_info(&input.previous_output.txid, None)?;
		let value = tx_info.vout[input.previous_output.vout as usize].value;
		println!("====> In  ({})", value);
	}

	for output in tx.output.iter() {
		println!("====> Out ({})", output.value);
	}

	println!("\nSending Tx (id={})...\n", tx.compute_txid());

	starting_node.broadcast_transactions(&[&tx])?;
	let tx_id = bitcoind.send_raw_transaction(&tx)?;
	println!("Tx Sent (id={})\n", tx_id);

	wait_for_block(&bitcoind, 3)?;

	for node in nodes {
		node.sync_wallet(false)?;
	}

	let balance = wallet_total_balance(&bitcoind, receiver)?;
	println!(
		"\n[{}][{}] Receiver Balances (b/a/delta): {} | {} | {}\n",
		starting_node.node_id(),
		starting_node.alias(),
		receiver_initial_balance,
		balance,
		balance - receiver_initial_balance,
	);

	for (idx, node) in nodes.iter().enumerate() {
		let before = nodes_balance[idx];
		let balance = node.balance().confirmed;
		if multisig_signers.is_none() && node.node_id() == starting_node.node_id() {
			println!(
				"[{}][{}] Balances (b/a/delta)         : {} | {} | -{}",
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

	Ok(())
}
