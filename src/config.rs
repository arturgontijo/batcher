use std::{error::Error, fs};

use bitcoin::{secp256k1::PublicKey, Network};
use serde::{Deserialize, Serialize};

use crate::bitcoind::BitcoindConfig;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NodeConfig {
	pub alias: String,
	pub secret: String,
	pub host: String,
	pub port: u16,
	pub network: Network,
	pub mnemonic: String,
	pub db_path: String,
	pub broker_config: BrokerConfig,
	pub bitcoind_config: BitcoindConfig,
}

impl NodeConfig {
	pub fn new(file_path: &str) -> Result<Self, Box<dyn Error>> {
		let file_contents = fs::read_to_string(file_path)?;
		let config: NodeConfig = toml::from_str(&file_contents)?;
		Ok(config)
	}
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct BrokerConfig {
	pub bootnodes: Vec<(PublicKey, String)>,
	pub minimum_fee: u64,
	pub max_utxo_count: u8,
}

impl BrokerConfig {
	pub fn new(bootnodes: Vec<(PublicKey, String)>, minimum_fee: u64, max_utxo_count: u8) -> Self {
		Self { bootnodes, minimum_fee, max_utxo_count }
	}
}
