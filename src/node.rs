use crate::batch::process_batch_messages;
use crate::broker::Broker;
use crate::events::SimpleEventHandler;
use crate::logger::SimpleLogger;
use crate::messages::{BatchMessage, BatchMessageHandler};
use crate::persister::InMemoryPersister;
use crate::types::{ChainMonitor, ChannelManager, FixedFeeEstimator, MockBroadcaster, PeerManager};

use bdk_wallet::{Balance, KeychainKind};
use bitcoin::absolute::LockTime;
use bitcoin::secp256k1::{PublicKey, Secp256k1};
use bitcoin::{Amount, FeeRate, Psbt, ScriptBuf, Transaction};
use bitcoincore_rpc::{Client, RpcApi};
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
use rand::{thread_rng, Rng};
use tokio::net::{TcpListener, TcpStream};

use std::collections::HashMap;
use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{self, Duration};

pub struct Node {
	pub peer_manager: Arc<PeerManager>,
	pub channel_manager: Arc<ChannelManager>,
	pub event_handler: Arc<SimpleEventHandler>,
	pub custom_message_handler: Arc<BatchMessageHandler>,
	pub broker: Broker,
	pub endpoint: String,
	node_id: PublicKey,
}

impl Node {
	pub fn new(port: u16, network: Network, db_path: String) -> Result<Self, Box<dyn Error>> {
		let mut rng = thread_rng();
		let seed = rng.gen_range(0..255);
		let secp = Secp256k1::new();

		// Step 1: Initialize KeysManager
		let seed_bytes = [seed; 32]; // Fixed seed for simplicity; use secure random seed in production
		let starting_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
		let keys_manager = Arc::new(KeysManager::new(
			&seed_bytes,
			starting_time.as_secs(),
			starting_time.subsec_nanos(),
		));

		let node_id = keys_manager.get_node_secret_key().public_key(&secp);

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

		let broker =
			Broker::new(node_id, &seed_bytes, network, &db_path, custom_message_handler.clone())?;

		let message_handler = MessageHandler {
			chan_handler: channel_manager.clone(),
			route_handler: Arc::new(IgnoringMessageHandler {}),
			onion_message_handler: Arc::new(IgnoringMessageHandler {}),
			custom_message_handler: custom_message_handler.clone(),
		};

		let ephemeral_random_data = [seed + 1; 32]; // Fixed for simplicity
		let peer_manager = Arc::new(PeerManager::new(
			message_handler,
			starting_time.as_secs() as u32,
			&ephemeral_random_data,
			logger.clone(),
			keys_manager.clone(),
		));

		Ok(Node {
			peer_manager,
			channel_manager,
			event_handler: Arc::new(SimpleEventHandler),
			custom_message_handler,
			broker,
			endpoint: format!("0.0.0.0:{}", port),
			node_id,
		})
	}

	pub fn node_id(&self) -> PublicKey {
		self.node_id
	}

	pub async fn start(&self) {
		let listener = TcpListener::bind(&self.endpoint).await.expect("Failed to bind port");
		println!("[{}] Node listening on {}", self.node_id, self.endpoint);

		// Clone to move into tasks
		let broker = self.broker.clone();
		let peer_manager = self.peer_manager.clone();
		let custom_message_handler = self.custom_message_handler.clone();
		let channel_manager = self.channel_manager.clone();

		// === 1. Background task: Event processing ===
		let cm_clone = channel_manager.clone();
		let eh_clone = self.event_handler.clone();
		tokio::spawn(async move {
			let mut interval = time::interval(Duration::from_millis(100));
			loop {
				interval.tick().await;
				cm_clone.process_pending_events(&*eh_clone);
			}
		});

		// === 2. Background task: Peer ticks (ping, cleanup) ===
		let pm_clone = peer_manager.clone();
		tokio::spawn(async move {
			let mut interval = time::interval(Duration::from_secs(10));
			loop {
				interval.tick().await;
				pm_clone.timer_tick_occurred();
			}
		});

		let broker_clone = broker.clone();
		let pm_clone = peer_manager.clone();
		let node_id = self.node_id;
		tokio::spawn(async move {
			let mut interval = time::interval(Duration::from_millis(50));
			loop {
				interval.tick().await;
				pm_clone.process_events();
				let messages: Vec<_> =
					custom_message_handler.queue.lock().unwrap().drain(..).collect();
				for (_, msg) in messages {
					process_batch_messages(&node_id, &broker_clone, pm_clone.clone(), msg).unwrap();
				}
			}
		});

		// === 3. Accept incoming connections ===
		loop {
			match listener.accept().await {
				Ok((stream, addr)) => {
					let node_id = self.node_id;
					let peer_manager = peer_manager.clone();
					println!("[{}] New connection from {}", node_id, addr);
					// Spawn a task to handle this connection
					tokio::spawn(async move {
						match stream.into_std() {
							Ok(std_stream) => {
								setup_inbound(peer_manager.clone(), std_stream).await;
							},
							Err(e) => {
								eprintln!(
									"[{}] Failed to convert tokio stream to std: {}",
									node_id, e
								);
							},
						}
					});
				},
				Err(e) => {
					eprintln!("[{}] Connection failed: {}", self.node_id, e);
				},
			}
		}
	}

	pub fn is_peer_connected(&self, their_node_id: &PublicKey) -> bool {
		self.peer_manager.peer_by_node_id(their_node_id).is_some()
	}

	pub async fn connect(&self, node_b: &Node) {
		let stream = TcpStream::connect(&node_b.endpoint).await.expect("Failed to connect");

		let node_b_id = node_b.node_id;
		let peer_manager = self.peer_manager.clone();
		tokio::spawn(async move {
			match stream.into_std() {
				Ok(std_stream) => setup_outbound(peer_manager.clone(), node_b_id, std_stream).await,
				Err(e) => eprintln!("❌ Failed to convert stream: {e}"),
			}
		});
	}

	pub fn build_psbt(
		&self, output_script: ScriptBuf, amount: Amount, fee_rate: FeeRate, locktime: LockTime,
	) -> Result<Psbt, Box<dyn Error>> {
		self.broker.build_psbt(output_script, amount, fee_rate, locktime)
	}

	pub fn init_psbt_batch(
		&self, their_node_id: PublicKey, output_script: ScriptBuf, amount: Amount,
		fee_rate: FeeRate, locktime: LockTime, uniform_amount: bool, fee_per_participant: Amount,
		max_participants: u8, max_utxo_count: u16,
	) -> Result<(), Box<dyn Error>> {
		self.peer_manager
			.peer_by_node_id(&their_node_id)
			.ok_or(format!("[{}] Peer not connected: {}", self.node_id, their_node_id))?;

		let mut psbt = self.build_psbt(output_script, amount, fee_rate, locktime)?;

		// Initiator must cover all the batch fees
		let total_fee = fee_per_participant * (max_participants as u64);

		self.add_utxos_to_psbt(&mut psbt, max_utxo_count, None, total_fee, true).unwrap();

		let fee_per_participant = fee_per_participant.to_sat();
		let uniform_amount = if uniform_amount { amount.to_sat() } else { 0 };

		let batch_psbt = BatchMessage::BatchPsbt {
			sender_node_id: self.node_id(),
			receiver_node_id: their_node_id,
			uniform_amount,
			fee_per_participant,
			max_participants: max_participants + 1,
			participants: vec![self.node_id()],
			hops: vec![self.node_id()],
			psbt_hex: psbt.serialize_hex(),
			sign: false,
		};

		self.broker.send(their_node_id, batch_psbt)?;

		Ok(())
	}

	pub async fn sync_wallet(&self, client: &Client, debug: bool) -> Result<(), Box<dyn Error>> {
		let mut wallet = self.broker.wallet.lock().unwrap();
		let latest = client.get_block_count()?;
		let stored = wallet.latest_checkpoint().block_id().height as u64;
		if debug {
			println!("[{}] SyncWalletBlock: (stored={} | latest={})", self.node_id, stored, latest);
		}
		for height in stored..latest {
			let hash = client.get_block_hash(height)?;
			let block = client.get_block(&hash)?;
			wallet.apply_block(&block, height as u32)?;
		}
		Ok(())
	}

	pub fn balance(&self) -> Balance {
		self.broker.balance()
	}

	pub fn add_utxos_to_psbt(
		&self, psbt: &mut Psbt, max_count: u16, uniform_amount: Option<Amount>, fee: Amount,
		payer: bool,
	) -> Result<(), Box<dyn std::error::Error>> {
		self.broker.add_utxos_to_psbt(psbt, max_count, uniform_amount, fee, payer)
	}

	pub fn broadcast_transactions(
		&self, txs: &[&Transaction],
	) -> Result<(), Box<dyn std::error::Error>> {
		self.broker.broadcast_transactions(txs)
	}
}

pub async fn fund_node(
	client: &Client, node: &Node, amount: Amount, utxos: u16,
) -> Result<(), Box<dyn std::error::Error>> {
	let mut wallet = node.broker.wallet.lock().unwrap();
	let mut rng = thread_rng();
	for _ in 0..utxos {
		let address = wallet.reveal_next_address(KeychainKind::External).address;
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
