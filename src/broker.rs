use std::{
	error::Error,
	sync::{Arc, Mutex},
};

use bdk_wallet::{
	bitcoin::{bip32::Xpriv, Amount, Network},
	file_store::Store,
	template::Bip84,
	Balance, KeychainKind, PersistedWallet, SignOptions, Wallet,
};
use bitcoin::{
	absolute::LockTime,
	psbt::{Input, Output},
	secp256k1::PublicKey,
	FeeRate, Psbt, ScriptBuf, Transaction, TxIn, TxOut,
};
use bitcoincore_rpc::{Client, RpcApi};

use crate::messages::{BatchMessage, BatchMessageHandler};

const DB_MAGIC: &str = "bdk-rpc-wallet-example";

#[derive(Clone)]
pub struct Broker {
	pub node_id: PublicKey,
	pub custom_message_handler: Arc<BatchMessageHandler>,
	pub wallet: Arc<Mutex<PersistedWallet<Store<bdk_wallet::ChangeSet>>>>,
	pub batch_psbts: Arc<Mutex<Vec<String>>>,
}

impl Broker {
	pub fn new(
		node_id: PublicKey, seed_bytes: &[u8; 32], network: Network, db_path: &str,
		custom_message_handler: Arc<BatchMessageHandler>,
	) -> Result<Self, Box<dyn Error>> {
		let mut db =
			Store::<bdk_wallet::ChangeSet>::open_or_create_new(DB_MAGIC.as_bytes(), db_path)?;

		let xprv = Xpriv::new_master(network, seed_bytes)
			.map_err(|e| format!("Failed to derive master secret: {}", e))?;

		let descriptor = Bip84(xprv, KeychainKind::External);
		let change_descriptor = Bip84(xprv, KeychainKind::Internal);

		let wallet_opt = Wallet::load()
			.descriptor(KeychainKind::External, Some(descriptor.clone()))
			.descriptor(KeychainKind::Internal, Some(change_descriptor.clone()))
			.extract_keys()
			.check_network(network)
			.load_wallet(&mut db)?;

		let wallet = match wallet_opt {
			Some(wallet) => wallet,
			None => Wallet::create(descriptor.clone(), change_descriptor.clone())
				.network(network)
				.create_wallet(&mut db)?,
		};

		let batch_psbts = Arc::new(Mutex::new(vec![]));
		Ok(Broker {
			node_id,
			custom_message_handler,
			wallet: Arc::new(Mutex::new(wallet)),
			batch_psbts,
		})
	}

	pub fn balance(&self) -> Balance {
		self.wallet.lock().unwrap().balance()
	}

	pub fn build_psbt(
		&self, output_script: ScriptBuf, amount: Amount, fee_rate: FeeRate, locktime: LockTime,
	) -> Result<Psbt, Box<dyn Error>> {
		let mut binding = self.wallet.lock().unwrap();
		let mut tx_builder = binding.build_tx();
		tx_builder.add_recipient(output_script, amount).fee_rate(fee_rate).nlocktime(locktime);

		let psbt = match tx_builder.finish() {
			Ok(psbt) => psbt,
			Err(err) => return Err(err.into()),
		};

		Ok(psbt)
	}

	pub fn add_utxos_to_psbt(
		&self, psbt: &mut Psbt, max_count: u16, uniform_amount: Option<Amount>, fee: Amount,
		payer: bool,
	) -> Result<(), Box<dyn std::error::Error>> {
		let mut count = 0;
		let mut receiver_utxos_value = Amount::from_sat(0);
		let mut wallet = self.wallet.lock().unwrap();
		let utxos: Vec<_> = wallet.list_unspent().collect();
		for utxo in utxos {
			let mut inserted = false;
			for input in psbt.unsigned_tx.input.clone() {
				if input.previous_output.txid == utxo.outpoint.txid
					&& input.previous_output.vout == utxo.outpoint.vout
				{
					inserted = true;
				}
			}
			if inserted {
				continue;
			}

			if let Some(canonical_tx) =
				wallet.transactions().find(|tx| tx.tx_node.compute_txid() == utxo.outpoint.txid)
			{
				let tx = (*canonical_tx.tx_node.tx).clone();
				let input = TxIn {
					previous_output: utxo.outpoint,
					script_sig: Default::default(),
					sequence: Default::default(),
					witness: Default::default(),
				};

				println!(
					"[{}] Adding UTXO [txid={:?} | vout={:?} | amt={}]",
					self.node_id, utxo.outpoint.txid, utxo.outpoint.vout, utxo.txout.value
				);

				psbt.inputs.push(Input { non_witness_utxo: Some(tx), ..Default::default() });
				psbt.unsigned_tx.input.push(input);
				receiver_utxos_value += utxo.txout.value;

				count += 1;
				if count >= max_count {
					break;
				}
			};
		}

		let mut value = receiver_utxos_value;
		if payer {
			value -= fee;
		} else {
			value += fee
		}

		if let Some(uniform_amount) = uniform_amount {
			let script_pubkey =
				wallet.reveal_next_address(KeychainKind::External).address.script_pubkey();

			let output = TxOut { value: uniform_amount, script_pubkey };
			psbt.outputs.push(Output::default());
			psbt.unsigned_tx.output.push(output);
			value -= uniform_amount;
		}

		let script_pubkey =
			wallet.reveal_next_address(KeychainKind::External).address.script_pubkey();

		let output = TxOut { value, script_pubkey };
		psbt.outputs.push(Output::default());
		psbt.unsigned_tx.output.push(output);

		Ok(())
	}

	pub fn sign_psbt(&self, psbt: &mut Psbt) -> Result<(), Box<dyn Error>> {
		self.wallet.lock().unwrap().sign(psbt, SignOptions::default())?;
		Ok(())
	}

	pub fn send(&self, their_node_id: PublicKey, msg: BatchMessage) -> Result<(), Box<dyn Error>> {
		println!("[{}] signer.send({})", self.node_id, their_node_id);
		self.custom_message_handler.send(their_node_id, msg);
		Ok(())
	}

	pub fn push_to_batch_psbts(&self, psbt_hex: String) -> Result<(), Box<dyn Error>> {
		let mut unlocked = self.batch_psbts.lock().unwrap();
		unlocked.push(psbt_hex.clone());
		Ok(())
	}

	pub fn get_batch_psbts(&self) -> Result<Vec<String>, Box<dyn Error>> {
		match self.batch_psbts.try_lock() {
			Ok(mut psbts) => Ok(std::mem::take(&mut *psbts)),
			Err(_) => Ok(vec![]),
		}
	}

	pub fn broadcast_transactions(&self, _txs: &[&Transaction]) -> Result<(), Box<dyn Error>> {
		// self.broadcaster.broadcast_transactions(txs);
		Ok(())
	}
}

pub fn create_wallet(
	seed_bytes: &[u8], network: Network,
) -> Result<Wallet, Box<dyn std::error::Error>> {
	let xprv = Xpriv::new_master(network, seed_bytes)
		.map_err(|e| format!("Failed to derive master secret: {}", e))?;

	let descriptor = Bip84(xprv, KeychainKind::External);
	let change_descriptor = Bip84(xprv, KeychainKind::Internal);

	let wallet = Wallet::create(descriptor, change_descriptor)
		.network(network)
		.create_wallet_no_persist()
		.map_err(|e| format!("Failed to set up wallet: {}", e))?;

	Ok(wallet)
}

pub fn sync_wallet(
	client: &Client, wallet: &mut Wallet, debug: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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
	if debug {
		println!("    -> WalletSyncBlock: Done!");
	}
	Ok(())
}

pub fn wallet_total_balance(
	bitcoind: &Client, wallet: &mut Wallet,
) -> Result<Amount, Box<dyn std::error::Error>> {
	sync_wallet(bitcoind, wallet, false)?;
	let balance = wallet.balance();
	Ok(balance.total())
}
