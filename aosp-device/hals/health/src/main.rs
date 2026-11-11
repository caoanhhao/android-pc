//! android.hardware.health HAL for PCs: AC adapters, USB-C chargers and laptop batteries
//! from `/sys/class/power_supply`; a desktop with none of them is reported as on mains
//! with no battery.

mod health;
mod supply;

use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use android_hardware_health::aidl::android::hardware::health::IHealth::{BnHealth, IHealth};
use binder::BinderFeatures;
use log::{error, warn};
use pchal::uevent::UeventSocket;
use pchal::Service;

use crate::health::Health;

const LOG_TAG: &str = "android.hardware.health-service.pc";
const POWER_SUPPLY_DIR: &str = "/sys/class/power_supply";
/// How often a battery's level and current are re-read when no uevent arrives (the
/// interval healthd uses while the screen is on).
const BATTERY_POLL: Duration = Duration::from_secs(60);

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let health = Health::new(Path::new(POWER_SUPPLY_DIR));
    spawn_uevent_listener(health.clone());
    spawn_battery_poller(health.clone());

    let binder = BnHealth::new_binder(health, BinderFeatures::default());
    let service = Service::new(Health::get_descriptor(), "default").extra_threads(1);
    let Err(err) = service.serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}

/// Pushes fresh health info to the clients on every `power_supply` uevent (plugging an
/// adapter in, a battery crossing a level step, ...).
fn spawn_uevent_listener(health: Health) {
    let socket = match UeventSocket::open() {
        Ok(socket) => socket,
        Err(err) => {
            warn!("no uevents, polling only: {err}");
            return;
        }
    };
    let spawned = thread::Builder::new().name("uevent".into()).spawn(move || loop {
        match socket.recv() {
            Ok(event) if event.subsystem() == Some("power_supply") => health.notify(),
            Ok(_) => {}
            Err(err) => {
                error!("{err}");
                return;
            }
        }
    });
    if let Err(err) = spawned {
        error!("cannot start the uevent thread: {err}");
    }
}

/// Batteries do not send a uevent for every change in current or charge, so their state
/// is also re-read periodically. Without a battery nothing changes on its own.
fn spawn_battery_poller(health: Health) {
    let spawned = thread::Builder::new().name("poll".into()).spawn(move || loop {
        thread::sleep(BATTERY_POLL);
        if health.has_battery() {
            health.notify();
        }
    });
    if let Err(err) = spawned {
        error!("cannot start the poll thread: {err}");
    }
}
