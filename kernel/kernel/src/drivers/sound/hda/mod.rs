//! Intel High Definition Audio controllers (PCI class 04.03): the
//! motherboard's analog codec (Realtek ALC897 on the target PC, QEMU's
//! `hda-output`/`hda-duplex`) and HDMI/DP audio of graphics cards.
//!
//! Bring-up: memory decoding and bus mastering, for Intel controllers
//! traffic class 0 and snooped DMA (TCSEL, DEVC.NOSNOOP), PCI power state
//! D0; controller reset (GCTL.CRST low, then high, then the codecs report
//! in STATESTS); interrupts stay off. Codec verbs go through the CORB
//! (commands) and RIRB (responses) rings, polled; if the RIRB never
//! answers the driver falls back to the immediate command registers
//! (ICOI/ICII/ICIS) for good.
//!
//! Each codec is parsed by [`codec`]; a controller with routed outputs
//! becomes a card with one playback PCM per codec (device 0 for the first
//! analog codec, 3, 7, 8, ... for HDMI codecs, as Linux numbers them) on
//! its own output stream descriptor (stream tag = index + 1). A PCM's
//! ring is a fixed 256 KiB of DMA memory split into the negotiated
//! periods by the buffer descriptor list; the position the PCM layer
//! reads is the stream's LPIB. Controls: "Master Playback Volume" and
//! "Master Playback Switch" (the paths' volume amplifiers) and a
//! read-only "<location> <device> Jack" per pin with presence detect.
//! The probe, the codec's pins and routes, stream start/stop and
//! underruns are logged (rate-limited), so the framebuffer console tells
//! what happened on a machine without serial port.

pub mod codec;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use spin::Mutex;

use super::control::{Element, Kind, TLV_DB_SCALE};
use super::pcm::{self, HwCaps, Params, PcmHw};
use crate::drivers::pci::{self, CMD_BUS_MASTER, CMD_MEMORY, wait_until};
use crate::errno::*;
use codec::{Codec, OutKind};

const GCAP: u64 = 0x00;
const VMIN: u64 = 0x02;
const GCTL: u64 = 0x08;
const WAKEEN: u64 = 0x0c;
const STATESTS: u64 = 0x0e;
const INTCTL: u64 = 0x20;
const CORBLBASE: u64 = 0x40;
const CORBUBASE: u64 = 0x44;
const CORBWP: u64 = 0x48;
const CORBRP: u64 = 0x4a;
const CORBCTL: u64 = 0x4c;
const CORBSIZE: u64 = 0x4e;
const RIRBLBASE: u64 = 0x50;
const RIRBUBASE: u64 = 0x54;
const RIRBWP: u64 = 0x58;
const RINTCNT: u64 = 0x5a;
const RIRBCTL: u64 = 0x5c;
const RIRBSTS: u64 = 0x5d;
const RIRBSIZE: u64 = 0x5e;
const ICOI: u64 = 0x60;
const ICII: u64 = 0x64;
const ICIS: u64 = 0x68;
const SD_BASE: u64 = 0x80;
const SD_STRIDE: u64 = 0x20;

const SD_CTL: u64 = 0x00;
const SD_STS: u64 = 0x03;
const SD_LPIB: u64 = 0x04;
const SD_CBL: u64 = 0x08;
const SD_LVI: u64 = 0x0c;
const SD_FMT: u64 = 0x12;
const SD_BDPL: u64 = 0x18;
const SD_BDPU: u64 = 0x1c;

const GCTL_CRST: u32 = 1;
const SD_CTL_SRST: u32 = 1;
const SD_CTL_RUN: u32 = 2;

/// The PCM ring (bytes) of each stream.
const RING_PAGES: usize = 64;
const RING_BYTES: usize = RING_PAGES * 4096;
const MAX_PERIODS: u32 = 32;
/// Verb response timeout.
const VERB_MS: u64 = 50;

fn r8(base: u64, off: u64) -> u8 {
    unsafe { core::ptr::read_volatile((base + off) as *const u8) }
}
fn w8(base: u64, off: u64, v: u8) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u8, v) }
}
fn r16(base: u64, off: u64) -> u16 {
    unsafe { core::ptr::read_volatile((base + off) as *const u16) }
}
fn w16(base: u64, off: u64, v: u16) {
    unsafe { core::ptr::write_volatile((base + off) as *mut u16, v) }
}
use pci::{mmio_read32 as r32, mmio_write32 as w32};

/// Busy wait (probe and prepare only).
fn delay_ms(ms: u64) {
    wait_until(ms, || false);
}

/// One controller's command interface.
pub struct Controller {
    regs: u64,
    pub name: String,
    corb: *mut u32,
    rirb: *const u64,
    corb_n: u16,
    rirb_n: u16,
    corb_wp: u16,
    rirb_rp: u16,
    immediate: bool,
    timeouts: u32,
}

unsafe impl Send for Controller {}

impl Controller {
    /// GCTL.CRST low then high; returns the codec mask (STATESTS).
    fn reset(&mut self) -> u16 {
        let r = self.regs;
        w16(r, STATESTS, 0x7fff);
        w32(r, GCTL, r32(r, GCTL) & !GCTL_CRST);
        if !wait_until(100, || r32(r, GCTL) & GCTL_CRST == 0) {
            log::warn!("hda {}: controller does not enter reset", self.name);
        }
        delay_ms(1);
        w32(r, GCTL, r32(r, GCTL) | GCTL_CRST);
        if !wait_until(100, || r32(r, GCTL) & GCTL_CRST != 0) {
            log::warn!("hda {}: controller does not leave reset", self.name);
        }
        // Codecs request their addresses within 521 us of the reset.
        delay_ms(1);
        wait_until(100, || r16(r, STATESTS) & 0x7fff != 0);
        r16(r, STATESTS) & 0x7fff
    }

    /// Rings of 256 entries (or what CORBSIZE/RIRBSIZE offer) in `page`.
    fn init_rings(&mut self, page: (u64, *mut u8)) {
        let r = self.regs;
        w8(r, CORBCTL, 0);
        w8(r, RIRBCTL, 0);
        wait_until(10, || r8(r, CORBCTL) & 2 == 0 && r8(r, RIRBCTL) & 2 == 0);
        let size = |cap: u8| -> (u8, u16) {
            if cap & 0x40 != 0 {
                (2, 256)
            } else if cap & 0x20 != 0 {
                (1, 16)
            } else {
                (0, 2)
            }
        };
        let (cs, cn) = size(r8(r, CORBSIZE));
        let (rs, rn) = size(r8(r, RIRBSIZE));
        w8(r, CORBSIZE, (r8(r, CORBSIZE) & !3) | cs);
        w8(r, RIRBSIZE, (r8(r, RIRBSIZE) & !3) | rs);
        let (phys, virt) = page;
        // CORB at offset 0 (1 KiB), RIRB at 2 KiB (2 KiB).
        w32(r, CORBLBASE, phys as u32);
        w32(r, CORBUBASE, (phys >> 32) as u32);
        w32(r, RIRBLBASE, (phys + 2048) as u32);
        w32(r, RIRBUBASE, ((phys + 2048) >> 32) as u32);
        w16(r, CORBWP, 0);
        // Read pointer reset: set, see it set, clear, see it clear.
        w16(r, CORBRP, 0x8000);
        wait_until(10, || r16(r, CORBRP) & 0x8000 != 0);
        w16(r, CORBRP, 0);
        wait_until(10, || r16(r, CORBRP) & 0x8000 == 0);
        w16(r, RIRBWP, 0x8000);
        w16(r, RINTCNT, 0xff);
        w8(r, RIRBSTS, 0x05);
        w8(r, CORBCTL, 0x02);
        w8(r, RIRBCTL, 0x02);
        self.corb = virt as *mut u32;
        self.rirb = unsafe { virt.add(2048) } as *const u64;
        self.corb_n = cn;
        self.rirb_n = rn;
        self.corb_wp = 0;
        self.rirb_rp = 0;
    }

    fn ring_verb(&mut self, cmd: u32) -> Option<u32> {
        let r = self.regs;
        let wp = (self.corb_wp + 1) % self.corb_n;
        unsafe { core::ptr::write_volatile(self.corb.add(wp as usize), cmd) };
        core::sync::atomic::fence(Ordering::SeqCst);
        w16(r, CORBWP, wp);
        self.corb_wp = wp;
        let mut resp = None;
        let n = self.rirb_n;
        wait_until(VERB_MS, || {
            let hw = r16(r, RIRBWP) % n;
            while self.rirb_rp != hw {
                self.rirb_rp = (self.rirb_rp + 1) % n;
                let e = unsafe { core::ptr::read_volatile(self.rirb.add(self.rirb_rp as usize)) };
                w8(r, RIRBSTS, 0x05);
                // Bit 4 of the extended word: unsolicited response.
                if (e >> 32) & 0x10 == 0 {
                    resp = Some(e as u32);
                    return true;
                }
            }
            false
        });
        resp
    }

    fn immediate_verb(&mut self, cmd: u32) -> Option<u32> {
        let r = self.regs;
        if !wait_until(VERB_MS, || r16(r, ICIS) & 1 == 0) {
            return None;
        }
        w16(r, ICIS, r16(r, ICIS) | 2);
        w32(r, ICOI, cmd);
        w16(r, ICIS, r16(r, ICIS) | 1);
        if !wait_until(VERB_MS, || r16(r, ICIS) & 3 == 2) {
            return None;
        }
        Some(r32(r, ICII))
    }

    /// Send `verb` (20 bits) to node `nid` of codec `cad`.
    pub fn verb(&mut self, cad: u8, nid: u16, verb: u32) -> Option<u32> {
        let cmd = (cad as u32) << 28 | ((nid as u32) & 0x7f) << 20 | (verb & 0xfffff);
        if !self.immediate {
            if let Some(v) = self.ring_verb(cmd) {
                return Some(v);
            }
            self.timeouts += 1;
            if self.timeouts < 3 {
                return None;
            }
            log::warn!("hda {}: CORB/RIRB does not answer, switching to immediate commands", self.name);
            w8(self.regs, CORBCTL, 0);
            w8(self.regs, RIRBCTL, 0);
            self.immediate = true;
        }
        self.immediate_verb(cmd)
    }

    pub fn param(&mut self, cad: u8, nid: u16, id: u32) -> Option<u32> {
        self.verb(cad, nid, codec::GET_PARAMETER << 8 | id)
    }
}

/// One playback PCM: a codec's outputs on an output stream descriptor.
pub struct HdaPcm {
    name: String,
    ctrl: Arc<Mutex<Controller>>,
    codec: Codec,
    /// Stream descriptor registers (mapped address).
    sd: u64,
    tag: u32,
    bdl: (u64, *mut u8),
    ring: (u64, *mut u8),
    caps: HwCaps,
    /// Master volume (steps, per channel) and switch (on = playing).
    vol: Mutex<([u32; 2], [bool; 2])>,
    log_slot: AtomicU64,
    starts: AtomicU32,
}

unsafe impl Send for HdaPcm {}
unsafe impl Sync for HdaPcm {}

impl HdaPcm {
    fn apply_volume(&self) {
        let (v, on) = *self.vol.lock();
        let mut c = self.ctrl.lock();
        self.codec.set_volume(&mut c, v, [!on[0], !on[1]]);
    }

    fn stop_reset(&self) {
        let sd = self.sd;
        w32(sd, SD_CTL, r32(sd, SD_CTL) & 0x00ff_fffc);
        wait_until(10, || r32(sd, SD_CTL) & SD_CTL_RUN == 0);
        w32(sd, SD_CTL, (r32(sd, SD_CTL) & 0x00ff_ffff) | SD_CTL_SRST);
        wait_until(50, || r32(sd, SD_CTL) & SD_CTL_SRST != 0);
        w32(sd, SD_CTL, r32(sd, SD_CTL) & 0x00ff_fffe);
        wait_until(50, || r32(sd, SD_CTL) & SD_CTL_SRST == 0);
        w8(sd, SD_STS, 0x1c);
    }
}

impl PcmHw for HdaPcm {
    fn caps(&self) -> HwCaps {
        self.caps.clone()
    }

    fn ring(&self) -> (*mut u8, usize) {
        (self.ring.1, RING_BYTES)
    }

    fn prepare(&self, p: &Params, ring_frames: u32) -> KResult<()> {
        let bits = if p.format == pcm::FORMAT_S16_LE { 16 } else if self.caps.msbits32 == 32 { 32 } else { 24 };
        let fmt = codec::stream_format(p.rate, bits, p.channels).ok_or(EINVAL)?;
        self.stop_reset();
        let sd = self.sd;
        let pb = p.period_bytes() as u64;
        // Buffer descriptor list: one entry per period of the ring
        // (buffer + pre-roll, whole periods), interrupt on completion
        // requested (harmless: stream interrupts are off).
        let entries = (ring_frames / p.period_frames).clamp(2, 256);
        let bdl = self.bdl.1 as *mut u32;
        for i in 0..entries as usize {
            let addr = self.ring.0 + i as u64 * pb;
            unsafe {
                core::ptr::write_volatile(bdl.add(i * 4), addr as u32);
                core::ptr::write_volatile(bdl.add(i * 4 + 1), (addr >> 32) as u32);
                core::ptr::write_volatile(bdl.add(i * 4 + 2), pb as u32);
                core::ptr::write_volatile(bdl.add(i * 4 + 3), 1);
            }
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        w32(sd, SD_BDPL, self.bdl.0 as u32);
        w32(sd, SD_BDPU, (self.bdl.0 >> 32) as u32);
        w32(sd, SD_CBL, (pb * entries as u64) as u32);
        w16(sd, SD_LVI, (entries - 1) as u16);
        w16(sd, SD_FMT, fmt);
        w32(sd, SD_CTL, (r32(sd, SD_CTL) & 0x000f_ffe0) | self.tag << 20);
        let mut c = self.ctrl.lock();
        self.codec.activate(&mut c);
        self.codec.setup_stream(&mut c, self.tag, fmt, p.channels);
        drop(c);
        self.apply_volume();
        Ok(())
    }

    fn trigger(&self, run: bool) {
        let sd = self.sd;
        let v = r32(sd, SD_CTL) & 0x00ff_ffff;
        w32(sd, SD_CTL, if run { v | SD_CTL_RUN } else { v & !SD_CTL_RUN });
        let n = if run { self.starts.fetch_add(1, Ordering::Relaxed) + 1 } else { self.starts.load(Ordering::Relaxed) };
        if super::ratelimit(&self.log_slot, 3000) {
            log::info!("hda {}: stream {} {} (start #{n}, fmt {:#06x})", self.name, self.tag, if run { "start" } else { "stop" }, r16(sd, SD_FMT));
        }
    }

    fn position(&self) -> u32 {
        r32(self.sd, SD_LPIB)
    }

    fn release(&self) {
        self.stop_reset();
        let mut c = self.ctrl.lock();
        self.codec.cleanup_stream(&mut c);
    }
}

/// "Master Playback Volume" / "Master Playback Switch".
struct MasterCtl {
    pcm: Arc<HdaPcm>,
    switch: bool,
    steps: u32,
}

impl Element for MasterCtl {
    fn name(&self) -> &str {
        if self.switch { "Master Playback Switch" } else { "Master Playback Volume" }
    }

    fn kind(&self) -> Kind {
        if self.switch { Kind::Boolean } else { Kind::Integer { min: 0, max: self.steps as i64 } }
    }

    fn count(&self) -> u32 {
        2
    }

    fn get(&self) -> Vec<i64> {
        let (v, on) = *self.pcm.vol.lock();
        if self.switch { on.iter().map(|&b| b as i64).collect() } else { v.iter().map(|&x| x as i64).collect() }
    }

    fn put(&self, vals: &[i64]) -> KResult<()> {
        {
            let mut st = self.pcm.vol.lock();
            for (i, &x) in vals.iter().take(2).enumerate() {
                if self.switch {
                    st.1[i] = x != 0;
                } else {
                    st.0[i] = x as u32;
                }
            }
        }
        self.pcm.apply_volume();
        Ok(())
    }

    fn tlv(&self) -> Option<Vec<u32>> {
        if self.switch {
            return None;
        }
        let (zero, step) = self.pcm.codec.volume_db();
        let min = -(zero as i32) * step;
        Some(alloc::vec![TLV_DB_SCALE, 8, min as u32, step as u32])
    }
}

/// A pin's jack presence, read-only.
struct JackCtl {
    pcm: Arc<HdaPcm>,
    pin: u16,
    name: String,
}

impl Element for JackCtl {
    fn name(&self) -> &str {
        &self.name
    }

    fn iface(&self) -> u32 {
        super::control::IFACE_CARD
    }

    fn kind(&self) -> Kind {
        Kind::Boolean
    }

    fn count(&self) -> u32 {
        1
    }

    fn writable(&self) -> bool {
        false
    }

    fn volatile(&self) -> bool {
        true
    }

    fn get(&self) -> Vec<i64> {
        let mut c = self.pcm.ctrl.lock();
        alloc::vec![self.pcm.codec.jack_present(&mut c, self.pin).unwrap_or(false) as i64]
    }

    fn put(&self, _: &[i64]) -> KResult<()> {
        Err(EPERM)
    }
}

/// PCI functions handled.
fn is_hda(d: &pci::Device) -> bool {
    d.class == 0x04 && d.subclass == 0x03
}

static CONTROLLERS: AtomicU32 = AtomicU32::new(0);

/// `snd.primary=hdmi` on the command line: the graphics card's HDMI/DP
/// audio becomes card 0, device 0 (what Android plays to) instead of the
/// motherboard's analog outputs.
fn hdmi_primary() -> bool {
    crate::boot::cmdline().split_ascii_whitespace().any(|w| w == "snd.primary=hdmi")
}

/// NVIDIA GPUs hide their HD Audio function (function 1) until bit 25 of
/// config register 0x488 of function 0 is set (Linux's
/// quirk_nvidia_hda).
fn nvidia_unhide() {
    for d in pci::devices() {
        if d.vendor != 0x10de || d.class != 0x03 || d.addr.func != 0 {
            continue;
        }
        let f1 = pci::Address { func: 1, ..d.addr };
        if pci::devices().iter().any(|x| x.addr == f1) {
            continue;
        }
        let Some(reg) = ecam(d.addr, 0x488) else {
            log::info!("hda: NVIDIA {:04x} without audio function (no ECAM to enable it)", d.device);
            continue;
        };
        let v = unsafe { core::ptr::read_volatile(reg) };
        if v & (1 << 25) == 0 {
            unsafe { core::ptr::write_volatile(reg, v | 1 << 25) };
            delay_ms(10);
        }
        match pci::rescan_function(f1) {
            Some(n) => log::info!("hda: NVIDIA {:04x} audio function {:04x}:{:04x} enabled", d.device, n.vendor, n.device),
            None => log::info!("hda: NVIDIA {:04x}: no audio function", d.device),
        }
    }
}

/// Extended configuration register `off` of `a` through ECAM, whose
/// base the Intel host bridge's PCIEXBAR (00:00.0, 0x60) holds (Core
/// platforms, QEMU's q35).
fn ecam(a: pci::Address, off: u64) -> Option<*mut u32> {
    let host = pci::Address { bus: 0, dev: 0, func: 0 };
    if host.read16(0) != 0x8086 {
        return None;
    }
    let lo = host.read32(0x60);
    if lo & 1 == 0 {
        return None;
    }
    let base = (host.read32(0x64) as u64) << 32 | (lo & 0xfc00_0000) as u64;
    let buses = match (lo >> 1) & 3 {
        0 => 256,
        1 => 128,
        2 => 64,
        _ => return None,
    };
    if a.bus as u32 >= buses {
        return None;
    }
    let page = base + ((a.bus as u64) << 20 | (a.dev as u64) << 15 | (a.func as u64) << 12);
    let va = crate::mm::paging::map_mmio(page, 4096).as_u64();
    Some((va + off) as *mut u32)
}

pub fn init() {
    nvidia_unhide();
    let mut devs: Vec<pci::Device> = pci::devices().into_iter().filter(is_hda).collect();
    if hdmi_primary() {
        devs.sort_by_key(|d| d.vendor == 0x8086);
    }
    for d in &devs {
        probe(d);
    }
}

fn probe(d: &pci::Device) {
    let a = d.addr;
    let name = format!("0000:{:02x}:{:02x}.{}", a.bus, a.dev, a.func);
    log::info!("hda {name}: {:04x}:{:04x} (class {:02x}.{:02x}.{:02x})", d.vendor, d.device, d.class, d.subclass, d.prog_if);
    // PCI power management: D0.
    if let Some(&pm) = d.capabilities(0x01).first() {
        let csr = a.read16(pm + 4);
        if csr & 3 != 0 {
            a.write16(pm + 4, csr & !3);
            delay_ms(10);
            log::info!("hda {name}: woken from D{}", csr & 3);
        }
    }
    d.enable(CMD_MEMORY | CMD_BUS_MASTER);
    if d.vendor == 0x8086 {
        // TCSEL: traffic class 0; DEVC: snooped DMA (NOSNOOP_EN off).
        a.write32(0x44, a.read32(0x44) & !7);
        a.write16(0x78, a.read16(0x78) & !(1 << 11));
    } else if d.vendor == 0x10de {
        // Coherent (snooped) transfers: TRANSREG 0x4e, OSTRM/ISTRM 0x4c/0x4d.
        let v = a.read32(0x4c);
        a.write32(0x4c, v | 0x0f << 16 | 1 << 8 | 1);
    }
    let Some((regs, _)) = pci::map_bar(d, 0) else {
        log::warn!("hda {name}: no memory BAR");
        return;
    };
    let gcap = r16(regs, GCAP);
    let (iss, oss) = (((gcap >> 8) & 0xf) as u64, ((gcap >> 12) & 0xf) as u64);
    log::info!("hda {name}: HDA {}.{}, {iss} input / {oss} output streams, 64-bit {}", r8(regs, VMIN + 1), r8(regs, VMIN), gcap & 1 != 0);
    if oss == 0 {
        return;
    }
    // Quiesce: streams, rings, interrupts.
    for s in 0..(iss + oss) {
        let sd = regs + SD_BASE + s * SD_STRIDE;
        w32(sd, SD_CTL, r32(sd, SD_CTL) & 0x00ff_fffc);
    }
    let Some(page) = pci::dma_alloc(1) else { return };
    let mut c = Controller {
        regs,
        name: name.clone(),
        corb: core::ptr::null_mut(),
        rirb: core::ptr::null(),
        corb_n: 2,
        rirb_n: 2,
        corb_wp: 0,
        rirb_rp: 0,
        immediate: false,
        timeouts: 0,
    };
    w8(regs, CORBCTL, 0);
    w8(regs, RIRBCTL, 0);
    w32(regs, INTCTL, 0);
    let mut mask = c.reset();
    w16(regs, WAKEEN, 0);
    w32(regs, INTCTL, 0);
    c.init_rings(page);
    if mask == 0 {
        // Some controllers report late; one more reset.
        delay_ms(20);
        mask = c.reset();
        c.init_rings(page);
    }
    log::info!("hda {name}: codec mask {mask:#06x}");
    let mut codecs: Vec<Codec> = Vec::new();
    for cad in 0..15u8 {
        if mask & (1 << cad) == 0 {
            continue;
        }
        let Some(mut cd) = Codec::probe(&mut c, cad) else {
            log::warn!("hda {name}: codec {cad} has no audio function group (or does not answer)");
            continue;
        };
        // Intel's display audio needs the i915 power well: not ours.
        if cd.vendor >> 16 == 0x8086 {
            log::info!("hda {name}: codec {cad}: Intel display audio {:08x}, skipped", cd.vendor);
            continue;
        }
        cd.parse_outputs();
        cd.log(&name);
        if !cd.outputs.is_empty() {
            codecs.push(cd);
        }
    }
    if codecs.is_empty() {
        return;
    }
    let ctrl = Arc::new(Mutex::new(c));
    let index = CONTROLLERS.fetch_add(1, Ordering::Relaxed);
    let first = &codecs[0];
    let analog = codecs.iter().any(|c| !c.is_hdmi());
    let (id, cname) = match (d.vendor, analog) {
        (0x10de, _) => (String::from("NVidia"), String::from("HDA NVidia")),
        (0x8086, true) => (String::from("PCH"), String::from("HDA Intel PCH")),
        (_, _) => (format!("Intel{}", if index > 0 { format!("{index}") } else { String::new() }), String::from("HDA Intel")),
    };
    let longname = format!("{cname} at {name} ({} {})", codec::vendor_name(first.vendor), codec::chip_name(first.vendor));
    let mixer = codecs.iter().map(|c| format!("{} {}", codec::vendor_name(c.vendor), codec::chip_name(c.vendor))).collect::<Vec<_>>().join(", ");
    let sysdir = pci::sysfs_dir(d, "snd_hda_intel");
    let Some(card) = super::new_card(&sysdir, &id, "HDA-Intel", &cname, &longname, &mixer) else { return };
    // HDMI PCMs are devices 3, 7, 8, ... (Linux), or 0 on the primary card.
    let mut hdmi_dev = if hdmi_primary() && card.index == 0 { 0 } else { 3 };
    let mut analog_dev = 0;
    for (i, cd) in codecs.into_iter().enumerate() {
        if i as u64 >= oss {
            break;
        }
        let device = if cd.is_hdmi() {
            let d = hdmi_dev;
            hdmi_dev = if hdmi_dev == 3 { 7 } else { hdmi_dev + 1 };
            d
        } else {
            let d = analog_dev;
            analog_dev += 1;
            d
        };
        let Some(ring) = pci::dma_alloc(RING_PAGES) else { return };
        let Some(bdl) = pci::dma_alloc(1) else { return };
        let dac_pcm = cd.dac_pcm();
        let mut formats = 0u64;
        if dac_pcm & (1 << 17) != 0 {
            formats |= 1 << pcm::FORMAT_S16_LE;
        }
        if dac_pcm & (3 << 19) != 0 {
            formats |= 1 << pcm::FORMAT_S32_LE;
        }
        if formats == 0 {
            formats = 1 << pcm::FORMAT_S16_LE;
        }
        let mut rates = codec::pcm_rates(dac_pcm);
        if rates.is_empty() {
            rates.push(48000);
        }
        let caps = HwCaps {
            formats,
            rates,
            channels_min: 2,
            channels_max: 2,
            // Half the ring: the rest is for the pre-roll.
            max_buffer_bytes: RING_BYTES as u32 / 2,
            period_align: 128,
            periods_max: MAX_PERIODS,
            msbits32: if dac_pcm & (1 << 20) != 0 { 32 } else { 24 },
        };
        let steps = cd.volume_steps();
        let zero = cd.volume_db().0.min(steps);
        let detect: Vec<(u16, String)> = cd
            .outputs
            .iter()
            .filter(|p| cd.can_detect(p.pin()))
            .map(|p| {
                let w = cd.widget(p.pin()).unwrap();
                let loc = match (w.config >> 24) & 0x3f {
                    0x02 => "Front ",
                    _ => "",
                };
                let what = match p.kind {
                    OutKind::LineOut => "Line Out",
                    OutKind::Speaker => "Speaker",
                    OutKind::Headphone => "Headphone",
                    OutKind::Hdmi => "HDMI/DP",
                };
                (p.pin(), format!("{loc}{what} Jack"))
            })
            .collect();
        let pcm_id = format!("{} {}", codec::vendor_name(cd.vendor), if cd.is_hdmi() { "HDMI" } else { "Analog" });
        let pcm_name = format!("{} {}", codec::chip_name(cd.vendor), if cd.is_hdmi() { "Digital" } else { "Analog" });
        let s = iss + i as u64;
        let hw = Arc::new(HdaPcm {
            name: name.clone(),
            ctrl: ctrl.clone(),
            codec: cd,
            sd: regs + SD_BASE + s * SD_STRIDE,
            tag: i as u32 + 1,
            bdl,
            ring,
            caps,
            vol: Mutex::new(([zero; 2], [true; 2])),
            log_slot: AtomicU64::new(0),
            starts: AtomicU32::new(0),
        });
        // Routes and volume programmed now, so the jacks are live.
        {
            let mut c = ctrl.lock();
            hw.codec.activate(&mut c);
        }
        hw.apply_volume();
        for (pin, jack) in &detect {
            let present = hw.codec.jack_present(&mut ctrl.lock(), *pin);
            let state = match present {
                Some(true) => "plugged",
                Some(false) => "empty",
                None => "?",
            };
            log::info!("hda {name}: {jack} (pin 0x{pin:02x}): {state}");
        }
        let mut seen: Vec<String> = Vec::new();
        if device == 0 || card.controls.lock().is_empty() {
            if steps > 0 {
                card.add_control(Arc::new(MasterCtl { pcm: hw.clone(), switch: false, steps }));
            }
            card.add_control(Arc::new(MasterCtl { pcm: hw.clone(), switch: true, steps }));
        }
        for (pin, mut name) in detect {
            if seen.contains(&name) {
                name = format!("{name},{pin}");
            }
            seen.push(name.clone());
            card.add_control(Arc::new(JackCtl { pcm: hw.clone(), pin, name }));
        }
        log::info!(
            "hda {name}: card {} pcm {device} ({pcm_id}): stream {} tag {}, {} DAC(s), volume 0..{steps} (0 dB at {zero})",
            card.index,
            s,
            hw.tag,
            hw.codec.dacs().len()
        );
        card.add_pcm(device, &pcm_id, &pcm_name, hw);
    }
}
