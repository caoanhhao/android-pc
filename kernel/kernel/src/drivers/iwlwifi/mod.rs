//! Intel Wi-Fi 6E AX211 (CNVi "so" MAC with a "GF" RF module, PCI
//! 8086:7a70 and relatives): Linux's iwlwifi with its mvm op mode, for
//! firmware API 89 (the MLD command API), as a `net::wireless` driver.
//!
//! Probe (boot): PCI function, firmware `iwlwifi-so-a0-gf-a0-89.ucode`
//! and `iwlwifi-so-a0-gf-a0.pnvm` (`drivers::firmware`), transport
//! bring-up and firmware load (`trans`), then the unified image's init
//! flow: ALIVE, the PNVM for the SKU ALIVE reports, INIT_EXTENDED_CFG,
//! NVM_ACCESS_COMPLETE, INIT_COMPLETE, NVM_GET_INFO; then the runtime
//! configuration Linux's iwl_mvm_up sends that matters here (SoC
//! latency, TX antennas, BT coexistence, regulatory MCC "ZZ", scan
//! config). Interface up adds the station MAC and its link (MAC_CONFIG,
//! LINK_CONFIG). Scans are UMAC scans (version 17); beacons and probe
//! responses arrive as RX MPDU notifications and go to the wireless
//! layer, SCAN_COMPLETE ends the scan.
//!
//! Joining an AP: PHY context and RLC config for its channel, the link
//! made active, the AP as firmware station 0 (STA_CONFIG), one TX queue
//! for its TID 8 (management and non-QoS data, SCD_QUEUE_CONFIG) and
//! session protection while authenticating; frames go out as TX_CMD
//! (v10) without a fixed rate. After association: MAC/LINK/STA updates
//! and rate control (TLC, legacy rates); keys with SEC_KEY_CMD, the
//! firmware encrypting and decrypting. Legacy (non-HT) rates only, no
//! QoS, no aggregation; untested on hardware. Everything is polled (no interrupts). Progress and errors
//! (firmware version, ALIVE status, NVM MAC, scan results, firmware
//! error tables) are logged, so the boot screen shows how far it got.

pub mod fw;
pub mod trans;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::errno::*;
use crate::net::wireless::{self, Band, Caps, Channel, Event, JoinParams, Key, ScanReq, TxStats, WifiDriver};
use crate::net::wireless::ieee80211 as w;
use fw::Firmware;
use trans::{CmdError, Packet, Trans};

const LEGACY: u8 = 0;
const LONG: u8 = 1;
const SYSTEM_GROUP: u8 = 2;
const MAC_CONF_GROUP: u8 = 3;
const REG_NVM_GROUP: u8 = 0xc;

const UCODE_ALIVE_NTFY: u8 = 0x1;
const INIT_COMPLETE_NOTIF: u8 = 0x4;
const SCAN_CFG_CMD: u8 = 0xc;
const SCAN_REQ_UMAC: u8 = 0xd;
const SCAN_ABORT_UMAC: u8 = 0xe;
const SCAN_COMPLETE_UMAC: u8 = 0xf;
const TX_ANT_CONFIGURATION_CMD: u8 = 0x98;
const BT_CONFIG: u8 = 0x9b;
const MISSED_BEACONS_NOTIFICATION: u8 = 0xa2;
const REPLY_RX_MPDU_CMD: u8 = 0xc1;
const MCC_UPDATE_CMD: u8 = 0xc8;
const REPLY_ERROR: u8 = 0x2;
const SOC_CONFIGURATION_CMD: u8 = 0x1;
const INIT_EXTENDED_CFG_CMD: u8 = 0x3;
const NVM_ACCESS_COMPLETE: u8 = 0x0;
const NVM_GET_INFO: u8 = 0x2;
const PNVM_INIT_COMPLETE_NTFY: u8 = 0xfe;
const MAC_CONFIG_CMD: u8 = 0x8;
const LINK_CONFIG_CMD: u8 = 0x9;

const FW_CTXT_ACTION_ADD: u32 = 1;
const FW_CTXT_ACTION_REMOVE: u32 = 3;
const FW_CTXT_INVALID: u32 = u32::MAX;
const FW_MAC_TYPE_BSS_STA: u32 = 5;
const MAC_CFG_FILTER_ACCEPT_GRP: u32 = 1 << 2;
const MAC_CFG_FILTER_ACCEPT_BEACON: u32 = 1 << 3;

const ALIVE_STATUS_OK: u16 = 0xcafe;
const CMD_TIMEOUT_MS: u64 = 2000;

/// PCI ids of AX211 ("so" MAC) functions; the RF module decides the
/// firmware name.
const DEVICE_IDS: &[u16] = &[0x7a70, 0x7af0, 0x51f0, 0x51f1, 0x54f0, 0x7f70, 0x7e40];
const FW_API: u32 = 89;

/// A link with its own MAC context: our station interface.
const MAC_ID: u32 = 0;
const LINK_ID: u32 = 0;
const PHY_ID: u32 = 0;
const STA_ID: u32 = 0;
/// Management and non-QoS data share the AP station's TID 8 queue.
const MGMT_TID: u8 = 8;
const TXQ_SIZE: usize = 256;
const CAPA_STA_EXP_MFP: u32 = 114;

const DATA_PATH_GROUP: u8 = 5;
const PHY_CONTEXT_CMD: u8 = 0x8;
const TX_CMD: u8 = 0x1c;
const RLC_CONFIG_CMD: u8 = 0x8;
const TLC_MNG_CONFIG_CMD: u8 = 0xf;
const SCD_QUEUE_CONFIG_CMD: u8 = 0x17;
const SEC_KEY_CMD: u8 = 0x18;
const SESSION_PROTECTION_CMD: u8 = 0x5;
const SESSION_PROTECTION_NOTIF: u8 = 0xfb;
const STA_CONFIG_CMD: u8 = 0xa;
const STA_REMOVE_CMD: u8 = 0xc;
const LINK_MODIFY_ACTIVE: u32 = 1 << 0;
const LINK_MODIFY_RATES_INFO: u32 = 1 << 1;
const LINK_MODIFY_PROTECT_FLAGS: u32 = 1 << 2;
const LINK_MODIFY_BEACON_TIMING: u32 = 1 << 4;

struct Joined {
    bssid: [u8; 6],
    freq: u32,
    beacon_int: u16,
    cck_basic: u32,
    ofdm_basic: u32,
    short_preamble: bool,
    short_slot: bool,
    txq: trans::TxQueue,
    assoc: bool,
}

pub struct Iwl {
    trans: Trans,
    fw: Firmware,
    caps: Caps,
    mac: [u8; 6],
    tx_chains: u32,
    rx_chains: u32,
    /// The MAC context and link exist in the firmware.
    vif: bool,
    /// The AP we are joined to: PHY context, link active, station 0 and
    /// its queue exist in the firmware.
    joined: Option<Joined>,
    scan_uid: Option<u32>,
    scan_seq: u32,
    events: Vec<Event>,
    /// Scan results seen in this scan (for the log).
    scan_found: u32,
    dead: bool,
    lmac_err: u32,
    umac_err: u32,
    stats: TxStats,
}

fn rf_name(rf_type: u32) -> Option<&'static str> {
    match rf_type {
        0x10d => Some("gf"),
        0x10e => Some("gf4"),
        _ => None,
    }
}

/// Probe every AX211.
pub fn init() {
    for d in crate::drivers::pci::devices() {
        if d.vendor != 0x8086 || !DEVICE_IDS.contains(&d.device) {
            continue;
        }
        log::info!("iwlwifi: {:02x}:{:02x}.{} {:04x}:{:04x}", d.addr.bus, d.addr.dev, d.addr.func, d.vendor, d.device);
        match probe(&d) {
            Ok(dev) => {
                let sys = crate::drivers::pci::sysfs_dir(&d, "iwlwifi");
                wireless::register(Box::new(dev), &sys);
            }
            Err(e) => log::warn!("iwlwifi: {e}"),
        }
    }
}

fn probe(d: &crate::drivers::pci::Device) -> Result<Iwl, String> {
    let mut trans = Trans::new(d)?;
    trans.start_hw()?;
    let rf = (trans.hw_rf_id >> 12) & 0xfff;
    log::info!("iwlwifi: hw_rev {:#x} rf_id {:#x}", trans.hw_rev, trans.hw_rf_id);
    let rfn = rf_name(rf).ok_or_else(|| format!("unsupported RF module (rf type {rf:#x})"))?;
    let fw_name = format!("iwlwifi-so-a0-{rfn}-a0-{FW_API}.ucode");
    let data = crate::drivers::firmware::request(&fw_name).ok_or_else(|| format!("{fw_name} missing (tools/mkusb.sh puts it on the stick)"))?;
    let fw = Firmware::parse(data).ok_or("bad firmware file")?;
    log::info!(
        "iwlwifi: firmware {}.{:08x}.{} ({})",
        fw.version.0,
        fw.version.1,
        fw.version.2,
        core::str::from_utf8(fw.human).unwrap_or("?")
    );
    if !fw.has_capa(fw::CAPA_MLD_API) {
        return Err("firmware without the MLD API".into());
    }
    let pnvm = crate::drivers::firmware::request(&format!("iwlwifi-so-a0-{rfn}-a0.pnvm"));
    let mut dev = Iwl {
        trans,
        fw,
        caps: Caps {
            driver: "iwlwifi",
            perm_addr: [0; 6],
            bands: Vec::new(),
            ciphers: alloc::vec![
                wireless::CIPHER_CCMP,
                wireless::CIPHER_GCMP,
                wireless::CIPHER_GCMP_256,
                wireless::CIPHER_AES_CMAC,
                wireless::CIPHER_BIP_GMAC_128,
                wireless::CIPHER_BIP_GMAC_256,
            ],
            max_scan_ssids: 20,
            max_scan_ie_len: 300,
            sae: true,
            random_scan_mac: false,
        },
        mac: [0; 6],
        tx_chains: 3,
        rx_chains: 3,
        vif: false,
        joined: None,
        scan_uid: None,
        scan_seq: 0,
        events: Vec::new(),
        scan_found: 0,
        dead: false,
        lmac_err: 0,
        umac_err: 0,
        stats: TxStats::default(),
    };
    dev.boot(pnvm)?;
    Ok(dev)
}

fn cmd_err(e: CmdError) -> String {
    format!("{e:?}")
}

fn le32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}

fn le16(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes(s.try_into().unwrap()))
}

impl Iwl {
    fn send(&mut self, group: u8, cmd: u8, data: &[u8]) -> Result<Packet, String> {
        let r = self.trans.send_cmd(group, cmd, 0, data, CMD_TIMEOUT_MS);
        if let Err(CmdError::FwError) = r {
            self.fw_error();
        }
        r.map_err(|e| format!("command {group:x}.{cmd:02x}: {}", cmd_err(e)))
    }

    /// Load the firmware and run the unified image's init flow.
    fn boot(&mut self, pnvm: Option<&'static [u8]>) -> Result<(), String> {
        let imr = true;
        self.trans.load_fw(&self.fw, imr)?;
        if !self.trans.wait_alive_irq(1000) {
            log::warn!(
                "iwlwifi: no ALIVE interrupt (CSR_INT {:#x}, GP_CNTRL {:#x}, SB CPU1 {:#x} CPU2 {:#x})",
                self.trans.r32(trans::CSR_INT),
                self.trans.r32(trans::CSR_GP_CNTRL),
                self.trans.read_umac_prph(trans::UMAG_SB_CPU_1_STATUS),
                self.trans.read_umac_prph(trans::UMAG_SB_CPU_2_STATUS)
            );
        }
        let alive = self.trans.wait_notif(LEGACY, UCODE_ALIVE_NTFY, 1000).ok_or_else(|| {
            format!(
                "no ALIVE notification (CSR_INT {:#x}, SB CPU1 {:#x} CPU2 {:#x})",
                self.trans.r32(trans::CSR_INT),
                self.trans.read_umac_prph(trans::UMAG_SB_CPU_1_STATUS),
                self.trans.read_umac_prph(trans::UMAG_SB_CPU_2_STATUS)
            )
        })?;
        let a = &alive.data;
        let status = le16(a, 0);
        // UCODE_ALIVE_NTFY version 6+: status, flags, 2 x LMAC (48 bytes
        // each), UMAC (16), SKU id (12), IMR.
        self.lmac_err = le32(a, 4 + 16);
        self.umac_err = le32(a, 4 + 96 + 8) & !0xc000_0000;
        let sku = [le32(a, 116), le32(a, 120), le32(a, 124)];
        log::info!(
            "iwlwifi: ALIVE status {status:#06x}, UMAC {}.{}, SKU {:08x}-{:08x}-{:08x}",
            le32(a, 100),
            le32(a, 104),
            sku[0],
            sku[1],
            sku[2]
        );
        if status != ALIVE_STATUS_OK {
            return Err(format!("firmware not valid (ALIVE status {status:#x})"));
        }

        // PNVM: the platform NVM section for this SKU and device.
        if sku != [0; 3] {
            let mac_type = ((self.trans.hw_rev >> 4) & 0xfff) as u16;
            let rf_type = ((self.trans.hw_rf_id >> 12) & 0xfff) as u16;
            match pnvm.and_then(|p| fw::pnvm_chunks(p, sku, mac_type, rf_type)) {
                Some((ver, chunks)) => {
                    self.trans.set_pnvm(&chunks)?;
                    log::info!("iwlwifi: PNVM {ver:08x} ({} payloads)", chunks.len());
                }
                None => log::warn!("iwlwifi: no PNVM for this SKU (mac {mac_type:#x} rf {rf_type:#x})"),
            }
            self.trans.write_umac_prph(trans::UREG_DOORBELL_TO_ISR6, trans::UREG_DOORBELL_TO_ISR6_PNVM);
            match self.trans.wait_notif(REG_NVM_GROUP, PNVM_INIT_COMPLETE_NTFY, 1000) {
                Some(p) => log::info!("iwlwifi: PNVM loaded (status {:#x})", le32(&p.data, 0)),
                None => return Err("timeout waiting for the PNVM load".into()),
            }
        }

        // The unified image: NVM init, then INIT_COMPLETE.
        self.send(SYSTEM_GROUP, INIT_EXTENDED_CFG_CMD, &(1u32 << 1).to_le_bytes())?;
        self.send(REG_NVM_GROUP, NVM_ACCESS_COMPLETE, &0u32.to_le_bytes())?;
        if self.trans.wait_notif(LEGACY, INIT_COMPLETE_NOTIF, 2000).is_none() {
            return Err("no INIT_COMPLETE".into());
        }
        self.read_nvm()?;
        self.runtime_config()?;
        log::info!("iwlwifi: ready, address {}", mac_str(&self.mac));
        Ok(())
    }

    fn read_nvm(&mut self) -> Result<(), String> {
        let r = self.send(REG_NVM_GROUP, NVM_GET_INFO, &0u32.to_le_bytes())?;
        let d = &r.data;
        let flags = le32(d, 0);
        let sku = le32(d, 8);
        self.tx_chains = le32(d, 12) & 0xf;
        self.rx_chains = le32(d, 16) & 0xf;
        let lar = le32(d, 20);
        self.mac = self.trans.mac_address();
        log::info!(
            "iwlwifi: NVM: {} bytes, flags {flags:#x}, sku {sku:#x}, chains tx {:#x} rx {:#x}, LAR {lar}, MAC {}",
            d.len(),
            self.tx_chains,
            self.rx_chains,
            mac_str(&self.mac)
        );
        if self.tx_chains == 0 {
            self.tx_chains = 3;
        }
        if self.rx_chains == 0 {
            self.rx_chains = 3;
        }
        self.caps.perm_addr = self.mac;
        let ch = |c: u8, b: u8, dfs: bool| Channel {
            freq: w::chan_to_freq(c, b),
            disabled: false,
            no_ir: dfs,
            radar: dfs,
            max_power: 2000,
        };
        let mut bands = Vec::new();
        if sku & 1 != 0 || sku == 0 {
            bands.push(Band {
                band: 0,
                channels: (1..=13).map(|c| ch(c, 0, false)).collect(),
                rates: alloc::vec![10, 20, 55, 110, 60, 90, 120, 180, 240, 360, 480, 540],
                ht: None,
                vht: None,
            });
        }
        if sku & 2 != 0 || sku == 0 {
            let five: &[u8] = &[36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165];
            bands.push(Band {
                band: 1,
                channels: five.iter().map(|&c| ch(c, 1, (52..=144).contains(&c))).collect(),
                rates: alloc::vec![60, 90, 120, 180, 240, 360, 480, 540],
                ht: None,
                vht: None,
            });
        }
        self.caps.bands = bands;
        Ok(())
    }

    /// What iwl_mvm_up sends before the interface exists.
    fn runtime_config(&mut self) -> Result<(), String> {
        if self.fw.has_capa(fw::CAPA_SOC_LATENCY) {
            // Integrated: LTR delay 2500 us, low-latency crystal, 12 ms.
            let mut c = Vec::new();
            c.extend_from_slice(&((2u32 << 2) | (1 << 1)).to_le_bytes());
            c.extend_from_slice(&12000u32.to_le_bytes());
            self.send(SYSTEM_GROUP, SOC_CONFIGURATION_CMD, &c)?;
        }
        self.send(LEGACY, TX_ANT_CONFIGURATION_CMD, &self.tx_chains.to_le_bytes())?;
        let mut bt = Vec::new();
        bt.extend_from_slice(&1u32.to_le_bytes()); // BT_COEX_NW
        bt.extend_from_slice(&((1u32 << 0) | (1 << 2) | (1 << 4)).to_le_bytes());
        self.send(LEGACY, BT_CONFIG, &bt)?;
        // Regulatory: the firmware's current domain (LAR).
        let mut mcc = alloc::vec![0u8; 32];
        mcc[0..2].copy_from_slice(&0x5a5au16.to_le_bytes());
        mcc[2] = 0x10; // MCC_SOURCE_GET_CURRENT
        match self.send(LONG, MCC_UPDATE_CMD, &mcc) {
            Ok(r) => {
                let d = &r.data;
                let m = le16(d, 4);
                log::info!(
                    "iwlwifi: regulatory {}{} (status {}, {} channels)",
                    (m >> 8) as u8 as char,
                    (m & 0xff) as u8 as char,
                    le32(d, 0),
                    le32(d, 16)
                );
            }
            Err(e) => log::warn!("iwlwifi: MCC update failed: {e}"),
        }
        let mut sc = Vec::new();
        sc.extend_from_slice(&[0, 0, 0xff, 0]);
        sc.extend_from_slice(&self.tx_chains.to_le_bytes());
        sc.extend_from_slice(&self.rx_chains.to_le_bytes());
        self.send(LONG, SCAN_CFG_CMD, &sc)?;
        Ok(())
    }

    fn mac_config(&mut self, action: u32) -> Result<(), String> {
        // MAC_CONFIG_CMD (struct iwl_mac_config_cmd_v3, 52 bytes).
        let mut c = alloc::vec![0u8; 52];
        c[0..4].copy_from_slice(&MAC_ID.to_le_bytes());
        c[4..8].copy_from_slice(&action.to_le_bytes());
        c[8..12].copy_from_slice(&FW_MAC_TYPE_BSS_STA.to_le_bytes());
        c[12..18].copy_from_slice(&self.mac);
        c[20..24].copy_from_slice(&(MAC_CFG_FILTER_ACCEPT_GRP | MAC_CFG_FILTER_ACCEPT_BEACON).to_le_bytes());
        // wifi_gen (8 bytes at 24): no HE/EHT; nic_not_ack_enabled at 32.
        // Client data at 36: not associated.
        self.send(MAC_CONF_GROUP, MAC_CONFIG_CMD, &c).map(|_| ())
    }

    fn link_config(&mut self, action: u32) -> Result<(), String> {
        let mut c = alloc::vec![0u8; LINK_CONFIG_SIZE];
        c[0..4].copy_from_slice(&action.to_le_bytes());
        c[4..8].copy_from_slice(&LINK_ID.to_le_bytes());
        c[8..12].copy_from_slice(&MAC_ID.to_le_bytes());
        c[12..16].copy_from_slice(&FW_CTXT_INVALID.to_le_bytes());
        c[16..22].copy_from_slice(&self.mac);
        self.send(MAC_CONF_GROUP, LINK_CONFIG_CMD, &c).map(|_| ())
    }

    /// The firmware stopped: dump its error tables.
    fn fw_error(&mut self) {
        if self.dead {
            return;
        }
        self.dead = true;
        log::warn!("iwlwifi: FIRMWARE ERROR (CSR_INT {:#x})", self.trans.r32(trans::CSR_INT));
        for (name, addr) in [("LMAC", self.lmac_err), ("UMAC", self.umac_err)] {
            if addr == 0 {
                continue;
            }
            let v = self.trans.read_mem(addr, 24);
            let line: String = v.iter().map(|x| format!(" {x:08x}")).collect();
            log::warn!("iwlwifi: {name} error table @{addr:#x}:{line}");
        }
    }

    /// PHY_CONTEXT_CMD (v4) for a 20 MHz channel at `freq`.
    fn phy_ctxt(&mut self, action: u32, freq: u32) -> Result<(), String> {
        let mut c = alloc::vec![0u8; 32];
        c[0..4].copy_from_slice(&PHY_ID.to_le_bytes());
        c[4..8].copy_from_slice(&action.to_le_bytes());
        c[8..12].copy_from_slice(&(w::freq_to_chan(freq) as u32).to_le_bytes());
        c[12] = if freq < 3000 { 1 } else { 0 }; // PHY_BAND_24 / PHY_BAND_5
        self.send(LEGACY, PHY_CONTEXT_CMD, &c)?;
        if action != FW_CTXT_ACTION_REMOVE {
            // RLC_CONFIG_CMD v2: both RX chains, MIMO count 2.
            let mut r = alloc::vec![0u8; 32];
            r[0..4].copy_from_slice(&PHY_ID.to_le_bytes());
            let chain = (self.rx_chains << 1) | (2 << 10) | (2 << 12);
            r[4..8].copy_from_slice(&chain.to_le_bytes());
            self.send(DATA_PATH_GROUP, RLC_CONFIG_CMD, &r)?;
        }
        Ok(())
    }

    /// LINK_CONFIG_CMD modify: the link on PHY 0, (in)active, the BSS's
    /// basic rates, preamble, slot time and beacon timing.
    fn link_modify(&mut self, active: bool, j: &Joined, mask: u32) -> Result<(), String> {
        let mut c = alloc::vec![0u8; LINK_CONFIG_SIZE];
        c[0..4].copy_from_slice(&2u32.to_le_bytes()); // FW_CTXT_ACTION_MODIFY
        c[4..8].copy_from_slice(&LINK_ID.to_le_bytes());
        c[8..12].copy_from_slice(&MAC_ID.to_le_bytes());
        let phy = if active { PHY_ID } else { FW_CTXT_INVALID };
        c[12..16].copy_from_slice(&phy.to_le_bytes());
        c[16..22].copy_from_slice(&self.mac);
        c[24..28].copy_from_slice(&mask.to_le_bytes());
        c[28..32].copy_from_slice(&(active as u32).to_le_bytes());
        c[36..40].copy_from_slice(&j.cck_basic.to_le_bytes());
        c[40..44].copy_from_slice(&j.ofdm_basic.to_le_bytes());
        c[44..48].copy_from_slice(&(j.short_preamble as u32).to_le_bytes());
        c[48..52].copy_from_slice(&(j.short_slot as u32).to_le_bytes());
        c[136..140].copy_from_slice(&(j.beacon_int as u32).to_le_bytes());
        c[140..144].copy_from_slice(&(j.beacon_int as u32).to_le_bytes());
        // LINK_FLG_BSS_COLOR_DIS (no HE), in flags and flags_mask (< v6).
        c[148..152].copy_from_slice(&1u32.to_le_bytes());
        c[152..156].copy_from_slice(&1u32.to_le_bytes());
        self.send(MAC_CONF_GROUP, LINK_CONFIG_CMD, &c).map(|_| ())
    }

    /// MAC_CONFIG_CMD modify: associated with `aid`, or not (None).
    fn mac_config_assoc(&mut self, aid: Option<u16>) -> Result<(), String> {
        let mut c = alloc::vec![0u8; 52];
        c[0..4].copy_from_slice(&MAC_ID.to_le_bytes());
        c[4..8].copy_from_slice(&2u32.to_le_bytes());
        c[8..12].copy_from_slice(&FW_MAC_TYPE_BSS_STA.to_le_bytes());
        c[12..18].copy_from_slice(&self.mac);
        let mut filter = MAC_CFG_FILTER_ACCEPT_GRP;
        match aid {
            Some(a) => {
                c[36] = 1;
                c[40..42].copy_from_slice(&a.to_le_bytes());
            }
            None => filter |= MAC_CFG_FILTER_ACCEPT_BEACON,
        }
        c[20..24].copy_from_slice(&filter.to_le_bytes());
        self.send(MAC_CONF_GROUP, MAC_CONFIG_CMD, &c).map(|_| ())
    }

    /// STA_CONFIG_CMD (v1, 96 bytes) for the AP as station 0.
    fn sta_config(&mut self, bssid: [u8; 6], aid: u16, mfp: bool) -> Result<(), String> {
        let mut c = alloc::vec![0u8; 96];
        c[0..4].copy_from_slice(&STA_ID.to_le_bytes());
        c[4..8].copy_from_slice(&LINK_ID.to_le_bytes());
        c[8..14].copy_from_slice(&bssid);
        c[16..22].copy_from_slice(&bssid);
        // station_type 0 (peer) at 24.
        c[28..32].copy_from_slice(&(aid as u32).to_le_bytes());
        c[36..40].copy_from_slice(&(mfp as u32).to_le_bytes());
        self.send(MAC_CONF_GROUP, STA_CONFIG_CMD, &c).map(|_| ())
    }

    fn do_join(&mut self, p: &JoinParams) -> Result<(), String> {
        let ap_rates = w::rates_of(p.ies);
        let (mut cck, mut ofdm) = (0u32, 0u32);
        for r in ap_rates.iter().filter(|r| **r & 0x80 != 0) {
            match r & 0x7f {
                2 => cck |= 1,
                4 => cck |= 2,
                11 => cck |= 4,
                22 => cck |= 8,
                12 => ofdm |= 1,
                18 => ofdm |= 2,
                24 => ofdm |= 4,
                36 => ofdm |= 8,
                48 => ofdm |= 16,
                72 => ofdm |= 32,
                96 => ofdm |= 64,
                108 => ofdm |= 128,
                _ => {}
            }
        }
        if p.freq > 3000 {
            cck = 0;
        }
        if ofdm == 0 {
            ofdm = 0x15;
        }
        let cap = 0u16;
        let txq = Trans::txq_new(TXQ_SIZE).ok_or("out of DMA memory")?;
        let mut j = Joined {
            bssid: p.bssid,
            freq: p.freq,
            beacon_int: p.beacon_int.max(1),
            cck_basic: cck,
            ofdm_basic: ofdm,
            short_preamble: cap & 0x20 != 0 || p.freq > 3000,
            short_slot: p.freq > 3000,
            txq,
            assoc: false,
        };
        log::info!("iwlwifi: joining {} on {} MHz", mac_str(&p.bssid), p.freq);
        self.phy_ctxt(FW_CTXT_ACTION_ADD, p.freq)?;
        self.link_modify(true, &j, LINK_MODIFY_ACTIVE | LINK_MODIFY_RATES_INFO)?;
        let mfp = self.fw.has_capa(CAPA_STA_EXP_MFP);
        self.sta_config(p.bssid, 0, mfp)?;
        let r = self.send(DATA_PATH_GROUP, SCD_QUEUE_CONFIG_CMD, &Trans::txq_add_cmd(&j.txq, 1 << STA_ID, MGMT_TID))?;
        j.txq.id = le16(&r.data, 0);
        j.txq.write = le16(&r.data, 4);
        // Stay on the channel for authentication and association.
        let mut sp = alloc::vec![0u8; 24];
        sp[0..4].copy_from_slice(&LINK_ID.to_le_bytes());
        sp[4..8].copy_from_slice(&FW_CTXT_ACTION_ADD.to_le_bytes());
        // conf_id 0: SESSION_PROTECT_CONF_ASSOC.
        sp[12..16].copy_from_slice(&900u32.to_le_bytes());
        sp[16..20].copy_from_slice(&1u32.to_le_bytes());
        self.send(MAC_CONF_GROUP, SESSION_PROTECTION_CMD, &sp)?;
        let started = self.trans.wait_notif(MAC_CONF_GROUP, SESSION_PROTECTION_NOTIF, 500);
        log::info!(
            "iwlwifi: joined: queue {}, session protection {}",
            j.txq.id,
            match &started {
                Some(n) => format!("status {} start {}", le32(&n.data, 4), le32(&n.data, 8)),
                None => "not started".into(),
            }
        );
        self.joined = Some(j);
        Ok(())
    }

    fn do_assoc(&mut self, a: &wireless::AssocInfo) -> Result<(), String> {
        let mut j = self.joined.take().ok_or("not joined")?;
        j.beacon_int = a.beacon_int.max(1);
        j.short_preamble = a.capability & 0x20 != 0 || j.freq > 3000;
        j.short_slot = a.capability & 0x400 != 0 || j.freq > 3000;
        let r = (|| {
            self.mac_config_assoc(Some(a.aid))?;
            self.link_modify(true, &j, LINK_MODIFY_RATES_INFO | LINK_MODIFY_PROTECT_FLAGS | LINK_MODIFY_BEACON_TIMING)?;
            let mfp = self.fw.has_capa(CAPA_STA_EXP_MFP);
            self.sta_config(j.bssid, a.aid, mfp)?;
            // Rate control (TLC_MNG_CONFIG_CMD v4): the AP's legacy rates.
            let mut supp = 0u16;
            for r in a.rates {
                let bit = match r & 0x7f {
                    2 => 0,
                    4 => 1,
                    11 => 2,
                    22 => 3,
                    12 => 4,
                    18 => 5,
                    24 => 6,
                    36 => 7,
                    48 => 8,
                    72 => 9,
                    96 => 10,
                    108 => 11,
                    _ => continue,
                };
                if j.freq > 3000 && bit < 4 {
                    continue;
                }
                supp |= 1 << bit;
            }
            let mut t = alloc::vec![0u8; 28];
            t[0] = STA_ID as u8;
            t[6] = self.tx_chains as u8;
            t[10..12].copy_from_slice(&supp.to_le_bytes());
            self.send(DATA_PATH_GROUP, TLC_MNG_CONFIG_CMD, &t)?;
            Ok::<(), String>(())
        })();
        j.assoc = r.is_ok();
        self.joined = Some(j);
        if r.is_ok() {
            log::info!("iwlwifi: associated (aid {})", a.aid);
        }
        r
    }

    fn do_set_key(&mut self, k: &Key) -> Result<(), String> {
        let (cipher, big) = match k.cipher {
            wireless::CIPHER_CCMP | wireless::CIPHER_AES_CMAC => (2u32, false),
            wireless::CIPHER_GCMP | wireless::CIPHER_BIP_GMAC_128 => (5, false),
            wireless::CIPHER_GCMP_256 | wireless::CIPHER_BIP_GMAC_256 => (5, true),
            _ => return Err(format!("cipher {:08x} not supported", k.cipher)),
        };
        if k.data.len() > 32 {
            return Err("key too long".into());
        }
        let mut flags = cipher;
        if big {
            flags |= 0x10;
        }
        if !k.pairwise {
            flags |= 0x40;
        }
        if matches!(k.cipher, wireless::CIPHER_AES_CMAC | wireless::CIPHER_BIP_GMAC_128 | wireless::CIPHER_BIP_GMAC_256) {
            flags |= 0x20;
        }
        let mut c = alloc::vec![0u8; 80];
        c[0..4].copy_from_slice(&1u32.to_le_bytes()); // FW_CTXT_ACTION_ADD
        c[4..8].copy_from_slice(&(1u32 << STA_ID).to_le_bytes());
        c[8..12].copy_from_slice(&(k.idx as u32).to_le_bytes());
        c[12..16].copy_from_slice(&flags.to_le_bytes());
        c[16..16 + k.data.len()].copy_from_slice(&k.data);
        let mut seq = [0u8; 8];
        let n = k.seq.len().min(8);
        seq[..n].copy_from_slice(&k.seq[..n]);
        c[64..72].copy_from_slice(&seq);
        self.send(DATA_PATH_GROUP, SEC_KEY_CMD, &c)?;
        log::info!("iwlwifi: {} key {} installed", if k.pairwise { "pairwise" } else { "group" }, k.idx);
        Ok(())
    }

    /// Send a frame to the AP on its queue (the firmware picks the rate
    /// and encrypts once keys are installed).
    fn tx_frame(&mut self, frame: &[u8], mgmt: bool) -> bool {
        let Some(j) = self.joined.as_mut() else { return false };
        if self.dead || frame.len() < 24 {
            return false;
        }
        let fc = w::fc(frame);
        let hl = if w::is_data(fc) { w::data_hdr_len(fc) } else { 24 };
        let eapol = !mgmt && frame.len() >= hl + 8 && frame[hl + 6..hl + 8] == [0x88, 0x8e];
        let mut flags: u16 = 0;
        if mgmt || eapol || !j.assoc {
            flags |= 1 << 2; // IWL_TX_FLAGS_HIGH_PRI
        }
        if mgmt && w::stype(fc) != w::STYPE_ACTION && w::stype(fc) != w::STYPE_DEAUTH && w::stype(fc) != w::STYPE_DISASSOC {
            flags |= 1 << 1; // IWL_TX_FLAGS_ENCRYPT_DIS
        }
        let mut cmd = alloc::vec![0u8; 24];
        cmd[0..2].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        cmd[2..4].copy_from_slice(&flags.to_le_bytes());
        cmd.extend_from_slice(&frame[..hl]);
        self.trans.tx(&mut j.txq, &cmd, &frame[hl..])
    }

    fn handle(&mut self, p: Packet) {
        if p.is(LEGACY, REPLY_RX_MPDU_CMD) {
            self.rx_mpdu(&p.data);
        } else if p.is(LONG, SCAN_COMPLETE_UMAC) {
            let uid = le32(&p.data, 0);
            let status = p.data.get(6).copied().unwrap_or(0);
            if self.scan_uid == Some(uid) {
                self.scan_uid = None;
                log::info!("iwlwifi: scan done (status {status}, {} frames)", self.scan_found);
                self.events.push(Event::ScanDone { aborted: status != 1 && status != 0 });
            }
        } else if p.is(LEGACY, TX_CMD) && p.seq & 0x8000 == 0 {
            // TX response: the frame at the sequence's index is done.
            if let Some(j) = self.joined.as_mut()
                && (p.seq >> 8) & 0x1f == j.txq.id
            {
                j.txq.reclaim(p.seq as u8);
                self.stats.tx_packets += 1;
                // iwl_tx_resp: status (TX_STATUS_SUCCESS 1, DIRECT_DONE 2) at 36.
                if le16(&p.data, 36) & 0xff != 1 && le16(&p.data, 36) & 0xff != 2 {
                    self.stats.tx_failed += 1;
                }
            }
        } else if p.is(LEGACY, MISSED_BEACONS_NOTIFICATION) {
            self.events.push(Event::ConnectionLost);
        } else if p.is(LEGACY, REPLY_ERROR) {
            log::warn!("iwlwifi: REPLY_ERROR {:02x?}", &p.data[..p.data.len().min(16)]);
        }
    }

    /// REPLY_RX_MPDU_CMD: 68-byte descriptor (AX210), then the frame.
    fn rx_mpdu(&mut self, d: &[u8]) {
        const DESC: usize = 68;
        if d.len() < DESC + 24 {
            return;
        }
        let len = le16(d, 0) as usize;
        let mac_flags2 = d[3];
        let status = le32(d, 12);
        let energy_a = d[20 + 20];
        let energy_b = d[20 + 21];
        let channel = d[20 + 22];
        let band = d[7];
        if len == 0 || DESC + len > d.len() || status & 0x3 != 0x3 {
            return;
        }
        let mut f = d[DESC..DESC + len].to_vec();
        let fc = w::fc(&f);
        let hl = if w::is_data(fc) { w::data_hdr_len(fc) } else { 24 };
        // Two pad bytes after the header (data frames).
        if mac_flags2 & 0x20 != 0 && hl + 2 <= f.len() {
            f.drain(hl..hl + 2);
        }
        if fc & w::FCTL_PROTECTED != 0 {
            // Decrypted by the firmware (CCMP/GCMP, MIC removed): drop
            // the IV.
            if status & (1 << 11) == 0 || f.len() < hl + 8 {
                return;
            }
            f.drain(hl..hl + 8);
            let nfc = fc & !w::FCTL_PROTECTED;
            f[0..2].copy_from_slice(&nfc.to_le_bytes());
        }
        let e = match (energy_a, energy_b) {
            (0, 0) => 90,
            (0, b) => b,
            (a, 0) => a,
            (a, b) => a.min(b),
        };
        // PHY band in bits 6-7: 0 5 GHz, 1 2.4 GHz, 2 6 GHz.
        let b = match band >> 6 {
            1 => 0,
            2 => 3,
            _ => 1,
        };
        let freq = w::chan_to_freq(channel, b);
        if w::is_mgmt(fc) && matches!(w::stype(fc), w::STYPE_BEACON | w::STYPE_PROBE_RESP) && self.scan_uid.is_some() {
            self.scan_found += 1;
        }
        self.events.push(Event::Rx { frame: f, freq, signal: -(e as i16).min(127) as i8 });
    }

    fn pump(&mut self) {
        if self.dead {
            return;
        }
        if self.trans.take_error().is_some() {
            self.fw_error();
            return;
        }
        let mut pk: Vec<Packet> = self.trans.pending.drain(..).collect();
        pk.extend(self.trans.poll_rx());
        for p in pk {
            self.handle(p);
        }
    }
}

/// sizeof(struct iwl_link_config_cmd) in Linux, which sends it for every
/// command version.
const LINK_CONFIG_SIZE: usize = 208;

fn mac_str(m: &[u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}

impl WifiDriver for Iwl {
    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn start(&mut self, mac: [u8; 6]) -> KResult<()> {
        if self.dead {
            return Err(EIO);
        }
        self.mac = mac;
        if !self.vif {
            self.mac_config(FW_CTXT_ACTION_ADD).map_err(|e| {
                log::warn!("iwlwifi: MAC add: {e}");
                EIO
            })?;
            self.link_config(FW_CTXT_ACTION_ADD).map_err(|e| {
                log::warn!("iwlwifi: link add: {e}");
                EIO
            })?;
            self.vif = true;
        }
        Ok(())
    }

    fn stop(&mut self) {
        if self.vif && !self.dead {
            let _ = self.link_config(FW_CTXT_ACTION_REMOVE);
            let _ = self.mac_config(FW_CTXT_ACTION_REMOVE);
        }
        self.vif = false;
        self.scan_uid = None;
    }

    fn scan(&mut self, req: &ScanReq) -> KResult<()> {
        if self.dead {
            return Err(EIO);
        }
        if self.scan_uid.is_some() {
            return Err(EBUSY);
        }
        self.scan_seq = (self.scan_seq + 1) & 0xff;
        let uid = self.scan_seq << 8;
        let cmd = build_scan(uid, self.mac, LINK_ID as u8, req);
        self.scan_found = 0;
        match self.send(LONG, SCAN_REQ_UMAC, &cmd) {
            Ok(_) => {
                self.scan_uid = Some(uid);
                Ok(())
            }
            Err(e) => {
                log::warn!("iwlwifi: scan request: {e}");
                Err(EIO)
            }
        }
    }

    fn abort_scan(&mut self) {
        if let Some(uid) = self.scan_uid {
            let mut c = Vec::new();
            c.extend_from_slice(&uid.to_le_bytes());
            c.extend_from_slice(&0u32.to_le_bytes());
            let _ = self.send(LONG, SCAN_ABORT_UMAC, &c);
        }
    }

    fn join(&mut self, p: &JoinParams) -> KResult<()> {
        if self.dead {
            return Err(EIO);
        }
        if self.joined.is_some() {
            self.leave();
        }
        self.do_join(p).map_err(|e| {
            log::warn!("iwlwifi: join {} failed: {e}", mac_str(&p.bssid));
            EIO
        })
    }

    fn associated(&mut self, a: &wireless::AssocInfo) -> KResult<()> {
        self.do_assoc(a).map_err(|e| {
            log::warn!("iwlwifi: association setup failed: {e}");
            EIO
        })
    }

    fn leave(&mut self) {
        if let Some(j) = self.joined.take()
            && !self.dead
        {
            let _ = self.send(MAC_CONF_GROUP, STA_REMOVE_CMD, &STA_ID.to_le_bytes());
            let mut q = alloc::vec![0u8; 12];
            q[0..4].copy_from_slice(&1u32.to_le_bytes()); // IWL_SCD_QUEUE_REMOVE
            q[4..8].copy_from_slice(&(1u32 << STA_ID).to_le_bytes());
            q[8..12].copy_from_slice(&(MGMT_TID as u32).to_le_bytes());
            let _ = self.send(DATA_PATH_GROUP, SCD_QUEUE_CONFIG_CMD, &q);
            let _ = self.link_modify(false, &j, LINK_MODIFY_ACTIVE);
            let _ = self.mac_config_assoc(None);
            let _ = self.phy_ctxt(FW_CTXT_ACTION_REMOVE, j.freq);
            log::info!("iwlwifi: left {}", mac_str(&j.bssid));
        }
    }

    fn tx_mgmt(&mut self, frame: &[u8]) -> KResult<()> {
        if self.tx_frame(frame, true) { Ok(()) } else { Err(EIO) }
    }

    fn tx_data(&mut self, frame: &[u8], _eapol: bool) -> bool {
        self.tx_frame(frame, false)
    }

    fn can_tx(&mut self) -> bool {
        self.joined.as_ref().is_none_or(|j| j.txq.space())
    }

    fn set_key(&mut self, k: &Key) -> KResult<()> {
        self.do_set_key(k).map_err(|e| {
            log::warn!("iwlwifi: key {} failed: {e}", k.idx);
            EIO
        })
    }

    fn poll(&mut self, out: &mut Vec<Event>) {
        self.pump();
        out.append(&mut self.events);
    }

    fn tx_stats(&self) -> TxStats {
        self.stats
    }
}

/// SCAN_REQ_UMAC version 17 (struct iwl_scan_req_umac_v17, 1940 bytes).
fn build_scan(uid: u32, mac: [u8; 6], link: u8, req: &ScanReq) -> Vec<u8> {
    const GP: usize = 8;
    const CP: usize = GP + 36;
    const PP: usize = CP + 540;
    const PB: usize = PP + 12;
    const SIZE: usize = PB + 1344;
    let mut c = alloc::vec![0u8; SIZE];
    let put16 = |c: &mut Vec<u8>, o: usize, v: u16| c[o..o + 2].copy_from_slice(&v.to_le_bytes());
    let put32 = |c: &mut Vec<u8>, o: usize, v: u32| c[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put32(&mut c, 0, uid);
    put32(&mut c, 4, 6); // IWL_SCAN_PRIORITY_EXT_6
    let ssids: Vec<&Vec<u8>> = req.ssids.iter().take(20).collect();
    let directed = ssids.iter().any(|s| !s.is_empty());
    // General parameters.
    let mut flags: u16 = (1 << 1) | (1 << 7); // PASS_ALL | ADAPTIVE_DWELL
    if ssids.is_empty() {
        flags |= 1 << 11; // FORCE_PASSIVE
    } else if ssids.len() == 1 && !ssids[0].is_empty() {
        flags |= 1 << 6; // USE_ALL_RX_CHAINS
    }
    put16(&mut c, GP, flags);
    c[GP + 3] = link;
    c[GP + 4] = 10;
    c[GP + 5] = 10;
    c[GP + 6] = 2;
    c[GP + 7] = 8;
    c[GP + 8] = 10;
    put16(&mut c, GP + 10, if directed { 100 } else { 300 });
    put32(&mut c, GP + 28, 6);
    c[GP + 32] = 110;
    c[GP + 33] = 110;
    // Probe request template: header, then band IEs, then common IEs.
    let b = PB;
    let mut fr = w::mgmt_header(w::STYPE_PROBE_REQ, w::BROADCAST, mac, w::BROADCAST);
    fr.extend_from_slice(&[0, 0]);
    let mut band24 = Vec::new();
    w::push_ie(&mut band24, w::EID_SUPP_RATES, &[0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);
    w::push_ie(&mut band24, w::EID_EXT_SUPP_RATES, &[0x30, 0x48, 0x60, 0x6c]);
    w::push_ie(&mut band24, w::EID_DS_PARAMS, &[0]);
    let mut band5 = Vec::new();
    w::push_ie(&mut band5, w::EID_SUPP_RATES, &[0x8c, 0x12, 0x98, 0x24, 0xb0, 0x48, 0x60, 0x6c]);
    let common = &req.ies[..req.ies.len().min(512 - 24 - band24.len() - band5.len())];
    let buf = b + 20;
    c[buf..buf + 24].copy_from_slice(&fr[..24]);
    let mut pos = 24;
    put16(&mut c, b, 0);
    put16(&mut c, b + 2, 26);
    for (i, ies) in [&band24, &band5, &Vec::new()].iter().enumerate() {
        put16(&mut c, b + 4 + i * 4, pos as u16);
        put16(&mut c, b + 6 + i * 4, ies.len() as u16);
        c[buf + pos..buf + pos + ies.len()].copy_from_slice(ies);
        pos += ies.len();
    }
    put16(&mut c, b + 16, pos as u16);
    put16(&mut c, b + 18, common.len() as u16);
    c[buf + pos..buf + pos + common.len()].copy_from_slice(common);
    // Direct SSIDs (reverse order, like iwl_scan_build_ssids).
    let ds = b + 532 + 4;
    let mut bitmap = 0u32;
    for (i, s) in ssids.iter().rev().enumerate() {
        let o = ds + i * 34;
        c[o] = w::EID_SSID;
        c[o + 1] = s.len().min(32) as u8;
        c[o + 2..o + 2 + s.len().min(32)].copy_from_slice(&s[..s.len().min(32)]);
        bitmap |= 1 << i;
    }
    // Periodic parameters: one iteration.
    c[PP + 2] = 1;
    // Channels.
    c[CP] = 1 << 5; // ENABLE_CHAN_ORDER
    c[CP + 2] = 10;
    c[CP + 3] = 2;
    let mut n = 0;
    for &f in req.freqs.iter().take(67) {
        let band: u32 = if f < 3000 { 1 } else if f < 5900 { 0 } else { continue };
        let o = CP + 4 + n * 8;
        put32(&mut c, o, bitmap | (band << 30));
        c[o + 4] = w::freq_to_chan(f);
        c[o + 6] = 1;
        n += 1;
    }
    c[CP + 1] = n as u8;
    c
}
