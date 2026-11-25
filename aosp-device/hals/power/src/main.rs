//! android.hardware.power HAL for PCs: Android's boost and mode hints steer
//! `intel_pstate` (energy/performance preference, performance limits, turbo), or, without
//! it, the cpufreq floor and cap of every policy (ARM SoCs on schedutil).

mod freq;
mod power;
mod pstate;

use std::path::Path;
use std::process::ExitCode;

use android_hardware_power::aidl::android::hardware::power::IPower::{BnPower, IPower};
use binder::BinderFeatures;
use log::{error, info};
use pchal::Service;

use crate::freq::FreqLimits;
use crate::power::Power;
use crate::pstate::CpuFreq;

const LOG_TAG: &str = "android.hardware.power-service.pc";
const CPUFREQ_DIR: &str = "/sys/devices/system/cpu/cpufreq";
const PSTATE_DIR: &str = "/sys/devices/system/cpu/intel_pstate";

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let mut pstate = CpuFreq::discover(Path::new(CPUFREQ_DIR), Path::new(PSTATE_DIR));
    power::log_support();
    let power = Power::new();
    let spawned = if pstate.is_active() {
        power.spawn_worker(move |profile| pstate.apply(&pstate.settings(profile)))
    } else {
        let mut limits = FreqLimits::discover(Path::new(CPUFREQ_DIR));
        if limits.is_empty() {
            info!("no cpufreq policies either, power hints are ignored");
        }
        power.spawn_worker(move |profile| limits.apply(profile))
    };
    if let Err(err) = spawned {
        error!("cannot start the cpufreq thread, hints are ignored: {err}");
    }

    let binder = BnPower::new_binder(power, BinderFeatures::default());
    // getCpuHeadroom waits for its measurement window, so it gets threads of its own.
    let service = Service::new(Power::get_descriptor(), "default").extra_threads(3);
    let Err(err) = service.serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
