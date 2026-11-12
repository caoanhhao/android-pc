//! android.hardware.wifi HAL for PCs: the chip behind `wlan0` (Intel iwlmvm on the PC,
//! mac80211_hwsim on Cuttlefish) through nl80211.
//!
//! The HAL reports the device's interface combinations, creates and deletes the AP
//! interfaces the framework asks for, sets MAC addresses and the regulatory domain, and
//! lists usable channels. Scanning and connecting stay with wificond and wpa_supplicant,
//! which talk nl80211 themselves; vendor extras (link layer statistics, background scans,
//! RTT, NAN, debug ring buffers) are reported as unsupported.

mod chip;
mod concurrency;
mod iface;
mod netdev;
mod nl80211;
mod props;
mod status;
mod wifi;

use std::process::ExitCode;

use android_hardware_wifi::aidl::android::hardware::wifi::IWifi::{BnWifi, IWifi};
use binder::BinderFeatures;
use log::error;
use pchal::Service;

use crate::wifi::Wifi;

const LOG_TAG: &str = "android.hardware.wifi-service.pc";

fn main() -> ExitCode {
    pchal::init_logging(LOG_TAG);

    let binder = BnWifi::new_binder(Wifi::default(), BinderFeatures::default());
    let Err(err) = Service::new(Wifi::get_descriptor(), "default").serve(binder.as_binder());
    error!("{err}");
    ExitCode::FAILURE
}
