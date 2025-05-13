mod commands;

use std::{
	env,
	error::Error,
	process::exit,
	sync::{Arc, RwLock},
};

use batcher::{config::NodeConfig, node::Node};
use bitcoincore_rpc::Client;
use commands::{handle_command, print_help};
use rustyline::{
	history::{History, MemHistory},
	Config, Editor,
};
use tokio::{
	runtime::Runtime,
	select,
	signal::{
		ctrl_c,
		unix::{signal, SignalKind},
	},
};

fn run_node(config_path: &str) -> Result<(), Box<dyn Error>> {
	let config = NodeConfig::new(config_path)?;
	let log_file_path = config.logger.file_path.clone();
	let node = Node::new_from_config(config)?;

	println!(
		"\n[{}][{}] Starting Node at: {} [ tail -f {} ]",
		node.node_id(),
		node.alias(),
		node.endpoint(),
		log_file_path
	);
	node.start()?;

	let runtime = Runtime::new()?;
	runtime.block_on(async {
		let mut sigterm_stream = match signal(SignalKind::terminate()) {
			Ok(stream) => stream,
			Err(_) => exit(1),
		};
		loop {
			select! {
				_ = ctrl_c() => break,
				_ = sigterm_stream.recv() => break,
			}
		}
	});

	match node.stop() {
		Ok(_) => println!("Node stopped."),
		Err(_) => eprintln!("Node failed to stop!"),
	}

	Ok(())
}

fn interactive() -> Result<(), Box<dyn Error>> {
	let node: Arc<RwLock<Option<Arc<Node>>>> = Default::default();
	let client: Arc<RwLock<Option<Arc<Client>>>> = Default::default();

	let node_cmd = node.clone();
	let client_cmd = client.clone();

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

					if let Err(e) = handle_command(line, &node_cmd, &client_cmd) {
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

fn main() -> Result<(), Box<dyn Error>> {
	let args: Vec<String> = env::args().collect();

	let mut op = "run";
	if args.len() >= 2 {
		op = &args[1];
	}

	match op.trim() {
		"run" => {
			let config_path = if args.len() >= 3 { &args[2] } else { "config.toml" };
			run_node(config_path)?;
		},
		"i" | "-i" | "interactive" => interactive()?,
		_ => eprintln!("Invalid option"),
	};

	Ok(())
}
