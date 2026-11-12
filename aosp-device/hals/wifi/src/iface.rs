//! The interface objects the chip hands out: IWifiStaIface, IWifiApIface, IWifiP2pIface.
//!
//! Each one names a kernel network interface. The objects stay valid until the chip
//! removes the interface or the HAL stops; after that every call fails with
//! `ERROR_WIFI_IFACE_INVALID`, as clients holding stale objects expect.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use android_hardware_wifi::aidl::android::hardware::wifi::{
    CachedScanData::CachedScanData,
    IWifiApIface::IWifiApIface,
    IWifiP2pIface::IWifiP2pIface,
    IWifiStaIface::{FeatureSetMask::FeatureSetMask as StaFeature, IWifiStaIface},
    IWifiStaIfaceEventCallback::IWifiStaIfaceEventCallback,
    StaApfPacketFilterCapabilities::StaApfPacketFilterCapabilities,
    StaBackgroundScanCapabilities::StaBackgroundScanCapabilities,
    StaBackgroundScanParameters::StaBackgroundScanParameters,
    StaLinkLayerStats::StaLinkLayerStats,
    StaRoamingCapabilities::StaRoamingCapabilities,
    StaRoamingConfig::StaRoamingConfig,
    StaRoamingState::StaRoamingState,
    TwtCapabilities::TwtCapabilities,
    TwtRequest::TwtRequest,
    WifiDebugRxPacketFateReport::WifiDebugRxPacketFateReport,
    WifiDebugTxPacketFateReport::WifiDebugTxPacketFateReport,
    WifiStatusCode::WifiStatusCode,
};
use binder::{Interface, Strong};
use log::info;

use crate::netdev::{format_mac, MacAddress, Netdev};
use crate::nl80211::Nl80211;
use crate::status::{self, error, not_supported, unknown};

/// The APF version reported to the framework: APFv4, the oldest the framework's
/// ApfFilter and VTS (vendor API level U and later) accept.
const APF_VERSION: i32 = 4;
/// The packet filter memory, as in the emulator's HAL (VTS wants at least 2000 bytes).
const APF_MEMORY: usize = 4096;

/// Whether an interface object still stands for an interface of the chip.
#[derive(Debug, Clone)]
pub struct Validity(Arc<AtomicBool>);

impl Validity {
    /// A valid handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later call on the object fail.
    pub fn invalidate(&self) {
        self.0.store(false, Ordering::Release);
    }

    /// `ERROR_WIFI_IFACE_INVALID` once invalidated.
    fn check(&self) -> binder::Result<()> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(error(WifiStatusCode::ERROR_WIFI_IFACE_INVALID))
        }
    }
}

impl Default for Validity {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}

/// Sets the MAC address of `name`, logging the change.
fn set_mac(netdev: &Netdev, name: &str, mac: MacAddress) -> binder::Result<()> {
    netdev.set_mac(name, mac).map_err(|err| unknown(format!("{name}: set MAC address: {err}")))?;
    info!("{name}: MAC address {}", format_mac(mac));
    Ok(())
}

fn factory_mac(netdev: &Netdev, name: &str) -> binder::Result<MacAddress> {
    netdev.permanent_mac(name).map_err(|err| unknown(format!("{name}: factory MAC address: {err}")))
}

/// A Wi-Fi client interface. Scanning and connecting go through wpa_supplicant and
/// wificond; this object only manages the MAC address and keeps the APF program.
#[derive(Debug)]
pub struct StaIface {
    name: String,
    valid: Validity,
    netdev: Arc<Netdev>,
    /// The framework's APF program and data. Wi-Fi drivers on PCs do not run APF, so it
    /// is only stored, like the emulator HAL does; every packet still reaches the host.
    apf: Mutex<Vec<u8>>,
}

impl StaIface {
    /// The interface object for network interface `name`.
    pub fn new(name: String, valid: Validity, netdev: Arc<Netdev>) -> Self {
        Self { name, valid, netdev, apf: Mutex::new(vec![0; APF_MEMORY]) }
    }

    fn unsupported<T>(&self) -> binder::Result<T> {
        self.valid.check()?;
        not_supported()
    }
}

impl Interface for StaIface {}

impl IWifiStaIface for StaIface {
    fn getName(&self) -> binder::Result<String> {
        self.valid.check()?;
        Ok(self.name.clone())
    }

    fn getFeatureSet(&self) -> binder::Result<i32> {
        self.valid.check()?;
        Ok(StaFeature::APF.0)
    }

    fn getApfPacketFilterCapabilities(&self) -> binder::Result<StaApfPacketFilterCapabilities> {
        self.valid.check()?;
        Ok(StaApfPacketFilterCapabilities {
            version: APF_VERSION,
            maxLength: i32::try_from(APF_MEMORY).expect("APF memory size fits in i32"),
        })
    }

    fn installApfPacketFilter(&self, program: &[u8]) -> binder::Result<()> {
        self.valid.check()?;
        if program.len() > APF_MEMORY {
            return status::invalid_args();
        }
        self.apf.lock().expect("APF lock poisoned")[..program.len()].copy_from_slice(program);
        Ok(())
    }

    fn readApfPacketFilterData(&self) -> binder::Result<Vec<u8>> {
        self.valid.check()?;
        Ok(self.apf.lock().expect("APF lock poisoned").clone())
    }

    fn getFactoryMacAddress(&self) -> binder::Result<[u8; 6]> {
        self.valid.check()?;
        factory_mac(&self.netdev, &self.name)
    }

    fn setMacAddress(&self, mac: &[u8; 6]) -> binder::Result<()> {
        self.valid.check()?;
        set_mac(&self.netdev, &self.name, *mac)
    }

    fn registerEventCallback(
        &self,
        _callback: &Strong<dyn IWifiStaIfaceEventCallback>,
    ) -> binder::Result<()> {
        // Every event source (background scans, RSSI monitoring, TWT) is unsupported, so
        // there is nothing to deliver and no need to keep the callback.
        self.valid.check()
    }

    fn configureRoaming(&self, _config: &StaRoamingConfig) -> binder::Result<()> {
        self.unsupported()
    }

    fn disableLinkLayerStatsCollection(&self) -> binder::Result<()> {
        self.unsupported()
    }

    fn enableLinkLayerStatsCollection(&self, _debug: bool) -> binder::Result<()> {
        self.unsupported()
    }

    fn enableNdOffload(&self, _enable: bool) -> binder::Result<()> {
        self.unsupported()
    }

    fn getBackgroundScanCapabilities(&self) -> binder::Result<StaBackgroundScanCapabilities> {
        self.unsupported()
    }

    fn getDebugRxPacketFates(&self) -> binder::Result<Vec<WifiDebugRxPacketFateReport>> {
        self.unsupported()
    }

    fn getDebugTxPacketFates(&self) -> binder::Result<Vec<WifiDebugTxPacketFateReport>> {
        self.unsupported()
    }

    fn getLinkLayerStats(&self) -> binder::Result<StaLinkLayerStats> {
        self.unsupported()
    }

    fn getRoamingCapabilities(&self) -> binder::Result<StaRoamingCapabilities> {
        self.unsupported()
    }

    fn setRoamingState(&self, _state: StaRoamingState) -> binder::Result<()> {
        self.unsupported()
    }

    fn setScanMode(&self, _enable: bool) -> binder::Result<()> {
        self.unsupported()
    }

    fn startBackgroundScan(
        &self,
        _cmd_id: i32,
        _params: &StaBackgroundScanParameters,
    ) -> binder::Result<()> {
        self.unsupported()
    }

    fn startDebugPacketFateMonitoring(&self) -> binder::Result<()> {
        self.unsupported()
    }

    fn startRssiMonitoring(
        &self,
        _cmd_id: i32,
        _max_rssi: i32,
        _min_rssi: i32,
    ) -> binder::Result<()> {
        self.unsupported()
    }

    fn startSendingKeepAlivePackets(
        &self,
        _cmd_id: i32,
        _ip_packet_data: &[u8],
        _ether_type: u16,
        _src_address: &[u8; 6],
        _dst_address: &[u8; 6],
        _period_in_ms: i32,
    ) -> binder::Result<()> {
        self.unsupported()
    }

    fn stopBackgroundScan(&self, _cmd_id: i32) -> binder::Result<()> {
        self.unsupported()
    }

    fn stopRssiMonitoring(&self, _cmd_id: i32) -> binder::Result<()> {
        self.unsupported()
    }

    fn stopSendingKeepAlivePackets(&self, _cmd_id: i32) -> binder::Result<()> {
        self.unsupported()
    }

    fn setDtimMultiplier(&self, _multiplier: i32) -> binder::Result<()> {
        self.unsupported()
    }

    fn getCachedScanData(&self) -> binder::Result<CachedScanData> {
        self.unsupported()
    }

    fn twtGetCapabilities(&self) -> binder::Result<TwtCapabilities> {
        self.unsupported()
    }

    fn twtSessionSetup(&self, _cmd_id: i32, _request: &TwtRequest) -> binder::Result<()> {
        self.unsupported()
    }

    fn twtSessionUpdate(
        &self,
        _cmd_id: i32,
        _session_id: i32,
        _request: &TwtRequest,
    ) -> binder::Result<()> {
        self.unsupported()
    }

    fn twtSessionSuspend(&self, _cmd_id: i32, _session_id: i32) -> binder::Result<()> {
        self.unsupported()
    }

    fn twtSessionResume(&self, _cmd_id: i32, _session_id: i32) -> binder::Result<()> {
        self.unsupported()
    }

    fn twtSessionTeardown(&self, _cmd_id: i32, _session_id: i32) -> binder::Result<()> {
        self.unsupported()
    }

    fn twtSessionGetStats(&self, _cmd_id: i32, _session_id: i32) -> binder::Result<()> {
        self.unsupported()
    }
}

/// A soft AP interface, which hostapd runs.
#[derive(Debug)]
pub struct ApIface {
    name: String,
    valid: Validity,
    netdev: Arc<Netdev>,
    nl80211: Arc<Mutex<Nl80211>>,
}

impl ApIface {
    /// The interface object for network interface `name`.
    pub fn new(
        name: String,
        valid: Validity,
        netdev: Arc<Netdev>,
        nl80211: Arc<Mutex<Nl80211>>,
    ) -> Self {
        Self { name, valid, netdev, nl80211 }
    }
}

impl Interface for ApIface {}

impl IWifiApIface for ApIface {
    fn getName(&self) -> binder::Result<String> {
        self.valid.check()?;
        Ok(self.name.clone())
    }

    fn getBridgedInstances(&self) -> binder::Result<Vec<String>> {
        // No bridged (dual-band) AP: an AP is one interface.
        self.valid.check()?;
        Ok(Vec::new())
    }

    fn getFactoryMacAddress(&self) -> binder::Result<[u8; 6]> {
        self.valid.check()?;
        factory_mac(&self.netdev, &self.name)
    }

    fn setCountryCode(&self, code: &[u8; 2]) -> binder::Result<()> {
        self.valid.check()?;
        crate::chip::set_country_code(&self.nl80211, *code)
    }

    fn resetToFactoryMacAddress(&self) -> binder::Result<()> {
        self.valid.check()?;
        let mac = factory_mac(&self.netdev, &self.name)?;
        set_mac(&self.netdev, &self.name, mac)
    }

    fn setMacAddress(&self, mac: &[u8; 6]) -> binder::Result<()> {
        self.valid.check()?;
        set_mac(&self.netdev, &self.name, *mac)
    }

    fn usesMlo(&self) -> binder::Result<bool> {
        self.valid.check()?;
        Ok(false)
    }
}

/// A Wi-Fi Direct interface; wpa_supplicant creates and runs the kernel side.
#[derive(Debug)]
pub struct P2pIface {
    name: String,
    valid: Validity,
}

impl P2pIface {
    /// The interface object for P2P interface `name`.
    pub fn new(name: String, valid: Validity) -> Self {
        Self { name, valid }
    }
}

impl Interface for P2pIface {}

impl IWifiP2pIface for P2pIface {
    fn getName(&self) -> binder::Result<String> {
        self.valid.check()?;
        Ok(self.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidated_objects_fail() {
        let valid = Validity::new();
        let iface = P2pIface::new("p2p-dev-wlan0".into(), valid.clone());
        assert_eq!(iface.getName().expect("valid"), "p2p-dev-wlan0");
        valid.invalidate();
        let err = iface.getName().expect_err("invalidated");
        assert_eq!(err.service_specific_error(), WifiStatusCode::ERROR_WIFI_IFACE_INVALID.0);
    }

    #[test]
    fn apf_program_is_kept() {
        let netdev = Arc::new(Netdev::open().expect("socket"));
        let sta = StaIface::new("wlan0".into(), Validity::new(), netdev);
        let caps = sta.getApfPacketFilterCapabilities().expect("caps");
        assert!(caps.version >= 4 && caps.maxLength >= 2000, "VTS minimums: {caps:?}");

        sta.installApfPacketFilter(&[1, 2, 3]).expect("install");
        let data = sta.readApfPacketFilterData().expect("read");
        assert_eq!(data.len(), APF_MEMORY);
        assert_eq!(&data[..4], &[1, 2, 3, 0]);

        let too_big = vec![0; APF_MEMORY + 1];
        let err = sta.installApfPacketFilter(&too_big).expect_err("too big");
        assert_eq!(err.service_specific_error(), WifiStatusCode::ERROR_INVALID_ARGS.0);
        assert_ne!(sta.getFeatureSet().expect("features"), 0, "VTS wants a non-empty feature set");
    }
}
