//! IEEE 802.11 frames: header fields, the management frames the station
//! sends (authentication, association, deauthentication, probe request),
//! information elements, and the conversion between 802.11 data frames
//! and the Ethernet frames of the network stack.

use alloc::vec::Vec;

pub const FTYPE_MGMT: u16 = 0x0000;
pub const FTYPE_DATA: u16 = 0x0008;
pub const FTYPE_MASK: u16 = 0x000c;
pub const STYPE_MASK: u16 = 0x00f0;

pub const STYPE_ASSOC_REQ: u16 = 0x0000;
pub const STYPE_ASSOC_RESP: u16 = 0x0010;
pub const STYPE_REASSOC_REQ: u16 = 0x0020;
pub const STYPE_REASSOC_RESP: u16 = 0x0030;
pub const STYPE_PROBE_REQ: u16 = 0x0040;
pub const STYPE_PROBE_RESP: u16 = 0x0050;
pub const STYPE_BEACON: u16 = 0x0080;
pub const STYPE_DISASSOC: u16 = 0x00a0;
pub const STYPE_AUTH: u16 = 0x00b0;
pub const STYPE_DEAUTH: u16 = 0x00c0;
pub const STYPE_ACTION: u16 = 0x00d0;

/// Data subtypes: plain data, QoS data; the null-function ones carry no
/// payload (bit 0x40).
pub const STYPE_QOS: u16 = 0x0080;
pub const STYPE_NODATA: u16 = 0x0040;

pub const FCTL_TODS: u16 = 0x0100;
pub const FCTL_FROMDS: u16 = 0x0200;
pub const FCTL_PROTECTED: u16 = 0x4000;
pub const FCTL_ORDER: u16 = 0x8000;

pub const WLAN_CAPABILITY_ESS: u16 = 0x0001;
pub const WLAN_CAPABILITY_PRIVACY: u16 = 0x0010;
pub const WLAN_CAPABILITY_SHORT_PREAMBLE: u16 = 0x0020;
pub const WLAN_CAPABILITY_SPECTRUM_MGMT: u16 = 0x0100;
pub const WLAN_CAPABILITY_SHORT_SLOT_TIME: u16 = 0x0400;

pub const EID_SSID: u8 = 0;
pub const EID_SUPP_RATES: u8 = 1;
pub const EID_DS_PARAMS: u8 = 3;
pub const EID_TIM: u8 = 5;
pub const EID_COUNTRY: u8 = 7;
pub const EID_PWR_CAPABILITY: u8 = 33;
pub const EID_SUPPORTED_CHANNELS: u8 = 36;
pub const EID_HT_CAPABILITY: u8 = 45;
pub const EID_RSN: u8 = 48;
pub const EID_EXT_SUPP_RATES: u8 = 50;
pub const EID_HT_OPERATION: u8 = 61;
pub const EID_EXT_CAPABILITY: u8 = 127;
pub const EID_VHT_CAPABILITY: u8 = 191;
pub const EID_VHT_OPERATION: u8 = 192;
pub const EID_VENDOR: u8 = 221;

pub const ETH_P_PAE: u16 = 0x888e;
pub const ETH_P_PREAUTH: u16 = 0x88c7;

/// Authentication algorithms.
pub const AUTH_OPEN: u16 = 0;
pub const AUTH_SHARED_KEY: u16 = 1;
pub const AUTH_SAE: u16 = 3;

pub const RFC1042: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00];

pub const BROADCAST: [u8; 6] = [0xff; 6];

pub fn fc(f: &[u8]) -> u16 {
    u16::from_le_bytes([f[0], f[1]])
}

pub fn addr(f: &[u8], i: usize) -> [u8; 6] {
    let o = 4 + 6 * (i - 1);
    f[o..o + 6].try_into().unwrap()
}

pub fn is_mgmt(fc: u16) -> bool {
    fc & FTYPE_MASK == FTYPE_MGMT
}

pub fn is_data(fc: u16) -> bool {
    fc & FTYPE_MASK == FTYPE_DATA
}

pub fn stype(fc: u16) -> u16 {
    fc & STYPE_MASK
}

/// A management frame header (24 bytes): to `da`, from `sa`, BSSID
/// `bssid`; the sequence number is filled in when sent.
pub fn mgmt_header(stype: u16, da: [u8; 6], sa: [u8; 6], bssid: [u8; 6]) -> Vec<u8> {
    let mut f = Vec::with_capacity(256);
    f.extend_from_slice(&(FTYPE_MGMT | stype).to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&da);
    f.extend_from_slice(&sa);
    f.extend_from_slice(&bssid);
    f.extend_from_slice(&[0, 0]);
    f
}

/// Set the sequence control field (fragment 0).
pub fn set_seq(f: &mut [u8], seq: u16) {
    if f.len() >= 24 {
        f[22..24].copy_from_slice(&((seq & 0xfff) << 4).to_le_bytes());
    }
}

/// An Authentication frame. `body` starts at the transaction sequence
/// number (nl80211's AUTH_DATA) or is empty for the first open-system
/// frame.
pub fn auth(da: [u8; 6], sa: [u8; 6], alg: u16, body: &[u8], ies: &[u8]) -> Vec<u8> {
    let mut f = mgmt_header(STYPE_AUTH, da, sa, da);
    f.extend_from_slice(&alg.to_le_bytes());
    if body.is_empty() {
        f.extend_from_slice(&1u16.to_le_bytes());
        f.extend_from_slice(&0u16.to_le_bytes());
    } else {
        f.extend_from_slice(body);
    }
    f.extend_from_slice(ies);
    f
}

/// A Deauthentication (or Disassociation) frame.
pub fn deauth(stype: u16, da: [u8; 6], sa: [u8; 6], bssid: [u8; 6], reason: u16) -> Vec<u8> {
    let mut f = mgmt_header(stype, da, sa, bssid);
    f.extend_from_slice(&reason.to_le_bytes());
    f
}

/// Information elements: (id, data).
pub fn ies(b: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 <= b.len() {
        let len = b[i + 1] as usize;
        if i + 2 + len > b.len() {
            break;
        }
        out.push((b[i], &b[i + 2..i + 2 + len]));
        i += 2 + len;
    }
    out
}

pub fn find_ie(b: &[u8], id: u8) -> Option<&[u8]> {
    ies(b).into_iter().find(|&(i, _)| i == id).map(|(_, d)| d)
}

pub fn push_ie(out: &mut Vec<u8>, id: u8, data: &[u8]) {
    out.push(id);
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

/// The SSID of an element list (None without the SSID element).
pub fn ssid_of(ies_buf: &[u8]) -> Option<&[u8]> {
    find_ie(ies_buf, EID_SSID)
}

/// The rates (500 kb/s units, high bit = basic) of the Supported Rates
/// and Extended Supported Rates elements.
pub fn rates_of(ies_buf: &[u8]) -> Vec<u8> {
    let mut r = Vec::new();
    for (id, d) in ies(ies_buf) {
        if id == EID_SUPP_RATES || id == EID_EXT_SUPP_RATES {
            r.extend_from_slice(d);
        }
    }
    r
}

/// The primary channel's number from the DS Parameter Set or HT
/// Operation element.
pub fn channel_of(ies_buf: &[u8]) -> Option<u8> {
    find_ie(ies_buf, EID_DS_PARAMS).and_then(|d| d.first().copied()).or_else(|| find_ie(ies_buf, EID_HT_OPERATION).and_then(|d| d.first().copied()))
}

/// 802.11 channel number → centre frequency (MHz) on `band` (0 2.4 GHz,
/// 1 5 GHz, 3 6 GHz).
pub fn chan_to_freq(chan: u8, band: u8) -> u32 {
    let c = chan as u32;
    match band {
        0 if c == 14 => 2484,
        0 => 2407 + 5 * c,
        3 if c == 2 => 5935,
        3 => 5950 + 5 * c,
        _ => 5000 + 5 * c,
    }
}

pub fn freq_to_chan(freq: u32) -> u8 {
    match freq {
        2484 => 14,
        2412..=2472 => ((freq - 2407) / 5) as u8,
        5935 => 2,
        5955..=7115 => ((freq - 5950) / 5) as u8,
        4910..=5885 => ((freq - 5000) / 5) as u8,
        _ => 0,
    }
}

pub fn band_of(freq: u32) -> u8 {
    match freq {
        0..=2500 => 0,
        5925.. => 3,
        _ => 1,
    }
}

/// Length of a data frame's MAC header (QoS control and HT control
/// included; never four-address frames from an AP to a station).
pub fn data_hdr_len(fc: u16) -> usize {
    let mut n = 24;
    if fc & (FCTL_TODS | FCTL_FROMDS) == FCTL_TODS | FCTL_FROMDS {
        n += 6;
    }
    if stype(fc) & STYPE_QOS != 0 {
        n += 2;
        if fc & FCTL_ORDER != 0 {
            n += 4;
        }
    }
    n
}

/// An Ethernet frame → a station's 802.11 data frame to the AP `bssid`
/// (To DS, RFC 1042 encapsulation). `qos` adds a QoS control field
/// (TID from the IP precedence is left 0, best effort).
pub fn eth_to_80211(eth: &[u8], bssid: [u8; 6], own: [u8; 6], qos: bool) -> Option<Vec<u8>> {
    if eth.len() < 14 {
        return None;
    }
    let mut f = Vec::with_capacity(eth.len() + 34);
    let fc = FTYPE_DATA | if qos { STYPE_QOS } else { 0 } | FCTL_TODS;
    f.extend_from_slice(&fc.to_le_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&bssid);
    f.extend_from_slice(&own);
    f.extend_from_slice(&eth[0..6]);
    f.extend_from_slice(&[0, 0]);
    if qos {
        f.extend_from_slice(&[0, 0]);
    }
    f.extend_from_slice(&RFC1042);
    f.extend_from_slice(&eth[12..]);
    Some(f)
}

/// A received data frame (decrypted, no security header) → Ethernet
/// frames (several for an A-MSDU).
pub fn data_to_eth(f: &[u8]) -> Vec<Vec<u8>> {
    let fc = fc(f);
    let hl = data_hdr_len(fc);
    let mut out = Vec::new();
    if f.len() < hl || stype(fc) & STYPE_NODATA != 0 {
        return out;
    }
    let (da, sa) = match fc & (FCTL_TODS | FCTL_FROMDS) {
        FCTL_FROMDS => (addr(f, 1), addr(f, 3)),
        FCTL_TODS => (addr(f, 3), addr(f, 2)),
        0 => (addr(f, 1), addr(f, 2)),
        _ => (addr(f, 3), f[24..30].try_into().unwrap()),
    };
    let amsdu = stype(fc) & STYPE_QOS != 0 && f[hl - 2 - if fc & FCTL_ORDER != 0 { 4 } else { 0 }] & 0x80 != 0;
    let body = &f[hl..];
    if !amsdu {
        if let Some(e) = llc_to_eth(da, sa, body) {
            out.push(e);
        }
        return out;
    }
    let mut i = 0;
    while i + 14 <= body.len() {
        let sda: [u8; 6] = body[i..i + 6].try_into().unwrap();
        let ssa: [u8; 6] = body[i + 6..i + 12].try_into().unwrap();
        let len = u16::from_be_bytes([body[i + 12], body[i + 13]]) as usize;
        if i + 14 + len > body.len() {
            break;
        }
        if let Some(e) = llc_to_eth(sda, ssa, &body[i + 14..i + 14 + len]) {
            out.push(e);
        }
        i = (i + 14 + len + 3) & !3;
    }
    out
}

fn llc_to_eth(da: [u8; 6], sa: [u8; 6], p: &[u8]) -> Option<Vec<u8>> {
    if p.len() < 8 || p[..6] != RFC1042 && p[..6] != [0xaa, 0xaa, 0x03, 0x00, 0x00, 0xf8] {
        return None;
    }
    let mut e = Vec::with_capacity(p.len() + 6);
    e.extend_from_slice(&da);
    e.extend_from_slice(&sa);
    e.extend_from_slice(&p[6..]);
    Some(e)
}

/// Ethertype of an Ethernet frame.
pub fn ethertype(eth: &[u8]) -> u16 {
    if eth.len() < 14 { 0 } else { u16::from_be_bytes([eth[12], eth[13]]) }
}
