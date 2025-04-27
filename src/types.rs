use bdk_wallet::rusqlite::Connection;
use lightning::chain::chaininterface::BroadcasterInterface;
use lightning::chain::chaininterface::FeeEstimator;
use lightning::chain::chainmonitor;
use lightning::chain::Filter;
use lightning::ln::peer_handler::IgnoringMessageHandler;
use lightning::routing::gossip;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringFeeParameters};
use lightning::sign::InMemorySigner;
use lightning::sign::KeysManager;

use lightning_net_tokio::SocketDescriptor;

use std::sync::{Arc, Mutex};

use crate::logger::SimpleLogger;
use crate::messages::BatchMessageHandler;
use crate::persister::InMemoryPersister;

// Mock Filter for chain monitoring
pub struct MockFilter;
impl Filter for MockFilter {
	fn register_tx(&self, _txid: &lightning::bitcoin::Txid, _script_pubkey: &bitcoin::Script) {}
	fn register_output(&self, _watched: lightning::chain::WatchedOutput) {}
}

// Mock Fee Estimator with a fixed fee rate
pub struct FixedFeeEstimator {
	pub sat_per_kw: u32,
}
impl FeeEstimator for FixedFeeEstimator {
	fn get_est_sat_per_1000_weight(
		&self, _confirmation_target: lightning::chain::chaininterface::ConfirmationTarget,
	) -> u32 {
		self.sat_per_kw
	}
}

// Mock Broadcaster that logs transactions
pub struct MockBroadcaster;
impl BroadcasterInterface for MockBroadcaster {
	fn broadcast_transactions(&self, txs: &[&lightning::bitcoin::Transaction]) {
		for tx in txs {
			println!("Broadcasting transaction: {:?}", tx);
		}
	}
}

pub type ChainMonitor = chainmonitor::ChainMonitor<
	InMemorySigner,
	Arc<MockFilter>,
	Arc<MockBroadcaster>,
	Arc<FixedFeeEstimator>,
	Arc<SimpleLogger>,
	Arc<InMemoryPersister>,
>;

pub type PeerManager = lightning::ln::peer_handler::PeerManager<
	SocketDescriptor,
	Arc<ChannelManager>,
	Arc<IgnoringMessageHandler>,
	Arc<IgnoringMessageHandler>,
	Arc<SimpleLogger>,
	Arc<BatchMessageHandler>,
	Arc<KeysManager>,
>;

pub type ChannelManager = lightning::ln::channelmanager::ChannelManager<
	Arc<ChainMonitor>,
	Arc<MockBroadcaster>,
	Arc<KeysManager>,
	Arc<KeysManager>,
	Arc<KeysManager>,
	Arc<FixedFeeEstimator>,
	Arc<Router>,
	Arc<MessageRouter>,
	Arc<SimpleLogger>,
>;

pub type PersistedWallet = bdk_wallet::PersistedWallet<Connection>;

// pub type Wallet =
// 	crate::wallet::Wallet<Arc<Broadcaster>, Arc<OnchainFeeEstimator>, Arc<SimpleLogger>>;

// pub type KeysManager =
// 	crate::wallet::WalletKeysManager<Arc<Broadcaster>, Arc<OnchainFeeEstimator>, Arc<SimpleLogger>>;

pub type Router = DefaultRouter<
	Arc<Graph>,
	Arc<SimpleLogger>,
	Arc<KeysManager>,
	Arc<Mutex<Scorer>>,
	ProbabilisticScoringFeeParameters,
	Scorer,
>;
pub type Scorer = ProbabilisticScorer<Arc<Graph>, Arc<SimpleLogger>>;

pub type Graph = gossip::NetworkGraph<Arc<SimpleLogger>>;

pub type MessageRouter = lightning::onion_message::messenger::DefaultMessageRouter<
	Arc<Graph>,
	Arc<SimpleLogger>,
	Arc<KeysManager>,
>;

// pub type OnionMessenger = lightning::onion_message::messenger::OnionMessenger<
//     Arc<KeysManager>,
//     Arc<KeysManager>,
//     Arc<SimpleLogger>,
//     Arc<ChannelManager>,
//     Arc<MessageRouter>,
//     Arc<ChannelManager>,
//     IgnoringMessageHandler,
//     IgnoringMessageHandler,
//     IgnoringMessageHandler,
// >;
