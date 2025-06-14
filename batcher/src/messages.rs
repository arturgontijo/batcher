use std::collections::VecDeque;
use std::sync::Mutex;

use bitcoin::io::Read;
use bitcoin::key::constants::PUBLIC_KEY_SIZE;
use bitcoin::secp256k1::PublicKey;
use lightning::ln::msgs::{DecodeError, Init, LightningError};
use lightning::ln::peer_handler::CustomMessageHandler;
use lightning::ln::wire::{self, CustomMessageReader};
use lightning::types::features::{InitFeatures, NodeFeatures};
use lightning::util::ser::{Readable, Writeable, Writer};

#[derive(Debug, Clone)]
pub enum BatchMessage {
	BatchPsbt {
		sender_node_id: PublicKey,
		receiver_node_id: PublicKey,
		id: u32,
		uniform_amount: u64,
		fee_per_participant: u64,
		max_utxo_per_participant: u8,
		max_participants: u8,
		max_hops: u8,
		/// Vec of participants' node_id
		participants: Vec<PublicKey>,
		endpoints: Vec<String>,
		not_participants: Vec<PublicKey>,
		hops: u8,
		/// PSBT bytes
		psbt: Vec<u8>,
		/// Signing workflow
		sign: bool,
	},
	Announcement {
		sender_node_id: PublicKey,
		receiver_node_id: PublicKey,
		endpoint: String,
	},
}

macro_rules! batchmessage_type_id {
	() => {
		42069
	};
}

impl wire::Type for BatchMessage {
	fn type_id(&self) -> u16 {
		batchmessage_type_id!()
	}
}

impl Writeable for BatchMessage {
	fn write<W: Writer>(&self, w: &mut W) -> Result<(), lightning::io::Error> {
		match self {
			BatchMessage::BatchPsbt {
				sender_node_id,
				receiver_node_id,
				id,
				uniform_amount,
				fee_per_participant,
				max_utxo_per_participant,
				max_participants,
				max_hops,
				participants,
				endpoints,
				not_participants,
				hops,
				psbt,
				sign,
			} => {
				w.write_all(&[0x01])?;
				sender_node_id.write(w)?;
				receiver_node_id.write(w)?;
				id.write(w)?;
				uniform_amount.write(w)?;
				fee_per_participant.write(w)?;
				max_utxo_per_participant.write(w)?;
				max_participants.write(w)?;
				max_hops.write(w)?;
				// Vectors must tell us their length
				let mut participants_len = 0;
				for _ in participants.iter() {
					participants_len += PUBLIC_KEY_SIZE;
				}
				(participants_len as u16).write(w)?;
				for pubkey in participants.iter() {
					pubkey.write(w)?;
				}

				let mut endpoints_len = 0;
				for ep in endpoints.iter() {
					endpoints_len += ep.len();
				}
				(endpoints_len as u16).write(w)?;
				for ep in endpoints.iter() {
					ep.write(w)?;
				}

				let mut not_participants_len = 0;
				for _ in not_participants.iter() {
					not_participants_len += PUBLIC_KEY_SIZE;
				}
				(not_participants_len as u16).write(w)?;
				for pubkey in not_participants.iter() {
					pubkey.write(w)?;
				}
				// ---
				hops.write(w)?;
				psbt.write(w)?;
				sign.write(w)
			},
			BatchMessage::Announcement { sender_node_id, receiver_node_id, endpoint } => {
				w.write_all(&[0x02])?;
				sender_node_id.write(w)?;
				receiver_node_id.write(w)?;
				endpoint.write(w)
			},
		}
	}
}

impl Readable for BatchMessage {
	fn read<R: Read>(r: &mut R) -> Result<Self, DecodeError> {
		let mut msg_type = [0; 1];
		r.read_exact(&mut msg_type)?;

		let sender_node_id = PublicKey::read(r)?;
		let receiver_node_id = PublicKey::read(r)?;

		match msg_type[0] {
			0x01 => {
				let id = u32::read(r)?;

				let uniform_amount: u64 = Readable::read(r)?;
				let fee_per_participant: u64 = Readable::read(r)?;
				let max_utxo_per_participant: u8 = Readable::read(r)?;
				let max_participants: u8 = Readable::read(r)?;
				let max_hops: u8 = Readable::read(r)?;

				let participants_len: u16 = Readable::read(r)?;
				let mut participants: Vec<PublicKey> = Vec::new();
				let mut addr_readpos = 0;
				loop {
					if participants_len <= addr_readpos {
						break;
					}
					match Readable::read(r) {
						Ok(pubkey) => {
							if participants_len < addr_readpos + (PUBLIC_KEY_SIZE as u16) {
								return Err(DecodeError::BadLengthDescriptor);
							}
							addr_readpos += PUBLIC_KEY_SIZE as u16;
							participants.push(pubkey);
						},
						Err(DecodeError::ShortRead) => {
							return Err(DecodeError::BadLengthDescriptor)
						},
						Err(e) => return Err(e),
					}
				}

				let endpoints_len: u16 = Readable::read(r)?;
				let mut endpoints: Vec<String> = Vec::new();
				let mut addr_readpos = 0;

				while addr_readpos < endpoints_len {
					let ep = String::read(r)?;
					addr_readpos += ep.len() as u16;
					endpoints.push(ep);
				}

				let not_participants_len: u16 = Readable::read(r)?;
				let mut not_participants: Vec<PublicKey> = Vec::new();
				let mut addr_readpos = 0;
				loop {
					if not_participants_len <= addr_readpos {
						break;
					}
					match Readable::read(r) {
						Ok(pubkey) => {
							if not_participants_len < addr_readpos + (PUBLIC_KEY_SIZE as u16) {
								return Err(DecodeError::BadLengthDescriptor);
							}
							addr_readpos += PUBLIC_KEY_SIZE as u16;
							not_participants.push(pubkey);
						},
						Err(DecodeError::ShortRead) => {
							return Err(DecodeError::BadLengthDescriptor)
						},
						Err(e) => return Err(e),
					}
				}

				let hops = u8::read(r)?;
				let psbt = Vec::<u8>::read(r)?;
				let sign = bool::read(r)?;

				Ok(BatchMessage::BatchPsbt {
					sender_node_id,
					receiver_node_id,
					id,
					uniform_amount,
					fee_per_participant,
					max_utxo_per_participant,
					max_participants,
					max_hops,
					participants,
					endpoints,
					not_participants,
					hops,
					psbt,
					sign,
				})
			},
			0x02 => {
				let endpoint = String::read(r)?;
				Ok(BatchMessage::Announcement { sender_node_id, receiver_node_id, endpoint })
			},
			_ => Err(DecodeError::UnknownVersion),
		}
	}
}

pub struct BatchMessageHandler {
	pub pending: Mutex<VecDeque<(PublicKey, BatchMessage)>>,
	pub queue: Mutex<VecDeque<(PublicKey, BatchMessage)>>,
}

impl BatchMessageHandler {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn send(&self, node_id: PublicKey, msg: BatchMessage) {
		self.pending.lock().unwrap().push_back((node_id, msg));
	}
}

impl Default for BatchMessageHandler {
	fn default() -> Self {
		Self { pending: Mutex::new(VecDeque::new()), queue: Mutex::new(VecDeque::new()) }
	}
}

impl CustomMessageHandler for BatchMessageHandler {
	fn handle_custom_message(
		&self, msg: Self::CustomMessage, sender_node_id: PublicKey,
	) -> Result<(), LightningError> {
		self.queue.lock().unwrap().push_back((sender_node_id, msg));
		Ok(())
	}

	fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
		self.pending.lock().unwrap().drain(..).collect()
	}

	fn peer_disconnected(&self, _their_node_id: PublicKey) {
		// println!("📩 peer_disconnected: {}", their_node_id);
	}

	fn peer_connected(
		&self, _their_node_id: PublicKey, _msg: &Init, _inbound: bool,
	) -> Result<(), ()> {
		// println!("📩 peer_connected: {}", their_node_id);
		Ok(())
	}

	fn provided_node_features(&self) -> NodeFeatures {
		NodeFeatures::empty()
	}

	fn provided_init_features(&self, _their_node_id: PublicKey) -> InitFeatures {
		InitFeatures::empty()
	}
}

impl CustomMessageReader for BatchMessageHandler {
	type CustomMessage = BatchMessage;
	fn read<R: Read>(
		&self, msg_type: u16, r: &mut R,
	) -> Result<Option<Self::CustomMessage>, DecodeError> {
		match msg_type {
			42069 => Ok(Some(BatchMessage::read(r)?)),
			_ => Ok(None),
		}
	}
}
