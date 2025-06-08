mod commands;

use std::{
	env,
	process::exit,
	sync::{Arc, RwLock},
};

use batcher::{config::NodeConfig, node::Node, types::BoxError};
use bitcoincore_rpc::Client;
use commands::{handle_command, print_subcommands_help};
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

fn run_node(config_path: &str) -> Result<(), BoxError> {
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
		select! {
			_ = ctrl_c() => (),
			_ = sigterm_stream.recv() => (),
		}
	});

	match node.stop() {
		Ok(_) => println!("Node stopped."),
		Err(_) => eprintln!("Node failed to stop!"),
	}

	Ok(())
}

fn interactive() -> Result<(), BoxError> {
	let node: Arc<RwLock<Option<Arc<Node>>>> = Default::default();
	let client: Arc<RwLock<Option<Arc<Client>>>> = Default::default();

	let node_cmd = node.clone();
	let client_cmd = client.clone();

	let runtime = Runtime::new()?;
	runtime.block_on(async {
		let config = Config::builder().build();
		let mut rl = Editor::<(), MemHistory>::with_history(config, MemHistory::new()).unwrap();

		let prompt = ">>> ";

		print_subcommands_help();
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

fn print_help() {
	println!("\nBatcher CLI:");
	println!("  r  | -r | run <CONFIG.TOML>    - Run a node using CONFIG.toml file");
	println!("  i  | -i |  interactive         - Run CLI in interactive mode");
	println!("  h  | -h | help                 - Show this help message");
	println!();
}

fn main() -> Result<(), BoxError> {
	let args: Vec<String> = env::args().collect();

	let mut op = "help";
	if args.len() >= 2 {
		op = &args[1];
	}

	match op.trim() {
		"r" | "-r" | "run" => {
			let config_path = if args.len() >= 3 { &args[2] } else { "config.toml" };
			run_node(config_path)?;
		},
		"i" | "-i" | "interactive" => interactive()?,
		"h" | "-h" | "help" => print_help(),
		_ => {
			eprintln!("Invalid option!");
			print_help();
		},
	};

	Ok(())
}
