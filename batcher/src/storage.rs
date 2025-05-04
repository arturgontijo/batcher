use bdk_wallet::rusqlite::{params, Connection, Result};
use bitcoin::secp256k1::PublicKey;
use std::str::FromStr;

pub struct PeerStorage {
	conn: Connection,
}

impl PeerStorage {
	pub fn new(path: &str) -> Result<Self> {
		let conn = Connection::open(path)?;
		conn.execute(
			"CREATE TABLE IF NOT EXISTS peers (
                node_id TEXT PRIMARY KEY,
                node_addr TEXT NOT NULL
            )",
			[],
		)?;
		Ok(Self { conn })
	}

	pub fn insert_peer(&self, node_id: &PublicKey, addr: String) -> Result<()> {
		self.conn.execute(
			"INSERT OR IGNORE INTO peers (node_id, node_addr) VALUES (?1, ?2)",
			params![node_id.to_string(), addr.to_string()],
		)?;
		Ok(())
	}

	pub fn update_peer(&self, node_id: &PublicKey, new_addr: String) -> Result<()> {
		self.conn.execute(
			"UPDATE peers SET node_addr = ?1 WHERE node_id = ?2",
			params![new_addr.to_string(), node_id.to_string()],
		)?;
		Ok(())
	}

	pub fn upsert_peer(&self, node_id: &PublicKey, addr: String) -> Result<()> {
		self.conn.execute(
			"INSERT INTO peers (node_id, node_addr)
             VALUES (?1, ?2)
             ON CONFLICT(node_id) DO UPDATE SET node_addr = excluded.node_addr",
			params![node_id.to_string(), addr.to_string()],
		)?;
		Ok(())
	}

	pub fn delete_peer(&self, node_id: &PublicKey) -> Result<()> {
		self.conn.execute("DELETE FROM peers WHERE node_id = ?1", params![node_id.to_string()])?;
		Ok(())
	}

	pub fn list_peers(&self) -> Result<Vec<(PublicKey, String)>> {
		let mut stmt = self.conn.prepare("SELECT node_id, node_addr FROM peers")?;
		let rows = stmt.query_map([], |row| {
			let node_id_str: String = row.get(0)?;
			let addr_str: String = row.get(1)?;
			let node_id = PublicKey::from_str(&node_id_str).unwrap();
			Ok((node_id, addr_str))
		})?;
		Ok(rows.map(Result::unwrap).collect())
	}
}
