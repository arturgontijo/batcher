use bdk_wallet::rusqlite::{params, Connection};
use bitcoin::{consensus::encode, secp256k1::PublicKey, Psbt, Txid};
use serde::{Deserialize, Serialize};
use std::{
	str::FromStr,
	time::{SystemTime, UNIX_EPOCH},
};

use crate::types::BoxError;

pub struct PeerStorage {
	conn: Connection,
}

impl PeerStorage {
	pub fn new(path: &str) -> Result<Self, BoxError> {
		let conn = Connection::open(path)?;
		conn.execute(
			"CREATE TABLE IF NOT EXISTS peers (
                id TEXT PRIMARY KEY,
                endpoint TEXT NOT NULL,
				last_seen INTEGER NOT NULL
            )",
			[],
		)?;
		Ok(Self { conn })
	}

	pub fn upsert_peer(&self, node_id: &PublicKey, endpoint: String) -> Result<(), BoxError> {
		self.conn.execute(
			"INSERT INTO peers (id, endpoint, last_seen)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET endpoint = excluded.endpoint, last_seen = excluded.last_seen",
			params![node_id.to_string(), endpoint, now()],
		)?;
		Ok(())
	}

	pub fn delete_peer(&self, node_id: &PublicKey) -> Result<(), BoxError> {
		self.conn.execute("DELETE FROM peers WHERE node_id = ?1", params![node_id.to_string()])?;
		Ok(())
	}

	pub fn list_peers(&self) -> Result<Vec<(PublicKey, String, u64)>, BoxError> {
		let mut stmt = self.conn.prepare("SELECT id, endpoint, last_seen FROM peers")?;
		let rows = stmt.query_map([], |row| {
			let node_id_str: String = row.get(0)?;
			let endpoint: String = row.get(1)?;
			let node_id = PublicKey::from_str(&node_id_str).unwrap();
			let last_seen: u64 = row.get(2)?;
			Ok((node_id, endpoint, last_seen))
		})?;
		Ok(rows.map(Result::unwrap).collect())
	}
}

pub struct BrokerStorage {
	conn: Connection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BatchPsbtStatus {
	Created = 0,
	Ready = 1,
	Completed = 2,
}

impl From<u32> for BatchPsbtStatus {
	fn from(status: u32) -> Self {
		match status {
			0 => BatchPsbtStatus::Created,
			1 => BatchPsbtStatus::Ready,
			2 => BatchPsbtStatus::Completed,
			_ => panic!("Invalid status: {}", status),
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BatchPsbtKind {
	Node = 0,
	Multisig = 1,
	Foreign = 2,
}

impl From<u32> for BatchPsbtKind {
	fn from(kind: u32) -> Self {
		match kind {
			0 => BatchPsbtKind::Node,
			1 => BatchPsbtKind::Multisig,
			2 => BatchPsbtKind::Foreign,
			_ => panic!("Invalid PSBT kind: {}", kind),
		}
	}
}

#[derive(Clone, Debug)]
pub struct BatchPsbtStored {
	pub id: u32,
	pub txid: Txid,
	pub status: BatchPsbtStatus,
	pub kind: BatchPsbtKind,
	pub updated_at: u64,
	pub psbt: Psbt,
}

impl BrokerStorage {
	pub fn new(path: &str) -> Result<Self, BoxError> {
		let conn = Connection::open(path)?;
		conn.execute(
			"CREATE TABLE IF NOT EXISTS broker (
					id INTEGER PRIMARY KEY NOT NULL,
					txid BLOB UNIQUE NOT NULL,
					status INTEGER NOT NULL CHECK (status IN (0,1,2)),
					kind INTEGER NOT NULL CHECK (status IN (0,1,2)),
					updated_at INTEGER,
					psbt BLOB UNIQUE NOT NULL
            )",
			[],
		)?;
		Ok(Self { conn })
	}

	pub fn insert(
		&self, status: BatchPsbtStatus, kind: BatchPsbtKind, psbt: &Psbt,
	) -> Result<u32, BoxError> {
		let id = self.next_id()?;
		let status = status as u8;
		let kind = kind as u8;
		let updated_at = now();
		let txid = psbt.unsigned_tx.compute_txid()[..].to_vec();
		let psbt_ser = psbt.serialize();
		self.conn.execute(
			"INSERT INTO broker (id, txid, status, kind, updated_at, psbt) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
			params![id, txid, status, kind, updated_at, psbt_ser],
		)?;
		Ok(id)
	}

	pub fn update(&self, id: u32, status: BatchPsbtStatus, psbt: &Psbt) -> Result<(), BoxError> {
		let status = status as u8;
		let updated_at = now();
		let txid = psbt.unsigned_tx.compute_txid()[..].to_vec();
		let psbt_ser = psbt.serialize();
		self.conn.execute(
			"UPDATE broker SET txid = ?1, status = ?2, updated_at = ?3, psbt = ?4 WHERE id = ?5",
			params![txid, status, updated_at, psbt_ser, id],
		)?;
		Ok(())
	}

	pub fn delete(&self, id: u32) -> Result<(), BoxError> {
		self.conn.execute("DELETE FROM broker WHERE id = ?1", params![id])?;
		Ok(())
	}

	pub fn psbt_by_id(&self, id: u32) -> Result<Option<BatchPsbtStored>, BoxError> {
		let mut stmt = self
			.conn
			.prepare("SELECT txid, status, kind, updated_at, psbt FROM broker WHERE id = ?")?;
		let mut rows = stmt.query([id])?;
		match rows.next()? {
			Some(row) => {
				let txid: Vec<u8> = row.get(0)?;
				let txid: Txid = encode::deserialize(&txid).expect("must not fail, txid");
				let status: u32 = row.get(1)?;
				let status: BatchPsbtStatus = status.into();
				let kind: u32 = row.get(2)?;
				let kind: BatchPsbtKind = kind.into();
				let updated_at: u64 = row.get(3)?;
				let psbt_bytes: Vec<u8> = row.get(4)?;
				let psbt = Psbt::deserialize(&psbt_bytes)?;
				Ok(Some(BatchPsbtStored { id, txid, status, kind, updated_at, psbt }))
			},
			None => Ok(None),
		}
	}

	pub fn psbts(&self, status: BatchPsbtStatus) -> Result<Vec<BatchPsbtStored>, BoxError> {
		let mut stmt = self.conn.prepare(
			"SELECT id, txid, status, kind, updated_at, psbt FROM broker WHERE status = ?1",
		)?;
		let rows = stmt.query_map([status as u8], |row| {
			let id: u32 = row.get(0)?;
			let txid: Vec<u8> = row.get(1)?;
			let txid: Txid = encode::deserialize(&txid).expect("must not fail, txid");
			let status: u32 = row.get(2)?;
			let status: BatchPsbtStatus = status.into();
			let kind: u32 = row.get(3)?;
			let kind: BatchPsbtKind = kind.into();
			let updated_at: u64 = row.get(4)?;
			let psbt_bytes: Vec<u8> = row.get(5)?;
			let psbt = Psbt::deserialize(&psbt_bytes).unwrap();
			Ok(BatchPsbtStored { id, txid, status, kind, updated_at, psbt })
		})?;
		Ok(rows.map(Result::unwrap).collect())
	}

	pub fn next_id(&self) -> Result<u32, BoxError> {
		let mut stmt = self.conn.prepare("SELECT COUNT(*) FROM broker")?;
		let count: u32 = stmt.query_row([], |row| row.get(0))?;
		Ok(count)
	}
}

pub fn now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).expect("cannot fail").as_secs()
}
