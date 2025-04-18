use bitcoin::hashes::Hash;
use lightning::chain::chainmonitor::Persist;
use lightning::sign::InMemorySigner;
use lightning::util::ser::Writeable;

use std::collections::HashMap;
use std::sync::Mutex;

// In-Memory Persister for channel monitors
pub struct InMemoryPersister {
	pub(crate) monitors: Mutex<HashMap<[u8; 32], Vec<u8>>>,
}

impl Persist<InMemorySigner> for InMemoryPersister {
	fn persist_new_channel(
		&self, funding_txo: lightning::chain::transaction::OutPoint,
		monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
	) -> lightning::chain::ChannelMonitorUpdateStatus {
		let mut serialized = Vec::new();
		monitor.write(&mut serialized).unwrap();
		self.monitors
			.lock()
			.unwrap()
			.insert(funding_txo.txid.as_raw_hash().to_byte_array(), serialized);
		lightning::chain::ChannelMonitorUpdateStatus::Completed
	}

	fn update_persisted_channel(
		&self, funding_txo: lightning::chain::transaction::OutPoint,
		_monitor_update: Option<&lightning::chain::channelmonitor::ChannelMonitorUpdate>,
		monitor: &lightning::chain::channelmonitor::ChannelMonitor<InMemorySigner>,
	) -> lightning::chain::ChannelMonitorUpdateStatus {
		let mut serialized = Vec::new();
		monitor.write(&mut serialized).unwrap();
		self.monitors
			.lock()
			.unwrap()
			.insert(funding_txo.txid.as_raw_hash().to_byte_array(), serialized);
		lightning::chain::ChannelMonitorUpdateStatus::Completed
	}

	fn archive_persisted_channel(
		&self, _channel_funding_outpoint: lightning::chain::transaction::OutPoint,
	) {
		// No-op for mock
	}
}
