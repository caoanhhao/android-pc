//! The iwlwifi firmware file (`iwlwifi-<family>-<rf>-<api>.ucode`): a
//! TLV image. What the driver needs from it:
//! - the runtime image's sections (SEC_RT): LMAC sections, a separator
//!   (0xFFFFCCCC), UMAC sections, a separator (0xAAAABBBB), paging
//!   sections; each starts with its load address, which the context info
//!   does not carry (the ROM knows the layout);
//! - the image loader (IML) that the ROM runs first;
//! - capabilities and API flags, and the command version table
//!   (CMD_VERSIONS: group, command, command version, notification
//!   version), which decide the layouts of the commands.
//!
//! The PNVM file (`iwlwifi-<family>-<rf>.pnvm`) holds per-SKU platform
//! NVM sections; the one for the SKU id the firmware reports in ALIVE and
//! the device's MAC/RF type is loaded into DRAM for the firmware.

use alloc::vec::Vec;

const TLV_SEC_RT: u32 = 19;
const TLV_API_CHANGES_SET: u32 = 29;
const TLV_ENABLED_CAPABILITIES: u32 = 30;
const TLV_N_SCAN_CHANNELS: u32 = 31;
const TLV_FW_VERSION: u32 = 36;
const TLV_CMD_VERSIONS: u32 = 48;
const TLV_IML: u32 = 52;
const TLV_HW_TYPE: u32 = 58;
const TLV_PNVM_VERSION: u32 = 62;
const TLV_PNVM_SKU: u32 = 64;
const TLV_FW_NUM_STATIONS: u32 = 0x100;

const MAGIC: u32 = 0x0a4c_5749;
pub const SEPARATOR_CPU: u32 = 0xffff_cccc;
pub const SEPARATOR_PAGING: u32 = 0xaaaa_bbbb;

pub const CAPA_LAR_SUPPORT: u32 = 1;
pub const CAPA_MULTI_QUEUE_RX: u32 = 68;
pub const CAPA_FRAGMENTED_PNVM_IMG: u32 = 32;
pub const CAPA_MLD_API: u32 = 110;
pub const CAPA_SOC_LATENCY: u32 = 37;
pub const API_REGULATORY_NVM_INFO: u32 = 48;
pub const API_BAND_IN_RX_DATA: u32 = 59;

pub struct Section {
    pub addr: u32,
    pub data: &'static [u8],
}

pub struct Firmware {
    pub version: (u32, u32, u32),
    pub human: &'static [u8],
    pub sections: Vec<Section>,
    pub iml: &'static [u8],
    capa: [u32; 8],
    api: [u32; 8],
    /// (group, command, command version, notification version).
    cmd_versions: Vec<[u8; 4]>,
    pub n_scan_channels: u32,
    pub num_stations: u32,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl Firmware {
    pub fn parse(d: &'static [u8]) -> Option<Firmware> {
        if d.len() < 88 || u32_at(d, 0) != 0 || u32_at(d, 4) != MAGIC {
            return None;
        }
        let human = &d[8..72];
        let human = &human[..human.iter().position(|&c| c == 0).unwrap_or(64)];
        let mut fw = Firmware {
            version: (u32_at(d, 72) >> 24, 0, 0),
            human,
            sections: Vec::new(),
            iml: &[],
            capa: [0; 8],
            api: [0; 8],
            cmd_versions: Vec::new(),
            n_scan_channels: 0,
            num_stations: 16,
        };
        let mut off = 88;
        while off + 8 <= d.len() {
            let ty = u32_at(d, off);
            let len = u32_at(d, off + 4) as usize;
            let p = d.get(off + 8..off + 8 + len)?;
            match ty {
                TLV_SEC_RT if len >= 4 => fw.sections.push(Section { addr: u32_at(p, 0), data: &p[4..] }),
                TLV_IML => fw.iml = p,
                TLV_ENABLED_CAPABILITIES if len >= 8 => {
                    let i = u32_at(p, 0) as usize;
                    if i < 8 {
                        fw.capa[i] = u32_at(p, 4);
                    }
                }
                TLV_API_CHANGES_SET if len >= 8 => {
                    let i = u32_at(p, 0) as usize;
                    if i < 8 {
                        fw.api[i] = u32_at(p, 4);
                    }
                }
                TLV_CMD_VERSIONS => fw.cmd_versions = p.chunks_exact(4).map(|c| [c[1], c[0], c[2], c[3]]).collect(),
                TLV_FW_VERSION if len >= 12 => fw.version = (u32_at(p, 0), u32_at(p, 4), u32_at(p, 8)),
                TLV_N_SCAN_CHANNELS if len >= 4 => fw.n_scan_channels = u32_at(p, 0),
                TLV_FW_NUM_STATIONS if len >= 4 => fw.num_stations = u32_at(p, 0),
                _ => {}
            }
            off += 8 + ((len + 3) & !3);
        }
        Some(fw)
    }

    pub fn has_capa(&self, c: u32) -> bool {
        self.capa[(c / 32) as usize] & (1 << (c % 32)) != 0
    }

    pub fn has_api(&self, a: u32) -> bool {
        self.api[(a / 32) as usize] & (1 << (a % 32)) != 0
    }

    /// The version of command `cmd` in `group` (`default` if not listed).
    /// Legacy commands may be listed in group 0 or 1.
    pub fn cmd_ver(&self, group: u8, cmd: u8, default: u8) -> u8 {
        self.cmd_versions
            .iter()
            .find(|v| v[1] == cmd && (v[0] == group || group <= 1 && v[0] <= 1))
            .map_or(default, |v| v[2])
    }

    pub fn notif_ver(&self, group: u8, cmd: u8, default: u8) -> u8 {
        self.cmd_versions
            .iter()
            .find(|v| v[1] == cmd && (v[0] == group || group <= 1 && v[0] <= 1))
            .map_or(default, |v| v[3])
    }

    /// The runtime image split at the separators: (LMAC, UMAC, paging).
    pub fn split(&self) -> (&[Section], &[Section], &[Section]) {
        let s = &self.sections[..];
        let a = s.iter().position(|x| x.addr == SEPARATOR_CPU).unwrap_or(s.len());
        let rest = &s[(a + 1).min(s.len())..];
        let b = rest.iter().position(|x| x.addr == SEPARATOR_PAGING).unwrap_or(rest.len());
        (&s[..a], &rest[..b], &rest[(b + 1).min(rest.len())..])
    }
}

/// The PNVM payloads for SKU `sku` on a device of MAC type `mac_type`
/// and RF type `rf_type` (CSR_HW_REV_TYPE / CSR_HW_RFID_TYPE).
pub fn pnvm_chunks(d: &'static [u8], sku: [u32; 3], mac_type: u16, rf_type: u16) -> Option<(u32, Vec<&'static [u8]>)> {
    let mut off = 0;
    while off + 8 <= d.len() {
        let ty = u32_at(d, off);
        let len = u32_at(d, off + 4) as usize;
        let p = d.get(off + 8..off + 8 + len)?;
        off += 8 + ((len + 3) & !3);
        if ty != TLV_PNVM_SKU || len < 12 || [u32_at(p, 0), u32_at(p, 4), u32_at(p, 8)] != sku {
            continue;
        }
        // This SKU's section: until the next SKU.
        let (mut version, mut hw_match, mut chunks) = (0, false, Vec::new());
        let mut o = off;
        while o + 8 <= d.len() {
            let t = u32_at(d, o);
            let l = u32_at(d, o + 4) as usize;
            let q = d.get(o + 8..o + 8 + l)?;
            match t {
                TLV_PNVM_SKU => break,
                TLV_PNVM_VERSION if l >= 4 => version = u32_at(q, 0),
                TLV_HW_TYPE if l >= 4 => {
                    let m = u16::from_le_bytes([q[0], q[1]]);
                    let r = u16::from_le_bytes([q[2], q[3]]);
                    hw_match |= m == mac_type && r == rf_type;
                }
                TLV_SEC_RT if l >= 4 && u32_at(q, 0) != 0xdddd_eeee => chunks.push(&q[4..]),
                _ => {}
            }
            o += 8 + ((l + 3) & !3);
        }
        if hw_match && !chunks.is_empty() {
            return Some((version, chunks));
        }
    }
    None
}
