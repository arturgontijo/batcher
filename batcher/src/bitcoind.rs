use std::env;

use bitcoin::{Address, Amount};
use bitcoincore_rpc::{json::AddressType, Auth, Client, RpcApi};
use bitcoind::BitcoinD;
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};

use crate::types::BoxError;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct BitcoindConfig {
	pub rpc_address: String,
	pub rpc_user: String,
	pub rpc_pass: String,
}

impl BitcoindConfig {
	pub fn new(rpc_address: &str, rpc_user: &str, rpc_pass: &str) -> Self {
		Self {
			rpc_address: rpc_address.to_string(),
			rpc_user: rpc_user.to_string(),
			rpc_pass: rpc_pass.to_string(),
		}
	}
}

pub fn bitcoind_client(
	rpc_address: String, rpc_user: String, rpc_pass: String, wallet: Option<&str>,
) -> Result<Client, bitcoincore_rpc::Error> {
	let auth = Auth::UserPass(rpc_user, rpc_pass);
	let mut client = Client::new(&rpc_address, auth.clone())?;
	if let Some(wallet) = wallet {
		let _ = client.create_wallet(wallet, None, None, None, None);
		client = Client::new(format!("{}/wallet/{}", rpc_address, wallet).as_str(), auth)?;
	}
	Ok(client)
}

pub fn fund_address(
	client: &Client, address: Address, amount: Amount, utxos: u16,
) -> Result<(), BoxError> {
	let _ = client.load_wallet("miner");
	let mut rng = thread_rng();
	for _ in 0..utxos {
		// range -15% and +15%
		let variation_factor = rng.gen_range(-0.15..=0.15);
		let amount_u64 = amount.to_sat();
		let random_amount = amount_u64 as f64 * (1.0 + variation_factor);
		client.send_to_address(
			&address,
			Amount::from_sat(random_amount.round() as u64),
			None,
			None,
			None,
			None,
			None,
			None,
		)?;
	}
	wait_for_block(client, 1)?;
	Ok(())
}

pub fn setup_bitcoind() -> Result<BitcoinD, BoxError> {
	let bitcoind_exe = env::var("BITCOIND_EXE")
		.ok()
		.or_else(|| bitcoind::downloaded_exe_path().ok())
		.expect("bitcoind not found");
	let mut conf = bitcoind::Conf::default();
	conf.args.push("-rpcauth=bitcoind:cccd5d7fd36e55c1b8576b8077dc1b83$60b5676a09f8518dcb4574838fb86f37700cd690d99bd2fdc2ea2bf2ab80ead6");
	conf.args.push("-txindex");
	let bitcoind = BitcoinD::with_conf(bitcoind_exe, &conf)?;
	let _ = bitcoind.client.create_wallet("miner", None, None, None, None);
	wait_for_block(&bitcoind.client, 110)?;
	Ok(bitcoind)
}

pub fn wait_for_block(client: &Client, blocks: u64) -> Result<(), BoxError> {
	let _ = client.load_wallet("miner");
	let address = client
		.get_new_address(Some("miner"), Some(AddressType::Legacy))
		.expect("failed to get new address")
		.assume_checked();
	client.generate_to_address(blocks, &address)?;
	Ok(())
}
