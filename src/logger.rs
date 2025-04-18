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
