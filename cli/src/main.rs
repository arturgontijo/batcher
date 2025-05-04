mod commands;

use std::{
	error::Error,
	sync::{Arc, RwLock},
};

use batcher::node::Node;
use commands::{handle_command, print_help};
use rustyline::{
	history::{History, MemHistory},
	Config, Editor,
};
use tokio::runtime::Runtime;

fn main() -> Result<(), Box<dyn Error>> {
	let node: Arc<RwLock<Option<Arc<Node>>>> = Default::default();

	let node_cmd = node.clone();

	let runtime = Runtime::new()?;
	runtime.block_on(async {
		let config = Config::builder().build();
		let mut rl = Editor::<(), MemHistory>::with_history(config, MemHistory::new()).unwrap();

		let prompt = ">>> ";

		print_help();
		loop {
			match rl.readline(prompt) {
				Ok(line) => {
					let line = line.trim();
					if ["exit", "quit", "q"].contains(&line) {
						break;
					}
					rl.history_mut().add(line).unwrap();

					if let Err(e) = handle_command(line, &node_cmd) {
						eprintln!("Error: {}", e);
					}
				},
				Err(err) => {
					eprintln!("{}", err);
					break;
				},
			}
		}
	});

	let unlocked = node.write().unwrap();
	if let Some(node) = unlocked.clone() {
		match node.stop() {
			Ok(_) => println!("Node stopped."),
			Err(_) => eprintln!("Node failed to stop!"),
		}
	}
	Ok(())
}
