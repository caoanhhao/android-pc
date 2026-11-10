//! Wi-Fi: what Linux splits into cfg80211 and mac80211, for station
//! interfaces driven by wpa_supplicant over nl80211.
//!
//! A radio driver ([`WifiDriver`]: `hwsim`, the simulated radio and
//! access points for tests, or `drivers::iwlwifi`) registers a wiphy
//! (`phy0`) with one station interface, `wlan0`, which is a link of the
//! IP stack like an Ethernet card ([`WlanDev`] is its `EthDevice`). This
//! layer keeps per wiphy:
//! - the BSS table, filled from the beacons and probe responses received
//!   during scans (`nl80211` TRIGGER_SCAN / GET_SCAN);
//! - the station MLME, as mac80211 does it for wpa_supplicant's own SME:
//!   NL80211_CMD_AUTHENTICATE sends Authentication frames (open system,
//!   or SAE with the commit/confirm bodies wpa_supplicant computes) and
//!   reports the AP's answers, NL80211_CMD_ASSOCIATE builds the
//!   (Re)Association Request from the AP's rates and the IEs
//!   wpa_supplicant passes (RSN, ...) and reports the response; three
//!   tries, 200 ms each; deauthentication/disassociation either way;
//! - the data path: Ethernet frames ↔ 802.11 data frames (RFC 1042),
//!   with only EAPOL passing until wpa_supplicant marks the AP authorized
//!   (SET_STATION), so the 4-way handshake runs over AF_PACKET as on
//!   Linux; keys (NEW_KEY) go to the driver, which encrypts/decrypts;
//! - management frame registrations (REGISTER_FRAME / FRAME: action
//!   frames such as SA Query for PMF).
//!
//! The carrier of wlan0 is on while associated. Events go to nl80211's
//! multicast groups. Drivers are polled from a soft interrupt (every
//! timer tick while a wiphy is in use), which also runs the MLME's
//! timeouts. Locking: the wiphy list lock is taken after the IP stack's
//! (`WlanDev` runs under it), so link changes are applied after it is
//! released ([`refresh_links`]).

pub mod crypto;
pub mod hwsim;
pub mod ieee80211;
pub mod nl80211;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::errno::*;
use crate::net::inet::EthDevice;
use ieee80211 as w;

/// A channel of a band.
#[derive(Clone)]
pub struct Channel {
    pub freq: u32,
    pub disabled: bool,
    /// Passive scanning only (no probe requests).
    pub no_ir: bool,
    pub radar: bool,
    /// mBm.
    pub max_power: u32,
}

#[derive(Clone)]
pub struct HtCap {
    pub cap: u16,
    pub ampdu_factor: u8,
    pub ampdu_density: u8,
    pub mcs: [u8; 16],
}

#[derive(Clone)]
pub struct VhtCap {
    pub cap: u32,
    pub mcs: [u8; 8],
}

#[derive(Clone)]
pub struct Band {
    /// NL80211_BAND_*: 0 2.4 GHz, 1 5 GHz, 3 6 GHz.
    pub band: u8,
    pub channels: Vec<Channel>,
    /// Bit rates in 100 kb/s.
    pub rates: Vec<u16>,
    pub ht: Option<HtCap>,
    pub vht: Option<VhtCap>,
}

/// What a radio can do.
pub struct Caps {
    pub driver: &'static str,
    pub perm_addr: [u8; 6],
    pub bands: Vec<Band>,
    /// Cipher suite selectors (00-0F-AC:n as 0x000fac0n).
    pub ciphers: Vec<u32>,
    pub max_scan_ssids: u8,
    pub max_scan_ie_len: u16,
    /// SAE authentication frames go through (WPA3-Personal).
    pub sae: bool,
    /// Scans may use a random source address.
    pub random_scan_mac: bool,
}

pub const CIPHER_WEP40: u32 = 0x000f_ac01;
pub const CIPHER_TKIP: u32 = 0x000f_ac02;
pub const CIPHER_CCMP: u32 = 0x000f_ac04;
pub const CIPHER_WEP104: u32 = 0x000f_ac05;
pub const CIPHER_AES_CMAC: u32 = 0x000f_ac06;
pub const CIPHER_GCMP: u32 = 0x000f_ac08;
pub const CIPHER_GCMP_256: u32 = 0x000f_ac09;
pub const CIPHER_CCMP_256: u32 = 0x000f_ac0a;
pub const CIPHER_BIP_GMAC_128: u32 = 0x000f_ac0b;
pub const CIPHER_BIP_GMAC_256: u32 = 0x000f_ac0c;
pub const CIPHER_BIP_CMAC_256: u32 = 0x000f_ac0d;

pub struct ScanReq {
    /// SSIDs to probe for (an empty one = wildcard; none = passive).
    pub ssids: Vec<Vec<u8>>,
    pub freqs: Vec<u32>,
    /// Extra IEs for the probe requests.
    pub ies: Vec<u8>,
    /// Source address of the probe requests.
    pub mac: [u8; 6],
}

/// What a driver needs to talk to an AP before association.
pub struct JoinParams<'a> {
    pub bssid: [u8; 6],
    pub freq: u32,
    pub beacon_int: u16,
    /// The AP's IEs (beacon / probe response).
    pub ies: &'a [u8],
    pub own: [u8; 6],
}

/// The association that the AP accepted.
pub struct AssocInfo<'a> {
    pub bssid: [u8; 6],
    pub aid: u16,
    pub capability: u16,
    /// The AP's rates (500 kb/s units, high bit basic).
    pub rates: &'a [u8],
    pub resp_ies: &'a [u8],
    pub beacon_int: u16,
    /// Whether data frames are QoS frames (WMM negotiated).
    pub qos: bool,
}

#[derive(Clone)]
pub struct Key {
    pub cipher: u32,
    pub idx: u8,
    pub data: Vec<u8>,
    /// Receive sequence counter (little endian, up to 6 bytes).
    pub seq: Vec<u8>,
    pub pairwise: bool,
    /// The peer (pairwise keys).
    pub addr: [u8; 6],
}

/// Something a driver reports.
pub enum Event {
    /// A received frame: management frames whole, data frames decrypted
    /// with the security header removed and FCTL_PROTECTED cleared.
    Rx { frame: Vec<u8>, freq: u32, signal: i8 },
    ScanDone { aborted: bool },
    /// The firmware lost the AP (missed beacons).
    ConnectionLost,
}

#[derive(Clone, Copy, Default)]
pub struct TxStats {
    pub tx_packets: u64,
    pub tx_failed: u64,
    pub tx_retries: u64,
    /// Last transmit rate, 100 kb/s.
    pub tx_rate: u32,
}

pub trait WifiDriver: Send {
    fn caps(&self) -> &Caps;
    /// Power the radio for a station interface with address `mac`.
    fn start(&mut self, mac: [u8; 6]) -> KResult<()>;
    fn stop(&mut self);
    fn scan(&mut self, req: &ScanReq) -> KResult<()>;
    fn abort_scan(&mut self) {}
    /// Tune to an AP and accept its frames (before authentication).
    fn join(&mut self, p: &JoinParams) -> KResult<()>;
    fn associated(&mut self, a: &AssocInfo) -> KResult<()>;
    /// Forget the AP (deauthenticated, failure).
    fn leave(&mut self);
    /// Send a management frame (sequence number already set).
    fn tx_mgmt(&mut self, frame: &[u8]) -> KResult<()>;
    /// Send a data frame (802.11 header, RFC 1042 payload); false when
    /// dropped. `eapol`: a handshake frame, before keys may be in use.
    fn tx_data(&mut self, frame: &[u8], eapol: bool) -> bool;
    fn can_tx(&mut self) -> bool {
        true
    }
    fn set_key(&mut self, k: &Key) -> KResult<()>;
    fn del_key(&mut self, _k: &Key) {}
    fn set_authorized(&mut self, _on: bool) {}
    /// Collect what happened since the last call.
    fn poll(&mut self, out: &mut Vec<Event>);
    fn tx_stats(&self) -> TxStats {
        TxStats::default()
    }
}

#[derive(Clone)]
pub struct Bss {
    pub bssid: [u8; 6],
    pub freq: u32,
    pub tsf: u64,
    pub beacon_int: u16,
    pub capability: u16,
    /// IEs of the last probe response (or beacon if none).
    pub ies: Vec<u8>,
    pub beacon_ies: Option<Vec<u8>>,
    pub signal_mbm: i32,
    pub seen_ns: u64,
}

#[derive(Clone)]
enum Mlme {
    Idle,
    /// Authentication frame sent, waiting for the AP's.
    Auth { bssid: [u8; 6], freq: u32, alg: u16, frame: Vec<u8>, tries: u8, deadline: u64 },
    /// The last authentication exchange succeeded (or SAE is between its
    /// commit and confirm steps, `done` false).
    Authed { bssid: [u8; 6], freq: u32, done: bool },
    Assoc { bssid: [u8; 6], freq: u32, frame: Vec<u8>, req_ies: Vec<u8>, tries: u8, deadline: u64 },
    Connected { bssid: [u8; 6], freq: u32, aid: u16, req_ies: Vec<u8>, resp_ies: Vec<u8>, since: u64, qos: bool },
}

struct ScanState {
    ssids: Vec<Vec<u8>>,
    freqs: Vec<u32>,
    flags: u32,
    deadline: u64,
}

struct FrameReg {
    portid: u32,
    ftype: u16,
    pattern: Vec<u8>,
}

pub struct Wiphy {
    pub idx: u32,
    pub name: String,
    drv: Box<dyn WifiDriver>,
    pub ifindex: u32,
    pub ifname: String,
    pub mac: [u8; 6],
    /// wlan0 is up (wanted by the IP stack) / the driver was started.
    want_up: bool,
    started: bool,
    pub iftype: u32,
    pub bss: Vec<Bss>,
    scan: Option<ScanState>,
    pub generation: u32,
    mlme: Mlme,
    authorized: bool,
    rx_eth: VecDeque<Vec<u8>>,
    seq: u16,
    regs: Vec<FrameReg>,
    cookie: u64,
    /// FRAME transmissions to report (after the command's reply).
    tx_status: Vec<(Vec<u8>, u64)>,
    stats: crate::net::inet::Stats,
    last_rx_ns: u64,
    signal: i8,
    rfkill: Option<u32>,
}

/// Every registered radio.
static WIPHYS: Mutex<Vec<Wiphy>> = Mutex::new(Vec::new());
static PENDING: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// A link's carrier changed: apply it once the wiphy lock is released.
static LINKS_DIRTY: AtomicBool = AtomicBool::new(false);
/// Frames wait in the receive queue of a wlan link.
static RX_WAITING: AtomicBool = AtomicBool::new(false);

const AUTH_TIMEOUT_NS: u64 = 200_000_000;
const ASSOC_TIMEOUT_NS: u64 = 200_000_000;
const MAX_TRIES: u8 = 3;
const SCAN_TIMEOUT_NS: u64 = 10_000_000_000;
const BSS_EXPIRE_NS: u64 = 30_000_000_000;
const RX_QUEUE_MAX: usize = 512;

pub fn now_ns() -> u64 {
    crate::time::monotonic_ns()
}

fn mac_str(m: &[u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

/// Register a radio: it becomes `phyN` with the station link `wlanN`.
/// `sysdev` is its device's sysfs directory (relative to /sys).
pub fn register(drv: Box<dyn WifiDriver>, sysdev: &str) {
    let mac = drv.caps().perm_addr;
    let driver = drv.caps().driver;
    let idx = WIPHYS.lock().len() as u32;
    let name = format!("phy{idx}");
    let ifname = format!("wlan{idx}");
    let rfkill = crate::drivers::rfkill::register(crate::drivers::rfkill::TYPE_WLAN, &name);
    WIPHYS.lock().push(Wiphy {
        idx,
        name: name.clone(),
        drv,
        ifindex: 0,
        ifname: ifname.clone(),
        mac,
        want_up: false,
        started: false,
        iftype: nl80211::IFTYPE_STATION,
        bss: Vec::new(),
        scan: None,
        generation: 1,
        mlme: Mlme::Idle,
        authorized: false,
        rx_eth: VecDeque::new(),
        seq: 0,
        regs: Vec::new(),
        cookie: 1,
        tx_status: Vec::new(),
        stats: Default::default(),
        last_rx_ns: 0,
        signal: 0,
        rfkill: Some(rfkill),
    });
    let phydir = format!("{sysdev}/ieee80211/{name}");
    crate::fs::sysfs::add_device(&phydir, "ieee80211", "class/ieee80211", String::new(), false);
    crate::fs::sysfs::set(&phydir, "index", &format!("{idx}\n"));
    crate::fs::sysfs::set(&phydir, "name", &format!("{name}\n"));
    crate::fs::sysfs::set(&phydir, "macaddress", &format!("{}\n", mac_str(&mac)));
    let ifindex = crate::net::inet::add_link(
        Box::new(WlanDev { phy: idx }),
        sysdev,
        &ifname,
        "DEVTYPE=wlan\n",
        false,
    );
    WIPHYS.lock()[idx as usize].ifindex = ifindex;
    let netdir = format!("{sysdev}/net/{ifname}");
    crate::fs::sysfs::symlink(&netdir, "phy80211", &crate::fs::sysfs::relative(&netdir, &phydir));
    crate::fs::sysfs::dir(&format!("{netdir}/wireless"));
    log::info!("wifi: {name} ({driver}) is {ifname}, address {}", mac_str(&mac));
}

/// Run `f` on the wiphy `idx` (then apply link changes).
pub fn with_wiphy<R>(idx: u32, f: impl FnOnce(&mut Wiphy) -> R) -> Option<R> {
    let r = {
        let mut g = WIPHYS.lock();
        let w = g.get_mut(idx as usize)?;
        Some(f(w))
    };
    after_unlock();
    r
}

/// The wiphy of interface `ifindex`.
pub fn by_ifindex(ifindex: u32) -> Option<u32> {
    WIPHYS.lock().iter().find(|w| w.ifindex == ifindex).map(|w| w.idx)
}

pub fn count() -> u32 {
    WIPHYS.lock().len() as u32
}

/// Run `f` on every wiphy.
pub fn for_each<R>(mut f: impl FnMut(&mut Wiphy) -> R) -> Vec<R> {
    let r = WIPHYS.lock().iter_mut().map(&mut f).collect();
    after_unlock();
    r
}

fn after_unlock() {
    if LINKS_DIRTY.swap(false, Ordering::AcqRel) {
        refresh_links();
    }
    if RX_WAITING.swap(false, Ordering::AcqRel) {
        crate::net::inet::request_poll();
    }
}

/// A wlan link's carrier changed: the IP stack polls it and publishes
/// the change (flags, sysfs, RTM_NEWLINK).
fn refresh_links() {
    crate::net::inet::carrier_changed();
}

/// From the timer interrupt (BSP).
pub fn tick() {
    if ACTIVE.load(Ordering::Relaxed) {
        PENDING.store(true, Ordering::Relaxed);
    }
}

pub fn pending() -> bool {
    PENDING.load(Ordering::Relaxed)
}

/// Poll the drivers and run the MLME timers (task context).
pub fn softirq() {
    {
        let Some(mut g) = WIPHYS.try_lock() else { return };
        PENDING.store(false, Ordering::Relaxed);
        let mut active = false;
        for w in g.iter_mut() {
            w.poll();
            active |= w.started;
        }
        ACTIVE.store(active, Ordering::Relaxed);
    }
    after_unlock();
}

/// rfkill changed the soft block of a radio.
pub fn rfkill_changed(id: u32, blocked: bool) {
    for_each(|w| {
        if w.rfkill == Some(id) && blocked && w.started {
            log::info!("wifi: {} blocked by rfkill", w.name);
            w.disconnect_local(3, true);
            w.drv.stop();
            w.started = false;
        }
    });
}

impl Wiphy {
    pub fn caps(&self) -> &Caps {
        self.drv.caps()
    }

    pub fn wdev(&self) -> u64 {
        ((self.idx as u64) << 32) | 1
    }

    pub fn is_up(&self) -> bool {
        self.want_up
    }

    fn next_seq(&mut self) -> u16 {
        self.seq = self.seq.wrapping_add(1) & 0xfff;
        self.seq
    }

    /// Start the driver if the interface is up.
    pub fn ensure_started(&mut self) -> KResult<()> {
        if !self.want_up {
            return Err(ENETDOWN);
        }
        if self.rfkill.is_some_and(crate::drivers::rfkill::blocked) {
            return Err(ERFKILL);
        }
        if !self.started {
            self.drv.start(self.mac)?;
            self.started = true;
            ACTIVE.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    fn tx_mgmt(&mut self, mut f: Vec<u8>) -> KResult<()> {
        let s = self.next_seq();
        w::set_seq(&mut f, s);
        self.drv.tx_mgmt(&f)
    }

    pub fn connected_bssid(&self) -> Option<[u8; 6]> {
        match &self.mlme {
            Mlme::Connected { bssid, .. } => Some(*bssid),
            _ => None,
        }
    }

    /// (bssid, freq, ssid) of the current association.
    pub fn link_info(&self) -> Option<([u8; 6], u32, Vec<u8>)> {
        match &self.mlme {
            Mlme::Connected { bssid, freq, req_ies, .. } => {
                Some((*bssid, *freq, w::ssid_of(req_ies).map(|s| s.to_vec()).unwrap_or_default()))
            }
            _ => None,
        }
    }

    /// The BSS's state for scan results (nl80211 BSS_STATUS).
    pub fn bss_status(&self, bssid: &[u8; 6]) -> Option<u32> {
        match &self.mlme {
            Mlme::Connected { bssid: b, .. } if b == bssid => Some(1),
            Mlme::Authed { bssid: b, .. } | Mlme::Assoc { bssid: b, .. } if b == bssid => Some(0),
            _ => None,
        }
    }

    pub fn station(&self) -> Option<StationInfo> {
        let Mlme::Connected { bssid, since, .. } = &self.mlme else { return None };
        let t = self.drv.tx_stats();
        Some(StationInfo {
            bssid: *bssid,
            connected_s: ((now_ns() - since) / 1_000_000_000) as u32,
            inactive_ms: ((now_ns().saturating_sub(self.last_rx_ns)) / 1_000_000) as u32,
            rx_packets: self.stats.rx_packets,
            rx_bytes: self.stats.rx_bytes,
            tx_packets: self.stats.tx_packets.max(t.tx_packets),
            tx_bytes: self.stats.tx_bytes,
            tx_failed: t.tx_failed,
            tx_retries: t.tx_retries,
            signal: self.signal,
            tx_rate: if t.tx_rate != 0 { t.tx_rate } else { 540 },
            authorized: self.authorized,
        })
    }

    // ---- scanning ----

    pub fn start_scan(&mut self, mut req: ScanReq, flags: u32) -> KResult<()> {
        self.ensure_started()?;
        if self.scan.is_some() {
            return Err(EBUSY);
        }
        if req.freqs.is_empty() {
            req.freqs = self.caps().bands.iter().flat_map(|b| b.channels.iter().filter(|c| !c.disabled).map(|c| c.freq)).collect();
        }
        if flags & nl80211::SCAN_FLAG_RANDOM_ADDR == 0 || !self.caps().random_scan_mac {
            req.mac = self.mac;
        }
        self.drv.scan(&req)?;
        self.scan = Some(ScanState { ssids: req.ssids, freqs: req.freqs, flags, deadline: now_ns() + SCAN_TIMEOUT_NS });
        let s = self.scan.as_ref().unwrap();
        nl80211::event_scan(self, nl80211::CMD_TRIGGER_SCAN, &s.ssids, &s.freqs, flags);
        Ok(())
    }

    pub fn abort_scan(&mut self) {
        if self.scan.is_some() {
            self.drv.abort_scan();
        }
    }

    fn scan_done(&mut self, aborted: bool) {
        let Some(s) = self.scan.take() else { return };
        let now = now_ns();
        let cur = self.connected_bssid();
        self.bss.retain(|b| now - b.seen_ns < BSS_EXPIRE_NS || Some(b.bssid) == cur);
        self.generation += 1;
        log::info!("wifi: {} scan {} ({} BSS)", self.ifname, if aborted { "aborted" } else { "done" }, self.bss.len());
        let cmd = if aborted { nl80211::CMD_SCAN_ABORTED } else { nl80211::CMD_NEW_SCAN_RESULTS };
        nl80211::event_scan(self, cmd, &s.ssids, &s.freqs, s.flags);
    }

    fn rx_beacon(&mut self, f: &[u8], freq: u32, signal: i8) {
        if f.len() < 36 {
            return;
        }
        let beacon = w::stype(w::fc(f)) == w::STYPE_BEACON;
        let bssid = w::addr(f, 3);
        let tsf = u64::from_le_bytes(f[24..32].try_into().unwrap());
        let beacon_int = u16::from_le_bytes([f[32], f[33]]);
        let capability = u16::from_le_bytes([f[34], f[35]]);
        let ies = &f[36..];
        // The channel the AP announces (2.4 GHz frames leak into the
        // neighbouring channels).
        let freq = match w::find_ie(ies, w::EID_DS_PARAMS).and_then(|d| d.first().copied()) {
            Some(ch) if ch != 0 => w::chan_to_freq(ch, w::band_of(freq)),
            _ => freq,
        };
        if self.connected_bssid() == Some(bssid) {
            self.signal = signal;
        }
        let now = now_ns();
        let pos = self.bss.iter().position(|b| b.bssid == bssid);
        let b = match pos {
            Some(i) => &mut self.bss[i],
            None => {
                self.bss.push(Bss {
                    bssid,
                    freq,
                    tsf,
                    beacon_int,
                    capability,
                    ies: Vec::new(),
                    beacon_ies: None,
                    signal_mbm: 0,
                    seen_ns: now,
                });
                self.bss.last_mut().unwrap()
            }
        };
        b.freq = freq;
        b.tsf = tsf;
        b.beacon_int = beacon_int;
        b.capability = capability;
        b.signal_mbm = signal as i32 * 100;
        b.seen_ns = now;
        if beacon {
            b.beacon_ies = Some(ies.to_vec());
            if b.ies.is_empty() {
                b.ies = ies.to_vec();
            }
        } else {
            b.ies = ies.to_vec();
        }
    }

    // ---- MLME ----

    fn find_bss(&self, bssid: &[u8; 6]) -> Option<&Bss> {
        self.bss.iter().find(|b| &b.bssid == bssid)
    }

    /// NL80211_CMD_AUTHENTICATE.
    pub fn authenticate(&mut self, bssid: [u8; 6], freq: u32, nl_auth_type: u32, data: &[u8], ies: &[u8]) -> KResult<()> {
        self.ensure_started()?;
        let alg = match nl_auth_type {
            nl80211::AUTHTYPE_OPEN_SYSTEM => w::AUTH_OPEN,
            nl80211::AUTHTYPE_SHARED_KEY => w::AUTH_SHARED_KEY,
            nl80211::AUTHTYPE_SAE if self.caps().sae => w::AUTH_SAE,
            _ => return Err(EOPNOTSUPP),
        };
        let bss = self.find_bss(&bssid).cloned().ok_or(ENOENT)?;
        // Already talking to another AP: leave it first (mac80211 does
        // the same, reporting a local deauthentication).
        let same = match &self.mlme {
            Mlme::Idle => false,
            Mlme::Auth { bssid: b, .. } | Mlme::Authed { bssid: b, .. } | Mlme::Assoc { bssid: b, .. } => *b == bssid,
            Mlme::Connected { .. } => false,
        };
        if !same && !matches!(self.mlme, Mlme::Idle) {
            self.disconnect_local(3, true);
        }
        if !same {
            self.drv.join(&JoinParams { bssid, freq: if freq != 0 { freq } else { bss.freq }, beacon_int: bss.beacon_int, ies: &bss.ies, own: self.mac })?;
        }
        log::info!("wifi: {} authenticating with {} ({} MHz, alg {alg})", self.ifname, mac_str(&bssid), bss.freq);
        let frame = w::auth(bssid, self.mac, alg, data, ies);
        self.mlme = Mlme::Auth { bssid, freq: bss.freq, alg, frame: frame.clone(), tries: 1, deadline: now_ns() + AUTH_TIMEOUT_NS };
        self.tx_mgmt(frame)
    }

    /// NL80211_CMD_ASSOCIATE.
    pub fn associate(&mut self, bssid: [u8; 6], ssid: &[u8], user_ies: &[u8], prev_bssid: Option<[u8; 6]>) -> KResult<()> {
        self.ensure_started()?;
        let freq = match &self.mlme {
            Mlme::Authed { bssid: b, freq, .. } if *b == bssid => *freq,
            _ => return Err(ENOLINK),
        };
        let bss = self.find_bss(&bssid).cloned().ok_or(ENOENT)?;
        let stype = if prev_bssid.is_some() { w::STYPE_REASSOC_REQ } else { w::STYPE_ASSOC_REQ };
        let mut f = w::mgmt_header(stype, bssid, self.mac, bssid);
        let mut cap = w::WLAN_CAPABILITY_ESS;
        if bss.capability & w::WLAN_CAPABILITY_SHORT_PREAMBLE != 0 {
            cap |= w::WLAN_CAPABILITY_SHORT_PREAMBLE;
        }
        if bss.capability & w::WLAN_CAPABILITY_SHORT_SLOT_TIME != 0 && w::band_of(freq) == 0 {
            cap |= w::WLAN_CAPABILITY_SHORT_SLOT_TIME;
        }
        let spectrum = bss.capability & w::WLAN_CAPABILITY_SPECTRUM_MGMT != 0 && w::band_of(freq) != 0;
        if spectrum {
            cap |= w::WLAN_CAPABILITY_SPECTRUM_MGMT;
        }
        f.extend_from_slice(&cap.to_le_bytes());
        f.extend_from_slice(&10u16.to_le_bytes());
        if let Some(p) = prev_bssid {
            f.extend_from_slice(&p);
        }
        let mut ies = Vec::new();
        w::push_ie(&mut ies, w::EID_SSID, ssid);
        let rates = self.assoc_rates(freq, &bss.ies);
        w::push_ie(&mut ies, w::EID_SUPP_RATES, &rates[..rates.len().min(8)]);
        if rates.len() > 8 {
            w::push_ie(&mut ies, w::EID_EXT_SUPP_RATES, &rates[8..]);
        }
        if spectrum {
            w::push_ie(&mut ies, w::EID_PWR_CAPABILITY, &[0, 20]);
            let band = w::band_of(freq);
            let mut ch = Vec::new();
            for c in self.caps().bands.iter().filter(|b| b.band == band).flat_map(|b| b.channels.iter()).filter(|c| !c.disabled) {
                ch.push(w::freq_to_chan(c.freq));
                ch.push(1);
            }
            w::push_ie(&mut ies, w::EID_SUPPORTED_CHANNELS, &ch);
        }
        // The IEs of wpa_supplicant (RSN, RSNXE, extended capabilities,
        // vendor elements) keep their order, after ours.
        ies.extend_from_slice(user_ies);
        f.extend_from_slice(&ies);
        log::info!("wifi: {} associating with {}", self.ifname, mac_str(&bssid));
        self.mlme = Mlme::Assoc { bssid, freq, frame: f.clone(), req_ies: ies, tries: 1, deadline: now_ns() + ASSOC_TIMEOUT_NS };
        self.tx_mgmt(f)
    }

    /// Our rates for an association on `freq`: the band's, with the
    /// AP's basic rates marked basic.
    fn assoc_rates(&self, freq: u32, ap_ies: &[u8]) -> Vec<u8> {
        let ap = w::rates_of(ap_ies);
        let band = w::band_of(freq);
        let mut out = Vec::new();
        if let Some(b) = self.caps().bands.iter().find(|b| b.band == band) {
            for &r in &b.rates {
                let u = (r / 5) as u8;
                let basic = ap.iter().any(|&a| a & 0x7f == u && a & 0x80 != 0);
                out.push(u | if basic { 0x80 } else { 0 });
            }
        }
        out
    }

    /// NL80211_CMD_DEAUTHENTICATE / DISASSOCIATE / DISCONNECT from user
    /// space.
    pub fn deauthenticate(&mut self, bssid: Option<[u8; 6]>, reason: u16, disassoc: bool, local_only: bool) -> KResult<()> {
        let cur = match &self.mlme {
            Mlme::Idle => return Ok(()),
            Mlme::Auth { bssid, .. } | Mlme::Authed { bssid, .. } | Mlme::Assoc { bssid, .. } | Mlme::Connected { bssid, .. } => *bssid,
        };
        if bssid.is_some_and(|b| b != cur) {
            return Err(ENOTCONN);
        }
        let stype = if disassoc { w::STYPE_DISASSOC } else { w::STYPE_DEAUTH };
        let f = w::deauth(stype, cur, self.mac, cur, reason);
        if !local_only && self.started {
            let _ = self.tx_mgmt(f.clone());
        }
        let was_connected = matches!(self.mlme, Mlme::Connected { .. });
        log::info!("wifi: {} leaving {} (reason {reason})", self.ifname, mac_str(&cur));
        nl80211::event_frame(self, if disassoc { nl80211::CMD_DISASSOCIATE } else { nl80211::CMD_DEAUTHENTICATE }, &f);
        self.reset_link();
        if was_connected {
            nl80211::event_disconnect(self, reason, false);
        }
        Ok(())
    }

    /// Drop the association from our side (reason `reason`), telling
    /// user space; `tx`: send the deauthentication frame.
    fn disconnect_local(&mut self, reason: u16, tx: bool) {
        let _ = self.deauthenticate(None, reason, false, !tx);
    }

    fn reset_link(&mut self) {
        if self.started && !matches!(self.mlme, Mlme::Idle) {
            self.drv.leave();
        }
        let was = matches!(self.mlme, Mlme::Connected { .. });
        self.mlme = Mlme::Idle;
        self.authorized = false;
        self.rx_eth.clear();
        if was {
            LINKS_DIRTY.store(true, Ordering::Release);
        }
    }

    fn rx_auth(&mut self, f: &[u8]) {
        let Mlme::Auth { bssid, freq, alg, .. } = self.mlme.clone() else { return };
        if w::addr(f, 2) != bssid || f.len() < 30 {
            return;
        }
        let ralg = u16::from_le_bytes([f[24], f[25]]);
        let trans = u16::from_le_bytes([f[26], f[27]]);
        let status = u16::from_le_bytes([f[28], f[29]]);
        log::info!("wifi: {} auth reply from {}: alg {ralg} seq {trans} status {status}", self.ifname, mac_str(&bssid));
        nl80211::event_frame(self, nl80211::CMD_AUTHENTICATE, f);
        if ralg != alg {
            return;
        }
        if status == 0 && (alg != w::AUTH_SAE || trans == 2) {
            self.mlme = Mlme::Authed { bssid, freq, done: true };
        } else if alg == w::AUTH_SAE && (status == 0 || status == 126 || status == 76) {
            // SAE commit answered: wpa_supplicant sends the confirm (or
            // a new commit) with another AUTHENTICATE.
            self.mlme = Mlme::Authed { bssid, freq, done: false };
        } else {
            self.drv.leave();
            self.mlme = Mlme::Idle;
        }
    }

    fn rx_assoc_resp(&mut self, f: &[u8]) {
        let Mlme::Assoc { bssid, freq, req_ies, .. } = self.mlme.clone() else { return };
        if w::addr(f, 2) != bssid || f.len() < 30 {
            return;
        }
        let capability = u16::from_le_bytes([f[24], f[25]]);
        let status = u16::from_le_bytes([f[26], f[27]]);
        let aid = u16::from_le_bytes([f[28], f[29]]) & 0x3fff;
        let resp_ies = f[30..].to_vec();
        log::info!("wifi: {} assoc reply from {}: status {status} aid {aid}", self.ifname, mac_str(&bssid));
        if status != 0 {
            nl80211::event_assoc(self, f, &req_ies);
            nl80211::event_connect(self, bssid, status, &req_ies, &resp_ies);
            self.drv.leave();
            self.mlme = Mlme::Idle;
            return;
        }
        let bss = self.find_bss(&bssid).cloned();
        let beacon_int = bss.as_ref().map_or(100, |b| b.beacon_int);
        let mut rates = w::rates_of(&resp_ies);
        if rates.is_empty() {
            rates = bss.as_ref().map(|b| w::rates_of(&b.ies)).unwrap_or_default();
        }
        let info = AssocInfo { bssid, aid, capability, rates: &rates, resp_ies: &resp_ies, beacon_int, qos: false };
        if let Err(e) = self.drv.associated(&info) {
            log::warn!("wifi: {} driver refused the association: {e:?}", self.ifname);
            nl80211::event_assoc_timeout(self, bssid);
            self.drv.leave();
            self.mlme = Mlme::Idle;
            return;
        }
        self.mlme = Mlme::Connected { bssid, freq, aid, req_ies: req_ies.clone(), resp_ies: resp_ies.clone(), since: now_ns(), qos: false };
        self.last_rx_ns = now_ns();
        self.signal = bss.as_ref().map_or(-50, |b| (b.signal_mbm / 100) as i8);
        LINKS_DIRTY.store(true, Ordering::Release);
        nl80211::event_assoc(self, f, &req_ies);
        nl80211::event_connect(self, bssid, 0, &req_ies, &resp_ies);
    }

    fn rx_deauth(&mut self, f: &[u8]) {
        let cur = match &self.mlme {
            Mlme::Idle => return,
            Mlme::Auth { bssid, .. } | Mlme::Authed { bssid, .. } | Mlme::Assoc { bssid, .. } | Mlme::Connected { bssid, .. } => *bssid,
        };
        if w::addr(f, 2) != cur || f.len() < 26 {
            return;
        }
        let reason = u16::from_le_bytes([f[24], f[25]]);
        let disassoc = w::stype(w::fc(f)) == w::STYPE_DISASSOC;
        log::info!("wifi: {} {} by {} (reason {reason})", self.ifname, if disassoc { "disassociated" } else { "deauthenticated" }, mac_str(&cur));
        let was_connected = matches!(self.mlme, Mlme::Connected { .. });
        nl80211::event_frame(self, if disassoc { nl80211::CMD_DISASSOCIATE } else { nl80211::CMD_DEAUTHENTICATE }, f);
        self.reset_link();
        if was_connected {
            nl80211::event_disconnect(self, reason, true);
        }
    }

    fn timers(&mut self) {
        let now = now_ns();
        match &mut self.mlme {
            Mlme::Auth { bssid, frame, tries, deadline, .. } if now >= *deadline => {
                if *tries < MAX_TRIES {
                    *tries += 1;
                    *deadline = now + AUTH_TIMEOUT_NS;
                    let f = frame.clone();
                    let _ = self.tx_mgmt(f);
                } else {
                    let b = *bssid;
                    log::info!("wifi: {} authentication with {} timed out", self.ifname, mac_str(&b));
                    self.drv.leave();
                    self.mlme = Mlme::Idle;
                    nl80211::event_auth_timeout(self, b);
                }
            }
            Mlme::Assoc { bssid, frame, tries, deadline, .. } if now >= *deadline => {
                if *tries < MAX_TRIES {
                    *tries += 1;
                    *deadline = now + ASSOC_TIMEOUT_NS;
                    let f = frame.clone();
                    let _ = self.tx_mgmt(f);
                } else {
                    let b = *bssid;
                    log::info!("wifi: {} association with {} timed out", self.ifname, mac_str(&b));
                    self.drv.leave();
                    self.mlme = Mlme::Idle;
                    nl80211::event_assoc_timeout(self, b);
                }
            }
            _ => {}
        }
        if self.scan.as_ref().is_some_and(|s| now >= s.deadline) {
            log::warn!("wifi: {} scan timed out", self.ifname);
            self.drv.abort_scan();
            self.scan_done(true);
        }
    }

    fn poll(&mut self) {
        for (f, c) in core::mem::take(&mut self.tx_status) {
            nl80211::event_tx_status(self, &f, c);
        }
        if !self.started {
            return;
        }
        let mut evs = Vec::new();
        self.drv.poll(&mut evs);
        for e in evs {
            match e {
                Event::Rx { frame, freq, signal } => self.rx(frame, freq, signal),
                Event::ScanDone { aborted } => self.scan_done(aborted),
                Event::ConnectionLost => {
                    if matches!(self.mlme, Mlme::Connected { .. }) {
                        log::info!("wifi: {} lost the AP", self.ifname);
                        // WLAN_REASON_DISASSOC_DUE_TO_INACTIVITY, like
                        // mac80211's connection monitor.
                        self.disconnect_local(4, false);
                    }
                }
            }
        }
        self.timers();
    }

    fn rx(&mut self, f: Vec<u8>, freq: u32, signal: i8) {
        if f.len() < 24 {
            return;
        }
        let fc = w::fc(&f);
        if w::is_data(fc) {
            self.rx_data(&f, signal);
            return;
        }
        if !w::is_mgmt(fc) {
            return;
        }
        let da = w::addr(&f, 1);
        let for_us = da == self.mac || da[0] & 1 != 0;
        match w::stype(fc) {
            w::STYPE_BEACON | w::STYPE_PROBE_RESP => self.rx_beacon(&f, freq, signal),
            w::STYPE_AUTH if for_us => self.rx_auth(&f),
            w::STYPE_ASSOC_RESP | w::STYPE_REASSOC_RESP if da == self.mac => self.rx_assoc_resp(&f),
            w::STYPE_DEAUTH | w::STYPE_DISASSOC if for_us => self.rx_deauth(&f),
            st if for_us => self.rx_registered(&f, st, freq, signal),
            _ => {}
        }
    }

    fn rx_data(&mut self, f: &[u8], signal: i8) {
        let Mlme::Connected { bssid, .. } = self.mlme else { return };
        let fc = w::fc(f);
        if fc & (w::FCTL_TODS | w::FCTL_FROMDS) != w::FCTL_FROMDS || w::addr(f, 2) != bssid {
            return;
        }
        self.last_rx_ns = now_ns();
        self.signal = signal;
        for e in w::data_to_eth(f) {
            // Our own broadcasts reflected by the AP.
            if e[6..12] == self.mac {
                continue;
            }
            if !self.authorized && w::ethertype(&e) != w::ETH_P_PAE {
                continue;
            }
            if self.rx_eth.len() < RX_QUEUE_MAX {
                self.stats.rx_packets += 1;
                self.stats.rx_bytes += e.len() as u64;
                self.rx_eth.push_back(e);
                RX_WAITING.store(true, Ordering::Release);
            }
        }
    }

    // ---- registered management frames (action frames, ...) ----

    pub fn register_frame(&mut self, portid: u32, ftype: u16, pattern: &[u8]) -> KResult<()> {
        if self.regs.iter().any(|r| r.portid == portid && r.ftype == ftype && r.pattern == pattern) {
            return Err(EALREADY);
        }
        self.regs.push(FrameReg { portid, ftype, pattern: pattern.to_vec() });
        Ok(())
    }

    fn rx_registered(&mut self, f: &[u8], stype: u16, freq: u32, signal: i8) {
        let body = &f[24..];
        let ftype = w::FTYPE_MGMT | stype;
        let mut dead = Vec::new();
        for r in &self.regs {
            if r.ftype == ftype && body.starts_with(&r.pattern) {
                if !nl80211::event_rx_frame(self, r.portid, f, freq, signal) {
                    dead.push(r.portid);
                }
                break;
            }
        }
        self.regs.retain(|r| !dead.contains(&r.portid));
    }

    /// NL80211_CMD_FRAME: send a management frame; the cookie.
    pub fn tx_frame(&mut self, frame: &[u8]) -> KResult<u64> {
        self.ensure_started()?;
        if frame.len() < 24 || !w::is_mgmt(w::fc(frame)) {
            return Err(EINVAL);
        }
        self.tx_mgmt(frame.to_vec())?;
        self.cookie += 1;
        self.tx_status.push((frame.to_vec(), self.cookie));
        PENDING.store(true, Ordering::Relaxed);
        Ok(self.cookie)
    }

    // ---- keys and the controlled port ----

    pub fn new_key(&mut self, k: Key) -> KResult<()> {
        self.ensure_started()?;
        log::info!("wifi: {} {} key {} cipher {:08x}", self.ifname, if k.pairwise { "pairwise" } else { "group" }, k.idx, k.cipher);
        self.drv.set_key(&k)
    }

    pub fn del_key(&mut self, k: Key) {
        if self.started {
            self.drv.del_key(&k);
        }
    }

    pub fn set_authorized(&mut self, mac: [u8; 6], on: bool) -> KResult<()> {
        if self.connected_bssid() != Some(mac) {
            return Err(ENOENT);
        }
        if on && !self.authorized {
            log::info!("wifi: {} connected to {}, port authorized", self.ifname, mac_str(&mac));
        }
        self.authorized = on;
        self.drv.set_authorized(on);
        Ok(())
    }

    // ---- the link (IP stack side) ----

    fn send_eth(&mut self, frame: &[u8]) -> bool {
        let Mlme::Connected { bssid, qos, .. } = self.mlme else { return false };
        let eapol = w::ethertype(frame) == w::ETH_P_PAE;
        if !self.authorized && !eapol {
            return false;
        }
        let Some(mut f) = w::eth_to_80211(frame, bssid, self.mac, qos) else { return false };
        let s = self.next_seq();
        w::set_seq(&mut f, s);
        let ok = self.drv.tx_data(&f, eapol);
        if ok {
            self.stats.tx_packets += 1;
            self.stats.tx_bytes += frame.len() as u64;
        }
        ok
    }

    fn set_up(&mut self, up: bool) {
        if up == self.want_up {
            return;
        }
        self.want_up = up;
        if up {
            if let Err(e) = self.ensure_started() {
                log::warn!("wifi: {} could not start the radio: {e:?}", self.ifname);
            }
        } else {
            if !matches!(self.mlme, Mlme::Idle) {
                self.disconnect_local(3, true);
            }
            if self.scan.is_some() {
                self.drv.abort_scan();
                self.scan_done(true);
            }
        }
    }
}

pub struct StationInfo {
    pub bssid: [u8; 6],
    pub connected_s: u32,
    pub inactive_ms: u32,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_failed: u64,
    pub tx_retries: u64,
    pub signal: i8,
    pub tx_rate: u32,
    pub authorized: bool,
}

/// wlan<N> as a link of the IP stack.
pub struct WlanDev {
    phy: u32,
}

impl WlanDev {
    fn with<R>(&self, f: impl FnOnce(&mut Wiphy) -> R) -> R {
        // Called with the IP stack locked: never refresh links here.
        let mut g = WIPHYS.lock();
        f(&mut g[self.phy as usize])
    }
}

impl EthDevice for WlanDev {
    fn mac(&self) -> [u8; 6] {
        self.with(|w| w.mac)
    }

    fn recv(&mut self) -> Option<Vec<u8>> {
        self.with(|w| w.rx_eth.pop_front())
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        self.with(|w| w.send_eth(frame))
    }

    fn can_send(&mut self) -> bool {
        self.with(|w| !w.started || w.drv.can_tx())
    }

    fn set_mac(&mut self, mac: [u8; 6]) -> bool {
        self.with(|w| {
            if w.want_up || mac[0] & 1 != 0 {
                return false;
            }
            if w.started {
                w.drv.stop();
                w.started = false;
            }
            w.mac = mac;
            true
        })
    }

    fn set_up(&mut self, up: bool) {
        self.with(|w| w.set_up(up));
    }

    fn driver(&self) -> &'static str {
        self.with(|w| w.caps().driver)
    }

    fn carrier(&mut self) -> Option<bool> {
        Some(self.with(|w| matches!(w.mlme, Mlme::Connected { .. })))
    }
}

/// Register the drivers that are not on a bus (the simulated radio).
pub fn init() {
    hwsim::init();
}
