use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::Path;

use bitcoin::secp256k1::PublicKey;
use chrono::Utc;
use lightning::util::logger::Level;
use lightning::util::logger::Logger;
use lightning::util::logger::Record;

pub struct SimpleLogger {
	file_path: String,
	log_max_level: Level,
}

impl SimpleLogger {
	pub fn new(file_path: String, log_max_level: Level) -> Result<Self, Box<dyn Error>> {
		if let Some(parent_dir) = Path::new(&file_path).parent() {
			match fs::create_dir_all(parent_dir) {
				Ok(_) => {},
				Err(err) => {
					return Err(
						format!("ERROR: Failed to create log parent directory: {}", err).into()
					)
				},
			}

			// make sure the file exists.
			match fs::OpenOptions::new().create(true).append(true).open(&file_path) {
				Ok(_) => {},
				Err(err) => return Err(format!("ERROR: Failed to open log file: {}", err).into()),
			}
		}

		Ok(Self { file_path, log_max_level })
	}
}

impl Logger for SimpleLogger {
	fn log(&self, record: Record) {
		if record.level < self.log_max_level {
			return;
		}

		let log = format!(
			"{} {:<5} [{}:{}] {}\n",
			Utc::now().format("%Y-%m-%d %H:%M:%S"),
			record.level.to_string(),
			record.module_path,
			record.line,
			record.args
		);

		fs::OpenOptions::new()
			.create(true)
			.append(true)
			.open(&self.file_path)
			.expect("Failed to open log file")
			.write_all(log.as_bytes())
			.expect("Failed to write to log file")
	}
}

pub fn print_pubkey(pubkey: &PublicKey) -> String {
	let chars: Vec<char> = pubkey.to_string().chars().collect();
	let len = chars.len();
	let first: String = chars[..5].iter().collect();
	let last: String = chars[len - 5..].iter().collect();
	format!("{}...{}", first, last)
}
