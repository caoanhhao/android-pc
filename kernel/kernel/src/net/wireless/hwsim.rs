//! A simulated radio with simulated access points (like Linux's
//! mac80211_hwsim plus a hostapd), to test the Wi-Fi stack and Android's
//! Wi-Fi framework in QEMU.
//!
//! `wifisim=AP[,AP...]` on the command line registers the radio; each AP
//! is `SSID[:PASSPHRASE][@CHANNEL]` (WPA2-Personal with a passphrase,
//! open without; channel 6 by default, 36+ is 5 GHz). The APs answer
//! scans with probe responses, authentication (open system) and
//! association requests, run the authenticator side of the WPA2 4-way
//! handshake (PSK, CCMP, HMAC-SHA1 MICs, the group key wrapped with AES;
//! a wrong passphrase gets the station deauthenticated with reason 15),
//! and bridge the station's data frames to an uplink: the virtio-net
//! card at PCI slot `wifisim.uplink=BB:DD.F` (claimed before it becomes
//! an Ethernet link, see `drivers::virtio::net`), so QEMU's user-mode
//! network gives the station DHCP and the internet. No encryption is
//! applied over the simulated air: keys are only checked to be
//! installed.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use super::crypto;
use super::ieee80211 as w;
use super::*;
use crate::net::inet::EthDevice;

static UPLINK: Mutex<Option<Box<dyn EthDevice>>> = Mutex::new(None);

/// The PCI slot ("00:09.0") of the virtio-net card that is the APs'
/// uplink, if the command line names one.
pub fn uplink_slot() -> Option<&'static str> {
    crate::boot::cmdline().split_ascii_whitespace().find_map(|w| w.strip_prefix("wifisim.uplink="))
}

/// The virtio-net driver hands over the uplink card.
pub fn set_uplink(dev: Box<dyn EthDevice>) {
    log::info!("wifisim: uplink card claimed");
    *UPLINK.lock() = Some(dev);
}

const AP_RATES_24: &[u8] = &[0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];
const AP_EXT_RATES_24: &[u8] = &[0x30, 0x48, 0x60, 0x6c];
const AP_RATES_5: &[u8] = &[0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c];
/// RSN element: CCMP group and pairwise, PSK.
const RSN_PSK: &[u8] = &[1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0, 0];

#[derive(Clone, Copy, PartialEq)]
enum StaState {
    None,
    Authed,
    /// Associated; handshake step (1: msg1 sent, 3: msg3 sent, 4: done).
    Assoc(u8),
}

struct Ap {
    bssid: [u8; 6],
    ssid: Vec<u8>,
    chan: u8,
    freq: u32,
    pmk: Option<[u8; 32]>,
    gtk: [u8; 16],
    sta: [u8; 6],
    state: StaState,
    anonce: [u8; 32],
    replay: u64,
    ptk: Vec<u8>,
}

impl Ap {
    fn ies(&self) -> Vec<u8> {
        let mut ies = Vec::new();
        w::push_ie(&mut ies, w::EID_SSID, &self.ssid);
        if self.freq < 3000 {
            w::push_ie(&mut ies, w::EID_SUPP_RATES, AP_RATES_24);
            w::push_ie(&mut ies, w::EID_DS_PARAMS, &[self.chan]);
            w::push_ie(&mut ies, w::EID_EXT_SUPP_RATES, AP_EXT_RATES_24);
        } else {
            w::push_ie(&mut ies, w::EID_SUPP_RATES, AP_RATES_5);
            w::push_ie(&mut ies, w::EID_DS_PARAMS, &[self.chan]);
        }
        if self.pmk.is_some() {
            w::push_ie(&mut ies, w::EID_RSN, RSN_PSK);
        }
        ies
    }

    fn capability(&self) -> u16 {
        w::WLAN_CAPABILITY_ESS
            | w::WLAN_CAPABILITY_SHORT_PREAMBLE
            | if self.freq < 3000 { w::WLAN_CAPABILITY_SHORT_SLOT_TIME } else { 0 }
            | if self.pmk.is_some() { w::WLAN_CAPABILITY_PRIVACY } else { 0 }
    }

    /// A beacon (to broadcast) or probe response (to `da`).
    fn beacon(&self, da: Option<[u8; 6]>) -> Vec<u8> {
        let st = if da.is_some() { w::STYPE_PROBE_RESP } else { w::STYPE_BEACON };
        let mut f = w::mgmt_header(st, da.unwrap_or(w::BROADCAST), self.bssid, self.bssid);
        f.extend_from_slice(&(super::now_ns() / 1000).to_le_bytes());
        f.extend_from_slice(&100u16.to_le_bytes());
        f.extend_from_slice(&self.capability().to_le_bytes());
        f.extend_from_slice(&self.ies());
        f
    }

    /// A data frame from the AP to the station (From DS).
    fn data_to_sta(&self, da: [u8; 6], sa: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut f = Vec::with_capacity(payload.len() + 32);
        f.extend_from_slice(&(w::FTYPE_DATA | w::FCTL_FROMDS).to_le_bytes());
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&da);
        f.extend_from_slice(&self.bssid);
        f.extend_from_slice(&sa);
        f.extend_from_slice(&[0, 0]);
        f.extend_from_slice(&w::RFC1042);
        f.extend_from_slice(&ethertype.to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    /// An EAPOL-Key frame of the handshake (MIC filled in when `mic`).
    fn eapol_key(&self, info: u16, nonce: &[u8; 32], key_data: &[u8], mic: bool) -> Vec<u8> {
        let mut k = Vec::with_capacity(99 + key_data.len());
        k.push(2); // RSN key descriptor
        k.extend_from_slice(&info.to_be_bytes());
        k.extend_from_slice(&16u16.to_be_bytes());
        k.extend_from_slice(&self.replay.to_be_bytes());
        k.extend_from_slice(nonce);
        k.extend_from_slice(&[0; 16 + 8 + 8 + 16]);
        k.extend_from_slice(&(key_data.len() as u16).to_be_bytes());
        k.extend_from_slice(key_data);
        let mut e = Vec::with_capacity(4 + k.len());
        e.extend_from_slice(&[2, 3]);
        e.extend_from_slice(&(k.len() as u16).to_be_bytes());
        e.extend_from_slice(&k);
        if mic {
            let m = crypto::hmac_sha1(&self.ptk[..16], &[&e]);
            e[81..97].copy_from_slice(&m[..16]);
        }
        e
    }

    fn msg1(&mut self) -> Vec<u8> {
        crate::random::fill(&mut self.anonce);
        self.replay += 1;
        self.state = StaState::Assoc(1);
        let e = self.eapol_key(0x008a, &self.anonce, &[], false);
        self.data_to_sta(self.sta, self.bssid, w::ETH_P_PAE, &e)
    }

    /// The station's EAPOL frame: the next message, or Err(()) for a
    /// MIC failure (wrong passphrase).
    fn eapol_in(&mut self, e: &[u8]) -> Result<Option<Vec<u8>>, ()> {
        if e.len() < 99 || e[1] != 3 || e[4] != 2 {
            return Ok(None);
        }
        let info = u16::from_be_bytes([e[5], e[6]]);
        let replay = u64::from_be_bytes(e[9..17].try_into().unwrap());
        if replay != self.replay || info & 0x0100 == 0 {
            return Ok(None);
        }
        let Some(pmk) = self.pmk else { return Ok(None) };
        match self.state {
            StaState::Assoc(1) => {
                let snonce: [u8; 32] = e[17..49].try_into().unwrap();
                let (a1, a2) = if self.bssid < self.sta { (self.bssid, self.sta) } else { (self.sta, self.bssid) };
                let (n1, n2) = if self.anonce < snonce { (self.anonce, snonce) } else { (snonce, self.anonce) };
                let mut data = Vec::new();
                data.extend_from_slice(&a1);
                data.extend_from_slice(&a2);
                data.extend_from_slice(&n1);
                data.extend_from_slice(&n2);
                self.ptk = crypto::prf(&pmk, b"Pairwise key expansion", &data, 48);
                if !self.mic_ok(e) {
                    return Err(());
                }
                // Message 3: our RSN element and the group key, wrapped.
                let mut kd = Vec::new();
                w::push_ie(&mut kd, w::EID_RSN, RSN_PSK);
                let mut gtk_kde = alloc::vec![0x00, 0x0f, 0xac, 0x01, 0x01, 0x00];
                gtk_kde.extend_from_slice(&self.gtk);
                w::push_ie(&mut kd, w::EID_VENDOR, &gtk_kde);
                if kd.len() % 8 != 0 || kd.len() < 16 {
                    kd.push(0xdd);
                    while kd.len() % 8 != 0 || kd.len() < 16 {
                        kd.push(0);
                    }
                }
                let kek: [u8; 16] = self.ptk[16..32].try_into().unwrap();
                let wrapped = crypto::aes_wrap(&kek, &kd);
                self.replay += 1;
                self.state = StaState::Assoc(3);
                let m = self.eapol_key(0x13ca, &self.anonce.clone(), &wrapped, true);
                Ok(Some(self.data_to_sta(self.sta, self.bssid, w::ETH_P_PAE, &m)))
            }
            StaState::Assoc(3) => {
                if !self.mic_ok(e) {
                    return Err(());
                }
                self.state = StaState::Assoc(4);
                log::info!("wifisim: {} 4-way handshake complete", String::from_utf8_lossy(&self.ssid));
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn mic_ok(&self, e: &[u8]) -> bool {
        let len = (4 + u16::from_be_bytes([e[2], e[3]]) as usize).min(e.len());
        let mut z = e[..len].to_vec();
        z[81..97].fill(0);
        let m = crypto::hmac_sha1(&self.ptk[..16], &[&z]);
        m[..16] == e[81..97]
    }

    fn authorized(&self) -> bool {
        match self.state {
            StaState::Assoc(4) => true,
            StaState::Assoc(_) => self.pmk.is_none(),
            _ => false,
        }
    }
}

pub struct Hwsim {
    caps: Caps,
    mac: [u8; 6],
    on: bool,
    freq: u32,
    aps: Vec<Ap>,
    /// Frames and events for the station, with the time they are due.
    rx: VecDeque<(u64, Event)>,
    scan_done_at: Option<u64>,
    tx: TxStats,
}

fn channels(list: &[(u8, u8)]) -> Vec<Channel> {
    list.iter()
        .map(|&(c, b)| Channel { freq: w::chan_to_freq(c, b), disabled: false, no_ir: false, radar: false, max_power: 2000 })
        .collect()
}

fn parse_aps(spec: &str) -> Vec<Ap> {
    let mut aps = Vec::new();
    for (i, a) in spec.split(',').filter(|a| !a.is_empty()).enumerate() {
        let (rest, chan) = match a.rsplit_once('@') {
            Some((r, c)) => (r, c.parse().unwrap_or(6)),
            None => (a, 6u8),
        };
        let (ssid, pass) = match rest.split_once(':') {
            Some((s, p)) => (s, Some(p)),
            None => (rest, None),
        };
        let band = if chan > 14 { 1 } else { 0 };
        let mut gtk = [0u8; 16];
        crate::random::fill(&mut gtk);
        aps.push(Ap {
            bssid: [0x02, 0x00, 0x00, 0x00, (i + 1) as u8, 0x00],
            ssid: ssid.as_bytes().to_vec(),
            chan,
            freq: w::chan_to_freq(chan, band),
            pmk: pass.map(|p| crypto::wpa_psk(p.as_bytes(), ssid.as_bytes())),
            gtk,
            sta: [0; 6],
            state: StaState::None,
            anonce: [0; 32],
            replay: 0,
            ptk: Vec::new(),
        });
    }
    aps
}

/// Register the simulated radio if the command line asks for it.
pub fn init() {
    let Some(spec) = crate::boot::cmdline().split_ascii_whitespace().find_map(|w| w.strip_prefix("wifisim=")) else {
        return;
    };
    let aps = parse_aps(spec);
    for ap in &aps {
        log::info!(
            "wifisim: AP \"{}\" {} on {} MHz{}",
            String::from_utf8_lossy(&ap.ssid),
            super::mac_str(&ap.bssid),
            ap.freq,
            if ap.pmk.is_some() { " (WPA2-PSK)" } else { " (open)" }
        );
    }
    let mac = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let ch24: Vec<(u8, u8)> = (1..=13).map(|c| (c, 0)).collect();
    let ch5: Vec<(u8, u8)> = [36, 40, 44, 48, 149, 153, 157, 161, 165].iter().map(|&c| (c, 1)).collect();
    let caps = Caps {
        driver: "hwsim",
        perm_addr: mac,
        bands: alloc::vec![
            Band { band: 0, channels: channels(&ch24), rates: alloc::vec![10, 20, 55, 110, 60, 90, 120, 180, 240, 360, 480, 540], ht: None, vht: None },
            Band { band: 1, channels: channels(&ch5), rates: alloc::vec![60, 90, 120, 180, 240, 360, 480, 540], ht: None, vht: None },
        ],
        ciphers: alloc::vec![CIPHER_CCMP, CIPHER_TKIP, CIPHER_AES_CMAC],
        max_scan_ssids: 4,
        max_scan_ie_len: 512,
        sae: true,
        random_scan_mac: true,
    };
    let sim = Hwsim { caps, mac, on: false, freq: 0, aps, rx: VecDeque::new(), scan_done_at: None, tx: TxStats::default() };
    crate::fs::sysfs::add_device("devices/virtual/mac80211_hwsim/hwsim0", "mac80211_hwsim", "class/mac80211_hwsim", String::new(), false);
    super::register(Box::new(sim), "devices/virtual/mac80211_hwsim/hwsim0");
}

impl Hwsim {
    fn queue(&mut self, delay_ms: u64, e: Event) {
        self.rx.push_back((super::now_ns() + delay_ms * 1_000_000, e));
    }

    fn to_sta(&mut self, delay_ms: u64, frame: Vec<u8>, i: usize) {
        let freq = self.aps[i].freq;
        self.queue(delay_ms, Event::Rx { frame, freq, signal: -40 - i as i8 * 5 });
    }

    fn ap_on_channel(&self, bssid: [u8; 6]) -> Option<usize> {
        self.aps.iter().position(|a| a.bssid == bssid && a.freq == self.freq)
    }

    fn mgmt_in(&mut self, f: &[u8]) {
        let fc = w::fc(f);
        let Some(i) = self.ap_on_channel(w::addr(f, 1)) else { return };
        let sta = w::addr(f, 2);
        match w::stype(fc) {
            w::STYPE_AUTH if f.len() >= 30 => {
                let alg = u16::from_le_bytes([f[24], f[25]]);
                let ap = &mut self.aps[i];
                let status: u16 = if alg == w::AUTH_OPEN { 0 } else { 13 };
                let mut r = w::mgmt_header(w::STYPE_AUTH, sta, ap.bssid, ap.bssid);
                r.extend_from_slice(&alg.to_le_bytes());
                r.extend_from_slice(&2u16.to_le_bytes());
                r.extend_from_slice(&status.to_le_bytes());
                if status == 0 {
                    ap.sta = sta;
                    ap.state = StaState::Authed;
                }
                self.to_sta(2, r, i);
            }
            w::STYPE_ASSOC_REQ | w::STYPE_REASSOC_REQ => {
                let ap = &mut self.aps[i];
                let off = if w::stype(fc) == w::STYPE_REASSOC_REQ { 34 } else { 28 };
                let ies = f.get(off..).unwrap_or(&[]);
                let ok_ssid = w::ssid_of(ies) == Some(&ap.ssid[..]);
                let rsn = w::find_ie(ies, w::EID_RSN).is_some();
                let status: u16 = if ap.sta != sta || ap.state == StaState::None {
                    9
                } else if !ok_ssid {
                    1
                } else if ap.pmk.is_some() != rsn {
                    43
                } else {
                    0
                };
                let st = if w::stype(fc) == w::STYPE_ASSOC_REQ { w::STYPE_ASSOC_RESP } else { w::STYPE_REASSOC_RESP };
                let mut r = w::mgmt_header(st, sta, ap.bssid, ap.bssid);
                r.extend_from_slice(&ap.capability().to_le_bytes());
                r.extend_from_slice(&status.to_le_bytes());
                r.extend_from_slice(&(0xc001u16).to_le_bytes());
                w::push_ie(&mut r, w::EID_SUPP_RATES, if ap.freq < 3000 { AP_RATES_24 } else { AP_RATES_5 });
                if ap.freq < 3000 {
                    w::push_ie(&mut r, w::EID_EXT_SUPP_RATES, AP_EXT_RATES_24);
                }
                self.to_sta(2, r, i);
                if status == 0 {
                    let ap = &mut self.aps[i];
                    ap.replay = 0;
                    ap.state = StaState::Assoc(0);
                    if ap.pmk.is_some() {
                        let m1 = ap.msg1();
                        self.to_sta(20, m1, i);
                    }
                }
            }
            w::STYPE_DEAUTH | w::STYPE_DISASSOC => {
                let ap = &mut self.aps[i];
                if ap.sta == sta {
                    ap.state = StaState::None;
                }
            }
            _ => {}
        }
    }

    fn deauth_sta(&mut self, i: usize, reason: u16) {
        let ap = &mut self.aps[i];
        ap.state = StaState::None;
        let f = w::deauth(w::STYPE_DEAUTH, ap.sta, ap.bssid, ap.bssid, reason);
        self.to_sta(1, f, i);
    }

    fn data_in(&mut self, f: &[u8]) {
        let fc = w::fc(f);
        if fc & (w::FCTL_TODS | w::FCTL_FROMDS) != w::FCTL_TODS {
            return;
        }
        let Some(i) = self.ap_on_channel(w::addr(f, 1)) else { return };
        if self.aps[i].sta != w::addr(f, 2) || !matches!(self.aps[i].state, StaState::Assoc(_)) {
            return;
        }
        for e in w::data_to_eth(f) {
            if w::ethertype(&e) == w::ETH_P_PAE {
                match self.aps[i].eapol_in(&e[14..]) {
                    Ok(Some(reply)) => self.to_sta(2, reply, i),
                    Ok(None) => {}
                    Err(()) => {
                        log::info!("wifisim: MIC failure (wrong passphrase?), deauthenticating");
                        self.deauth_sta(i, 15);
                    }
                }
            } else if self.aps[i].authorized()
                && let Some(u) = UPLINK.lock().as_mut()
            {
                u.send(&e);
            }
        }
    }

    /// Frames from the uplink for the station.
    fn uplink_in(&mut self) {
        let mut frames = Vec::new();
        if let Some(u) = UPLINK.lock().as_mut() {
            while let Some(e) = u.recv() {
                frames.push(e);
                if frames.len() >= 64 {
                    break;
                }
            }
        }
        for e in frames {
            if e.len() < 14 {
                continue;
            }
            let da: [u8; 6] = e[0..6].try_into().unwrap();
            let sa: [u8; 6] = e[6..12].try_into().unwrap();
            for i in 0..self.aps.len() {
                let ap = &self.aps[i];
                if ap.freq == self.freq && ap.authorized() && (da == ap.sta || da[0] & 1 != 0) {
                    let f = ap.data_to_sta(da, sa, w::ethertype(&e), &e[14..]);
                    self.to_sta(0, f, i);
                }
            }
        }
    }
}

impl WifiDriver for Hwsim {
    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn start(&mut self, mac: [u8; 6]) -> KResult<()> {
        self.mac = mac;
        self.on = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.on = false;
        self.rx.clear();
        self.scan_done_at = None;
    }

    fn scan(&mut self, req: &ScanReq) -> KResult<()> {
        let now = super::now_ns();
        for i in 0..self.aps.len() {
            let ap = &self.aps[i];
            if !req.freqs.contains(&ap.freq) {
                continue;
            }
            let wanted = req.ssids.is_empty() || req.ssids.iter().any(|s| s.is_empty() || *s == ap.ssid);
            if !wanted {
                continue;
            }
            let f = if req.ssids.is_empty() { ap.beacon(None) } else { ap.beacon(Some(req.mac)) };
            let freq = ap.freq;
            self.rx.push_back((now + 30_000_000, Event::Rx { frame: f, freq, signal: -40 - i as i8 * 5 }));
        }
        self.scan_done_at = Some(now + 100_000_000);
        Ok(())
    }

    fn abort_scan(&mut self) {
        if self.scan_done_at.is_some() {
            self.scan_done_at = None;
            self.rx.push_back((0, Event::ScanDone { aborted: true }));
        }
    }

    fn join(&mut self, p: &JoinParams) -> KResult<()> {
        self.freq = p.freq;
        Ok(())
    }

    fn associated(&mut self, _a: &AssocInfo) -> KResult<()> {
        Ok(())
    }

    fn leave(&mut self) {}

    fn tx_mgmt(&mut self, frame: &[u8]) -> KResult<()> {
        self.tx.tx_packets += 1;
        self.mgmt_in(frame);
        Ok(())
    }

    fn tx_data(&mut self, frame: &[u8], _eapol: bool) -> bool {
        self.tx.tx_packets += 1;
        self.data_in(frame);
        true
    }

    fn set_key(&mut self, k: &Key) -> KResult<()> {
        if k.cipher != CIPHER_CCMP && k.cipher != CIPHER_TKIP && k.cipher != CIPHER_AES_CMAC {
            return Err(EOPNOTSUPP);
        }
        Ok(())
    }

    fn poll(&mut self, out: &mut Vec<Event>) {
        if !self.on {
            return;
        }
        self.uplink_in();
        let now = super::now_ns();
        // Due events in order; later ones stay queued.
        while let Some((t, _)) = self.rx.front() {
            if *t > now {
                break;
            }
            out.push(self.rx.pop_front().unwrap().1);
        }
        if self.scan_done_at.is_some_and(|t| now >= t) && self.rx.iter().all(|(t, _)| *t > now) {
            self.scan_done_at = None;
            out.push(Event::ScanDone { aborted: false });
        }
    }

    fn tx_stats(&self) -> TxStats {
        TxStats { tx_rate: 540, ..self.tx }
    }
}
