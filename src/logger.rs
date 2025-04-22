use bitcoin::secp256k1::PublicKey;
// use lightning::util::logger::Level;
use lightning::util::logger::Logger;
use lightning::util::logger::Record;

pub struct SimpleLogger;

impl Logger for SimpleLogger {
	fn log(&self, _record: Record) {
		// match record.level {
		// 	Level::Trace => {},
		// 	_ => println!("{}: {}", record.level, record.args),
		// }
	}
}

pub fn print_pubkey(pubkey: &PublicKey) -> String {
	let chars: Vec<char> = pubkey.to_string().chars().collect();
	let len = chars.len();
	let first: String = chars[..5].iter().collect();
	let last: String = chars[len - 5..].iter().collect();
	format!("{}...{}", first, last)
}
