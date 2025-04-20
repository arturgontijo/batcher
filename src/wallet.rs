use std::error::Error;

use bdk_wallet::{template::Bip84, KeychainKind};
use bitcoin::{bip32::Xpriv, secp256k1::PublicKey, Amount, Network};
use bitcoincore_rpc::{Client, RpcApi};

use crate::{broker::Broker, types::PersistedWallet};

pub fn create_wallet(
	seed_bytes: &[u8], network: Network, db_path: String,
) -> Result<PersistedWallet, Box<dyn Error>> {
	let xprv = Xpriv::new_master(network, seed_bytes)
		.map_err(|e| format!("Failed to derive master secret: {}", e))?;

	let descriptor = Bip84(xprv, KeychainKind::External);
	let change_descriptor = Bip84(xprv, KeychainKind::Internal);

	let wallet = Broker::create_persisted_wallet(network, db_path, descriptor, change_descriptor)?;
	Ok(wallet)
}

pub fn create_sender_multisig(
	network: Network, sender_wif: String, node_pubkey: &PublicKey, db_path: String,
) -> Result<PersistedWallet, Box<dyn Error>> {
	// 2-of-2 descriptor
	let descriptor = format!("wsh(multi(2,{},{}))", node_pubkey, sender_wif);
	// Change is controlled just by other
	let change_descriptor = format!("wpkh({})", sender_wif);

	// If we want to also use 2-of-2 for change
	// let change_descriptor = format!("wsh(sortedmulti(2,{},{}))", node_pubkey, sender_pubkey);

	let wallet = Broker::create_persisted_wallet(network, db_path, descriptor, change_descriptor)?;
	Ok(wallet)
}

pub fn sync_wallet(
	client: &Client, wallet: &mut PersistedWallet, debug: bool,
) -> Result<(), Box<dyn Error>> {
	let latest = client.get_block_count()?;
	let stored = wallet.latest_checkpoint().block_id().height as u64;
	if debug {
		println!("    -> WalletSyncBlock: (stored={} | latest={})", stored, latest);
	}
	for height in stored..latest {
		let hash = client.get_block_hash(height)?;
		let block = client.get_block(&hash)?;
		wallet.apply_block(&block, height as u32)?;
	}
	Ok(())
}

pub fn wallet_total_balance(
	bitcoind: &Client, wallet: &mut PersistedWallet,
) -> Result<Amount, Box<dyn Error>> {
	sync_wallet(bitcoind, wallet, false)?;
	let balance = wallet.balance();
	Ok(balance.total())
}
