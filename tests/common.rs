use bitcoin::Network;
use node::Node;

use std::{error::Error, sync::Arc};
use tokio::time::Duration;

use batcher::node;

pub async fn setup_nodes(
	count: u8, mut port: u16, network: Network,
) -> Result<Vec<Arc<Node>>, Box<dyn Error>> {
	let mut nodes = vec![];
	for i in 0..count {
		let node = Arc::new(Node::new(
			format!("node-{}", i),
			&[42 + i; 32],
			port,
			network,
			format!("data/wallet_{}.db", i),
		)?);
		let node_clone = node.clone();
		node_clone.start()?;
		nodes.push(node);
		port += 1;
	}
	tokio::time::sleep(Duration::from_secs(1)).await;
	Ok(nodes)
}
