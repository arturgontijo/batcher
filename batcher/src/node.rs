use crate::batch::process_batch_messages;
use crate::bitcoind::BitcoindConfig;
use crate::broker::Broker;
use crate::config::{BrokerConfig, LoggerConfig, NodeConfig};
use crate::events::SimpleEventHandler;
use crate::logger::{print_pubkey, SimpleLogger};
use crate::messages::{BatchMessage, BatchMessageHandler};
use crate::persister::InMemoryPersister;
use crate::storage::PeerStorage;
use crate::types::{ChainMonitor, ChannelManager, FixedFeeEstimator, MockBroadcaster, PeerManager};

use bdk_wallet::{Balance, LocalOutput};
use bip39::{Language, Mnemonic};
use bitcoin::absolute::LockTime;
use bitcoin::secp256k1::{PublicKey, Secp256k1};
use bitcoin::Txid;
use bitcoin::{hex::FromHex, Address, Amount, FeeRate, Psbt, ScriptBuf, Transaction};
use lightning::bitcoin::network::Network;
use lightning::events::EventsProvider;
use lightning::ln::channelmanager::ChainParameters;
use lightning::ln::peer_handler::{IgnoringMessageHandler, MessageHandler};
use lightning::onion_message::messenger::DefaultMessageRouter;
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{
	ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lightning::sign::KeysManager;
use lightning::util::config::UserConfig;
use lightning::util::logger::Level;
use lightning::util::logger::Logger;
use lightning::{log_debug, log_error, log_info};
use lightning_net_tokio::{setup_inbound, setup_outbound};

use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{self, Duration};

#[derive(Clone)]
pub struct Node {
	id: PublicKey,
	alias: String,
	endpoint: String,
	pub peer_manager: Arc<PeerManager>,
	peer_storage: Arc<Mutex<PeerStorage>>,
	pub channel_manager: Arc<ChannelManager>,
	pub event_handler: Arc<SimpleEventHandler>,
	pub custom_message_handler: Arc<BatchMessageHandler>,
	pub broker: Broker,
	pub logger: Arc<SimpleLogger>,
	runtime: Arc<RwLock<Option<Arc<Runtime>>>>,
}

impl Node {
	pub fn new(
		alias: String, secret: &[u8; 32], host: String, port: u16, network: Network,
		peers_db_path: String, bitcoind_config: BitcoindConfig, wallet_secret: &[u8; 32],
		wallet_file_path: String, broker_config: BrokerConfig, logger_config: LoggerConfig,
	) -> Result<Self, Box<dyn Error>> {
		Self::inner_new(
			alias,
			secret,
			host,
			port,
			network,
			peers_db_path,
			bitcoind_config,
			wallet_secret,
			wallet_file_path,
			broker_config,
			logger_config,
		)
	}

	pub fn new_from_config(config: NodeConfig) -> Result<Self, Box<dyn Error>> {
		let mnemonic = Mnemonic::parse_in(Language::English, config.wallet.mnemonic)?;
		let seed = mnemonic.to_seed("");
		let wallet_secret: &[u8; 32] = seed[..32].try_into().unwrap();

		let secret = <[u8; 32]>::from_hex(&config.secret)?;

		Self::inner_new(
			config.alias,
			&secret,
			config.host,
			config.port,
			config.network,
			config.peers_db_path,
			config.bitcoind,
			wallet_secret,
			config.wallet.file_path,
			config.broker,
			config.logger,
		)
	}

	fn inner_new(
		alias: String, secret: &[u8; 32], host: String, port: u16, network: Network,
		peers_db_path: String, bitcoind_config: BitcoindConfig, wallet_secret: &[u8; 32],
		wallet_file_path: String, broker_config: BrokerConfig, logger_config: LoggerConfig,
	) -> Result<Self, Box<dyn Error>> {
		let secp = Secp256k1::new();

		// Step 1: Initialize KeysManager
		let starting_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
		let keys_manager = Arc::new(KeysManager::new(
			secret,
			starting_time.as_secs(),
			starting_time.subsec_nanos(),
		));

		let id = keys_manager.get_node_secret_key().public_key(&secp);

		// Step 2: Set up peripheral components
		let log_max_level = match logger_config.max_level.as_str() {
			"info" => Level::Info,
			"warn" => Level::Warn,
			"debug" => Level::Debug,
			"trace" => Level::Trace,
			"gossip" => Level::Gossip,
			"error" => Level::Error,
			_ => Level::Info,
		};
		let logger = Arc::new(SimpleLogger::new(logger_config.file_path, log_max_level)?);

		let fee_estimator = Arc::new(FixedFeeEstimator { sat_per_kw: 1000 });
		let broadcaster = Arc::new(MockBroadcaster);
		let persister = Arc::new(InMemoryPersister { monitors: Mutex::new(HashMap::new()) });

		// Step 3: Create ChainMonitor
		let chain_monitor: ChainMonitor = ChainMonitor::new(
			None, // No blockchain filter
			broadcaster.clone(),
			logger.clone(),
			fee_estimator.clone(),
			persister.clone(),
		);
		let chain_monitor = Arc::new(chain_monitor);

		// Step 4: Set up NetworkGraph and Router
		let network_graph = Arc::new(NetworkGraph::new(network, logger.clone()));
		let scorer = Arc::new(Mutex::new(ProbabilisticScorer::new(
			ProbabilisticScoringDecayParameters::default(),
			network_graph.clone(),
			logger.clone(),
		)));
		let router = Arc::new(DefaultRouter::new(
			network_graph.clone(),
			logger.clone(),
			keys_manager.clone(),
			scorer.clone(),
			ProbabilisticScoringFeeParameters::default(),
		));
		let message_router =
			Arc::new(DefaultMessageRouter::new(network_graph.clone(), keys_manager.clone()));

		// Step 5: Create ChannelManager
		let user_config = UserConfig::default();
		let chain_params = ChainParameters {
			network,
			best_block: lightning::chain::BestBlock::from_network(network),
		};
		let current_block_height = 0; // Placeholder; replace with actual block height
		let channel_manager: ChannelManager = ChannelManager::new(
			fee_estimator.clone(),
			chain_monitor.clone(),
			broadcaster.clone(),
			router.clone(),
			message_router.clone(),
			logger.clone(),
			keys_manager.clone(),
			keys_manager.clone(),
			keys_manager.clone(),
			user_config,
			chain_params,
			current_block_height,
		);
		let channel_manager = Arc::new(channel_manager);

		let custom_message_handler = Arc::new(BatchMessageHandler::new());

		let broker = Broker::new(
			id,
			alias.clone(),
			wallet_secret,
			network,
			broker_config,
			bitcoind_config,
			wallet_file_path,
			custom_message_handler.clone(),
			logger.clone(),
		)?;

		let message_handler = MessageHandler {
			chan_handler: channel_manager.clone(),
			route_handler: Arc::new(IgnoringMessageHandler {}),
			onion_message_handler: Arc::new(IgnoringMessageHandler {}),
			custom_message_handler: custom_message_handler.clone(),
		};

		let peer_manager = Arc::new(PeerManager::new(
			message_handler,
			starting_time.as_secs() as u32,
			secret,
			logger.clone(),
			keys_manager.clone(),
		));

		let peer_storage = Arc::new(Mutex::new(PeerStorage::new(&peers_db_path)?));

		Ok(Node {
			id,
			alias,
			endpoint: format!("{}:{}", host, port),
			peer_manager,
			peer_storage,
			channel_manager,
			event_handler: Arc::new(SimpleEventHandler),
			custom_message_handler,
			broker,
			logger,
			runtime: Default::default(),
		})
	}

	pub fn node_id(&self) -> PublicKey {
		self.id
	}

	pub fn alias(&self) -> String {
		self.alias.clone()
	}

	pub fn endpoint(&self) -> String {
		self.endpoint.clone()
	}

	pub fn pubkey(&self) -> PublicKey {
		self.broker.pubkey
	}

	pub fn create_multisig(
		&self, network: Network, other: &PublicKey, db_path: String,
	) -> Result<(), Box<dyn Error>> {
		self.broker.create_multisig(network, other, db_path)
	}

	pub fn multisig_sync(&self, other: &PublicKey) -> Result<(), Box<dyn Error>> {
		self.broker.multisig_sync(other)
	}

	pub fn start(&self) -> Result<(), Box<dyn Error>> {
		let mut runtime_lock = self.runtime.write().unwrap();
		if runtime_lock.is_some() {
			return Err("Node is already running!".into());
		}

		let runtime =
			Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap());

		// Clone to move into tasks
		let broker = self.broker.clone();
		let peer_manager = self.peer_manager.clone();
		let custom_message_handler = self.custom_message_handler.clone();
		let channel_manager = self.channel_manager.clone();

		// === 1. Background task: Event processing ===
		let cm_clone = channel_manager.clone();
		let eh_clone = self.event_handler.clone();
		runtime.spawn(async move {
			let mut interval = time::interval(Duration::from_millis(100));
			loop {
				interval.tick().await;
				cm_clone.process_pending_events(&*eh_clone);
			}
		});

		// === 2. Background task: Peer ticks (ping, cleanup) ===
		let pm_clone = peer_manager.clone();
		runtime.spawn(async move {
			let mut interval = time::interval(Duration::from_secs(10));
			loop {
				interval.tick().await;
				pm_clone.timer_tick_occurred();
			}
		});

		let logger_clone = self.logger.clone();
		let broker_clone = broker.clone();
		let pm_clone = peer_manager.clone();
		let peer_storage = self.peer_storage.clone();
		let node_alias = self.alias.clone();
		let node_id = self.id;
		let node_endpoint = self.endpoint.clone();
		runtime.spawn(async move {
			let mut interval = time::interval(Duration::from_millis(50));
			loop {
				interval.tick().await;
				pm_clone.process_events();
				let messages: Vec<_> =
					custom_message_handler.queue.lock().unwrap().drain(..).collect();
				for (_, msg) in messages {
					match process_batch_messages(
						&node_alias,
						&node_id,
						node_endpoint.clone(),
						&broker_clone,
						pm_clone.clone(),
						peer_storage.clone(),
						&logger_clone,
						msg,
					) {
						Ok(_) => {},
						Err(err) => {
							log_error!(
								logger_clone,
								"[{}][{}] process_batch_messages() {}",
								node_id,
								node_alias,
								err
							);
						},
					}
				}
			}
		});

		// === 3. Accept incoming connections ===
		let node_id = self.id;
		let node_alias = self.alias.clone();
		let endpoint = self.endpoint.clone();
		let logger = self.logger.clone();
		runtime.spawn(async move {
			let listener = TcpListener::bind(&endpoint).await.expect("Failed to bind port");
			log_info!(logger, "[{}][{}] Node listening on {}", node_id, node_alias, endpoint);
			loop {
				let node_alias = node_alias.clone();
				let logger = logger.clone();
				let peer_manager = peer_manager.clone();
				match listener.accept().await {
					Ok((stream, addr)) => {
						log_info!(
							logger,
							"[{}][{}] New connection from {}",
							node_id,
							node_alias,
							addr
						);
						// Spawn a task to handle this connection
						tokio::spawn(async move {
							match stream.into_std() {
								Ok(std_stream) => {
									setup_inbound(peer_manager.clone(), std_stream).await;
								},
								Err(e) => {
									log_error!(
										logger,
										"[{}][{}] Failed to convert tokio stream to std: {}",
										node_id,
										node_alias,
										e
									);
								},
							}
						});
					},
					Err(e) => {
						log_error!(
							logger,
							"[{}][{}] Connection failed: {}",
							node_id,
							node_alias,
							e
						);
					},
				}
			}
		});

		let mut peers_in_memory: HashMap<PublicKey, String> = Default::default();

		// Reconnect from PeerStorage list
		let peer_storage = self.peer_storage.lock().unwrap();
		for (pubkey, ep) in peer_storage.list_peers()? {
			if !self.is_peer_connected(&pubkey) {
				log_info!(
					self.logger,
					"[{}][{}] PeerStorage reconnecting {}@{}",
					self.id,
					self.alias,
					pubkey,
					ep
				);
				self._connect(&runtime, pubkey, ep.clone(), false)?;
				peers_in_memory.insert(pubkey, ep);
			}
		}

		// Reconnect from config.toml bootnodes list
		for (pubkey, ep) in self.broker.config.bootnodes.clone() {
			if !self.is_peer_connected(&pubkey) {
				log_info!(
					self.logger,
					"[{}][{}] Bootnode connecting -> {}@{}",
					self.id,
					self.alias,
					pubkey,
					ep
				);
				self._connect(&runtime, pubkey, ep.clone(), true)?;
				peers_in_memory.insert(pubkey, ep);
			}
		}

		// Wallet sync
		let node_id = self.id;
		let node_alias = self.alias.clone();
		let broker_clone = broker.clone();
		let wallet_sync_interval = broker.config.wallet_sync_interval;
		let logger = self.logger.clone();
		runtime.spawn(async move {
			let mut interval = time::interval(Duration::from_secs(wallet_sync_interval as u64));
			loop {
				interval.tick().await;
				broker_clone.sync_wallet().unwrap();
				let utxos = broker_clone.list_unspent().unwrap_or_default();
				let mut value = Amount::ZERO;
				for utxo in &utxos {
					value += utxo.txout.value;
				}
				log_debug!(
					logger,
					"[{}][{}] Synching wallet -> utxos={} | value={}",
					node_id,
					node_alias,
					utxos.len(),
					value
				);
			}
		});

		*runtime_lock = Some(runtime);

		Ok(())
	}

	pub fn stop(&self) -> Result<(), Box<dyn Error>> {
		let mut runtime_lock = self.runtime.write().unwrap();
		if let Some(runtime) = runtime_lock.take() {
			self.peer_manager.disconnect_all_peers();
			std::thread::spawn(move || {
				drop(runtime); // Gracefully shut down the runtime
			});
			Ok(())
		} else {
			Err("Node is not running!".into())
		}
	}

	pub fn is_peer_connected(&self, their_node_id: &PublicKey) -> bool {
		self.peer_manager.peer_by_node_id(their_node_id).is_some()
	}

	pub fn connect(
		&self, other_id: PublicKey, other_endpoint: String,
	) -> Result<(), Box<dyn Error>> {
		let runtime_lock = self.runtime.read().unwrap();
		let runtime = runtime_lock.as_ref().unwrap();
		self._connect(runtime, other_id, other_endpoint, true)?;
		Ok(())
	}

	fn _connect(
		&self, runtime: &Arc<Runtime>, other_id: PublicKey, other_endpoint: String, persist: bool,
	) -> Result<(), Box<dyn Error>> {
		let node_id = self.node_id();
		let node_alias = self.alias();
		let logger = self.logger.clone();
		let peer_storage = self.peer_storage.clone();
		let peer_manager = self.peer_manager.clone();
		runtime.spawn(async move {
			match TcpStream::connect(&other_endpoint).await {
				Ok(stream) => match stream.into_std() {
					Ok(std_stream) => {
						if persist {
							let result = {
								let storage = peer_storage.lock().unwrap();
								storage.upsert_peer(&other_id, other_endpoint.clone())
							};
							match result {
								Ok(_) => {
									log_info!(
										logger,
										"[{}][{}] PeerStorage connecting {}@{}",
										node_id,
										node_alias,
										other_id,
										other_endpoint
									);
								},
								Err(err) => {
									log_error!(
										logger,
										"[{}][{}][ERROR] PeerStorage failed: {}@{} | {}",
										node_id,
										node_alias,
										other_id,
										other_endpoint,
										err
									);
								},
							}
							drop(peer_storage);
						}
						setup_outbound(peer_manager.clone(), other_id, std_stream).await
					},
					Err(e) => {
						log_error!(
							logger,
							"[{}][{}][ERROR] Failed to convert stream: {e}",
							node_id,
							node_alias
						);
					},
				},
				Err(_) => {
					log_error!(
						logger,
						"[{}][{}][ERROR] Failed to connect to peer {} @ {}",
						node_id,
						node_alias,
						other_id,
						other_endpoint
					);
				},
			}
		});
		Ok(())
	}

	pub fn build_psbt(
		&self, output_script: ScriptBuf, amount: Amount, fee_rate: FeeRate, locktime: LockTime,
	) -> Result<Psbt, Box<dyn Error>> {
		self.broker.build_psbt(output_script, amount, fee_rate, locktime)
	}

	pub fn init_psbt_batch(
		&self, their_node_id: PublicKey, output_script: ScriptBuf, amount: Amount,
		fee_rate: FeeRate, locktime: LockTime, uniform_amount: bool, fee_per_participant: Amount,
		max_participants: u8, max_utxo_per_participant: u8, max_hops: u8,
	) -> Result<(), Box<dyn Error>> {
		match self.peer_manager.peer_by_node_id(&their_node_id) {
			Some(_) => {},
			None => {
				log_error!(
					self.logger,
					"[{}][{}] Peer not connected: {}",
					self.id,
					self.alias,
					their_node_id
				)
			},
		}

		let mut psbt = self.build_psbt(output_script, amount, fee_rate, locktime)?;

		// Initiator must cover all the batch fees
		let total_fee = fee_per_participant * (max_participants as u64);

		self.add_utxos_to_psbt(&mut psbt, max_utxo_per_participant + 1, None, total_fee, true)?;

		let fee_per_participant = fee_per_participant.to_sat();
		let uniform_amount = if uniform_amount { amount.to_sat() } else { 0 };

		let psbt_bytes = psbt.serialize();

		log_info!(
			self.logger,
			"[{}][{}] Initializing BatchPsbt: next={} | uamt={} | fee={} | utxos={} | max_p={} | max_hops={} | len={}",
			self.node_id(),
			self.alias(),
			print_pubkey(&their_node_id),
			uniform_amount,
			fee_per_participant,
			max_utxo_per_participant,
			max_participants,
			max_hops,
			psbt_bytes.len(),
		);

		let batch_psbt = BatchMessage::BatchPsbt {
			sender_node_id: self.node_id(),
			receiver_node_id: their_node_id,
			uniform_amount,
			fee_per_participant,
			max_utxo_per_participant,
			max_participants: max_participants + 1,
			max_hops,
			participants: vec![self.node_id()],
			endpoints: vec![self.endpoint()],
			not_participants: vec![],
			hops: 0,
			psbt: psbt_bytes,
			sign: false,
		};

		self.broker.send(their_node_id, batch_psbt)?;

		Ok(())
	}

	pub fn init_multisig_psbt_batch(
		&self, other: &PublicKey, output_script: ScriptBuf, amount: Amount, fee_rate: FeeRate,
		locktime: LockTime, uniform_amount: bool, fee_per_participant: Amount,
		max_participants: u8, max_utxo_per_participant: u8, max_hops: u8,
	) -> Result<(), Box<dyn Error>> {
		let mut psbt =
			self.broker.multisig_build_psbt(other, output_script, amount, fee_rate, locktime)?;

		// Initiator must cover all the batch fees
		let total_fee = fee_per_participant * (max_participants as u64);

		self.multisig_add_utxos_to_psbt(
			other,
			&mut psbt,
			max_utxo_per_participant + 1,
			None,
			total_fee,
			true,
		)?;

		// Adding node's wallet UTXOs
		self.add_utxos_to_psbt(
			&mut psbt,
			max_utxo_per_participant,
			Some(amount),
			fee_per_participant,
			false,
		)?;

		let fee_per_participant = fee_per_participant.to_sat();
		let uniform_amount = if uniform_amount { amount.to_sat() } else { 0 };

		if let Some(pd) = self.peer_manager.list_peers().first() {
			let batch_psbt = BatchMessage::BatchPsbt {
				sender_node_id: self.node_id(),
				receiver_node_id: pd.counterparty_node_id,
				uniform_amount,
				fee_per_participant,
				max_utxo_per_participant,
				max_participants,
				max_hops,
				participants: vec![self.node_id()],
				endpoints: vec![self.endpoint()],
				not_participants: vec![],
				hops: 0,
				psbt: psbt.serialize(),
				sign: false,
			};
			self.broker.send(pd.counterparty_node_id, batch_psbt)?;
		} else {
			return Err("Node has no connected peers!".into());
		}

		Ok(())
	}

	pub fn sync_wallet(&self) -> Result<(), Box<dyn Error>> {
		self.broker.sync_wallet()
	}

	pub fn balance(&self) -> Balance {
		self.broker.balance()
	}

	pub fn multisig_balance(&self, other: &PublicKey) -> Result<Balance, Box<dyn Error>> {
		self.broker.multisig_balance(other)
	}

	pub fn multisig_new_address(&self, other: &PublicKey) -> Result<Address, Box<dyn Error>> {
		self.broker.multisig_new_address(other)
	}

	pub fn wallet_new_address(&self) -> Result<Address, Box<dyn Error>> {
		self.broker.wallet_new_address()
	}

	pub fn add_utxos_to_psbt(
		&self, psbt: &mut Psbt, max_count: u8, uniform_amount: Option<Amount>, fee: Amount,
		payer: bool,
	) -> Result<(), Box<dyn Error>> {
		self.broker.add_utxos_to_psbt(psbt, max_count, uniform_amount, fee, payer)
	}

	pub fn multisig_add_utxos_to_psbt(
		&self, other: &PublicKey, psbt: &mut Psbt, max_count: u8, uniform_amount: Option<Amount>,
		fee: Amount, payer: bool,
	) -> Result<(), Box<dyn Error>> {
		self.broker.multisig_add_utxos_to_psbt(other, psbt, max_count, uniform_amount, fee, payer)
	}

	pub fn sign_psbt(&self, psbt: &mut Psbt) -> Result<(), Box<dyn Error>> {
		self.broker.sign_psbt(psbt)
	}

	pub fn multisig_sign_psbt(
		&self, other: &PublicKey, psbt: &mut Psbt,
	) -> Result<(), Box<dyn Error>> {
		self.broker.multisig_sign_psbt(other, psbt)
	}

	pub fn broadcast_transactions(&self, tx: &Transaction) -> Result<Txid, Box<dyn Error>> {
		self.broker.broadcast_transaction(tx)
	}

	pub fn build_foreign_psbt(
		&self, change_scriptbuf: ScriptBuf, output_script: ScriptBuf, amount: Amount,
		utxos: Vec<LocalOutput>, locktime: LockTime, uniform_amount: bool,
		fee_per_participant: Amount, max_participants: u8, max_utxo_per_participant: u8,
		max_hops: u8,
	) -> Result<(), Box<dyn Error>> {
		if let Some(pd) = self.peer_manager.list_peers().first() {
			let uniform_amount_opt = if uniform_amount { Some(amount) } else { None };

			let psbt = self.broker.build_foreign_psbt(
				change_scriptbuf,
				output_script,
				amount,
				utxos,
				locktime,
				uniform_amount_opt,
				fee_per_participant,
				max_participants,
				max_utxo_per_participant,
			)?;

			let batch_psbt = BatchMessage::BatchPsbt {
				sender_node_id: self.node_id(),
				receiver_node_id: pd.counterparty_node_id,
				uniform_amount: if uniform_amount_opt.is_some() { amount.to_sat() } else { 0 },
				fee_per_participant: fee_per_participant.to_sat(),
				max_utxo_per_participant,
				max_participants,
				max_hops,
				participants: vec![self.node_id()],
				endpoints: vec![self.endpoint()],
				not_participants: vec![],
				hops: 0,
				psbt: psbt.serialize(),
				sign: false,
			};

			self.broker.send(pd.counterparty_node_id, batch_psbt)?;

			return Ok(());
		}
		Err("Can't build the PSBT!".into())
	}
}
