mod common;

use std::sync::Arc;

use batcher::{
	bitcoind::{fund_address, setup_bitcoind, wait_for_block},
	config::{BrokerConfig, LoggerConfig, NodeConfig, WalletConfig},
	node::Node,
	storage::BatchPsbtStatus,
	types::BoxError,
	wallet::create_wallet,
};

use bdk_wallet::KeychainKind;
use bitcoin::{absolute::LockTime, Amount, FeeRate, Network};
use common::{broadcast_tx, create_temp_dir, setup_nodes};

#[test]
fn batcher_from_config() -> Result<(), BoxError> {
	let network = Network::Regtest;

	let bitcoind = setup_bitcoind()?;

	let temp_dir = create_temp_dir("batcher_from_config")?;

	let mut config = NodeConfig::new("config.example.toml")?;
	config.bitcoind.rpc_address = bitcoind.rpc_url();
	config.wallet.file_path = format!("{}/wallet_from_config.db", temp_dir.display());
	config.peers_db_path = format!("{}/peers_from_config.db", temp_dir.display());
	config.logger.file_path = format!("{}/logger_from_config.log", temp_dir.display());
	config.broker.storage_path = format!("{}/broker_from_config.db", temp_dir.display());

	let bitcoind_config = config.bitcoind.clone();

	let mut others =
		setup_nodes(3, 7777, network, bitcoind_config.clone(), &temp_dir, vec![], 25_000, 2, 60)?;

	// Node that does not participate
	let np_config = NodeConfig {
		alias: "NpNode".to_string(),
		secret: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
		host: "0.0.0.0".to_string(),
		port: 7071,
		network,
		peers_db_path: format!("{}/peers_np.db", temp_dir.display()),
		wallet: WalletConfig {
			mnemonic:
				"egg glad reflect finish crash veteran tiny dance blouse garlic stock solution"
					.to_string(),
			file_path: format!("{}/wallet_np.db", temp_dir.display()),
		},
		logger: LoggerConfig::new(
			format!("{}/logger_np.log", temp_dir.display()),
			"trace".to_string(),
		),
		broker: BrokerConfig {
			storage_path: format!("{}/broker_np.db", temp_dir.display()),
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
			wallet_sync_interval: 60,
		},
		bitcoind: bitcoind_config.clone(),
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

	node.sync_wallet()?;
	for node in &others {
		node.sync_wallet()?;
	}

	// Starting Batch workflow
	let mut receiver =
		create_wallet(&[255u8; 64], network, format!("{}/receiver.db", temp_dir.display()))?;

	let amount = Amount::from_sat(777_777);
	let script_pubkey =
		receiver.reveal_next_address(KeychainKind::External).address.script_pubkey();
	let uniform_amount = true;
	let fee_rate = FeeRate::from_sat_per_vb(50).unwrap();
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

	node.sync_wallet()?;
	for node in &others {
		node.sync_wallet()?;
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

	let mut batch_psbts = node.broker.stored_psbts(BatchPsbtStatus::Ready)?;
	while batch_psbts.is_empty() {
		wait_for_block(&bitcoind.client, 2)?;
		batch_psbts = node.broker.stored_psbts(BatchPsbtStatus::Ready)?;
	}

	let batch_psbt = batch_psbts.first().unwrap();
	let mut psbt = batch_psbt.psbt.clone();

	np_node.sign_psbt(&mut psbt)?;
	np_node.broker.upsert_psbt(Some(0), BatchPsbtStatus::Ready, &psbt)?;

	broadcast_tx(&bitcoind.client, &np_node, &others, &mut receiver, None)?;

	for node in &others {
		println!("[{}][{}] Stopping...", node.node_id(), node.alias());
		node.stop()?;
	}

	Ok(())
}
