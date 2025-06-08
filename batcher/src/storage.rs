use bdk_wallet::rusqlite::{params, Connection};
use bitcoin::secp256k1::PublicKey;
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

pub fn now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).expect("cannot fail").as_secs()
}
