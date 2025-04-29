mod common;

use std::{error::Error, sync::Arc};

use batcher::{
	bitcoind::{fund_address, setup_bitcoind, wait_for_block},
	config::{BrokerConfig, NodeConfig},
	node::Node,
	wallet::create_wallet,
};

use bdk_wallet::KeychainKind;
use bitcoin::{
	absolute::LockTime, policy::DEFAULT_MIN_RELAY_TX_FEE, Amount, FeeRate, Network, Psbt,
};
use common::{broadcast_tx, setup_nodes};

#[test]
fn node_from_config() -> Result<(), Box<dyn Error>> {
	let network = Network::Regtest;

	let bitcoind = setup_bitcoind()?;

	let mut config = NodeConfig::new("config.toml")?;
	config.bitcoind_config.rpc_address = bitcoind.rpc_url();

	let bitcoind_config = config.bitcoind_config.clone();

	let broker_config = BrokerConfig::new(vec![], 25_000, 2);
	let mut others = setup_nodes(3, 7777, network, bitcoind_config.clone(), broker_config)?;

	// Node that does not participate
	let np_config = NodeConfig {
		alias: "NpNode".to_string(),
		secret: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
		host: "0.0.0.0".to_string(),
		port: 7071,
		network,
		mnemonic: "egg glad reflect finish crash veteran tiny dance blouse garlic stock solution"
			.to_string(),
		db_path: "/tmp/batcher/wallet_np.db".to_string(),
		broker_config: BrokerConfig {
			bootnodes: vec![
				// Node[0]
				(others[0].node_id(), others[0].endpoint()),
				// Node[1]
				(others[1].node_id(), others[1].endpoint()),
				// Node[2]
				(others[2].node_id(), others[2].endpoint()),
			],
			minimum_fee: 1_000_000,
			max_utxo_count: 1,
		},
		bitcoind_config: bitcoind_config.clone(),
	};
	let np_node = Arc::new(Node::new_from_config(np_config)?);
	np_node.start()?;

	others.push(np_node.clone());

	let node = Arc::new(Node::new_from_config(config.clone())?);
	node.start()?;

	fund_address(&bitcoind.client, node.wallet_new_address()?, Amount::from_sat(1_000_000), 5)?;
	for node in &others {
		fund_address(&bitcoind.client, node.wallet_new_address()?, Amount::from_sat(1_000_000), 5)?;
	}

	wait_for_block(&bitcoind.client, 2)?;

	//            N0
	//          /
	//   N -- Np -- N1
	//          \
	//            N2

	node.sync_wallet(false)?;
	for node in &others {
		node.sync_wallet(false)?;
	}

	// Starting Batch workflow
	let mut receiver =
		create_wallet(&[255u8; 64], network, "/tmp/batcher/receiver.db".to_string())?;

	let amount = Amount::from_sat(777_777);
	let script_pubkey =
		receiver.reveal_next_address(KeychainKind::External).address.script_pubkey();
	let uniform_amount = true;
	let fee_rate = FeeRate::from_sat_per_vb(DEFAULT_MIN_RELAY_TX_FEE as u64).unwrap();
	let locktime: LockTime = LockTime::ZERO;
	let max_utxo_per_participant = 4;
	let fee_per_participant = Amount::from_sat(99_999);
	let max_participants = 3;
	let max_hops = 4;

	node.init_psbt_batch(
		np_node.node_id(),
		script_pubkey,
		amount,
		fee_rate,
		locktime,
		uniform_amount,
		fee_per_participant,
		max_participants,
		max_utxo_per_participant,
		max_hops,
	)?;

	others.push(node.clone());

	broadcast_tx(&bitcoind.client, &node, &others, &mut receiver, None)?;

	// Foreign workflow
	println!("\n----- Foreign workflow -----\n");

	node.sync_wallet(false)?;
	for node in &others {
		node.sync_wallet(false)?;
	}

	let script_pubkey =
		receiver.reveal_next_address(KeychainKind::External).address.script_pubkey();
	let change_scriptbuf = np_node.wallet_new_address()?.script_pubkey();
	let utxos = np_node.broker.list_unspent()?;

	node.build_foreign_psbt(
		change_scriptbuf,
		script_pubkey,
		amount,
		utxos[..2].to_vec(),
		locktime,
		uniform_amount,
		fee_per_participant,
		max_participants,
		max_utxo_per_participant,
		max_hops,
	)?;

	let mut batch_psbts = node.broker.get_batch_psbts()?;
	while batch_psbts.is_empty() {
		wait_for_block(&bitcoind.client, 2)?;
		batch_psbts = node.broker.get_batch_psbts()?;
	}

	let psbt_hex = batch_psbts.first().unwrap();
	let mut psbt = Psbt::deserialize(&psbt_hex)?;

	np_node.sign_psbt(&mut psbt)?;
	np_node.broker.push_to_batch_psbts(psbt.serialize())?;

	broadcast_tx(&bitcoind.client, &np_node, &others, &mut receiver, None)?;

	for node in &others {
		println!("[{}][{}] Stopping...", node.node_id(), node.alias());
		node.stop()?;
	}

	Ok(())
}
