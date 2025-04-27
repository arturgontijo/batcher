use std::{error::Error, thread::sleep, time::Duration};

use bitcoin::{Address, Amount};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use rand::{thread_rng, Rng};
use serde::{Deserialize, Serialize};

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
	let mut bitcoind = Client::new(&rpc_address, auth.clone())?;
	if let Some(wallet) = wallet {
		let _ = bitcoind.create_wallet(wallet, None, None, None, None);
		bitcoind = Client::new(format!("{}/wallet/{}", rpc_address, wallet).as_str(), auth)?;
	}
	Ok(bitcoind)
}

pub fn fund_address(
	client: &Client, address: Address, amount: Amount, utxos: u16,
) -> Result<(), Box<dyn Error>> {
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
	Ok(())
}

pub fn wait_for_block(bitcoind: &Client, blocks: u64) -> Result<(), Box<dyn Error>> {
	let initial_block = bitcoind.get_block_count()?;
	let target_block = initial_block + blocks;
	loop {
		let block_num = bitcoind.get_block_count()?;
		if block_num >= target_block {
			break;
		}
		println!("    -> Block {:?} [target={:?}]", block_num, target_block);
		sleep(Duration::from_secs(11));
	}
	Ok(())
}
