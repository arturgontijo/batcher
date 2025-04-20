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

#[derive(Debug)]
pub enum BatchMessage {
	BatchPsbt {
		sender_node_id: PublicKey,
		receiver_node_id: PublicKey,
		uniform_amount: u64,
		fee_per_participant: u64,
		max_participants: u8,
		/// Vec of participants' node_id
		participants: Vec<PublicKey>,
		endpoints: Vec<String>,
		/// PSBT hex string
		psbt_hex: String,
		/// Signing workflow
		sign: bool,
	},
	AnyOther {
		sender_node_id: PublicKey,
		receiver_node_id: PublicKey,
		data: String,
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
				uniform_amount,
				fee_per_participant,
				max_participants,
				participants,
				endpoints,
				psbt_hex,
				sign,
			} => {
				w.write_all(&[0x01])?;
				sender_node_id.write(w)?;
				receiver_node_id.write(w)?;
				uniform_amount.write(w)?;
				fee_per_participant.write(w)?;
				max_participants.write(w)?;
				// Vectors must tell us their length
				let mut participants_len = 0;
				for _ in participants.iter() {
					participants_len += PUBLIC_KEY_SIZE;
				}
				(participants_len as u16).write(w)?;
				for pub_key in participants.iter() {
					pub_key.write(w)?;
				}

				let mut endpoints_len = 0;
				for ep in endpoints.iter() {
					endpoints_len += ep.len();
				}
				(endpoints_len as u16).write(w)?;
				for ep in endpoints.iter() {
					ep.write(w)?;
				}
				// ---
				psbt_hex.write(w)?;
				sign.write(w)
			},
			BatchMessage::AnyOther { sender_node_id, receiver_node_id, data } => {
				w.write_all(&[0x02])?;
				sender_node_id.write(w)?;
				receiver_node_id.write(w)?;
				data.write(w)
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
		let uniform_amount: u64 = Readable::read(r)?;
		let fee_per_participant: u64 = Readable::read(r)?;
		let max_participants: u8 = Readable::read(r)?;

		let participants_len: u16 = Readable::read(r)?;
		let mut participants: Vec<PublicKey> = Vec::new();
		let mut addr_readpos = 0;
		loop {
			if participants_len <= addr_readpos {
				break;
			}
			match Readable::read(r) {
				Ok(pub_key) => {
					if participants_len < addr_readpos + (PUBLIC_KEY_SIZE as u16) {
						return Err(DecodeError::BadLengthDescriptor);
					}
					addr_readpos += PUBLIC_KEY_SIZE as u16;
					participants.push(pub_key);
				},
				Err(DecodeError::ShortRead) => return Err(DecodeError::BadLengthDescriptor),
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

		let psbt_hex = String::read(r)?;
		let sign = bool::read(r)?;

		match msg_type[0] {
			0x01 => Ok(BatchMessage::BatchPsbt {
				sender_node_id,
				receiver_node_id,
				uniform_amount,
				fee_per_participant,
				max_participants,
				participants,
				endpoints,
				psbt_hex,
				sign,
			}),
			0x02 => Ok(BatchMessage::AnyOther { sender_node_id, receiver_node_id, data: psbt_hex }),
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
		Self { pending: Mutex::new(VecDeque::new()), queue: Mutex::new(VecDeque::new()) }
	}

	pub fn send(&self, node_id: PublicKey, msg: BatchMessage) {
		self.pending.lock().unwrap().push_back((node_id, msg));
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
		todo!()
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
