use std::{
	collections::HashMap,
	sync::{Arc, Mutex},
};

use bdk_wallet::{
	bitcoin::{bip32::Xpriv, Amount, Network},
	descriptor::IntoWalletDescriptor,
	rusqlite::Connection,
	template::Bip84,
	Balance, KeychainKind, LocalOutput, SignOptions, Wallet,
};
use bitcoin::{
	absolute::LockTime,
	key::Secp256k1,
	opcodes,
	psbt::{Input, Output},
	secp256k1::{PublicKey, SecretKey},
	Address, FeeRate, NetworkKind, Psbt, ScriptBuf, Transaction, TxIn, TxOut, Txid, Weight,
};
use bitcoincore_rpc::{Client, RpcApi};

use lightning::util::logger::Logger;
use lightning::{log_debug, log_info};

use crate::{
	bitcoind::{bitcoind_client, BitcoindConfig},
	config::BrokerConfig,
	logger::SimpleLogger,
	messages::{BatchMessage, BatchMessageHandler},
	storage::{BatchPsbtKind, BatchPsbtStatus, BatchPsbtStored, BrokerStorage},
	types::{BoxError, PersistedWallet},
	wallet::sync_wallet,
};

#[derive(Clone)]
pub struct Broker {
	pub node_id: PublicKey,
	pub node_alias: String,
	pub config: BrokerConfig,
	pub custom_message_handler: Arc<BatchMessageHandler>,
	pub pubkey: PublicKey,
	pub wif: String,
	pub wallets: Arc<Mutex<HashMap<PublicKey, PersistedWallet>>>,
	pub storage: Arc<Mutex<BrokerStorage>>,
	pub bitcoind_client: Arc<Client>,
	pub persister: Arc<Mutex<Connection>>,
	pub logger: Arc<SimpleLogger>,
}

impl Broker {
	pub fn new(
		node_id: PublicKey, node_alias: String, wallet_secret: &[u8; 32], network: Network,
		config: BrokerConfig, bitcoind_config: BitcoindConfig, wallet_file_path: String,
		custom_message_handler: Arc<BatchMessageHandler>, logger: Arc<SimpleLogger>,
	) -> Result<Self, BoxError> {
		let secp = Secp256k1::new();
		let secret_key = SecretKey::from_slice(wallet_secret)?;
		let pubkey = PublicKey::from_secret_key(&secp, &secret_key);

		let wif =
			bitcoin::PrivateKey { compressed: true, network: NetworkKind::Test, inner: secret_key }
				.to_wif();

		let storage = BrokerStorage::new(&config.storage_path)?;

		let mut persister = Connection::open(wallet_file_path)?;
		let wallet = Self::create_wallet(wallet_secret, network, &mut persister)?;

		let wallets = Arc::new(Mutex::new(HashMap::from([(pubkey, wallet)])));

		let BitcoindConfig { rpc_address, rpc_user, rpc_pass } = bitcoind_config;

		Ok(Broker {
			node_id,
			node_alias,
			config,
			custom_message_handler,
			pubkey,
			wif,
			wallets,
			storage: Arc::new(Mutex::new(storage)),
			bitcoind_client: Arc::new(bitcoind_client(rpc_address, rpc_user, rpc_pass, None)?),
			persister: Arc::new(Mutex::new(persister)),
			logger,
		})
	}

	fn create_wallet(
		seed_bytes: &[u8; 32], network: Network, persister: &mut Connection,
	) -> Result<PersistedWallet, BoxError> {
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
	) -> Result<(), BoxError> {
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
	) -> Result<PersistedWallet, BoxError> {
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

	pub fn sync_wallet(&self) -> Result<(), BoxError> {
		let mut binding = self.wallets.lock().unwrap();
		let wallet = binding.get_mut(&self.pubkey).unwrap();
		sync_wallet(&self.bitcoind_client, wallet)?;
		let mut persister = self.persister.lock().unwrap();
		wallet.persist(&mut persister)?;
		Ok(())
	}

	pub fn multisig_sync(&self, other: &PublicKey) -> Result<(), BoxError> {
		let mut multisigs = self.wallets.lock().unwrap();
		if let Some(wallet) = multisigs.get_mut(other) {
			sync_wallet(&self.bitcoind_client, wallet)?;
		}
		Ok(())
	}

	pub fn multisig_new_address(&self, other: &PublicKey) -> Result<Address, BoxError> {
		let mut multisigs = self.wallets.lock().unwrap();
		if let Some(wallet) = multisigs.get_mut(other) {
			let address = wallet.reveal_next_address(KeychainKind::External).address;
			return Ok(address);
		}
		Err("Multisig not found!".into())
	}

	pub fn wallet_new_address(&self) -> Result<Address, BoxError> {
		let mut wallets = self.wallets.lock().unwrap();
		let wallet = wallets.get_mut(&self.pubkey).unwrap();
		let address = wallet.reveal_next_address(KeychainKind::External).address;
		Ok(address)
	}

	pub fn balance(&self) -> Balance {
		self.wallets.lock().unwrap().get(&self.pubkey).unwrap().balance()
	}

	pub fn multisig_balance(&self, other: &PublicKey) -> Result<Balance, BoxError> {
		let multisigs = self.wallets.lock().unwrap();
		match multisigs.get(other) {
			Some(wallet) => Ok(wallet.balance()),
			None => Err("Multisig not found!".into()),
		}
	}

	pub fn list_unspent(&self) -> Result<Vec<LocalOutput>, BoxError> {
		let mut wallets = self.wallets.lock().unwrap();
		let wallet = wallets.get_mut(&self.pubkey).unwrap();
		let utxos = wallet.list_unspent().collect();
		Ok(utxos)
	}

	pub fn build_psbt(
		&self, output_script: ScriptBuf, amount: Amount, fee_rate: FeeRate, locktime: LockTime,
	) -> Result<Psbt, BoxError> {
		let mut wallets = self.wallets.lock().unwrap();
		let wallet = wallets.get_mut(&self.pubkey).unwrap();
		self._build_psbt(wallet, output_script, amount, fee_rate, locktime)
	}

	pub fn multisig_build_psbt(
		&self, other: &PublicKey, script_pubkey: ScriptBuf, amount: Amount, fee_rate: FeeRate,
		locktime: LockTime,
	) -> Result<Psbt, BoxError> {
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
	) -> Result<Psbt, BoxError> {
		let mut tx_builder = wallet.build_tx();
		tx_builder.add_recipient(output_script, amount).fee_rate(fee_rate).nlocktime(locktime);
		let psbt = match tx_builder.finish() {
			Ok(psbt) => psbt,
			Err(err) => return Err(err.into()),
		};

		Ok(psbt)
	}

	pub fn add_utxos_to_psbt(
		&self, psbt: &mut Psbt, max_count: u8, uniform_amount: Option<Amount>, fee: Amount,
		payer: bool,
	) -> Result<(), BoxError> {
		let mut wallets = self.wallets.lock().unwrap();
		let wallet = wallets.get_mut(&self.pubkey).unwrap();
		self._add_utxos_to_psbt(wallet, None, psbt, max_count, uniform_amount, fee, payer)
	}

	pub fn multisig_add_utxos_to_psbt(
		&self, other: &PublicKey, psbt: &mut Psbt, max_count: u8, uniform_amount: Option<Amount>,
		fee: Amount, payer: bool,
	) -> Result<(), BoxError> {
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
		max_count: u8, uniform_amount: Option<Amount>, fee: Amount, payer: bool,
	) -> Result<(), BoxError> {
		let mut count = 0;
		let mut utxos_value = Amount::from_sat(0);

		let redeem_script = if let Some(other) = other_opt {
			Some(
				bitcoin::blockdata::script::Builder::new()
					.push_opcode(opcodes::all::OP_PUSHNUM_2)
					.push_key(&self.pubkey.into())
					.push_key(&(*other).into())
					.push_opcode(opcodes::all::OP_PUSHNUM_2)
					.push_opcode(opcodes::all::OP_CHECKMULTISIG)
					.into_script(),
			)
		} else {
			None
		};

		let mut utxos: Vec<LocalOutput> = wallet.list_unspent().collect();
		utxos.sort_by(|a, b| b.txout.value.cmp(&a.txout.value));

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

				log_info!(
					self.logger,
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
				utxos_value += utxo.txout.value;

				count += 1;
				if count >= max_count {
					break;
				}
			};
		}

		let mut value = utxos_value;
		if payer {
			// We do not have enough funds for target total fee
			if value <= fee {
				return Err("Payer has not enought funds for fee.".into());
			}
			value -= fee;
		} else {
			value += fee
		}

		if let Some(uniform_amount) = uniform_amount {
			// We do not have enough funds for target uniform_amount
			if value <= uniform_amount {
				return Err("Participant has not enought funds for target uniform_amount.".into());
			}

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

	pub fn build_foreign_psbt(
		&self, change_scriptbuf: ScriptBuf, output_script: ScriptBuf, amount: Amount,
		utxos: Vec<LocalOutput>, locktime: LockTime, uniform_amount: Option<Amount>,
		fee_per_participant: Amount, max_participants: u8, max_utxo_per_participant: u8,
	) -> Result<Psbt, BoxError> {
		let mut wallets = self.wallets.lock().unwrap();
		if let Some(wallet) = wallets.get_mut(&self.pubkey) {
			let mut tx_builder = wallet.build_tx();

			tx_builder.manually_selected_only();
			let mut total_value = Amount::ZERO;
			for utxo in utxos {
				let tx = self.bitcoind_client.get_raw_transaction(&utxo.outpoint.txid, None)?;
				let input = Input {
					non_witness_utxo: Some(tx),
					witness_utxo: Some(utxo.txout.clone()),
					..Default::default()
				};

				log_info!(
					self.logger,
					"[{}][{}] Adding Foreign UTXO [txid={:?} | vout={:?} | amt={}]",
					self.node_id,
					self.node_alias,
					utxo.outpoint.txid,
					utxo.outpoint.vout,
					utxo.txout.value
				);

				tx_builder.add_foreign_utxo(utxo.outpoint, input, Weight::from_vb_unwrap(2))?;
				total_value += utxo.txout.value;
			}

			// +1 to cover tx fee
			let total_fee = fee_per_participant * ((max_participants + 1) as u64);
			let change = total_value - amount - total_fee;

			tx_builder
				.add_recipient(output_script.clone(), amount)
				.add_recipient(change_scriptbuf.clone(), change)
				.nlocktime(locktime);

			let wallet_psbt = tx_builder.finish()?;
			let mut psbt = wallet_psbt.clone();

			let mut outputs = vec![];
			for output in wallet_psbt.unsigned_tx.output {
				if output.script_pubkey != output_script && output.script_pubkey != change_scriptbuf
				{
					continue;
				}
				outputs.push(output.clone());
			}
			psbt.unsigned_tx.output = outputs;

			// Adding our own UTXOs too
			let max_count = self.config.max_utxo_count.min(max_utxo_per_participant);
			self._add_utxos_to_psbt(
				wallet,
				None,
				&mut psbt,
				max_count,
				uniform_amount,
				fee_per_participant,
				false,
			)?;

			return Ok(psbt);
		}
		Err("Can't build the PSBT!".into())
	}

	pub fn multisig_sign_psbt(&self, other: &PublicKey, psbt: &mut Psbt) -> Result<(), BoxError> {
		let mut wallets = self.wallets.lock().unwrap();
		if let Some(wallet) = wallets.get_mut(other) {
			wallet.sign(psbt, SignOptions::default())?;
		} else {
			return Err("Multisig not found!".into());
		}
		Ok(())
	}

	pub fn sign_psbt(&self, psbt: &mut Psbt) -> Result<(), BoxError> {
		let mut wallets = self.wallets.lock().unwrap();
		if let Some(wallet) = wallets.get_mut(&self.pubkey) {
			wallet.sign(psbt, SignOptions::default())?;
		} else {
			return Err("Node wallet not found!".into());
		}
		Ok(())
	}

	pub fn check_psbt(&self, psbt: &Psbt, fee: Amount) -> Result<bool, BoxError> {
		let mut input_value = Amount::ZERO;
		let mut output_value = Amount::ZERO;

		let utxos = self.list_unspent()?;

		let mut wallets = self.wallets.lock().unwrap();
		if let Some(wallet) = wallets.get_mut(&self.pubkey) {
			for input in &psbt.unsigned_tx.input {
				for utxo in &utxos {
					if input.previous_output == utxo.outpoint {
						input_value += utxo.txout.value;
						break;
					}
				}
			}
			for output in &psbt.unsigned_tx.output {
				if wallet.is_mine(output.script_pubkey.clone()) {
					output_value += output.value;
				}
			}
		} else {
			return Err("Node wallet not found!".into());
		}
		Ok(input_value + fee == output_value)
	}

	pub fn send(&self, their_node_id: &PublicKey, msg: BatchMessage) -> Result<(), BoxError> {
		log_debug!(
			self.logger,
			"[{}][{}] Sending: to={} | {:?}",
			self.node_id,
			self.node_alias,
			their_node_id,
			msg,
		);
		self.custom_message_handler.send(*their_node_id, msg);
		Ok(())
	}

	pub fn next_id(&self) -> Result<u32, BoxError> {
		let storage = self.storage.lock().unwrap();
		storage.next_id()
	}

	pub fn insert_psbt(&self, kind: BatchPsbtKind, psbt: &Psbt) -> Result<u32, BoxError> {
		let storage = self.storage.lock().unwrap();
		let id = storage.insert(BatchPsbtStatus::Created, kind, psbt)?;
		Ok(id)
	}

	pub fn update_psbt(
		&self, id: u32, status: BatchPsbtStatus, psbt: &Psbt,
	) -> Result<(), BoxError> {
		let storage = self.storage.lock().unwrap();
		storage.update(id, status, psbt)?;
		Ok(())
	}

	pub fn psbt_by_id(&self, id: u32) -> Result<Option<BatchPsbtStored>, BoxError> {
		let storage = self.storage.lock().unwrap();
		storage.psbt_by_id(id)
	}

	pub fn stored_psbts(&self, status: BatchPsbtStatus) -> Result<Vec<BatchPsbtStored>, BoxError> {
		let storage = self.storage.lock().unwrap();
		storage.psbts(status)
	}

	pub fn broadcast_transaction(&self, tx: &Transaction) -> Result<Txid, BoxError> {
		match self.bitcoind_client.send_raw_transaction(tx) {
			Ok(txid) => Ok(txid),
			Err(err) => Err(format!("Failed to broadcast Tx :{}", err).into()),
		}
	}
}
