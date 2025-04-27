use crate::batch::process_batch_messages;
use crate::bitcoind::BitcoindConfig;
use crate::broker::Broker;
use crate::config::{BrokerConfig, NodeConfig};
use crate::events::SimpleEventHandler;
use crate::logger::SimpleLogger;
use crate::messages::{BatchMessage, BatchMessageHandler};
use crate::persister::InMemoryPersister;
use crate::types::{ChainMonitor, ChannelManager, FixedFeeEstimator, MockBroadcaster, PeerManager};

use bdk_wallet::Balance;
use bip39::{Language, Mnemonic};
use bitcoin::absolute::LockTime;
use bitcoin::secp256k1::{PublicKey, Secp256k1};
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
use lightning_net_tokio::{setup_inbound, setup_outbound};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{self, Duration};

pub struct Node {
	id: PublicKey,
	alias: String,
	endpoint: String,
	pub peer_manager: Arc<PeerManager>,
	pub channel_manager: Arc<ChannelManager>,
	pub event_handler: Arc<SimpleEventHandler>,
	pub custom_message_handler: Arc<BatchMessageHandler>,
	pub broker: Broker,
	runtime: Arc<RwLock<Option<Arc<Runtime>>>>,
}

impl Node {
	pub fn new(
		alias: String, secret: &[u8; 32], host: String, port: u16, network: Network,
		bitcoind_config: BitcoindConfig, wallet_secret: &[u8; 32], db_path: String,
		broker_config: BrokerConfig,
	) -> Result<Self, Box<dyn Error>> {
		Self::inner_new(
			alias,
			secret,
			host,
			port,
			network,
			bitcoind_config,
			wallet_secret,
			db_path,
			broker_config,
		)
	}

	pub fn new_from_config(config: NodeConfig) -> Result<Self, Box<dyn Error>> {
		let mnemonic = Mnemonic::parse_in(Language::English, config.mnemonic)?;
		let seed = mnemonic.to_seed("");
		let wallet_secret: &[u8; 32] = seed[..32].try_into().unwrap();

		let secret = <[u8; 32]>::from_hex(&config.secret)?;

		Self::inner_new(
			config.alias,
			&secret,
			config.host,
			config.port,
			config.network,
			config.bitcoind_config,
			wallet_secret,
			config.db_path,
			config.broker_config,
		)
	}

	fn inner_new(
		alias: String, secret: &[u8; 32], host: String, port: u16, network: Network,
		bitcoind_config: BitcoindConfig, wallet_secret: &[u8; 32], db_path: String,
		broker_config: BrokerConfig,
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
		let fee_estimator = Arc::new(FixedFeeEstimator { sat_per_kw: 1000 });
		let logger = Arc::new(SimpleLogger);
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
			db_path,
			custom_message_handler.clone(),
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

		Ok(Node {
			id,
			alias,
			endpoint: format!("{}:{}", host, port),
			peer_manager,
			channel_manager,
			event_handler: Arc::new(SimpleEventHandler),
			custom_message_handler,
			broker,
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

	pub fn multisig_sync(&self, other: &PublicKey, debug: bool) -> Result<(), Box<dyn Error>> {
		self.broker.multisig_sync(other, debug)
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

		let broker_clone = broker.clone();
		let pm_clone = peer_manager.clone();
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
					process_batch_messages(
						&node_alias,
						&node_id,
						node_endpoint.clone(),
						&broker_clone,
						pm_clone.clone(),
						msg,
					)
					.unwrap();
				}
			}
		});

		// === 3. Accept incoming connections ===
		let node_id = self.id;
		let node_alias = self.alias.clone();
		let endpoint = self.endpoint.clone();
		runtime.spawn(async move {
			let listener = TcpListener::bind(&endpoint).await.expect("Failed to bind port");
			println!("[{}][{}] Node listening on {}", node_id, node_alias, endpoint);
			loop {
				let node_alias = node_alias.clone();
				let peer_manager = peer_manager.clone();
				match listener.accept().await {
					Ok((stream, addr)) => {
						println!("[{}][{}] New connection from {}", node_id, node_alias, addr);
						// Spawn a task to handle this connection
						tokio::spawn(async move {
							match stream.into_std() {
								Ok(std_stream) => {
									setup_inbound(peer_manager.clone(), std_stream).await;
								},
								Err(e) => {
									eprintln!(
										"[{}][{}] Failed to convert tokio stream to std: {}",
										node_id, node_alias, e
									);
								},
							}
						});
					},
					Err(e) => {
						eprintln!("[{}][{}] Connection failed: {}", node_id, node_alias, e);
					},
				}
			}
		});

		for (pubkey, ep) in self.broker.config.bootnodes.clone() {
			if !self.is_peer_connected(&pubkey) {
				println!("[{}][{}] Bootnode connecting -> {}@{}", self.id, self.alias, pubkey, ep);
				self._connect(&runtime, (pubkey, ep))?;
			}
		}

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

	pub fn connect(&self, other: &Node) -> Result<(), Box<dyn Error>> {
		let runtime_lock = self.runtime.read().unwrap();
		if let Some(runtime) = runtime_lock.as_ref() {
			return self._connect(runtime, (other.node_id(), other.endpoint()));
		}
		Err("Connect has failed!".into())
	}

	fn _connect(
		&self, runtime: &Arc<Runtime>, other: (PublicKey, String),
	) -> Result<(), Box<dyn Error>> {
		let other_id = other.0;
		let other_endpoint = other.1;
		let peer_manager = self.peer_manager.clone();
		runtime.spawn(async move {
			let stream = TcpStream::connect(&other_endpoint).await.expect("Failed to connect");
			match stream.into_std() {
				Ok(std_stream) => setup_outbound(peer_manager.clone(), other_id, std_stream).await,
				Err(e) => eprintln!("❌ Failed to convert stream: {e}"),
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
		max_participants: u8, max_utxo_per_participant: u8,
	) -> Result<(), Box<dyn Error>> {
		self.peer_manager
			.peer_by_node_id(&their_node_id)
			.ok_or(format!("[{}] Peer not connected: {}", self.id, their_node_id))?;

		let mut psbt = self.build_psbt(output_script, amount, fee_rate, locktime)?;

		// Initiator must cover all the batch fees
		let total_fee = fee_per_participant * (max_participants as u64);

		self.add_utxos_to_psbt(&mut psbt, max_utxo_per_participant + 1, None, total_fee, true)?;

		let fee_per_participant = fee_per_participant.to_sat();
		let uniform_amount = if uniform_amount { amount.to_sat() } else { 0 };

		let batch_psbt = BatchMessage::BatchPsbt {
			sender_node_id: self.node_id(),
			receiver_node_id: their_node_id,
			uniform_amount,
			fee_per_participant,
			max_utxo_per_participant,
			max_participants: max_participants + 1,
			participants: vec![self.node_id()],
			endpoints: vec![self.endpoint()],
			not_participants: vec![],
			psbt: psbt.serialize(),
			sign: false,
		};

		self.broker.send(their_node_id, batch_psbt)?;

		Ok(())
	}

	pub fn init_multisig_psbt_batch(
		&self, other: &PublicKey, output_script: ScriptBuf, amount: Amount, fee_rate: FeeRate,
		locktime: LockTime, uniform_amount: bool, fee_per_participant: Amount,
		max_participants: u8, max_utxo_per_participant: u8,
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
				participants: vec![self.node_id()],
				endpoints: vec![self.endpoint()],
				not_participants: vec![],
				psbt: psbt.serialize(),
				sign: false,
			};
			self.broker.send(pd.counterparty_node_id, batch_psbt)?;
		} else {
			return Err("Node has no peers connected!".into());
		}

		Ok(())
	}

	fn _psbt_batch(
		&self, their_node_id: PublicKey, output_script: ScriptBuf, amount: Amount,
		fee_rate: FeeRate, locktime: LockTime, uniform_amount: bool, fee_per_participant: Amount,
		max_participants: u8, max_utxo_per_participant: u8,
	) -> Result<(), Box<dyn Error>> {
		self.peer_manager
			.peer_by_node_id(&their_node_id)
			.ok_or(format!("[{}] Peer not connected: {}", self.id, their_node_id))?;

		let mut psbt = self.build_psbt(output_script, amount, fee_rate, locktime)?;

		// Initiator must cover all the batch fees
		let total_fee = fee_per_participant * (max_participants as u64);

		self.add_utxos_to_psbt(&mut psbt, max_utxo_per_participant + 1, None, total_fee, true)?;

		let fee_per_participant = fee_per_participant.to_sat();
		let uniform_amount = if uniform_amount { amount.to_sat() } else { 0 };

		let batch_psbt = BatchMessage::BatchPsbt {
			sender_node_id: self.node_id(),
			receiver_node_id: their_node_id,
			uniform_amount,
			fee_per_participant,
			max_utxo_per_participant,
			max_participants: max_participants + 1,
			participants: vec![self.node_id()],
			endpoints: vec![self.endpoint()],
			not_participants: vec![],
			psbt: psbt.serialize(),
			sign: false,
		};

		self.broker.send(their_node_id, batch_psbt)?;

		Ok(())
	}

	pub fn sync_wallet(&self, debug: bool) -> Result<(), Box<dyn Error>> {
		self.broker.sync_wallet(debug)
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

	pub fn multisig_sign_psbt(
		&self, other: &PublicKey, psbt: &mut Psbt,
	) -> Result<(), Box<dyn Error>> {
		self.broker.multisig_sign_psbt(other, psbt)
	}

	pub fn broadcast_transactions(&self, txs: &[&Transaction]) -> Result<(), Box<dyn Error>> {
		self.broker.broadcast_transactions(txs)
	}
}
