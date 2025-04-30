mod common;

use batcher::bitcoind::BitcoindConfig;
use batcher::bitcoind::{self, setup_bitcoind};
use batcher::config::BrokerConfig;
use batcher::wallet;

use bdk_wallet::KeychainKind;
use bitcoin::PrivateKey;
use bitcoin::{
	absolute::LockTime,
	key::Secp256k1,
	policy::DEFAULT_MIN_RELAY_TX_FEE,
	secp256k1::{PublicKey, SecretKey},
	Amount, FeeRate, Network, NetworkKind,
};

use bitcoind::{fund_address, wait_for_block};
use common::{broadcast_tx, create_temp_dir, remove_temp_dir, setup_nodes};
use wallet::{create_sender_multisig, create_wallet, wallet_total_balance};

use std::error::Error;
use std::thread::sleep;
use std::u8;
use tokio::time::Duration;

#[test]
fn batcher_as_multisig() -> Result<(), Box<dyn Error>> {
	let network = Network::Regtest;

	let bitcoind = setup_bitcoind()?;

	let temp_dir = create_temp_dir("batcher_as_multisig")?;

	let bitcoind_config = BitcoindConfig::new(&bitcoind.rpc_url(), "bitcoind", "bitcoind");

	let broker_config = BrokerConfig::new(vec![], 25_000, 4);
	let nodes = setup_nodes(12, 7777, network, bitcoind_config.clone(), broker_config)?;

	for (idx, node) in nodes.iter().enumerate() {
		// N2 and N10 must have no UTXOs
		if idx == 2 || idx == 10 {
			continue;
		}
		fund_address(
			&bitcoind.client,
			node.wallet_new_address()?,
			Amount::from_sat(1_000_000),
			10,
		)?;
	}

	wait_for_block(&bitcoind.client, 2)?;

	for node in &nodes {
		node.sync_wallet(true)?;
	}

	for node in &nodes {
		println!("[{}][{}] Balance: {}", node.node_id(), node.alias(), node.balance().total());
	}

	//      N5     N6 -- N7         N8
	//        \  /                 /
	//   N0 -- N1 -- N2 -- N3 -- N4
	//                \            \
	//                 N9           N10 -- N11
	nodes[0].connect(&nodes[1])?;
	nodes[1].connect(&nodes[2])?;
	nodes[1].connect(&nodes[5])?;
	nodes[1].connect(&nodes[6])?;
	nodes[6].connect(&nodes[7])?;
	nodes[2].connect(&nodes[3])?;
	nodes[2].connect(&nodes[9])?;
	nodes[3].connect(&nodes[4])?;
	nodes[4].connect(&nodes[8])?;
	nodes[4].connect(&nodes[10])?;
	nodes[10].connect(&nodes[11])?;

	sleep(Duration::from_millis(1_000));

	// Multisig party node index
	let starting_node_idx = 6;
	// Multisig node must start the Batch workflow by selecting an initial Node
	let initial_node_idx = 1;

	// Multisig party node must be connected to an initial Node
	while !nodes[starting_node_idx].is_peer_connected(&nodes[initial_node_idx].node_id()) {
		sleep(Duration::from_millis(250));
		println!(
			"Connecting to {} -> {} ...",
			nodes[starting_node_idx].alias(),
			nodes[initial_node_idx].alias()
		);
	}

	// Starting Batch workflow
	let mut receiver =
		create_wallet(&[255u8; 64], network, format!("{}/receiver.db", temp_dir.display()))?;

	let amount = Amount::from_sat(777_777);
	let script_pubkey =
		receiver.reveal_next_address(KeychainKind::External).address.script_pubkey();
	let uniform_amount = true;
	let fee_rate = FeeRate::from_sat_per_vb(DEFAULT_MIN_RELAY_TX_FEE as u64).unwrap();
	let locktime: LockTime = LockTime::ZERO;
	let max_utxo_count = 4;
	let fee_per_participant = Amount::from_sat(99_999);
	let max_participants = 10;
	let max_hops = u8::MAX;

	let secp = Secp256k1::new();
	let secret_key = SecretKey::from_slice(&[77u8; 32])?;
	let sender_pubkey = PublicKey::from_secret_key(&secp, &secret_key);
	let sender_pk = PrivateKey { compressed: true, network: NetworkKind::Test, inner: secret_key };
	let mut sender_multisig = create_sender_multisig(
		network,
		sender_pk.to_wif(),
		&nodes[starting_node_idx].pubkey(),
		format!("{}/multisig_by_sender.db", temp_dir.display()),
	)?;

	nodes[starting_node_idx].create_multisig(
		network,
		&sender_pubkey,
		format!("{}/multisig_by_node.db", temp_dir.display()),
	)?;

	let multisig_funding_addr = nodes[starting_node_idx].multisig_new_address(&sender_pubkey)?;

	// Funding multisig
	fund_address(&bitcoind.client, multisig_funding_addr, Amount::from_sat(1_000_000), 10)?;
	wait_for_block(&bitcoind.client, 2)?;

	nodes[starting_node_idx].multisig_sync(&sender_pubkey, true)?;
	let multisig_balance = wallet_total_balance(&bitcoind.client, &mut sender_multisig)?;

	println!(
		"MultisigNode(balance)   -> {}",
		nodes[starting_node_idx].multisig_balance(&sender_pubkey)?.total()
	);
	println!("MultisigSender(balance) -> {}", multisig_balance);

	nodes[starting_node_idx].init_multisig_psbt_batch(
		&sender_pubkey,
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

	broadcast_tx(
		&bitcoind.client,
		&nodes[starting_node_idx],
		&nodes,
		&mut receiver,
		Some((&nodes[starting_node_idx], &sender_pubkey, &sender_multisig)),
	)?;

	if multisig_balance.to_sat() > 0 {
		let balance = wallet_total_balance(&bitcoind.client, &mut sender_multisig)?;
		println!(
			"\n[{}][{}] Multisig Balances (b/a/delta): {} | {} | -{}\n",
			nodes[starting_node_idx].node_id(),
			nodes[starting_node_idx].alias(),
			multisig_balance,
			balance,
			multisig_balance - balance,
		);
	}

	for node in &nodes {
		println!("[{}][{}] Stopping...", node.node_id(), node.alias());
		node.stop()?;
	}

	remove_temp_dir("batcher_as_multisig")?;

	Ok(())
}
