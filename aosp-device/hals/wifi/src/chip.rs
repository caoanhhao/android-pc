//! IWifiChip: the wireless device behind the primary interface (`wlan0`), its interface
//! combinations, and the STA, AP and P2P interfaces the framework creates on it.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use android_hardware_wifi::aidl::android::hardware::wifi::{
    common::OuiKeyedData::OuiKeyedData,
    AfcChannelAllowance::AfcChannelAllowance,
    IWifiApIface::{BnWifiApIface, IWifiApIface},
    IWifiChip::{
        ApIfaceParams::ApIfaceParams, ChipConcurrencyCombination::ChipConcurrencyCombination,
        ChipConcurrencyCombinationLimit::ChipConcurrencyCombinationLimit,
        ChipDebugInfo::ChipDebugInfo, ChipMloMode::ChipMloMode, ChipMode::ChipMode,
        CoexUnsafeChannel::CoexUnsafeChannel, IWifiChip, LatencyMode::LatencyMode,
        MultiStaUseCase::MultiStaUseCase, TxPowerScenario::TxPowerScenario, VoipMode::VoipMode,
    },
    IWifiChipEventCallback::IWifiChipEventCallback,
    IWifiNanIface::IWifiNanIface,
    IWifiP2pIface::{BnWifiP2pIface, IWifiP2pIface},
    IWifiRttController::IWifiRttController,
    IWifiStaIface::{BnWifiStaIface, IWifiStaIface},
    IfaceConcurrencyType::IfaceConcurrencyType,
    IfaceType::IfaceType,
    WifiBand::WifiBand,
    WifiChannelWidthInMhz::WifiChannelWidthInMhz,
    WifiChipCapabilities::WifiChipCapabilities,
    WifiDebugHostWakeReasonStats::WifiDebugHostWakeReasonStats,
    WifiDebugRingBufferStatus::WifiDebugRingBufferStatus,
    WifiDebugRingBufferVerboseLevel::WifiDebugRingBufferVerboseLevel,
    WifiIfaceMode::WifiIfaceMode,
    WifiRadioCombination::WifiRadioCombination,
    WifiStatusCode::WifiStatusCode,
    WifiUsableChannel::WifiUsableChannel,
};
use binder::{BinderFeatures, ExceptionCode, FromIBinder, Interface, Strong};
use log::{info, warn};
use pchal::Callbacks;

use crate::concurrency::{self, Combination, Counts, Kind};
use crate::iface::{ApIface, P2pIface, StaIface, Validity};
use crate::netdev::{self, Netdev};
use crate::nl80211::{Band, Channel, Iftype, Nl80211};
use crate::props;
use crate::status::{error, invalid_args, not_supported, unknown};

/// The chip id `IWifi` reports; there is one chip.
pub const CHIP_ID: i32 = 0;
/// The id of the chip's only mode, which offers every interface combination.
const MODE_ID: i32 = 0;
/// Interface names `wlan0`..`wlan4`, as in AOSP's default HAL.
const MAX_WLAN_IFACES: u32 = 5;
/// Set to the first STA (or AP) interface, as AOSP's default HAL does.
const ACTIVE_IFACE_PROPERTY: &str = "wifi.active.interface";

/// The name of the `index`-th WLAN interface: `wifi.interface` / `wifi.concurrent.interface`
/// for the first two, then `wifi.interface.<index>`, else `wlan<index>`.
fn wlan_name(index: u32) -> String {
    let alias = match index {
        0 => props::read("wifi.interface"),
        1 => props::read("wifi.concurrent.interface"),
        _ => None,
    };
    alias
        .or_else(|| props::read(&format!("wifi.interface.{index}")))
        .unwrap_or_else(|| format!("wlan{index}"))
}

/// Asks the kernel for the regulatory rules of country `code`.
pub fn set_country_code(nl80211: &Mutex<Nl80211>, code: [u8; 2]) -> binder::Result<()> {
    nl80211.lock().expect("nl80211 lock poisoned").set_regulatory_domain(code).map_err(|err| {
        unknown(format!("set country code {:?}: {err}", String::from_utf8_lossy(&code)))
    })?;
    info!("country code {}", String::from_utf8_lossy(&code));
    Ok(())
}

/// An interface the chip handed out.
struct Entry<B: ?Sized + FromIBinder> {
    name: String,
    /// Whether this HAL created the kernel interface (and deletes it on removal); the
    /// driver's own interfaces (`wlan0`) stay.
    created: bool,
    valid: Validity,
    binder: Strong<B>,
}

impl<B: ?Sized + FromIBinder> Entry<B> {
    /// The binder of the entry `name` in `list`.
    fn find(list: &[Self], name: &str) -> binder::Result<Strong<B>> {
        list.iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.binder.clone())
            .ok_or_else(|| error(WifiStatusCode::ERROR_INVALID_ARGS))
    }

    /// Takes the entry `name` out of `list`.
    fn take(list: &mut Vec<Self>, name: &str) -> binder::Result<Self> {
        let index = list
            .iter()
            .position(|entry| entry.name == name)
            .ok_or_else(|| error(WifiStatusCode::ERROR_INVALID_ARGS))?;
        Ok(list.remove(index))
    }
}

/// The chip's state, shared with `IWifi`, which tears it down on `stop`.
pub struct ChipState {
    valid: bool,
    nl80211: Arc<Mutex<Nl80211>>,
    netdev: Arc<Netdev>,
    /// The primary interface (`wlan0`), whose driver reports the chip's information.
    primary: String,
    /// The wiphy new interfaces are created on.
    wiphy: u32,
    combinations: Vec<Combination>,
    mode: Option<i32>,
    sta: Vec<Entry<dyn IWifiStaIface>>,
    ap: Vec<Entry<dyn IWifiApIface>>,
    p2p: Vec<Entry<dyn IWifiP2pIface>>,
}

impl ChipState {
    /// Finds the wiphy of the primary interface and what it supports.
    ///
    /// # Errors
    ///
    /// Fails if there is no primary interface (no Wi-Fi driver loaded), it is not a
    /// wireless interface, or the device supports no interface type the HAL offers.
    pub fn probe() -> io::Result<Self> {
        let primary = wlan_name(0);
        let ifindex = netdev::index(&primary).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("no interface {primary}"))
        })?;
        let mut nl80211 = Nl80211::open()?;
        let index = nl80211.wiphy_of(ifindex)?;
        let wiphy = nl80211.wiphy(index)?;
        let combinations = concurrency::from_wiphy(&wiphy);
        if combinations.is_empty() {
            return Err(io::Error::other(format!("{}: no STA, AP or P2P support", wiphy.name)));
        }
        info!("{primary} on {}: combinations {combinations:?}", wiphy.name);
        Ok(Self {
            valid: true,
            nl80211: Arc::new(Mutex::new(nl80211)),
            netdev: Arc::new(Netdev::open()?),
            primary,
            wiphy: wiphy.index,
            combinations,
            mode: None,
            sta: Vec::new(),
            ap: Vec::new(),
            p2p: Vec::new(),
        })
    }

    /// Removes every interface this HAL created and invalidates the chip and all
    /// interface objects (`IWifi::stop`).
    pub fn teardown(&mut self) {
        for entry in std::mem::take(&mut self.sta) {
            self.retire(entry);
        }
        for entry in std::mem::take(&mut self.ap) {
            self.retire(entry);
        }
        for entry in std::mem::take(&mut self.p2p) {
            self.retire(entry);
        }
        self.valid = false;
        self.mode = None;
        self.update_active_iface();
    }

    fn counts(&self) -> Counts {
        let sta = self.sta.iter().map(|_| Kind::Sta);
        let ap = self.ap.iter().map(|_| Kind::Ap);
        let p2p = self.p2p.iter().map(|_| Kind::P2p);
        sta.chain(ap).chain(p2p).fold(Counts::default(), Counts::with)
    }

    /// `ERROR_NOT_AVAILABLE` unless the configured mode has room for one more `kind`.
    fn check_room(&self, kind: Kind) -> binder::Result<()> {
        if self.mode.is_some()
            && concurrency::any_allows(&self.combinations, self.counts().with(kind))
        {
            Ok(())
        } else {
            Err(error(WifiStatusCode::ERROR_NOT_AVAILABLE))
        }
    }

    fn uses(&self, name: &str) -> bool {
        self.sta.iter().any(|e| e.name == name) || self.ap.iter().any(|e| e.name == name)
    }

    fn allocate_wlan_name(&self, first: u32) -> binder::Result<String> {
        (first..MAX_WLAN_IFACES)
            .map(wlan_name)
            .find(|name| !self.uses(name))
            .ok_or_else(|| error(WifiStatusCode::ERROR_NOT_AVAILABLE))
    }

    /// The AP name: `ro.vendor.wifi.sap.interface`, else the first free `wlanN`, after the
    /// STA's `wlan0` when the two can run together (as AOSP's default HAL allocates them).
    fn allocate_ap_name(&self) -> binder::Result<String> {
        if let Some(name) = props::read("ro.vendor.wifi.sap.interface") {
            return Ok(name);
        }
        let sta_ap = Counts::default().with(Kind::Sta).with(Kind::Ap);
        let first = if concurrency::any_allows(&self.combinations, sta_ap) { 1 } else { 0 };
        self.allocate_wlan_name(first)
    }

    /// Makes sure network interface `name` exists, creating it on the chip's wiphy with
    /// `iftype` if needed. Returns whether it was created.
    fn ensure_netdev(&self, name: &str, iftype: Iftype) -> binder::Result<bool> {
        if netdev::index(name).is_some() {
            return Ok(false);
        }
        self.nl80211
            .lock()
            .expect("nl80211 lock poisoned")
            .new_interface(self.wiphy, name, iftype)
            .map_err(|err| unknown(format!("create {name}: {err}")))?;
        info!("created {name} ({iftype:?})");
        Ok(true)
    }

    fn delete_netdev(&self, name: &str) {
        let Some(ifindex) = netdev::index(name) else { return };
        match self.nl80211.lock().expect("nl80211 lock poisoned").del_interface(ifindex) {
            Ok(()) => info!("deleted {name}"),
            Err(err) => warn!("delete {name}: {err}"),
        }
    }

    fn update_active_iface(&self) {
        let first = self.sta.first().map(|e| &e.name).or_else(|| self.ap.first().map(|e| &e.name));
        props::write(ACTIVE_IFACE_PROPERTY, first.unwrap_or(&self.primary));
    }

    fn create_sta(&mut self) -> binder::Result<(Strong<dyn IWifiStaIface>, String)> {
        self.check_room(Kind::Sta)?;
        let name = self.allocate_wlan_name(0)?;
        let created = self.ensure_netdev(&name, Iftype::Station)?;
        let valid = Validity::new();
        let iface = StaIface::new(name.clone(), valid.clone(), Arc::clone(&self.netdev));
        let binder = BnWifiStaIface::new_binder(iface, BinderFeatures::default());
        self.sta.push(Entry { name: name.clone(), created, valid, binder: binder.clone() });
        self.update_active_iface();
        Ok((binder, name))
    }

    fn create_ap(&mut self) -> binder::Result<(Strong<dyn IWifiApIface>, String)> {
        self.check_room(Kind::Ap)?;
        let name = self.allocate_ap_name()?;
        let created = self.ensure_netdev(&name, Iftype::Ap)?;
        let valid = Validity::new();
        let iface = ApIface::new(
            name.clone(),
            valid.clone(),
            Arc::clone(&self.netdev),
            Arc::clone(&self.nl80211),
        );
        let binder = BnWifiApIface::new_binder(iface, BinderFeatures::default());
        self.ap.push(Entry { name: name.clone(), created, valid, binder: binder.clone() });
        self.update_active_iface();
        Ok((binder, name))
    }

    fn create_p2p(&mut self) -> binder::Result<(Strong<dyn IWifiP2pIface>, String)> {
        self.check_room(Kind::P2p)?;
        // wpa_supplicant creates the P2P device interface itself.
        let name = props::read("wifi.direct.interface").unwrap_or_else(|| "p2p0".to_owned());
        let valid = Validity::new();
        let binder = BnWifiP2pIface::new_binder(
            P2pIface::new(name.clone(), valid.clone()),
            BinderFeatures::default(),
        );
        self.p2p.push(Entry { name: name.clone(), created: false, valid, binder: binder.clone() });
        Ok((binder, name))
    }

    /// Invalidates a removed interface object and deletes its kernel interface if this
    /// HAL created it.
    fn retire<B: ?Sized + FromIBinder>(&self, entry: Entry<B>) {
        entry.valid.invalidate();
        if entry.created {
            self.delete_netdev(&entry.name);
        }
    }

    fn usable_channels(
        &self,
        band: WifiBand,
        mode_mask: i32,
    ) -> binder::Result<Vec<WifiUsableChannel>> {
        // Read again: the channels change with the regulatory domain.
        let wiphy = self
            .nl80211
            .lock()
            .expect("nl80211 lock poisoned")
            .wiphy(self.wiphy)
            .map_err(|err| unknown(format!("wiphy {}: {err}", self.wiphy)))?;
        Ok(wiphy
            .channels
            .iter()
            .filter(|channel| band.0 & band_mask(channel) != 0)
            .filter_map(|channel| {
                let modes = mode_mask & usable_modes(channel);
                let number = channel_number(channel.band, channel.mhz)?;
                (modes != 0).then_some(WifiUsableChannel {
                    channel: number,
                    channelBandwidth: WifiChannelWidthInMhz::WIDTH_20,
                    ifaceModeMask: modes,
                })
            })
            .collect())
    }
}

/// The `WifiBand` bit of `channel`.
fn band_mask(channel: &Channel) -> i32 {
    match channel.band {
        Band::Ghz2 => WifiBand::BAND_24GHZ.0,
        Band::Ghz5 if channel.dfs => WifiBand::BAND_5GHZ_DFS.0,
        Band::Ghz5 => WifiBand::BAND_5GHZ.0,
        Band::Ghz6 => WifiBand::BAND_6GHZ.0,
        Band::Ghz60 => WifiBand::BAND_60GHZ.0,
    }
}

/// The `WifiIfaceMode`s that may use `channel`: clients anywhere it is enabled, an AP or
/// P2P group owner only where the device may start transmitting.
fn usable_modes(channel: &Channel) -> i32 {
    let clients = WifiIfaceMode::IFACE_MODE_STA.0 | WifiIfaceMode::IFACE_MODE_P2P_CLIENT.0;
    let owners = WifiIfaceMode::IFACE_MODE_SOFTAP.0 | WifiIfaceMode::IFACE_MODE_P2P_GO.0;
    if channel.initiate_radiation {
        clients | owners
    } else {
        clients
    }
}

/// The IEEE 802.11 channel number of `mhz` in `band`.
fn channel_number(band: Band, mhz: u32) -> Option<i32> {
    let number = match (band, mhz) {
        (Band::Ghz2, 2484) => 14,
        (Band::Ghz2, 2412..=2472) => (mhz - 2407) / 5,
        (Band::Ghz5, 5000..=5925) => (mhz - 5000) / 5,
        (Band::Ghz6, 5935) => 2,
        (Band::Ghz6, 5955..=7115) => (mhz - 5950) / 5,
        (Band::Ghz60, 58320..=70200) => (mhz - 56160) / 2160,
        _ => return None,
    };
    i32::try_from(number).ok()
}

fn to_aidl(combination: &Combination) -> ChipConcurrencyCombination {
    let limits = combination
        .limits
        .iter()
        .map(|limit| ChipConcurrencyCombinationLimit {
            types: limit
                .kinds
                .iter()
                .map(|kind| match kind {
                    Kind::Sta => IfaceConcurrencyType::STA,
                    Kind::Ap => IfaceConcurrencyType::AP,
                    Kind::P2p => IfaceConcurrencyType::P2P,
                })
                .collect(),
            maxIfaces: i32::try_from(limit.max).expect("limits are capped"),
        })
        .collect();
    ChipConcurrencyCombination { limits }
}

/// Adds `callback` to `callbacks`; registering the same object again is not an error.
pub fn add_callback<I: ?Sized + FromIBinder>(
    callbacks: &Callbacks<I>,
    callback: &Strong<I>,
) -> binder::Result<()> {
    match callbacks.add(callback, ()) {
        Err(status) if status.exception_code() == ExceptionCode::ILLEGAL_ARGUMENT => Ok(()),
        result => result,
    }
}

/// The IWifiChip binder object.
pub struct Chip {
    state: Arc<Mutex<ChipState>>,
    callbacks: Callbacks<dyn IWifiChipEventCallback>,
}

impl Chip {
    /// The chip object over `state`.
    pub fn new(state: Arc<Mutex<ChipState>>) -> Self {
        Self { state, callbacks: Callbacks::new() }
    }

    /// The state, or `ERROR_WIFI_CHIP_INVALID` after `IWifi::stop`.
    fn lock(&self) -> binder::Result<MutexGuard<'_, ChipState>> {
        let state = self.state.lock().expect("chip lock poisoned");
        if state.valid {
            Ok(state)
        } else {
            Err(error(WifiStatusCode::ERROR_WIFI_CHIP_INVALID))
        }
    }

    /// `ERROR_WIFI_CHIP_INVALID` after `IWifi::stop`.
    fn check(&self) -> binder::Result<()> {
        self.lock().map(drop)
    }

    fn unsupported<T>(&self) -> binder::Result<T> {
        self.check()?;
        not_supported()
    }

    fn notify_added(&self, kind: IfaceType, name: &str) {
        self.callbacks.notify(|()| true, |callback| callback.onIfaceAdded(kind, name));
    }

    fn notify_removed(&self, kind: IfaceType, name: &str) {
        self.callbacks.notify(|()| true, |callback| callback.onIfaceRemoved(kind, name));
    }

    fn create_ap(&self, kind: IfaceConcurrencyType) -> binder::Result<Strong<dyn IWifiApIface>> {
        match kind {
            IfaceConcurrencyType::AP => {
                let (iface, name) = self.lock()?.create_ap()?;
                self.notify_added(IfaceType::AP, &name);
                Ok(iface)
            }
            // A bridged AP needs two AP interfaces at once; no combination offers that.
            IfaceConcurrencyType::AP_BRIDGED => {
                self.check()?;
                Err(error(WifiStatusCode::ERROR_NOT_AVAILABLE))
            }
            _ => invalid_args(),
        }
    }
}

impl Interface for Chip {}

impl IWifiChip for Chip {
    fn getId(&self) -> binder::Result<i32> {
        self.check()?;
        Ok(CHIP_ID)
    }

    fn registerEventCallback(
        &self,
        callback: &Strong<dyn IWifiChipEventCallback>,
    ) -> binder::Result<()> {
        self.check()?;
        add_callback(&self.callbacks, callback)
    }

    fn getFeatureSet(&self) -> binder::Result<i32> {
        // No optional chip feature (TX power scenarios, latency modes, AFC, MLO, ...).
        self.check()?;
        Ok(0)
    }

    fn getAvailableModes(&self) -> binder::Result<Vec<ChipMode>> {
        let state = self.lock()?;
        Ok(vec![ChipMode {
            id: MODE_ID,
            availableCombinations: state.combinations.iter().map(to_aidl).collect(),
        }])
    }

    fn configureChip(&self, mode_id: i32) -> binder::Result<()> {
        {
            let mut state = self.lock()?;
            if mode_id != MODE_ID {
                return invalid_args();
            }
            if state.mode == Some(mode_id) {
                return Ok(());
            }
            state.mode = Some(mode_id);
        }
        self.callbacks.notify(|()| true, |callback| callback.onChipReconfigured(mode_id));
        Ok(())
    }

    fn getMode(&self) -> binder::Result<i32> {
        self.lock()?.mode.ok_or_else(|| error(WifiStatusCode::ERROR_NOT_AVAILABLE))
    }

    fn requestChipDebugInfo(&self) -> binder::Result<ChipDebugInfo> {
        let state = self.lock()?;
        let info = state
            .netdev
            .driver_info(&state.primary)
            .map_err(|err| unknown(format!("{}: driver info: {err}", state.primary)))?;
        let or_unknown = |text: String| if text.is_empty() { "unknown".to_owned() } else { text };
        Ok(ChipDebugInfo {
            driverDescription: or_unknown(info.driver),
            firmwareDescription: or_unknown(info.firmware),
        })
    }

    fn setCountryCode(&self, code: &[u8; 2]) -> binder::Result<()> {
        let nl80211 = Arc::clone(&self.lock()?.nl80211);
        set_country_code(&nl80211, *code)
    }

    fn getUsableChannels(
        &self,
        band: WifiBand,
        iface_mode_mask: i32,
        _filter_mask: i32,
    ) -> binder::Result<Vec<WifiUsableChannel>> {
        self.lock()?.usable_channels(band, iface_mode_mask)
    }

    fn createStaIface(&self) -> binder::Result<Strong<dyn IWifiStaIface>> {
        let (iface, name) = self.lock()?.create_sta()?;
        self.notify_added(IfaceType::STA, &name);
        Ok(iface)
    }

    fn getStaIfaceNames(&self) -> binder::Result<Vec<String>> {
        Ok(self.lock()?.sta.iter().map(|e| e.name.clone()).collect())
    }

    fn getStaIface(&self, name: &str) -> binder::Result<Strong<dyn IWifiStaIface>> {
        let state = self.lock()?;
        Entry::find(&state.sta, name)
    }

    fn removeStaIface(&self, name: &str) -> binder::Result<()> {
        {
            let mut state = self.lock()?;
            let entry = Entry::take(&mut state.sta, name)?;
            state.retire(entry);
            state.update_active_iface();
        }
        self.notify_removed(IfaceType::STA, name);
        Ok(())
    }

    fn createApIface(&self) -> binder::Result<Strong<dyn IWifiApIface>> {
        self.create_ap(IfaceConcurrencyType::AP)
    }

    fn createBridgedApIface(&self) -> binder::Result<Strong<dyn IWifiApIface>> {
        self.create_ap(IfaceConcurrencyType::AP_BRIDGED)
    }

    fn createApOrBridgedApIface(
        &self,
        kind: IfaceConcurrencyType,
        _vendor_data: &[OuiKeyedData],
    ) -> binder::Result<Strong<dyn IWifiApIface>> {
        self.create_ap(kind)
    }

    fn createApOrBridgedApIfaceWithParams(
        &self,
        params: &ApIfaceParams,
    ) -> binder::Result<Strong<dyn IWifiApIface>> {
        if params.usesMlo {
            return self.unsupported();
        }
        self.create_ap(params.ifaceType)
    }

    fn getApIfaceNames(&self) -> binder::Result<Vec<String>> {
        Ok(self.lock()?.ap.iter().map(|e| e.name.clone()).collect())
    }

    fn getApIface(&self, name: &str) -> binder::Result<Strong<dyn IWifiApIface>> {
        let state = self.lock()?;
        Entry::find(&state.ap, name)
    }

    fn removeApIface(&self, name: &str) -> binder::Result<()> {
        {
            let mut state = self.lock()?;
            let entry = Entry::take(&mut state.ap, name)?;
            state.retire(entry);
            state.update_active_iface();
        }
        self.notify_removed(IfaceType::AP, name);
        Ok(())
    }

    fn removeIfaceInstanceFromBridgedApIface(
        &self,
        _bridge_name: &str,
        _instance_name: &str,
    ) -> binder::Result<()> {
        self.check()?;
        invalid_args()
    }

    fn createP2pIface(&self) -> binder::Result<Strong<dyn IWifiP2pIface>> {
        let (iface, name) = self.lock()?.create_p2p()?;
        self.notify_added(IfaceType::P2P, &name);
        Ok(iface)
    }

    fn getP2pIfaceNames(&self) -> binder::Result<Vec<String>> {
        Ok(self.lock()?.p2p.iter().map(|e| e.name.clone()).collect())
    }

    fn getP2pIface(&self, name: &str) -> binder::Result<Strong<dyn IWifiP2pIface>> {
        let state = self.lock()?;
        Entry::find(&state.p2p, name)
    }

    fn removeP2pIface(&self, name: &str) -> binder::Result<()> {
        {
            let mut state = self.lock()?;
            let entry = Entry::take(&mut state.p2p, name)?;
            state.retire(entry);
        }
        self.notify_removed(IfaceType::P2P, name);
        Ok(())
    }

    fn createNanIface(&self) -> binder::Result<Strong<dyn IWifiNanIface>> {
        // No combination offers Wi-Fi Aware.
        self.check()?;
        Err(error(WifiStatusCode::ERROR_NOT_AVAILABLE))
    }

    fn getNanIfaceNames(&self) -> binder::Result<Vec<String>> {
        self.check()?;
        Ok(Vec::new())
    }

    fn getNanIface(&self, _name: &str) -> binder::Result<Strong<dyn IWifiNanIface>> {
        self.check()?;
        invalid_args()
    }

    fn removeNanIface(&self, _name: &str) -> binder::Result<()> {
        self.check()?;
        invalid_args()
    }

    fn createRttController(
        &self,
        _bound_iface: &Strong<dyn IWifiStaIface>,
    ) -> binder::Result<Strong<dyn IWifiRttController>> {
        self.unsupported()
    }

    fn getSupportedRadioCombinations(&self) -> binder::Result<Vec<WifiRadioCombination>> {
        self.unsupported()
    }

    fn getWifiChipCapabilities(&self) -> binder::Result<WifiChipCapabilities> {
        self.unsupported()
    }

    fn enableDebugErrorAlerts(&self, _enable: bool) -> binder::Result<()> {
        self.unsupported()
    }

    fn flushRingBufferToFile(&self) -> binder::Result<()> {
        self.unsupported()
    }

    fn forceDumpToDebugRingBuffer(&self, _ring_name: &str) -> binder::Result<()> {
        self.unsupported()
    }

    fn getDebugHostWakeReasonStats(&self) -> binder::Result<WifiDebugHostWakeReasonStats> {
        self.unsupported()
    }

    fn getDebugRingBuffersStatus(&self) -> binder::Result<Vec<WifiDebugRingBufferStatus>> {
        self.unsupported()
    }

    fn requestDriverDebugDump(&self) -> binder::Result<Vec<u8>> {
        self.unsupported()
    }

    fn requestFirmwareDebugDump(&self) -> binder::Result<Vec<u8>> {
        self.unsupported()
    }

    fn startLoggingToDebugRingBuffer(
        &self,
        _ring_name: &str,
        _verbose_level: WifiDebugRingBufferVerboseLevel,
        _max_interval_in_sec: i32,
        _min_data_size_in_bytes: i32,
    ) -> binder::Result<()> {
        self.unsupported()
    }

    fn stopLoggingToDebugRingBuffer(&self) -> binder::Result<()> {
        self.unsupported()
    }

    fn resetTxPowerScenario(&self) -> binder::Result<()> {
        self.unsupported()
    }

    fn selectTxPowerScenario(&self, _scenario: TxPowerScenario) -> binder::Result<()> {
        self.unsupported()
    }

    fn setAfcChannelAllowance(&self, _allowance: &AfcChannelAllowance) -> binder::Result<()> {
        self.unsupported()
    }

    fn setCoexUnsafeChannels(
        &self,
        _unsafe_channels: &[CoexUnsafeChannel],
        _restrictions: i32,
    ) -> binder::Result<()> {
        self.unsupported()
    }

    fn setLatencyMode(&self, _mode: LatencyMode) -> binder::Result<()> {
        self.unsupported()
    }

    fn setMultiStaPrimaryConnection(&self, _iface_name: &str) -> binder::Result<()> {
        self.unsupported()
    }

    fn setMultiStaUseCase(&self, _use_case: MultiStaUseCase) -> binder::Result<()> {
        self.unsupported()
    }

    fn triggerSubsystemRestart(&self) -> binder::Result<()> {
        self.unsupported()
    }

    fn enableStaChannelForPeerNetwork(
        &self,
        _channel_category_enable_flag: i32,
    ) -> binder::Result<()> {
        self.unsupported()
    }

    fn setMloMode(&self, _mode: ChipMloMode) -> binder::Result<()> {
        self.unsupported()
    }

    fn setVoipMode(&self, _mode: VoipMode) -> binder::Result<()> {
        self.unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(band: Band, mhz: u32, initiate_radiation: bool, dfs: bool) -> Channel {
        Channel { band, mhz, initiate_radiation, dfs }
    }

    #[test]
    fn channel_numbers() {
        assert_eq!(channel_number(Band::Ghz2, 2412), Some(1));
        assert_eq!(channel_number(Band::Ghz2, 2472), Some(13));
        assert_eq!(channel_number(Band::Ghz2, 2484), Some(14));
        assert_eq!(channel_number(Band::Ghz5, 5180), Some(36));
        assert_eq!(channel_number(Band::Ghz5, 5825), Some(165));
        assert_eq!(channel_number(Band::Ghz6, 5935), Some(2));
        assert_eq!(channel_number(Band::Ghz6, 5955), Some(1));
        assert_eq!(channel_number(Band::Ghz6, 7115), Some(233));
        assert_eq!(channel_number(Band::Ghz60, 58320), Some(1));
        assert_eq!(channel_number(Band::Ghz2, 5180), None);
    }

    #[test]
    fn band_and_mode_masks() {
        let dfs = channel(Band::Ghz5, 5260, false, true);
        assert_eq!(band_mask(&dfs), WifiBand::BAND_5GHZ_DFS.0);
        assert_eq!(WifiBand::BAND_24GHZ_5GHZ.0 & band_mask(&dfs), 0, "DFS needs its own bit");
        assert_ne!(WifiBand::BAND_24GHZ_5GHZ_WITH_DFS.0 & band_mask(&dfs), 0);
        assert_eq!(usable_modes(&dfs) & WifiIfaceMode::IFACE_MODE_SOFTAP.0, 0, "no AP on NO_IR");
        let open = channel(Band::Ghz2, 2437, true, false);
        assert_ne!(usable_modes(&open) & WifiIfaceMode::IFACE_MODE_SOFTAP.0, 0);
    }

    #[test]
    fn aidl_combination() {
        let combination = Combination {
            limits: vec![concurrency::Limit { kinds: vec![Kind::Ap, Kind::P2p], max: 1 }],
        };
        let aidl = to_aidl(&combination);
        assert_eq!(aidl.limits.len(), 1);
        assert_eq!(aidl.limits[0].types, [IfaceConcurrencyType::AP, IfaceConcurrencyType::P2P]);
        assert_eq!(aidl.limits[0].maxIfaces, 1);
    }

    /// Read-only queries against this machine's Wi-Fi device, if it has one (the build
    /// host is the target PC); skipped without `wlan0`.
    #[test]
    fn probes_the_local_wifi_device() {
        if netdev::index(&wlan_name(0)).is_none() {
            return;
        }
        let state = ChipState::probe().expect("probe wlan0");
        let chip = Chip::new(Arc::new(Mutex::new(state)));
        let modes = chip.getAvailableModes().expect("modes");
        assert!(!modes[0].availableCombinations.is_empty());
        let info = chip.requestChipDebugInfo().expect("debug info");
        assert!(!info.driverDescription.is_empty() && !info.firmwareDescription.is_empty());
        let channels = chip
            .getUsableChannels(
                WifiBand::BAND_24GHZ_5GHZ_WITH_DFS_6GHZ,
                WifiIfaceMode::IFACE_MODE_STA.0,
                0,
            )
            .expect("channels");
        assert!(!channels.is_empty(), "no usable channel");
        println!("{modes:?}\n{info:?}\n{} channels: {channels:?}", channels.len());
    }

    #[test]
    fn default_names() {
        // No property service on the host: the AOSP defaults.
        assert_eq!(wlan_name(0), "wlan0");
        assert_eq!(wlan_name(3), "wlan3");
    }
}
