use std::{
	collections::HashMap,
	error::Error,
	sync::{Arc, Mutex},
};

use bdk_wallet::{
	bitcoin::{bip32::Xpriv, Amount, Network},
	descriptor::IntoWalletDescriptor,
	rusqlite::Connection,
	template::Bip84,
	Balance, KeychainKind, SignOptions, Wallet,
};
use bitcoin::{
	absolute::LockTime,
	key::Secp256k1,
	opcodes,
	psbt::{Input, Output},
	secp256k1::{PublicKey, SecretKey},
	Address, FeeRate, NetworkKind, Psbt, ScriptBuf, Transaction, TxIn, TxOut,
};
use bitcoincore_rpc::{Auth, Client};

use crate::{
	messages::{BatchMessage, BatchMessageHandler},
	types::PersistedWallet,
	wallet::sync_wallet,
};

#[derive(Clone)]
pub struct Broker {
	pub node_id: PublicKey,
	pub node_alias: String,
	pub custom_message_handler: Arc<BatchMessageHandler>,
	pub pubkey: PublicKey,
	pub wif: String,
	pub wallets: Arc<Mutex<HashMap<PublicKey, PersistedWallet>>>,
	pub batch_psbts: Arc<Mutex<Vec<String>>>,
	pub bitcoind_client: Arc<Client>,
	pub persister: Arc<Mutex<Connection>>,
}

impl Broker {
	pub fn new(
		node_id: PublicKey, node_alias: String, seed_bytes: &[u8; 32], network: Network,
		db_path: String, custom_message_handler: Arc<BatchMessageHandler>,
	) -> Result<Self, Box<dyn Error>> {
		let auth = Auth::UserPass("local".to_string(), "local".to_string());
		let bitcoind_client = Arc::new(Client::new("http://0.0.0.0:38332", auth.clone())?);

		let secp = Secp256k1::new();
		let secret_key = SecretKey::from_slice(seed_bytes)?;
		let pubkey = PublicKey::from_secret_key(&secp, &secret_key);

		let wif =
			bitcoin::PrivateKey { compressed: true, network: NetworkKind::Test, inner: secret_key }
				.to_wif();

		let mut persister = Connection::open(db_path)?;
		let wallet = Self::create_wallet(seed_bytes, network, &mut persister)?;

		let wallets = Arc::new(Mutex::new(HashMap::from([(pubkey, wallet)])));

		let batch_psbts = Arc::new(Mutex::new(vec![]));

		Ok(Broker {
			node_id,
			node_alias,
			custom_message_handler,
			pubkey,
			wif,
			wallets,
			batch_psbts,
			bitcoind_client,
			persister: Arc::new(Mutex::new(persister)),
		})
	}

	fn create_wallet(
		seed_bytes: &[u8; 32], network: Network, persister: &mut Connection,
	) -> Result<PersistedWallet, Box<dyn Error>> {
		let xprv = Xpriv::new_master(network, seed_bytes)
			.map_err(|e| format!("Failed to derive master secret: {}", e))?;
		Self::create_persisted_wallet(
			network,
			persister,
			Bip84(xprv, KeychainKind::External),
			Bip84(xprv, KeychainKind::Internal),
		)
	}

	pub fn create_multisig(
		&self, network: Network, other: &PublicKey, db_path: String,
	) -> Result<(), Box<dyn Error>> {
		let mut wallets = self.wallets.lock().unwrap();
		// 2-of-2 descriptor
		let descriptor = format!("wsh(multi(2,{},{}))", self.wif, other);
		// Change is controlled just by other
		let change_descriptor = format!("wpkh({})", other);

		// If we want to also use 2-of-2 for change
		// let change_descriptor = format!("wsh(sortedmulti(2,{},{}))", other, self.pubkey);

		let mut persister = Connection::open(db_path)?;
		let wallet =
			Self::create_persisted_wallet(network, &mut persister, descriptor, change_descriptor)?;
		wallets.insert(*other, wallet);
		Ok(())
	}

	pub fn create_persisted_wallet<D: IntoWalletDescriptor + Send + Clone + 'static>(
		network: Network, persister: &mut Connection, descriptor: D, change_descriptor: D,
	) -> Result<PersistedWallet, Box<dyn Error>> {
		let wallet_opt = Wallet::load()
			.descriptor(KeychainKind::External, Some(descriptor.clone()))
			.descriptor(KeychainKind::Internal, Some(change_descriptor.clone()))
			.extract_keys()
			.check_network(network)
			.load_wallet(persister)?;

		let wallet = match wallet_opt {
			Some(wallet) => wallet,
			None => Wallet::create(descriptor, change_descriptor)
				.network(network)
				.create_wallet(persister)?,
		};
		Ok(wallet)
	}

	pub fn sync_wallet(&self, debug: bool) -> Result<(), Box<dyn Error>> {
		let mut binding = self.wallets.lock().unwrap();
		let wallet = binding.get_mut(&self.pubkey).unwrap();
		sync_wallet(&self.bitcoind_client, wallet, debug)?;
		let mut persister = self.persister.lock().unwrap();
		wallet.persist(&mut persister)?;
		Ok(())
	}

	pub fn multisig_sync(&self, other: &PublicKey, debug: bool) -> Result<(), Box<dyn Error>> {
		let mut multisigs = self.wallets.lock().unwrap();
		if let Some(wallet) = multisigs.get_mut(other) {
			sync_wallet(&self.bitcoind_client, wallet, debug)?;
		}
		Ok(())
	}

	pub fn multisig_new_address(&self, other: &PublicKey) -> Result<Address, Box<dyn Error>> {
		let mut multisigs = self.wallets.lock().unwrap();
		if let Some(wallet) = multisigs.get_mut(other) {
			let address = wallet.reveal_next_address(KeychainKind::External).address;
			return Ok(address);
		}
		Err("Multisig not found!".into())
	}

	pub fn wallet_new_address(&self) -> Result<Address, Box<dyn Error>> {
		let mut wallets = self.wallets.lock().unwrap();
		let wallet = wallets.get_mut(&self.pubkey).unwrap();
		let address = wallet.reveal_next_address(KeychainKind::External).address;
		Ok(address)
	}

	pub fn balance(&self) -> Balance {
		self.wallets.lock().unwrap().get(&self.pubkey).unwrap().balance()
	}

	pub fn multisig_balance(&self, other: &PublicKey) -> Result<Balance, Box<dyn Error>> {
		let multisigs = self.wallets.lock().unwrap();
		match multisigs.get(other) {
			Some(wallet) => Ok(wallet.balance()),
			None => Err("Multisig not found!".into()),
		}
	}

	pub fn build_psbt(
		&self, output_script: ScriptBuf, amount: Amount, fee_rate: FeeRate, locktime: LockTime,
	) -> Result<Psbt, Box<dyn Error>> {
		let mut wallets = self.wallets.lock().unwrap();
		let wallet = wallets.get_mut(&self.pubkey).unwrap();
		self._build_psbt(wallet, output_script, amount, fee_rate, locktime)
	}

	pub fn multisig_build_psbt(
		&self, other: &PublicKey, script_pubkey: ScriptBuf, amount: Amount, fee_rate: FeeRate,
		locktime: LockTime,
	) -> Result<Psbt, Box<dyn Error>> {
		let mut multisigs = self.wallets.lock().unwrap();
		if let Some(wallet) = multisigs.get_mut(other) {
			let psbt = self._build_psbt(wallet, script_pubkey, amount, fee_rate, locktime)?;
			return Ok(psbt);
		}
		Err("Multisig not found!".into())
	}

	fn _build_psbt(
		&self, wallet: &mut PersistedWallet, output_script: ScriptBuf, amount: Amount,
		fee_rate: FeeRate, locktime: LockTime,
	) -> Result<Psbt, Box<dyn Error>> {
		let mut tx_builder = wallet.build_tx();
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
	) -> Result<(), Box<dyn Error>> {
		let mut wallets = self.wallets.lock().unwrap();
		let wallet = wallets.get_mut(&self.pubkey).unwrap();
		self._add_utxos_to_psbt(wallet, None, psbt, max_count, uniform_amount, fee, payer)
	}

	pub fn multisig_add_utxos_to_psbt(
		&self, other: &PublicKey, psbt: &mut Psbt, max_count: u16, uniform_amount: Option<Amount>,
		fee: Amount, payer: bool,
	) -> Result<(), Box<dyn Error>> {
		let mut wallets = self.wallets.lock().unwrap();
		if let Some(wallet) = wallets.get_mut(other) {
			self._add_utxos_to_psbt(
				wallet,
				Some(other),
				psbt,
				max_count,
				uniform_amount,
				fee,
				payer,
			)?;
		}
		Ok(())
	}

	fn _add_utxos_to_psbt(
		&self, wallet: &mut PersistedWallet, other_opt: Option<&PublicKey>, psbt: &mut Psbt,
		max_count: u16, uniform_amount: Option<Amount>, fee: Amount, payer: bool,
	) -> Result<(), Box<dyn Error>> {
		let mut count = 0;
		let mut receiver_utxos_value = Amount::from_sat(0);

		let redeem_script = if let Some(other) = other_opt {
			Some(
				bitcoin::blockdata::script::Builder::new()
					.push_opcode(opcodes::all::OP_PUSHNUM_2)
					.push_key(&self.pubkey.into())
					.push_key(&other.clone().into())
					.push_opcode(opcodes::all::OP_PUSHNUM_2)
					.push_opcode(opcodes::all::OP_CHECKMULTISIG)
					.into_script(),
			)
		} else {
			None
		};

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
					"[{}][{}] Adding UTXO [txid={:?} | vout={:?} | amt={}]",
					self.node_id,
					self.node_alias,
					utxo.outpoint.txid,
					utxo.outpoint.vout,
					utxo.txout.value
				);

				psbt.inputs.push(Input {
					non_witness_utxo: Some(tx),
					witness_utxo: Some(utxo.txout.clone()),
					redeem_script: redeem_script.clone(),
					..Default::default()
				});
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

	pub fn multisig_sign_psbt(
		&self, other: &PublicKey, psbt: &mut Psbt,
	) -> Result<(), Box<dyn Error>> {
		let mut wallets = self.wallets.lock().unwrap();
		if let Some(wallet) = wallets.get_mut(other) {
			wallet.sign(psbt, SignOptions::default())?;
		} else {
			return Err("Multisig not found!".into());
		}
		Ok(())
	}

	pub fn sign_psbt(&self, psbt: &mut Psbt) -> Result<(), Box<dyn Error>> {
		let mut wallets = self.wallets.lock().unwrap();
		if let Some(wallet) = wallets.get_mut(&self.pubkey) {
			wallet.sign(psbt, SignOptions::default())?;
		} else {
			return Err("Node wallet not found!".into());
		}
		Ok(())
	}

	pub fn send(&self, their_node_id: PublicKey, msg: BatchMessage) -> Result<(), Box<dyn Error>> {
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
