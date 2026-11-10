//! nl80211, the generic netlink family of Wi-Fi configuration: the
//! subset that wpa_supplicant (nl80211 driver, its own SME), wificond and
//! `iw` use for a station interface.
//!
//! - wiphy and interface queries: GET_PROTOCOL_FEATURES, GET_WIPHY
//!   (whole, or split dumps), GET_INTERFACE, SET_INTERFACE (station only),
//!   GET_REG;
//! - scanning: TRIGGER_SCAN, ABORT_SCAN, GET_SCAN; events TRIGGER_SCAN,
//!   NEW_SCAN_RESULTS, SCAN_ABORTED on the "scan" group;
//! - connecting: AUTHENTICATE, ASSOCIATE, DEAUTHENTICATE, DISASSOCIATE,
//!   DISCONNECT, NEW_KEY / SET_KEY / DEL_KEY, SET_STATION (authorized),
//!   GET_STATION; events AUTHENTICATE, ASSOCIATE, CONNECT,
//!   DEAUTHENTICATE, DISASSOCIATE, DISCONNECT on the "mlme" group;
//! - management frames: REGISTER_FRAME, FRAME (+ FRAME_TX_STATUS),
//!   received ones unicast as FRAME to the registering socket;
//! - accepted without effect: SET_POWER_SAVE, SET_CQM, SET_WIPHY,
//!   SET_TX_BITRATE_MASK, REQ_SET_REG, SET_QOS_MAP, SET_BSS,
//!   SET_MULTICAST_TO_UNICAST; everything else is EOPNOTSUPP (scheduled
//!   scans, remain-on-channel, AP mode, P2P, vendor commands, ...).

use alloc::vec::Vec;

use super::{Key, ScanReq, Wiphy, ieee80211 as w};
use crate::errno::*;
use crate::net::genl::{self, Answer, Family, Nla, Req, as_u16, as_u32, attrs, find};

pub const FAMILY_ID: u16 = 0x1d;

const GRP_CONFIG: u32 = 2;
const GRP_SCAN: u32 = 3;
const GRP_REGULATORY: u32 = 4;
const GRP_MLME: u32 = 5;
const GRP_VENDOR: u32 = 6;
const GRP_NAN: u32 = 7;
const GRP_TESTMODE: u32 = 8;

pub static FAMILY: Family = Family {
    name: "nl80211",
    id: FAMILY_ID,
    version: 1,
    maxattr: 341,
    ops: SUPPORTED_CMDS,
    groups: &[
        ("config", GRP_CONFIG),
        ("scan", GRP_SCAN),
        ("regulatory", GRP_REGULATORY),
        ("mlme", GRP_MLME),
        ("vendor", GRP_VENDOR),
        ("nan", GRP_NAN),
        ("testmode", GRP_TESTMODE),
    ],
    handler: request,
};

// Commands.
pub const CMD_GET_WIPHY: u8 = 1;
pub const CMD_SET_WIPHY: u8 = 2;
pub const CMD_NEW_WIPHY: u8 = 3;
pub const CMD_GET_INTERFACE: u8 = 5;
pub const CMD_SET_INTERFACE: u8 = 6;
pub const CMD_NEW_INTERFACE: u8 = 7;
pub const CMD_GET_KEY: u8 = 9;
pub const CMD_SET_KEY: u8 = 10;
pub const CMD_NEW_KEY: u8 = 11;
pub const CMD_DEL_KEY: u8 = 12;
pub const CMD_GET_STATION: u8 = 17;
pub const CMD_SET_STATION: u8 = 18;
pub const CMD_NEW_STATION: u8 = 19;
pub const CMD_SET_BSS: u8 = 25;
pub const CMD_REQ_SET_REG: u8 = 27;
pub const CMD_GET_REG: u8 = 31;
pub const CMD_GET_SCAN: u8 = 32;
pub const CMD_TRIGGER_SCAN: u8 = 33;
pub const CMD_NEW_SCAN_RESULTS: u8 = 34;
pub const CMD_SCAN_ABORTED: u8 = 35;
pub const CMD_AUTHENTICATE: u8 = 37;
pub const CMD_ASSOCIATE: u8 = 38;
pub const CMD_DEAUTHENTICATE: u8 = 39;
pub const CMD_DISASSOCIATE: u8 = 40;
pub const CMD_CONNECT: u8 = 46;
pub const CMD_DISCONNECT: u8 = 48;
pub const CMD_GET_SURVEY: u8 = 50;
pub const CMD_SET_TX_BITRATE_MASK: u8 = 57;
pub const CMD_REGISTER_FRAME: u8 = 58;
pub const CMD_FRAME: u8 = 59;
pub const CMD_FRAME_TX_STATUS: u8 = 60;
pub const CMD_SET_POWER_SAVE: u8 = 61;
pub const CMD_GET_POWER_SAVE: u8 = 62;
pub const CMD_SET_CQM: u8 = 63;
pub const CMD_FRAME_WAIT_CANCEL: u8 = 67;
pub const CMD_GET_PROTOCOL_FEATURES: u8 = 95;
pub const CMD_SET_QOS_MAP: u8 = 104;
pub const CMD_ABORT_SCAN: u8 = 114;
pub const CMD_SET_MULTICAST_TO_UNICAST: u8 = 121;

const SUPPORTED_CMDS: &[u8] = &[
    CMD_GET_WIPHY,
    CMD_SET_WIPHY,
    CMD_GET_INTERFACE,
    CMD_SET_INTERFACE,
    CMD_NEW_KEY,
    CMD_SET_KEY,
    CMD_DEL_KEY,
    CMD_GET_STATION,
    CMD_SET_STATION,
    CMD_SET_BSS,
    CMD_REQ_SET_REG,
    CMD_GET_REG,
    CMD_GET_SCAN,
    CMD_TRIGGER_SCAN,
    CMD_AUTHENTICATE,
    CMD_ASSOCIATE,
    CMD_DEAUTHENTICATE,
    CMD_DISASSOCIATE,
    CMD_DISCONNECT,
    CMD_GET_SURVEY,
    CMD_SET_TX_BITRATE_MASK,
    CMD_REGISTER_FRAME,
    CMD_FRAME,
    CMD_SET_POWER_SAVE,
    CMD_GET_POWER_SAVE,
    CMD_SET_CQM,
    CMD_FRAME_WAIT_CANCEL,
    CMD_GET_PROTOCOL_FEATURES,
    CMD_SET_QOS_MAP,
    CMD_ABORT_SCAN,
    CMD_SET_MULTICAST_TO_UNICAST,
];

// Attributes.
const ATTR_WIPHY: u16 = 1;
const ATTR_WIPHY_NAME: u16 = 2;
const ATTR_IFINDEX: u16 = 3;
const ATTR_IFNAME: u16 = 4;
const ATTR_IFTYPE: u16 = 5;
const ATTR_MAC: u16 = 6;
const ATTR_KEY_DATA: u16 = 7;
const ATTR_KEY_IDX: u16 = 8;
const ATTR_KEY_CIPHER: u16 = 9;
const ATTR_KEY_SEQ: u16 = 10;
const ATTR_STA_FLAGS: u16 = 17;
const ATTR_STA_INFO: u16 = 21;
const ATTR_WIPHY_BANDS: u16 = 22;
const ATTR_SUPPORTED_IFTYPES: u16 = 32;
const ATTR_REG_ALPHA2: u16 = 33;
const ATTR_REG_RULES: u16 = 34;
const ATTR_WIPHY_FREQ: u16 = 38;
const ATTR_IE: u16 = 42;
const ATTR_MAX_NUM_SCAN_SSIDS: u16 = 43;
const ATTR_SCAN_FREQUENCIES: u16 = 44;
const ATTR_SCAN_SSIDS: u16 = 45;
const ATTR_GENERATION: u16 = 46;
const ATTR_BSS: u16 = 47;
const ATTR_SUPPORTED_COMMANDS: u16 = 50;
const ATTR_FRAME: u16 = 51;
const ATTR_SSID: u16 = 52;
const ATTR_AUTH_TYPE: u16 = 53;
const ATTR_REASON_CODE: u16 = 54;
const ATTR_KEY_TYPE: u16 = 55;
const ATTR_MAX_SCAN_IE_LEN: u16 = 56;
const ATTR_CIPHER_SUITES: u16 = 57;
const ATTR_WIPHY_RETRY_SHORT: u16 = 61;
const ATTR_WIPHY_RETRY_LONG: u16 = 62;
const ATTR_WIPHY_FRAG_THRESHOLD: u16 = 63;
const ATTR_WIPHY_RTS_THRESHOLD: u16 = 64;
const ATTR_TIMED_OUT: u16 = 65;
const ATTR_STA_FLAGS2: u16 = 67;
const ATTR_DISCONNECTED_BY_AP: u16 = 71;
const ATTR_STATUS_CODE: u16 = 72;
const ATTR_REQ_IE: u16 = 77;
const ATTR_RESP_IE: u16 = 78;
const ATTR_PREV_BSSID: u16 = 79;
const ATTR_KEY: u16 = 80;
const ATTR_COOKIE: u16 = 88;
const ATTR_WIPHY_COVERAGE_CLASS: u16 = 89;
const ATTR_FRAME_MATCH: u16 = 91;
const ATTR_ACK: u16 = 92;
const ATTR_PS_STATE: u16 = 93;
const ATTR_LOCAL_STATE_CHANGE: u16 = 95;
const ATTR_TX_FRAME_TYPES: u16 = 99;
const ATTR_RX_FRAME_TYPES: u16 = 100;
const ATTR_FRAME_TYPE: u16 = 101;
const ATTR_INTERFACE_COMBINATIONS: u16 = 120;
const ATTR_SOFTWARE_IFTYPES: u16 = 121;
const ATTR_MAX_NUM_SCHED_SCAN_SSIDS: u16 = 123;
const ATTR_MAX_SCHED_SCAN_IE_LEN: u16 = 124;
const ATTR_MAX_MATCH_SETS: u16 = 133;
const ATTR_FEATURE_FLAGS: u16 = 143;
const ATTR_DFS_REGION: u16 = 146;
const ATTR_RX_SIGNAL_DBM: u16 = 151;
const ATTR_WDEV: u16 = 153;
const ATTR_AUTH_DATA: u16 = 156;
const ATTR_SCAN_FLAGS: u16 = 158;
const ATTR_CHANNEL_WIDTH: u16 = 159;
const ATTR_CENTER_FREQ1: u16 = 160;
const ATTR_PROTOCOL_FEATURES: u16 = 173;
const ATTR_SPLIT_WIPHY_DUMP: u16 = 174;
const ATTR_MAC_MASK: u16 = 215;
const ATTR_EXT_FEATURES: u16 = 217;
const ATTR_MAX_NUM_AKM_SUITES: u16 = 316;

const KEY_DATA: u16 = 1;
const KEY_IDX: u16 = 2;
const KEY_CIPHER: u16 = 3;
const KEY_SEQ: u16 = 4;
const KEY_TYPE: u16 = 7;

pub const IFTYPE_STATION: u32 = 2;

pub const AUTHTYPE_OPEN_SYSTEM: u32 = 0;
pub const AUTHTYPE_SHARED_KEY: u32 = 1;
pub const AUTHTYPE_SAE: u32 = 4;

pub const SCAN_FLAG_RANDOM_ADDR: u32 = 1 << 3;

const BAND_ATTR_FREQS: u16 = 1;
const BAND_ATTR_RATES: u16 = 2;
const BAND_ATTR_HT_MCS_SET: u16 = 3;
const BAND_ATTR_HT_CAPA: u16 = 4;
const BAND_ATTR_HT_AMPDU_FACTOR: u16 = 5;
const BAND_ATTR_HT_AMPDU_DENSITY: u16 = 6;
const BAND_ATTR_VHT_MCS_SET: u16 = 7;
const BAND_ATTR_VHT_CAPA: u16 = 8;
const FREQ_ATTR_FREQ: u16 = 1;
const FREQ_ATTR_DISABLED: u16 = 2;
const FREQ_ATTR_NO_IR: u16 = 3;
const FREQ_ATTR_RADAR: u16 = 5;
const FREQ_ATTR_MAX_TX_POWER: u16 = 6;
const BITRATE_ATTR_RATE: u16 = 1;

const BSS_BSSID: u16 = 1;
const BSS_FREQUENCY: u16 = 2;
const BSS_TSF: u16 = 3;
const BSS_BEACON_INTERVAL: u16 = 4;
const BSS_CAPABILITY: u16 = 5;
const BSS_INFORMATION_ELEMENTS: u16 = 6;
const BSS_SIGNAL_MBM: u16 = 7;
const BSS_STATUS: u16 = 9;
const BSS_SEEN_MS_AGO: u16 = 10;
const BSS_BEACON_IES: u16 = 11;
const BSS_CHAN_WIDTH: u16 = 12;
const BSS_LAST_SEEN_BOOTTIME: u16 = 15;

const STA_INFO_INACTIVE_TIME: u16 = 1;
const STA_INFO_RX_BYTES: u16 = 2;
const STA_INFO_TX_BYTES: u16 = 3;
const STA_INFO_SIGNAL: u16 = 7;
const STA_INFO_TX_BITRATE: u16 = 8;
const STA_INFO_RX_PACKETS: u16 = 9;
const STA_INFO_TX_PACKETS: u16 = 10;
const STA_INFO_TX_RETRIES: u16 = 11;
const STA_INFO_TX_FAILED: u16 = 12;
const STA_INFO_SIGNAL_AVG: u16 = 13;
const STA_INFO_RX_BITRATE: u16 = 14;
const STA_INFO_CONNECTED_TIME: u16 = 16;
const STA_INFO_STA_FLAGS: u16 = 17;
const STA_INFO_RX_BYTES64: u16 = 23;
const STA_INFO_TX_BYTES64: u16 = 24;
const RATE_INFO_BITRATE: u16 = 1;
const RATE_INFO_BITRATE32: u16 = 5;

const STA_FLAG_AUTHORIZED: u32 = 1;
const STA_FLAG_AUTHENTICATED: u32 = 5;
const STA_FLAG_ASSOCIATED: u32 = 7;

const IFACE_LIMIT_MAX: u16 = 1;
const IFACE_LIMIT_TYPES: u16 = 2;
const IFACE_COMB_LIMITS: u16 = 1;
const IFACE_COMB_MAXNUM: u16 = 2;
const IFACE_COMB_NUM_CHANNELS: u16 = 4;

const ATTR_REG_RULE_FLAGS: u16 = 1;
const ATTR_FREQ_RANGE_START: u16 = 2;
const ATTR_FREQ_RANGE_END: u16 = 3;
const ATTR_FREQ_RANGE_MAX_BW: u16 = 4;
const ATTR_POWER_RULE_MAX_ANT_GAIN: u16 = 5;
const ATTR_POWER_RULE_MAX_EIRP: u16 = 6;

const FEATURE_SAE: u32 = 1 << 5;
const FEATURE_SCAN_RANDOM_MAC_ADDR: u32 = 1 << 29;
const EXT_FEATURE_MFP_OPTIONAL: usize = 21;

fn request(r: &Req) -> KResult<Answer> {
    match r.cmd {
        CMD_GET_PROTOCOL_FEATURES => {
            let mut a = Nla::new();
            a.u32(ATTR_PROTOCOL_FEATURES, 1);
            Ok(Answer::One(CMD_GET_PROTOCOL_FEATURES, a.0))
        }
        CMD_GET_WIPHY => get_wiphy(r),
        CMD_GET_INTERFACE => get_interface(r),
        CMD_GET_REG => Ok(Answer::One(CMD_GET_REG, reg_attrs(None))),
        CMD_GET_SCAN => get_scan(r),
        CMD_GET_STATION => get_station(r),
        CMD_GET_SURVEY => Ok(Answer::Dump(Vec::new())),
        CMD_GET_POWER_SAVE => {
            let mut a = Nla::new();
            a.u32(ATTR_PS_STATE, 0);
            Ok(Answer::One(CMD_GET_POWER_SAVE, a.0))
        }
        CMD_SET_INTERFACE => {
            let phy = wiphy_of(r)?;
            match r.u32(ATTR_IFTYPE) {
                None | Some(IFTYPE_STATION) => {
                    let _ = phy;
                    Ok(Answer::None)
                }
                _ => Err(EOPNOTSUPP),
            }
        }
        CMD_TRIGGER_SCAN => trigger_scan(r),
        CMD_ABORT_SCAN => on_phy(r, |w| {
            w.abort_scan();
            Ok(())
        }),
        CMD_AUTHENTICATE => {
            let bssid = mac_attr(r, ATTR_MAC)?;
            let freq = r.u32(ATTR_WIPHY_FREQ).unwrap_or(0);
            let auth = r.u32(ATTR_AUTH_TYPE).ok_or(EINVAL)?;
            let data = r.get(ATTR_AUTH_DATA).unwrap_or(&[]).to_vec();
            let ies = r.get(ATTR_IE).unwrap_or(&[]).to_vec();
            if r.has(ATTR_LOCAL_STATE_CHANGE) {
                return Ok(Answer::None);
            }
            on_phy(r, |w| w.authenticate(bssid, freq, auth, &data, &ies))
        }
        CMD_ASSOCIATE => {
            let bssid = mac_attr(r, ATTR_MAC)?;
            let ssid = r.get(ATTR_SSID).ok_or(EINVAL)?.to_vec();
            let ies = r.get(ATTR_IE).unwrap_or(&[]).to_vec();
            let prev = mac_attr(r, ATTR_PREV_BSSID).ok();
            on_phy(r, |w| w.associate(bssid, &ssid, &ies, prev))
        }
        CMD_DEAUTHENTICATE | CMD_DISASSOCIATE => {
            let bssid = mac_attr(r, ATTR_MAC).ok();
            let reason = r.get(ATTR_REASON_CODE).and_then(as_u16).unwrap_or(3);
            let local = r.has(ATTR_LOCAL_STATE_CHANGE);
            let dis = r.cmd == CMD_DISASSOCIATE;
            on_phy(r, |w| w.deauthenticate(bssid, reason, dis, local))
        }
        CMD_DISCONNECT => {
            let reason = r.get(ATTR_REASON_CODE).and_then(as_u16).unwrap_or(3);
            on_phy(r, |w| w.deauthenticate(None, reason, false, false))
        }
        CMD_NEW_KEY | CMD_DEL_KEY => {
            let k = parse_key(r)?;
            if r.cmd == CMD_DEL_KEY {
                on_phy(r, |w| {
                    w.del_key(k);
                    Ok(())
                })
            } else {
                on_phy(r, |w| w.new_key(k))
            }
        }
        CMD_SET_KEY => Ok(Answer::None),
        CMD_SET_STATION => {
            let mac = mac_attr(r, ATTR_MAC)?;
            let Some(f) = r.get(ATTR_STA_FLAGS2) else { return Ok(Answer::None) };
            if f.len() < 8 {
                return Err(EINVAL);
            }
            let mask = as_u32(f).unwrap();
            let set = as_u32(&f[4..]).unwrap();
            if mask & (1 << STA_FLAG_AUTHORIZED) == 0 {
                return Ok(Answer::None);
            }
            on_phy(r, |w| w.set_authorized(mac, set & (1 << STA_FLAG_AUTHORIZED) != 0))
        }
        CMD_REGISTER_FRAME => {
            let ftype = r.get(ATTR_FRAME_TYPE).and_then(as_u16).unwrap_or(w::STYPE_ACTION);
            let pattern = r.get(ATTR_FRAME_MATCH).unwrap_or(&[]).to_vec();
            let port = r.portid;
            on_phy(r, |w| w.register_frame(port, ftype, &pattern))
        }
        CMD_FRAME => {
            let f = r.get(ATTR_FRAME).ok_or(EINVAL)?.to_vec();
            let idx = wiphy_of(r)?;
            let cookie = super::with_wiphy(idx, |w| w.tx_frame(&f)).ok_or(ENODEV)??;
            let mut a = Nla::new();
            a.u64(ATTR_COOKIE, cookie);
            Ok(Answer::One(CMD_FRAME, a.0))
        }
        CMD_SET_WIPHY | CMD_SET_POWER_SAVE | CMD_SET_CQM | CMD_SET_TX_BITRATE_MASK | CMD_REQ_SET_REG | CMD_SET_QOS_MAP
        | CMD_SET_BSS | CMD_SET_MULTICAST_TO_UNICAST | CMD_FRAME_WAIT_CANCEL => Ok(Answer::None),
        _ => Err(EOPNOTSUPP),
    }
}

fn mac_attr(r: &Req, ty: u16) -> KResult<[u8; 6]> {
    r.get(ty).and_then(|d| d.get(..6)).map(|d| d.try_into().unwrap()).ok_or(EINVAL)
}

/// The wiphy a request is about (IFINDEX, WDEV or WIPHY).
fn wiphy_of(r: &Req) -> KResult<u32> {
    if let Some(i) = r.u32(ATTR_IFINDEX) {
        return super::by_ifindex(i).ok_or(ENODEV);
    }
    if let Some(d) = r.get(ATTR_WDEV).and_then(genl::as_u64) {
        let p = (d >> 32) as u32;
        return if p < super::count() { Ok(p) } else { Err(ENODEV) };
    }
    if let Some(p) = r.u32(ATTR_WIPHY) {
        return if p < super::count() { Ok(p) } else { Err(ENODEV) };
    }
    Err(EINVAL)
}

fn on_phy(r: &Req, f: impl FnOnce(&mut Wiphy) -> KResult<()>) -> KResult<Answer> {
    let idx = wiphy_of(r)?;
    super::with_wiphy(idx, f).ok_or(ENODEV)??;
    Ok(Answer::None)
}

fn parse_key(r: &Req) -> KResult<Key> {
    // Newer wpa_supplicant nests the key in NL80211_ATTR_KEY.
    let (data, idx, cipher, seq, ty) = match r.get(ATTR_KEY) {
        Some(k) => (find(k, KEY_DATA), find(k, KEY_IDX), find(k, KEY_CIPHER), find(k, KEY_SEQ), find(k, KEY_TYPE)),
        None => (r.get(ATTR_KEY_DATA), r.get(ATTR_KEY_IDX), r.get(ATTR_KEY_CIPHER), r.get(ATTR_KEY_SEQ), r.get(ATTR_KEY_TYPE)),
    };
    let mac = mac_attr(r, ATTR_MAC).ok();
    let pairwise = match ty.and_then(as_u32) {
        Some(t) => t == 1,
        None => mac.is_some_and(|m| m[0] & 1 == 0),
    };
    Ok(Key {
        cipher: cipher.and_then(as_u32).unwrap_or(0),
        idx: idx.and_then(|d| d.first().copied()).unwrap_or(0),
        data: data.unwrap_or(&[]).to_vec(),
        seq: seq.unwrap_or(&[]).to_vec(),
        pairwise,
        addr: mac.unwrap_or([0xff; 6]),
    })
}

/// IFINDEX, WIPHY, WDEV of an event.
fn base(w: &Wiphy) -> Nla {
    let mut a = Nla::new();
    a.u32(ATTR_WIPHY, w.idx);
    a.u32(ATTR_IFINDEX, w.ifindex);
    a.u64(ATTR_WDEV, w.wdev());
    a
}

// ---- wiphy / interface / regulatory ----

fn wiphy_common(w: &Wiphy, a: &mut Nla) {
    a.u32(ATTR_WIPHY, w.idx);
    a.str(ATTR_WIPHY_NAME, &w.name);
    a.u32(ATTR_GENERATION, 1);
}

/// The parts of a wiphy description: the split dump sends each in its
/// own message, GET_WIPHY without the split flag all in one.
fn wiphy_parts(w: &Wiphy) -> Vec<Vec<u8>> {
    let c = w.caps();
    let mut parts = Vec::new();
    let mut a = Nla::new();
    a.u8(ATTR_WIPHY_RETRY_SHORT, 7);
    a.u8(ATTR_WIPHY_RETRY_LONG, 4);
    a.u32(ATTR_WIPHY_FRAG_THRESHOLD, u32::MAX);
    a.u32(ATTR_WIPHY_RTS_THRESHOLD, u32::MAX);
    a.u8(ATTR_WIPHY_COVERAGE_CLASS, 0);
    a.u8(ATTR_MAX_NUM_SCAN_SSIDS, c.max_scan_ssids);
    a.u8(ATTR_MAX_NUM_SCHED_SCAN_SSIDS, 0);
    a.u16(ATTR_MAX_SCAN_IE_LEN, c.max_scan_ie_len);
    a.u16(ATTR_MAX_SCHED_SCAN_IE_LEN, 0);
    a.u8(ATTR_MAX_MATCH_SETS, 0);
    let mut cs = Vec::new();
    for x in &c.ciphers {
        cs.extend_from_slice(&x.to_le_bytes());
    }
    a.put(ATTR_CIPHER_SUITES, &cs);
    let n = a.nest(ATTR_SUPPORTED_IFTYPES);
    a.flag(IFTYPE_STATION as u16);
    a.end(n);
    let n = a.nest(ATTR_SOFTWARE_IFTYPES);
    a.end(n);
    let mut feat = 0;
    if c.sae {
        feat |= FEATURE_SAE;
    }
    if c.random_scan_mac {
        feat |= FEATURE_SCAN_RANDOM_MAC_ADDR;
    }
    a.u32(ATTR_FEATURE_FLAGS, feat);
    let mut ext = [0u8; 9];
    ext[EXT_FEATURE_MFP_OPTIONAL / 8] |= 1 << (EXT_FEATURE_MFP_OPTIONAL % 8);
    a.put(ATTR_EXT_FEATURES, &ext);
    a.u16(ATTR_MAX_NUM_AKM_SUITES, 2);
    parts.push(a.0);
    for b in &c.bands {
        let mut a = Nla::new();
        let bands = a.nest(ATTR_WIPHY_BANDS);
        let bn = a.nest(b.band as u16);
        if let Some(ht) = &b.ht {
            a.put(BAND_ATTR_HT_MCS_SET, &ht.mcs);
            a.u16(BAND_ATTR_HT_CAPA, ht.cap);
            a.u8(BAND_ATTR_HT_AMPDU_FACTOR, ht.ampdu_factor);
            a.u8(BAND_ATTR_HT_AMPDU_DENSITY, ht.ampdu_density);
        }
        if let Some(v) = &b.vht {
            a.put(BAND_ATTR_VHT_MCS_SET, &v.mcs);
            a.u32(BAND_ATTR_VHT_CAPA, v.cap);
        }
        let fr = a.nest(BAND_ATTR_FREQS);
        for (i, ch) in b.channels.iter().enumerate() {
            let e = a.nest(i as u16);
            a.u32(FREQ_ATTR_FREQ, ch.freq);
            if ch.disabled {
                a.flag(FREQ_ATTR_DISABLED);
            }
            if ch.no_ir {
                a.flag(FREQ_ATTR_NO_IR);
            }
            if ch.radar {
                a.flag(FREQ_ATTR_RADAR);
            }
            a.u32(FREQ_ATTR_MAX_TX_POWER, ch.max_power);
            a.end(e);
        }
        a.end(fr);
        let rt = a.nest(BAND_ATTR_RATES);
        for (i, &r) in b.rates.iter().enumerate() {
            let e = a.nest(i as u16);
            a.u32(BITRATE_ATTR_RATE, r as u32);
            a.end(e);
        }
        a.end(rt);
        a.end(bn);
        a.end(bands);
        parts.push(a.0);
    }
    let mut a = Nla::new();
    let n = a.nest(ATTR_SUPPORTED_COMMANDS);
    for (i, &cmd) in SUPPORTED_CMDS.iter().enumerate() {
        a.u32(i as u16 + 1, cmd as u32);
    }
    a.end(n);
    for ty in [ATTR_TX_FRAME_TYPES, ATTR_RX_FRAME_TYPES] {
        let n = a.nest(ty);
        let s = a.nest(IFTYPE_STATION as u16);
        let subtypes: &[u16] = if ty == ATTR_TX_FRAME_TYPES {
            &[0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0]
        } else {
            &[w::STYPE_ACTION, w::STYPE_PROBE_REQ]
        };
        for &st in subtypes {
            a.u16(ATTR_FRAME_TYPE, w::FTYPE_MGMT | st);
        }
        a.end(s);
        a.end(n);
    }
    let n = a.nest(ATTR_INTERFACE_COMBINATIONS);
    let c1 = a.nest(1);
    let l = a.nest(IFACE_COMB_LIMITS);
    let l1 = a.nest(1);
    a.u32(IFACE_LIMIT_MAX, 1);
    let t = a.nest(IFACE_LIMIT_TYPES);
    a.flag(IFTYPE_STATION as u16);
    a.end(t);
    a.end(l1);
    a.end(l);
    a.u32(IFACE_COMB_MAXNUM, 1);
    a.u32(IFACE_COMB_NUM_CHANNELS, 1);
    a.end(c1);
    a.end(n);
    parts.push(a.0);
    parts
}

fn get_wiphy(r: &Req) -> KResult<Answer> {
    let want = wiphy_of(r).ok();
    let split = r.has(ATTR_SPLIT_WIPHY_DUMP);
    let msgs = super::for_each(|w| {
        if want.is_some_and(|i| i != w.idx) {
            return Vec::new();
        }
        let parts = wiphy_parts(w);
        if split && r.dump() {
            parts
                .into_iter()
                .map(|p| {
                    let mut a = Nla::new();
                    wiphy_common(w, &mut a);
                    a.0.extend_from_slice(&p);
                    (CMD_NEW_WIPHY, a.0)
                })
                .collect()
        } else {
            let mut a = Nla::new();
            wiphy_common(w, &mut a);
            for p in parts {
                a.0.extend_from_slice(&p);
            }
            alloc::vec![(CMD_NEW_WIPHY, a.0)]
        }
    });
    let mut all: Vec<(u8, Vec<u8>)> = msgs.into_iter().flatten().collect();
    if r.dump() {
        Ok(Answer::Dump(all))
    } else {
        if all.is_empty() {
            return Err(ENODEV);
        }
        let (c, a) = all.swap_remove(0);
        Ok(Answer::One(c, a))
    }
}

fn interface_attrs(w: &Wiphy) -> Vec<u8> {
    let mut a = base(w);
    a.str(ATTR_IFNAME, &w.ifname);
    a.u32(ATTR_IFTYPE, w.iftype);
    a.put(ATTR_MAC, &w.mac);
    a.u32(ATTR_GENERATION, 1);
    if let Some((_, freq, ssid)) = w.link_info() {
        a.put(ATTR_SSID, &ssid);
        a.u32(ATTR_WIPHY_FREQ, freq);
        a.u32(ATTR_CHANNEL_WIDTH, 1);
        a.u32(ATTR_CENTER_FREQ1, freq);
    }
    a.0
}

fn get_interface(r: &Req) -> KResult<Answer> {
    let want = wiphy_of(r).ok();
    if want.is_none() && !r.dump() {
        return Err(EINVAL);
    }
    let mut list: Vec<(u8, Vec<u8>)> = super::for_each(|w| (w.idx, interface_attrs(w)))
        .into_iter()
        .filter(|(i, _)| want.is_none_or(|x| x == *i))
        .map(|(_, a)| (CMD_NEW_INTERFACE, a))
        .collect();
    if r.dump() {
        return Ok(Answer::Dump(list));
    }
    if list.is_empty() {
        return Err(ENODEV);
    }
    let (c, a) = list.swap_remove(0);
    Ok(Answer::One(c, a))
}

/// The world regulatory domain ("00").
fn reg_attrs(_w: Option<&Wiphy>) -> Vec<u8> {
    let mut a = Nla::new();
    a.str(ATTR_REG_ALPHA2, "00");
    a.u8(ATTR_DFS_REGION, 0);
    let n = a.nest(ATTR_REG_RULES);
    for (i, &(lo, hi, bw, flags)) in [(2402_000u32, 2482_000u32, 40_000u32, 0u32), (5170_000, 5835_000, 160_000, 0)].iter().enumerate() {
        let e = a.nest(i as u16);
        a.u32(ATTR_REG_RULE_FLAGS, flags);
        a.u32(ATTR_FREQ_RANGE_START, lo);
        a.u32(ATTR_FREQ_RANGE_END, hi);
        a.u32(ATTR_FREQ_RANGE_MAX_BW, bw);
        a.u32(ATTR_POWER_RULE_MAX_ANT_GAIN, 0);
        a.u32(ATTR_POWER_RULE_MAX_EIRP, 2000);
        a.end(e);
    }
    a.end(n);
    a.0
}

// ---- scanning ----

fn trigger_scan(r: &Req) -> KResult<Answer> {
    let mut ssids = Vec::new();
    if let Some(n) = r.get(ATTR_SCAN_SSIDS) {
        for (_, s) in attrs(n) {
            ssids.push(s.to_vec());
        }
    }
    let mut freqs = Vec::new();
    if let Some(n) = r.get(ATTR_SCAN_FREQUENCIES) {
        for (_, f) in attrs(n) {
            if let Some(v) = as_u32(f) {
                freqs.push(v);
            }
        }
    }
    let flags = r.u32(ATTR_SCAN_FLAGS).unwrap_or(0);
    let ies = r.get(ATTR_IE).unwrap_or(&[]).to_vec();
    let mut mac = [0u8; 6];
    if flags & SCAN_FLAG_RANDOM_ADDR != 0 {
        crate::random::fill(&mut mac);
        if let (Ok(m), Ok(mask)) = (mac_attr(r, ATTR_MAC), mac_attr(r, ATTR_MAC_MASK)) {
            for i in 0..6 {
                mac[i] = (mac[i] & !mask[i]) | (m[i] & mask[i]);
            }
        }
        mac[0] = (mac[0] & 0xfe) | 0x02;
    }
    on_phy(r, |w| w.start_scan(ScanReq { ssids, freqs, ies, mac }, flags))
}

pub fn event_scan(w: &Wiphy, cmd: u8, ssids: &[Vec<u8>], freqs: &[u32], flags: u32) {
    let mut a = base(w);
    a.u32(ATTR_GENERATION, w.generation);
    let n = a.nest(ATTR_SCAN_SSIDS);
    for (i, s) in ssids.iter().enumerate() {
        a.put(i as u16, s);
    }
    a.end(n);
    let n = a.nest(ATTR_SCAN_FREQUENCIES);
    for (i, f) in freqs.iter().enumerate() {
        a.u32(i as u16, *f);
    }
    a.end(n);
    if flags != 0 {
        a.u32(ATTR_SCAN_FLAGS, flags);
    }
    genl::multicast(&FAMILY, GRP_SCAN, cmd, &a.0);
}

fn get_scan(r: &Req) -> KResult<Answer> {
    let idx = wiphy_of(r)?;
    let now = super::now_ns();
    // CLOCK_BOOTTIME is the monotonic clock (no suspend).
    let boot = now;
    let list = super::with_wiphy(idx, |w| {
        w.bss
            .iter()
            .map(|b| {
                let mut a = base(w);
                a.u32(ATTR_GENERATION, w.generation);
                let n = a.nest(ATTR_BSS);
                a.put(BSS_BSSID, &b.bssid);
                a.u32(BSS_FREQUENCY, b.freq);
                a.u64(BSS_TSF, b.tsf);
                a.u16(BSS_BEACON_INTERVAL, b.beacon_int);
                a.u16(BSS_CAPABILITY, b.capability);
                a.put(BSS_INFORMATION_ELEMENTS, &b.ies);
                if let Some(bi) = &b.beacon_ies {
                    a.put(BSS_BEACON_IES, bi);
                }
                a.u32(BSS_SIGNAL_MBM, b.signal_mbm as u32);
                let age = now.saturating_sub(b.seen_ns);
                a.u32(BSS_SEEN_MS_AGO, (age / 1_000_000) as u32);
                a.u64(BSS_LAST_SEEN_BOOTTIME, boot.saturating_sub(age));
                a.u32(BSS_CHAN_WIDTH, 0);
                if let Some(s) = w.bss_status(&b.bssid) {
                    a.u32(BSS_STATUS, s);
                }
                a.end(n);
                (CMD_NEW_SCAN_RESULTS, a.0)
            })
            .collect()
    })
    .ok_or(ENODEV)?;
    Ok(Answer::Dump(list))
}

// ---- stations ----

fn rate_info(a: &mut Nla, ty: u16, rate: u32) {
    let n = a.nest(ty);
    a.u32(RATE_INFO_BITRATE32, rate);
    a.u16(RATE_INFO_BITRATE, rate.min(u16::MAX as u32) as u16);
    a.end(n);
}

fn get_station(r: &Req) -> KResult<Answer> {
    let idx = wiphy_of(r)?;
    let want = mac_attr(r, ATTR_MAC).ok();
    let msg = super::with_wiphy(idx, |w| {
        let s = w.station()?;
        if want.is_some_and(|m| m != s.bssid) {
            return None;
        }
        let mut a = base(w);
        a.put(ATTR_MAC, &s.bssid);
        a.u32(ATTR_GENERATION, 1);
        let n = a.nest(ATTR_STA_INFO);
        a.u32(STA_INFO_INACTIVE_TIME, s.inactive_ms);
        a.u32(STA_INFO_RX_BYTES, s.rx_bytes as u32);
        a.u32(STA_INFO_TX_BYTES, s.tx_bytes as u32);
        a.u64(STA_INFO_RX_BYTES64, s.rx_bytes);
        a.u64(STA_INFO_TX_BYTES64, s.tx_bytes);
        a.u32(STA_INFO_RX_PACKETS, s.rx_packets as u32);
        a.u32(STA_INFO_TX_PACKETS, s.tx_packets as u32);
        a.u32(STA_INFO_TX_RETRIES, s.tx_retries as u32);
        a.u32(STA_INFO_TX_FAILED, s.tx_failed as u32);
        a.u8(STA_INFO_SIGNAL, s.signal as u8);
        a.u8(STA_INFO_SIGNAL_AVG, s.signal as u8);
        rate_info(&mut a, STA_INFO_TX_BITRATE, s.tx_rate);
        rate_info(&mut a, STA_INFO_RX_BITRATE, s.tx_rate);
        a.u32(STA_INFO_CONNECTED_TIME, s.connected_s);
        let mask = (1 << STA_FLAG_AUTHORIZED) | (1 << STA_FLAG_AUTHENTICATED) | (1 << STA_FLAG_ASSOCIATED);
        let set = (1 << STA_FLAG_AUTHENTICATED) | (1 << STA_FLAG_ASSOCIATED) | if s.authorized { 1 << STA_FLAG_AUTHORIZED } else { 0 };
        let mut f = Vec::new();
        f.extend_from_slice(&(mask as u32).to_le_bytes());
        f.extend_from_slice(&(set as u32).to_le_bytes());
        a.put(STA_INFO_STA_FLAGS, &f);
        a.end(n);
        Some(a.0)
    })
    .ok_or(ENODEV)?;
    match (msg, r.dump()) {
        (Some(m), true) => Ok(Answer::Dump(alloc::vec![(CMD_NEW_STATION, m)])),
        (None, true) => Ok(Answer::Dump(Vec::new())),
        (Some(m), false) => Ok(Answer::One(CMD_NEW_STATION, m)),
        (None, false) => Err(ENOENT),
    }
}

// ---- MLME events ----

/// AUTHENTICATE / DEAUTHENTICATE / DISASSOCIATE with the frame.
pub fn event_frame(w: &Wiphy, cmd: u8, frame: &[u8]) {
    let mut a = base(w);
    a.put(ATTR_FRAME, frame);
    genl::multicast(&FAMILY, GRP_MLME, cmd, &a.0);
}

pub fn event_auth_timeout(w: &Wiphy, bssid: [u8; 6]) {
    let mut a = base(w);
    a.flag(ATTR_TIMED_OUT);
    a.put(ATTR_MAC, &bssid);
    genl::multicast(&FAMILY, GRP_MLME, CMD_AUTHENTICATE, &a.0);
}

pub fn event_assoc_timeout(w: &Wiphy, bssid: [u8; 6]) {
    let mut a = base(w);
    a.flag(ATTR_TIMED_OUT);
    a.put(ATTR_MAC, &bssid);
    genl::multicast(&FAMILY, GRP_MLME, CMD_ASSOCIATE, &a.0);
}

pub fn event_assoc(w: &Wiphy, frame: &[u8], req_ies: &[u8]) {
    let mut a = base(w);
    a.put(ATTR_FRAME, frame);
    a.put(ATTR_REQ_IE, req_ies);
    genl::multicast(&FAMILY, GRP_MLME, CMD_ASSOCIATE, &a.0);
}

pub fn event_connect(w: &Wiphy, bssid: [u8; 6], status: u16, req_ies: &[u8], resp_ies: &[u8]) {
    let mut a = base(w);
    a.put(ATTR_MAC, &bssid);
    a.u16(ATTR_STATUS_CODE, status);
    a.put(ATTR_REQ_IE, req_ies);
    a.put(ATTR_RESP_IE, resp_ies);
    genl::multicast(&FAMILY, GRP_MLME, CMD_CONNECT, &a.0);
}

pub fn event_disconnect(w: &Wiphy, reason: u16, by_ap: bool) {
    let mut a = base(w);
    a.u16(ATTR_REASON_CODE, reason);
    if by_ap {
        a.flag(ATTR_DISCONNECTED_BY_AP);
    }
    genl::multicast(&FAMILY, GRP_MLME, CMD_DISCONNECT, &a.0);
}

/// A management frame from FRAME went out (drivers report no acks: it
/// counts as acknowledged).
pub fn event_tx_status(w: &Wiphy, frame: &[u8], cookie: u64) {
    let mut a = base(w);
    a.put(ATTR_FRAME, frame);
    a.u64(ATTR_COOKIE, cookie);
    a.flag(ATTR_ACK);
    genl::multicast(&FAMILY, GRP_MLME, CMD_FRAME_TX_STATUS, &a.0);
}

/// A received management frame for the socket that registered for it;
/// false if that socket is gone.
pub fn event_rx_frame(w: &Wiphy, portid: u32, frame: &[u8], freq: u32, signal: i8) -> bool {
    let mut a = base(w);
    a.u32(ATTR_WIPHY_FREQ, freq);
    a.u32(ATTR_RX_SIGNAL_DBM, signal as i32 as u32);
    a.put(ATTR_FRAME, frame);
    genl::unicast(&FAMILY, portid, CMD_FRAME, &a.0)
}
