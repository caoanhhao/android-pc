//! `log` facade backend writing to the serial console and the kernel log
//! buffer (`fs::kmsg`: /dev/kmsg, /proc/kmsg, syslog).

use core::fmt::Write;
use log::{Level, LevelFilter, Log, Metadata, Record};

/// A line formatted without allocating (the logger runs in any context).
struct LineBuf {
    buf: [u8; 400],
    len: usize,
}

impl Write for LineBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

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
        let level = match record.level() {
            Level::Error => 3,
            Level::Warn => 4,
            Level::Info => 6,
            Level::Debug | Level::Trace => 7,
        };
        let mut line = LineBuf { buf: [0; 400], len: 0 };
        let _ = write!(line, "{}", record.args());
        crate::fs::kmsg::record(0, level, &line.buf[..line.len]);
    }

    fn flush(&self) {}
}

static LOGGER: SerialLogger = SerialLogger;

/// Install the logger: Debug in debug builds, Info otherwise, or what
/// `loglevel=N` on the command line asks (Linux's console levels: 7
/// debug, 6 info, 4 warnings, 3 errors).
pub fn init() {
    log::set_logger(&LOGGER).expect("logger already set");
    let level = crate::boot::cmdline().split_ascii_whitespace().find_map(|w| w.strip_prefix("loglevel=")?.parse::<u8>().ok());
    log::set_max_level(match level {
        Some(8..) => LevelFilter::Trace,
        Some(7) => LevelFilter::Debug,
        Some(5 | 6) => LevelFilter::Info,
        Some(4) => LevelFilter::Warn,
        Some(_) => LevelFilter::Error,
        None if cfg!(debug_assertions) => LevelFilter::Debug,
        None => LevelFilter::Info,
    });
}
