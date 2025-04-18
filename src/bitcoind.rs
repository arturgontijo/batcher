use std::{thread::sleep, time::Duration};

use bitcoincore_rpc::{Auth, Client, RpcApi};

pub fn bitcoind_client(wallet: &str) -> Result<Client, bitcoincore_rpc::Error> {
	let auth = Auth::UserPass("local".to_string(), "local".to_string());
	let mut bitcoind = Client::new("http://0.0.0.0:38332", auth.clone())?;
	let _ = bitcoind.create_wallet(wallet, None, None, None, None);
	bitcoind = Client::new(format!("http://0.0.0.0:38332/wallet/{}", wallet).as_str(), auth)?;
	Ok(bitcoind)
}

pub fn wait_for_block(bitcoind: &Client, blocks: u64) -> Result<(), Box<dyn std::error::Error>> {
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
