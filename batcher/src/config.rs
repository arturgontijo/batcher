use std::fs;

use bitcoin::{secp256k1::PublicKey, Network};
use serde::{Deserialize, Serialize};

use crate::{bitcoind::BitcoindConfig, types::BoxError};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeConfig {
	pub alias: String,
	pub secret: String,
	pub host: String,
	pub port: u16,
	pub network: Network,
	pub peers_db_path: String,
	pub wallet: WalletConfig,
	pub logger: LoggerConfig,
	pub broker: BrokerConfig,
	pub bitcoind: BitcoindConfig,
}

impl NodeConfig {
	pub fn new(file_path: &str) -> Result<Self, BoxError> {
		let file_contents = fs::read_to_string(file_path)?;
		let config: NodeConfig = toml::from_str(&file_contents)?;
		Ok(config)
	}
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct WalletConfig {
	pub file_path: String,
	pub mnemonic: String,
}

impl WalletConfig {
	pub fn new(file_path: String, mnemonic: String) -> Self {
		Self { file_path, mnemonic }
	}
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BrokerConfig {
	pub storage_path: String,
	pub bootnodes: Vec<(PublicKey, String)>,
	pub minimum_fee: u64,
	pub max_utxo_count: u8,
	pub wallet_sync_interval: u8,
}

impl BrokerConfig {
	pub fn new(
		storage_path: String, bootnodes: Vec<(PublicKey, String)>, minimum_fee: u64,
		max_utxo_count: u8, wallet_sync_interval: u8,
	) -> Self {
		Self { storage_path, bootnodes, minimum_fee, max_utxo_count, wallet_sync_interval }
	}
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct LoggerConfig {
	pub file_path: String,
	pub max_level: String,
}

impl LoggerConfig {
	pub fn new(file_path: String, max_level: String) -> Self {
		Self { file_path, max_level }
	}
}
