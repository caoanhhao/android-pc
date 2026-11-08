//! `log` facade backend writing to the serial console.

use log::{Level, LevelFilter, Log, Metadata, Record};

struct SerialLogger;

impl Log for SerialLogger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let color = match record.level() {
            Level::Error => "\x1b[31m",
            Level::Warn => "\x1b[33m",
            Level::Info => "\x1b[32m",
            Level::Debug => "\x1b[36m",
            Level::Trace => "\x1b[90m",
        };
        let ms = crate::time::uptime_ms();
        kprintln!(
            "[{:>5}.{:03}] {color}{:<5}\x1b[0m cpu{} {}",
            ms / 1000,
            ms % 1000,
            record.level(),
            crate::arch::cpu_index(),
            record.args()
        );
    }

    fn flush(&self) {}
}

static LOGGER: SerialLogger = SerialLogger;

pub fn init() {
    log::set_logger(&LOGGER).expect("logger already set");
    log::set_max_level(if cfg!(debug_assertions) {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    });
}
