//! Logging to logcat through the `log` facade.

use std::panic;

use log::LevelFilter;

/// Sends `log` records to logcat under `tag` and logs panics before the process aborts.
///
/// Debug builds of the HAL log at `Debug`, release builds at `Info`. Calling this a second
/// time keeps the first configuration.
pub fn init_logging(tag: &str) {
    let level = if cfg!(debug_assertions) { LevelFilter::Debug } else { LevelFilter::Info };
    let config = logger::Config::default().with_tag_on_device(tag).with_max_level(level);
    if !logger::init(config) {
        log::warn!("logger already initialized, keeping the existing configuration");
        return;
    }

    // Rust panics print to stderr, which init sends to /dev/null for HAL services.
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        log::error!("{info}");
        default_hook(info);
    }));
}
