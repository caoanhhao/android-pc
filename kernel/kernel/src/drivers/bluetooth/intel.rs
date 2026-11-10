//! Intel Bluetooth controllers of the TLV generation (AX201/AX211 and
//! later, hardware variant 0x17 on): bring the controller from its
//! bootloader to the operational firmware, the way Linux's btintel does
//! it (behaviour, not code).
//!
//! 1. Intel Read Version (0xfc05 with parameter 0xff) answers a list of
//!    type-length-value items: CNVi/CNVr top and BT ids, image type (1
//!    bootloader, 2 intermediate loader, 3 operational), secure boot
//!    engine type, the address in OTP, ...
//! 2. In the bootloader, the firmware file `intel/ibt-<cnvi>-<cnvr>.sfi`
//!    goes down with Intel Secure Send (0xfc09, over the bulk OUT
//!    endpoint on USB): the 128-byte CSS header (fragment type 0), the
//!    public key (3) and the signature (2) of the RSA header (bytes
//!    0..644) or of the ECDSA one (644..964), as the engine type says,
//!    then the command buffer from byte 964 on in data fragments (1),
//!    each a run of whole commands ending 4-byte aligned, split into
//!    pieces of at most 252 bytes. The bootloader reports the result in
//!    a vendor event (0xff, sub-event 0x06).
//! 3. Intel Reset (0xfc01: soft reset, patch enable, boot from the
//!    address in the firmware's Write Boot Params command) starts the
//!    firmware, which says so with vendor event 0x02 (no Command
//!    Complete for the reset).
//! 4. The DDC file's records go down with 0xfc8b, Intel's event mask
//!    with 0xfc52; a second Read Version shows the running firmware.
//!
//! Controllers already running operational firmware (a warm reboot)
//! skip 2 and 3. The firmware is built into the kernel
//! (`build.rs` copies it from the build host's linux-firmware).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// TLV item types of Intel Read Version.
const TLV_CNVI_TOP: u8 = 0x10;
const TLV_CNVR_TOP: u8 = 0x11;
const TLV_CNVI_BT: u8 = 0x12;
const TLV_IMAGE_TYPE: u8 = 0x1c;
const TLV_TIME_STAMP: u8 = 0x1d;
const TLV_BUILD_TYPE: u8 = 0x1e;
const TLV_BUILD_NUM: u8 = 0x1f;
const TLV_SECURE_BOOT: u8 = 0x28;
const TLV_MIN_FW: u8 = 0x2d;
const TLV_SBE_TYPE: u8 = 0x2f;
const TLV_OTP_BDADDR: u8 = 0x30;

pub const IMG_BOOTLOADER: u8 = 1;
pub const IMG_IML: u8 = 2;
pub const IMG_OP: u8 = 3;

const RSA_HEADER_LEN: usize = 644;
const ECDSA_HEADER_LEN: usize = 320;
const CSS_HEADER_OFFSET: usize = 8;
const RSA_HEADER_VER: u32 = 0x0001_0000;
const ECDSA_HEADER_VER: u32 = 0x0002_0000;

const OP_RESET: u16 = 0xfc01;
const OP_READ_VERSION: u16 = 0xfc05;
const OP_SECURE_SEND: u16 = 0xfc09;
const OP_WRITE_BOOT_PARAMS: u16 = 0xfc0e;
const OP_SET_EVENT_MASK: u16 = 0xfc52;
const OP_WRITE_DDC: u16 = 0xfc8b;
const OP_HCI_RESET: u16 = 0x0c03;
const OP_READ_BD_ADDR: u16 = 0x1009;

/// Vendor event sub-codes (event 0xff).
const VEV_BOOTUP: u8 = 0x02;
const VEV_SECURE_SEND_RESULT: u8 = 0x06;

const CMD_TIMEOUT_MS: u64 = 2000;
const DOWNLOAD_TIMEOUT_MS: u64 = 5000;
const BOOT_TIMEOUT_MS: u64 = 5000;

/// The built-in firmware (empty when the build host had none).
static SFI: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ibt-0040-0041.sfi"));
static DDC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ibt-0040-0041.ddc"));
/// File names linux-firmware links to that one image (AX211 CNVi/CNVr
/// combinations).
const SFI_NAMES: [&str; 6] = ["0040-0041", "1040-0041", "0040-1050", "1040-1050", "0040-4150", "1040-4150"];

/// The firmware and DDC images for `ibt-<name>`.
pub fn firmware(name: &str) -> Option<(&'static [u8], &'static [u8])> {
    (SFI_NAMES.contains(&name) && !SFI.is_empty()).then_some((SFI, DDC))
}

/// The transport the setup runs over (USB, or a mock in tests).
pub trait Hci {
    /// Send a command packet (opcode, length, parameters); `bulk`: over
    /// the bulk OUT endpoint (Secure Send in the bootloader). False if
    /// the transfer failed.
    fn send_cmd(&mut self, pkt: &[u8], bulk: bool) -> bool;
    /// The next event (code, length, parameters) within `ms`.
    fn next_event(&mut self, ms: u64) -> Option<Vec<u8>>;
    /// Whether the bulk IN endpoint carries events (bootloader mode).
    fn set_bootloader(&mut self, on: bool);
    /// Progress for the log, `stage` and a percentage.
    fn progress(&mut self, _stage: &str, _pct: u32) {}
}

/// What Intel Read Version said.
#[derive(Default, Debug, Clone)]
pub struct Version {
    pub cnvi_top: u32,
    pub cnvr_top: u32,
    pub cnvi_bt: u32,
    pub img_type: u8,
    pub timestamp: (u8, u8),
    pub build_type: u8,
    pub build_num: u32,
    pub secure_boot: u8,
    pub min_fw: (u8, u8, u8),
    pub sbe_type: u8,
    pub otp_bdaddr: [u8; 6],
}

impl Version {
    pub fn hw_variant(&self) -> u8 {
        ((self.cnvi_bt >> 16) & 0x3f) as u8
    }

    /// "0040-0041": ibt-<this>.sfi.
    pub fn fw_name(&self) -> String {
        let pack = |top: u32| ((((top & 0xfff) << 4) | ((top >> 24) & 0xf)) as u16).swap_bytes();
        format!("{:04x}-{:04x}", pack(self.cnvi_top), pack(self.cnvr_top))
    }

    pub fn image(&self) -> &'static str {
        match self.img_type {
            IMG_BOOTLOADER => "bootloader",
            IMG_IML => "intermediate loader",
            IMG_OP => "operational firmware",
            _ => "unknown image",
        }
    }
}

pub fn bdaddr_str(a: &[u8; 6]) -> String {
    format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", a[5], a[4], a[3], a[2], a[1], a[0])
}

/// Parse the return parameters of Intel Read Version (status, then the
/// TLV items). None for the legacy fixed format or a failure status.
pub fn parse_version(r: &[u8]) -> Option<Version> {
    if r.is_empty() || r[0] != 0 {
        return None;
    }
    // Legacy controllers answer a fixed 10-byte structure, hardware
    // platform 0x37 in byte 1.
    if r.len() == 10 && r[1] == 0x37 {
        return None;
    }
    let mut v = Version::default();
    let mut i = 1;
    let mut seen = false;
    while i + 2 <= r.len() {
        let (ty, len) = (r[i], r[i + 1] as usize);
        let Some(val) = r.get(i + 2..i + 2 + len) else { break };
        let le32 = || {
            let mut b = [0u8; 4];
            b[..len.min(4)].copy_from_slice(&val[..len.min(4)]);
            u32::from_le_bytes(b)
        };
        match ty {
            TLV_CNVI_TOP => (v.cnvi_top, seen) = (le32(), true),
            TLV_CNVR_TOP => v.cnvr_top = le32(),
            TLV_CNVI_BT => v.cnvi_bt = le32(),
            TLV_IMAGE_TYPE if len >= 1 => v.img_type = val[0],
            TLV_TIME_STAMP if len >= 2 => v.timestamp = (val[0], val[1]),
            TLV_BUILD_TYPE if len >= 1 => v.build_type = val[0],
            TLV_BUILD_NUM => v.build_num = le32(),
            TLV_SECURE_BOOT if len >= 1 => v.secure_boot = val[0],
            TLV_MIN_FW if len >= 3 => v.min_fw = (val[0], val[1], val[2]),
            TLV_SBE_TYPE if len >= 1 => v.sbe_type = val[0],
            TLV_OTP_BDADDR if len >= 6 => v.otp_bdaddr.copy_from_slice(&val[..6]),
            _ => {}
        }
        i += 2 + len;
    }
    seen.then_some(v)
}

/// The boot address and firmware version (build number, week, year)
/// from the Write Boot Params command in the command buffer.
pub fn boot_params(fw: &[u8]) -> Option<(u32, (u8, u8, u8))> {
    let mut p = RSA_HEADER_LEN + ECDSA_HEADER_LEN;
    while p + 3 <= fw.len() {
        let op = u16::from_le_bytes([fw[p], fw[p + 1]]);
        let len = fw[p + 2] as usize;
        if op == OP_WRITE_BOOT_PARAMS && len >= 7 && p + 3 + 7 <= fw.len() {
            let b = &fw[p + 3..];
            return Some((u32::from_le_bytes([b[0], b[1], b[2], b[3]]), (b[4], b[5], b[6])));
        }
        p += 3 + len;
    }
    None
}

/// The Secure Send commands' parameters (fragment type, then at most
/// 252 bytes) that download `fw` for secure boot engine `sbe`.
pub fn secure_send_plan(fw: &[u8], hw_variant: u8, sbe: u8) -> Result<Vec<Vec<u8>>, &'static str> {
    let le32 = |o: usize| fw.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if fw.len() < RSA_HEADER_LEN + ECDSA_HEADER_LEN {
        return Err("firmware file too short");
    }
    if le32(CSS_HEADER_OFFSET) != Some(RSA_HEADER_VER) {
        return Err("unknown CSS header version");
    }
    if hw_variant < 0x17 {
        return Err("hardware variant before 0x17 (RSA-only firmware) not supported");
    }
    if fw[RSA_HEADER_LEN] != 0x06 || le32(RSA_HEADER_LEN + CSS_HEADER_OFFSET) != Some(ECDSA_HEADER_VER) {
        return Err("no ECDSA header after the RSA one");
    }
    let mut out = Vec::new();
    let mut send = |ty: u8, mut data: &[u8]| {
        while !data.is_empty() {
            let n = data.len().min(252);
            let mut c = Vec::with_capacity(n + 1);
            c.push(ty);
            c.extend_from_slice(&data[..n]);
            out.push(c);
            data = &data[n..];
        }
    };
    match sbe {
        0 => {
            send(0x00, &fw[..128]);
            send(0x03, &fw[128..384]);
            send(0x02, &fw[388..644]);
        }
        1 => {
            let e = RSA_HEADER_LEN;
            send(0x00, &fw[e..e + 128]);
            send(0x03, &fw[e + 128..e + 224]);
            send(0x02, &fw[e + 224..e + 320]);
        }
        _ => return Err("unknown secure boot engine type"),
    }
    // Whole commands, sent whenever the run so far ends 4-byte aligned.
    let mut start = RSA_HEADER_LEN + ECDSA_HEADER_LEN;
    let mut end = start;
    while end + 3 <= fw.len() {
        end += 3 + fw[end + 2] as usize;
        if end > fw.len() {
            return Err("truncated command in the firmware");
        }
        if (end - start) % 4 == 0 {
            send(0x01, &fw[start..end]);
            start = end;
        }
    }
    if start != fw.len() {
        return Err("firmware does not end on a 4-byte aligned command");
    }
    Ok(out)
}

/// The DDC file's records (length byte, then id and value).
pub fn ddc_records(ddc: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < ddc.len() {
        let n = ddc[p] as usize + 1;
        if n < 3 || p + n > ddc.len() {
            break;
        }
        out.push(&ddc[p..p + n]);
        p += n;
    }
    out
}

fn cmd_packet(op: u16, params: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(params.len() + 3);
    c.extend_from_slice(&op.to_le_bytes());
    c.push(params.len() as u8);
    c.extend_from_slice(params);
    c
}

/// Runs commands one at a time; vendor events seen meanwhile are kept.
struct Setup<'a, H: Hci> {
    h: &'a mut H,
    vendor: Vec<Vec<u8>>,
}

impl<H: Hci> Setup<'_, H> {
    /// Send a command and wait for its Command Complete (the return
    /// parameters) or a failing Command Status.
    fn cmd(&mut self, op: u16, params: &[u8], bulk: bool) -> Result<Vec<u8>, String> {
        if !self.h.send_cmd(&cmd_packet(op, params), bulk) {
            return Err(format!("command {op:#06x}: transfer failed"));
        }
        let deadline = crate::time::monotonic_ns() + CMD_TIMEOUT_MS * 1_000_000;
        loop {
            let left = deadline.saturating_sub(crate::time::monotonic_ns()) / 1_000_000;
            let Some(ev) = self.h.next_event(left.max(1)) else {
                return Err(format!("command {op:#06x}: no answer"));
            };
            match ev.as_slice() {
                [0x0e, _, _, lo, hi, rest @ ..] if u16::from_le_bytes([*lo, *hi]) == op => return Ok(rest.to_vec()),
                [0x0f, _, status, _, lo, hi, ..] if u16::from_le_bytes([*lo, *hi]) == op => {
                    if *status != 0 {
                        return Err(format!("command {op:#06x}: status {status:#04x}"));
                    }
                }
                [0xff, ..] => self.vendor.push(ev),
                _ => {}
            }
            if crate::time::monotonic_ns() >= deadline {
                return Err(format!("command {op:#06x}: no answer"));
            }
        }
    }

    /// Like `cmd`, also failing on a non-zero status byte.
    fn cmd_ok(&mut self, op: u16, params: &[u8], bulk: bool) -> Result<Vec<u8>, String> {
        let r = self.cmd(op, params, bulk)?;
        match r.first() {
            Some(0) => Ok(r),
            Some(s) => Err(format!("command {op:#06x}: status {s:#04x}")),
            None => Err(format!("command {op:#06x}: empty answer")),
        }
    }

    /// Wait for vendor event `code`; its parameters after the sub-code.
    fn vendor_event(&mut self, code: u8, ms: u64) -> Option<Vec<u8>> {
        if let Some(i) = self.vendor.iter().position(|e| e.get(2) == Some(&code)) {
            return Some(self.vendor.remove(i)[3..].to_vec());
        }
        let deadline = crate::time::monotonic_ns() + ms * 1_000_000;
        loop {
            let left = deadline.saturating_sub(crate::time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                return None;
            }
            let ev = self.h.next_event(left)?;
            if ev.first() == Some(&0xff) && ev.get(2) == Some(&code) {
                return Some(ev[3..].to_vec());
            }
        }
    }

    fn read_version(&mut self) -> Result<Version, String> {
        let r = self.cmd(OP_READ_VERSION, &[0xff], false)?;
        parse_version(&r).ok_or_else(|| format!("Intel Read Version: unsupported answer {:02x?}", &r[..r.len().min(12)]))
    }
}

/// What the setup achieved.
pub struct Outcome {
    pub version: Version,
    pub bdaddr: Option<[u8; 6]>,
    pub downloaded: bool,
}

/// Bring an Intel controller to its operational firmware; `fw` finds the
/// firmware and DDC images for a name like "0040-0041".
pub fn setup<H: Hci>(
    h: &mut H,
    fw: impl Fn(&str) -> Option<(&'static [u8], &'static [u8])>,
) -> Result<Outcome, String> {
    let mut s = Setup { h, vendor: Vec::new() };
    s.h.set_bootloader(true);
    // The first command after power-up can go unanswered: retry after an
    // HCI Reset.
    let mut ver = s.read_version();
    for _ in 0..2 {
        if ver.is_ok() {
            break;
        }
        log::warn!("bluetooth: intel: {}, retrying after HCI Reset", ver.as_ref().err().unwrap());
        let _ = s.cmd(OP_HCI_RESET, &[], false);
        ver = s.read_version();
    }
    let ver = ver?;
    let name = ver.fw_name();
    log::info!(
        "bluetooth: intel: hardware variant {:#04x}, CNVi {:#010x} CNVr {:#010x}, {} (build {} {:02}.{:02}, type {}), secure boot {} engine {}, OTP address {}",
        ver.hw_variant(),
        ver.cnvi_top,
        ver.cnvr_top,
        ver.image(),
        ver.build_num,
        ver.timestamp.1,
        ver.timestamp.0,
        ver.build_type,
        ver.secure_boot,
        ver.sbe_type,
        bdaddr_str(&ver.otp_bdaddr)
    );
    let images = fw(&name);
    let mut downloaded = false;
    match ver.img_type {
        IMG_OP => log::info!("bluetooth: intel: operational firmware already running, no download"),
        IMG_BOOTLOADER => {
            let Some((sfi, _)) = images else {
                return Err(format!("firmware intel/ibt-{name}.sfi is not built into the kernel"));
            };
            let plan = secure_send_plan(sfi, ver.hw_variant(), ver.sbe_type)?;
            let (boot_addr, fwv) = boot_params(sfi).ok_or("no Write Boot Params command in the firmware")?;
            log::info!(
                "bluetooth: intel: downloading intel/ibt-{name}.sfi ({} bytes, version {}-{}.{}, boot address {boot_addr:#x}) in {} commands",
                sfi.len(),
                fwv.0,
                fwv.1,
                fwv.2,
                plan.len()
            );
            let t0 = crate::time::monotonic_ns();
            let mut shown = 0;
            for (i, c) in plan.iter().enumerate() {
                s.cmd_ok(OP_SECURE_SEND, c, true).map_err(|e| format!("secure send {}/{}: {e}", i + 1, plan.len()))?;
                let pct = ((i + 1) * 100 / plan.len()) as u32;
                if pct >= shown + 25 {
                    shown = pct / 25 * 25;
                    s.h.progress("download", shown);
                }
            }
            let res = s.vendor_event(VEV_SECURE_SEND_RESULT, DOWNLOAD_TIMEOUT_MS).ok_or("no firmware download result")?;
            if res.first() != Some(&0) {
                return Err(format!("firmware download failed: result {:02x?}", res));
            }
            log::info!("bluetooth: intel: firmware loaded in {} ms", (crate::time::monotonic_ns() - t0) / 1_000_000);
            // Intel Reset: soft reset, patch enable, no DDC reload, boot
            // from the given address. No Command Complete comes back.
            let mut p = alloc::vec![0x00, 0x01, 0x00, 0x01];
            p.extend_from_slice(&boot_addr.to_le_bytes());
            let t1 = crate::time::monotonic_ns();
            // The controller may not finish the transfer as it reboots:
            // the bootup event is what counts.
            if !s.h.send_cmd(&cmd_packet(OP_RESET, &p), false) {
                log::warn!("bluetooth: intel: Intel Reset transfer failed, waiting for the bootup event anyway");
            }
            s.vendor_event(VEV_BOOTUP, BOOT_TIMEOUT_MS).ok_or("firmware did not boot (no bootup event in 5 s)")?;
            log::info!("bluetooth: intel: firmware booted in {} ms", (crate::time::monotonic_ns() - t1) / 1_000_000);
            downloaded = true;
        }
        IMG_IML => return Err("intermediate loader images (hardware variant 0x1e on) not supported".into()),
        t => return Err(format!("unknown image type {t}")),
    }
    s.h.set_bootloader(false);
    let recs = images.map_or(Vec::new(), |(_, ddc)| ddc_records(ddc));
    let mut ddc_ok = 0;
    for r in &recs {
        match s.cmd_ok(OP_WRITE_DDC, r, false) {
            Ok(_) => ddc_ok += 1,
            Err(e) => log::warn!("bluetooth: intel: DDC record {:02x?}: {e}", r),
        }
    }
    log::info!("bluetooth: intel: DDC parameters: {ddc_ok} of {} records applied", recs.len());
    if let Err(e) = s.cmd_ok(OP_SET_EVENT_MASK, &[0x87, 0x0c, 0, 0, 0, 0, 0, 0], false) {
        log::warn!("bluetooth: intel: event mask: {e}");
    }
    let version = match s.read_version() {
        Ok(v) => {
            log::info!(
                "bluetooth: intel: running {} build {} {:02}.{:02}",
                v.image(),
                v.build_num,
                v.timestamp.1,
                v.timestamp.0
            );
            v
        }
        Err(e) => {
            log::warn!("bluetooth: intel: {e}");
            ver
        }
    };
    if version.img_type != IMG_OP {
        return Err(format!("still in the {} after setup", version.image()));
    }
    let bdaddr = match s.cmd_ok(OP_READ_BD_ADDR, &[], false) {
        Ok(r) if r.len() >= 7 => Some(<[u8; 6]>::try_from(&r[1..7]).unwrap()),
        _ => None,
    };
    Ok(Outcome { version, bdaddr, downloaded })
}

/// A software Intel controller for the tests: TLV version answers, the
/// bootloader's Secure Send accounting (over bulk only), the vendor
/// events, then operational mode.
#[cfg(test)]
struct FakeIntel {
    operational: bool,
    expect_bytes: usize,
    got_bytes: usize,
    fragments: Vec<u8>,
    events: alloc::collections::VecDeque<Vec<u8>>,
    ddc: usize,
    log: Vec<u16>,
    bootloader_flag: bool,
}

#[cfg(test)]
impl FakeIntel {
    fn version(&self) -> Vec<u8> {
        let mut r = alloc::vec![0u8];
        let mut tlv = |t: u8, v: &[u8]| {
            r.push(t);
            r.push(v.len() as u8);
            r.extend_from_slice(v);
        };
        // Names like the test PC's AX211: CNVi top type 0x401 step 0
        // packs to 1040, CNVr type 0x410 step 0 to 0041.
        tlv(TLV_CNVI_TOP, &0x0000_0401u32.to_le_bytes());
        tlv(TLV_CNVR_TOP, &0x0000_0410u32.to_le_bytes());
        tlv(TLV_CNVI_BT, &0x0019_2a00u32.to_le_bytes());
        tlv(TLV_IMAGE_TYPE, &[if self.operational { IMG_OP } else { IMG_BOOTLOADER }]);
        tlv(TLV_TIME_STAMP, &[0x28, 0x13]);
        tlv(TLV_BUILD_NUM, &38u32.to_le_bytes());
        tlv(TLV_SBE_TYPE, &[1]);
        tlv(TLV_OTP_BDADDR, &[6, 5, 4, 3, 2, 1]);
        r
    }

    fn complete(&mut self, op: u16, ret: &[u8]) {
        let mut e = alloc::vec![0x0e, (3 + ret.len()) as u8, 1];
        e.extend_from_slice(&op.to_le_bytes());
        e.extend_from_slice(ret);
        self.events.push_back(e);
    }
}

#[cfg(test)]
impl Hci for FakeIntel {
    fn send_cmd(&mut self, pkt: &[u8], bulk: bool) -> bool {
        let op = u16::from_le_bytes([pkt[0], pkt[1]]);
        assert_eq!(pkt[2] as usize, pkt.len() - 3);
        self.log.push(op);
        assert_eq!(bulk, op == OP_SECURE_SEND && !self.operational);
        match op {
            OP_READ_VERSION => {
                let v = self.version();
                self.complete(op, &v);
            }
            OP_SECURE_SEND => {
                assert!(!self.operational && pkt.len() <= 3 + 253);
                self.fragments.push(pkt[3]);
                if pkt[3] == 1 {
                    assert_eq!((pkt.len() - 4) % 4, 0);
                    self.got_bytes += pkt.len() - 4;
                }
                self.complete(op, &[0]);
                if self.got_bytes == self.expect_bytes && pkt[3] == 1 {
                    self.events.push_back(alloc::vec![0xff, 5, VEV_SECURE_SEND_RESULT, 0, 0x09, 0xfc, 0]);
                }
            }
            OP_RESET => {
                assert_eq!(&pkt[3..7], &[0, 1, 0, 1]);
                assert_eq!(u32::from_le_bytes(pkt[7..11].try_into().unwrap()), boot_params(SFI).unwrap().0);
                self.operational = true;
                self.events.push_back(alloc::vec![0xff, 2, VEV_BOOTUP, 0]);
            }
            OP_WRITE_DDC => {
                assert!(self.operational);
                self.ddc += 1;
                self.complete(op, &[0]);
            }
            OP_READ_BD_ADDR => self.complete(op, &[0, 6, 5, 4, 3, 2, 1]),
            _ => self.complete(op, &[0]),
        }
        true
    }

    fn next_event(&mut self, _ms: u64) -> Option<Vec<u8>> {
        self.events.pop_front()
    }

    fn set_bootloader(&mut self, on: bool) {
        self.bootloader_flag = on;
    }
}

#[test_case]
fn bluetooth_intel_version_tlv() {
    let f = FakeIntel {
        operational: false,
        expect_bytes: 0,
        got_bytes: 0,
        fragments: Vec::new(),
        events: Default::default(),
        ddc: 0,
        log: Vec::new(),
        bootloader_flag: false,
    };
    let v = parse_version(&f.version()).unwrap();
    assert_eq!(v.fw_name(), "1040-0041");
    assert_eq!(v.hw_variant(), 0x19);
    assert_eq!(v.img_type, IMG_BOOTLOADER);
    assert_eq!(v.sbe_type, 1);
    assert_eq!(bdaddr_str(&v.otp_bdaddr), "01:02:03:04:05:06");
    // Legacy answers and failures are refused.
    assert!(parse_version(&[0, 0x37, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    assert!(parse_version(&[0x01]).is_none());
    let recs = ddc_records(&[0x03, 0x28, 0x01, 0x18, 0x04, 0x29, 0x01, 0x03, 0x00]);
    assert_eq!(recs, [&[0x03, 0x28, 0x01, 0x18][..], &[0x04, 0x29, 0x01, 0x03, 0x00][..]]);
}

#[test_case]
fn bluetooth_intel_download() {
    let Some((sfi, _)) = firmware("1040-0041") else {
        log::warn!("bluetooth_intel_download: no built-in firmware, skipped");
        return;
    };
    // The image of the test PC's controller (as Linux reports it).
    assert_eq!(boot_params(sfi), Some((0x100800, (24, 29, 26))));
    let plan = secure_send_plan(sfi, 0x19, 1).unwrap();
    assert!(plan.iter().all(|c| c.len() <= 253));
    assert_eq!((plan[0][0], plan[0].len()), (0, 129));
    assert_eq!((plan[1][0], plan[1].len()), (3, 97));
    assert_eq!((plan[2][0], plan[2].len()), (2, 97));
    let payload: usize = plan.iter().filter(|c| c[0] == 1).map(|c| c.len() - 1).sum();
    assert_eq!(payload, sfi.len() - 964);
    let mut f = FakeIntel {
        operational: false,
        expect_bytes: payload,
        got_bytes: 0,
        fragments: Vec::new(),
        events: Default::default(),
        ddc: 0,
        log: Vec::new(),
        bootloader_flag: false,
    };
    let out = setup(&mut f, firmware).unwrap();
    assert!(out.downloaded);
    assert_eq!(out.version.img_type, IMG_OP);
    assert_eq!(out.bdaddr, Some([6, 5, 4, 3, 2, 1]));
    assert_eq!(f.got_bytes, payload);
    assert_eq!(&f.fragments[..3], &[0, 3, 2]);
    assert_eq!(f.ddc, 2);
    assert!(!f.bootloader_flag);
    assert_eq!(f.log.iter().filter(|&&o| o == OP_RESET).count(), 1);
    assert_eq!(f.log.last(), Some(&OP_READ_BD_ADDR));
    // Already operational (warm reboot): no download.
    let mut g = FakeIntel { operational: true, log: Vec::new(), ..f };
    let out = setup(&mut g, firmware).unwrap();
    assert!(!out.downloaded);
    assert!(!g.log.contains(&OP_SECURE_SEND));
}
