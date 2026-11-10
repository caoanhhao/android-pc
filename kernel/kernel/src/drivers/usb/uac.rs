//! USB Audio Class 1 capture (E8): a microphone's AudioStreaming
//! interface (an isochronous IN endpoint carrying PCM, format type I)
//! becomes an ALSA capture card (`drivers::media::alsa`).
//!
//! The alternate setting with the best PCM format is used (48 kHz if
//! offered, else the highest rate; 16-bit preferred). Starting capture
//! selects it (`isoch`, one TD per interval, each completion raising an
//! event since packet sizes vary), and sets the sampling frequency on
//! the endpoint when it has that control (SET_CUR SAMPLING_FREQ). Each
//! packet's frames go to the PCM, which converts them to what the reader
//! asked for. UAC2 devices (protocol 0x20: clock entities, rates read
//! with GET_RANGE) are recognised but not supported yet.
//!
//! Playback settings are parsed but not bound (E5 brings sound output);
//! with `usb.isoch_selftest=1` on the command line one is streamed with
//! silence for 300 ms at bring-up and the completions counted
//! ("isoch OUT self-test: ..."), which tests the isochronous transfer
//! path on QEMU's usb-audio (`run-qemu.sh --hw-test`).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::isoch::{IsochDesc, IsochParams, IsochSink};
use super::{CTRLS, MediaDev, Xhci};
use crate::drivers::media::alsa::{self, CapturePcm, CaptureSource, HwFormat};
use crate::errno::*;

const CS_INTERFACE: u8 = 0x24;
const CS_ENDPOINT: u8 = 0x25;

/// A PCM capture alternate setting of an AudioStreaming interface.
#[derive(Clone, Debug, PartialEq)]
pub struct AsAlt {
    pub iface: u8,
    pub alt: u8,
    pub channels: u8,
    pub subframe: u8,
    pub bits: u8,
    /// Discrete rates, or (min, max) for a continuous range.
    pub rates: Vec<u32>,
    pub continuous: bool,
    pub ep: IsochDesc,
    /// The endpoint has the sampling frequency control.
    pub freq_ctl: bool,
}

#[derive(Debug)]
pub struct UacFunc {
    pub ac_iface: u8,
    pub bcd_adc: u16,
    /// Input terminal types (0x0201 microphone, ...).
    pub inputs: Vec<u16>,
    pub capture: Vec<AsAlt>,
    pub playback: Vec<AsAlt>,
    pub uac2: bool,
}

fn le16(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}

fn le24(d: &[u8], i: usize) -> u32 {
    d[i] as u32 | (d[i + 1] as u32) << 8 | (d[i + 2] as u32) << 16
}

/// Parse the audio function of a configuration descriptor.
pub fn parse(cfg: &[u8]) -> Option<UacFunc> {
    let mut f = UacFunc { ac_iface: 0xff, bcd_adc: 0, inputs: Vec::new(), capture: Vec::new(), playback: Vec::new(), uac2: false };
    // Whether the last setting parsed was a capture one.
    let mut last_in = true;
    let mut cur = (0xffu8, 0u8, 0u8, 0u8, 0u8);
    // The alternate setting being described: (channels, subframe, bits,
    // rates, continuous, PCM).
    let mut fmt: Option<(u8, u8, u8, Vec<u32>, bool)> = None;
    let mut pcm = false;
    let mut i = 0;
    while i + 2 <= cfg.len() {
        let (len, ty) = (cfg[i] as usize, cfg[i + 1]);
        if len < 2 || i + len > cfg.len() {
            break;
        }
        let d = &cfg[i..i + len];
        i += len;
        match ty {
            4 if len >= 9 => {
                cur = (d[2], d[3], d[5], d[6], d[7]);
                fmt = None;
                pcm = false;
                if d[5] == 1 && d[6] == 1 && f.ac_iface == 0xff {
                    f.ac_iface = d[2];
                    f.uac2 = d[7] == 0x20;
                }
            }
            CS_INTERFACE if cur.2 == 1 && cur.3 == 1 && cur.0 == f.ac_iface && len >= 5 => match d[2] {
                1 => f.bcd_adc = le16(d, 3),
                2 if len >= 6 => f.inputs.push(le16(d, 4)),
                _ => {}
            },
            CS_INTERFACE if cur.2 == 1 && cur.3 == 2 && cur.1 > 0 && len >= 4 => match d[2] {
                // AS_GENERAL: wFormatTag PCM (UAC1).
                1 if len >= 7 => pcm = le16(d, 5) == 1,
                // FORMAT_TYPE I.
                2 if len >= 8 && d[3] == 1 => {
                    let n = d[7] as usize;
                    let (rates, cont) = if n == 0 && len >= 14 {
                        (alloc::vec![le24(d, 8), le24(d, 11)], true)
                    } else {
                        ((0..n).filter(|k| 8 + 3 * k + 3 <= len).map(|k| le24(d, 8 + 3 * k)).collect(), false)
                    };
                    fmt = Some((d[4], d[5], d[6], rates, cont));
                }
                _ => {}
            },
            5 if len >= 7 && cur.2 == 1 && cur.3 == 2 && cur.1 > 0 => {
                let (addr, attr, mps, interval) = (d[2], d[3], le16(d, 4), d[6]);
                if attr & 3 == 1
                    && pcm
                    && let Some((channels, subframe, bits, rates, continuous)) = fmt.clone()
                {
                    last_in = addr & 0x80 != 0;
                    let list = if last_in { &mut f.capture } else { &mut f.playback };
                    list.push(AsAlt {
                        iface: cur.0,
                        alt: cur.1,
                        channels,
                        subframe,
                        bits,
                        rates,
                        continuous,
                        ep: IsochDesc { addr, max_packet: mps, interval, ss_burst: 0, ss_mult: 0 },
                        freq_ctl: false,
                    });
                }
            }
            CS_ENDPOINT if len >= 4 && d[2] == 1 => {
                let list = if last_in { &mut f.capture } else { &mut f.playback };
                if let Some(a) = list.last_mut()
                    && a.iface == cur.0
                    && a.alt == cur.1
                {
                    a.freq_ctl = d[3] & 1 != 0;
                }
            }
            _ => {}
        }
    }
    (f.ac_iface != 0xff).then_some(f)
}

/// The rate to run an alternate setting at: 48 kHz if it can, else its
/// highest.
fn alt_rate(a: &AsAlt) -> u32 {
    if a.continuous {
        let (lo, hi) = (a.rates[0], a.rates[1]);
        if (lo..=hi).contains(&48000) { 48000 } else { hi }
    } else if a.rates.contains(&48000) {
        48000
    } else {
        a.rates.iter().copied().max().unwrap_or(0)
    }
}

/// The capture setting to use: 16-bit and 48 kHz preferred, mono or
/// stereo, then the most channels.
pub fn choose(caps: &[AsAlt]) -> Option<&AsAlt> {
    caps.iter()
        .filter(|a| matches!(a.subframe, 2..=4) && (1..=8).contains(&a.channels) && alt_rate(a) > 0)
        .max_by_key(|a| (alt_rate(a) == 48000, a.subframe == 2, a.channels <= 2, a.channels, alt_rate(a)))
}

struct UacSource {
    ctrl: usize,
    media: usize,
    alt: AsAlt,
    rate: u32,
    label: String,
}

struct UacSink(Arc<CapturePcm>);

/// Silence for the OUT self-test: one interval's worth of frames.
struct Silence(usize);

impl IsochSink for Silence {
    fn packet(&mut self, _: &[u8], _: bool) {}

    fn fill(&mut self, buf: &mut [u8]) -> usize {
        let n = self.0.min(buf.len());
        buf[..n].fill(0);
        n
    }
}

impl IsochSink for UacSink {
    fn packet(&mut self, data: &[u8], _err: bool) {
        if !data.is_empty() {
            self.0.push(data);
        }
    }
}

impl UacSource {
    fn with_dev<T>(&self, f: impl FnOnce(&mut Xhci, &mut MediaDev) -> T) -> KResult<T> {
        let mut ctrls = CTRLS.lock();
        let x = ctrls.get_mut(self.ctrl).ok_or(ENODEV)?;
        let mut m = x.media.get_mut(self.media).and_then(Option::take).ok_or(EBUSY)?;
        let r = f(x, &mut m);
        x.media[self.media] = Some(m);
        Ok(r)
    }
}

impl CaptureSource for UacSource {
    fn start(&self, pcm: &Arc<CapturePcm>) -> KResult<()> {
        let a = &self.alt;
        let p = IsochParams { iface: a.iface, alt: a.alt, desc: a.ep, ioc_every: 1, sentinel: false, queue_ms: 64, label: format!("uac: {}", self.label) };
        let rate = self.rate;
        let label = &self.label;
        self.with_dev(|x, m| {
            x.isoch_start(&mut m.dev, &mut m.enabled, p, Box::new(UacSink(pcm.clone()))).map_err(|code| {
                log::warn!("uac: {label}: cannot start capture (completion code {code})");
                EIO
            })?;
            if a.freq_ctl {
                let b = rate.to_le_bytes();
                unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), x.data.1, 3) };
                // SET_CUR SAMPLING_FREQ_CONTROL on the endpoint.
                if !x.control_quiet(&mut m.dev, 0x22, 0x01, 0x0100, a.ep.addr as u16, 3, true) {
                    log::warn!("uac: {label}: setting {rate} Hz failed, using the device's rate");
                }
            }
            Ok(())
        })?
    }

    fn stop(&self, _pcm: &Arc<CapturePcm>) {
        let a = &self.alt;
        let _ = self.with_dev(|x, m| x.isoch_stop(&mut m.dev, &mut m.enabled, a.iface, a.ep.addr));
    }

    fn format(&self) -> HwFormat {
        HwFormat { rate: self.rate, channels: self.alt.channels as u32, bytes: self.alt.subframe as u32 }
    }
}

impl Xhci {
    /// Bind the audio function of a configured device stored at
    /// `self.media[media]` (capture only).
    pub(super) fn uac_bind(&mut self, media: usize, f: UacFunc, name: &str, iface_dir: &dyn Fn(u8) -> String) {
        let path = self.media[media].as_ref().map_or("?".into(), |m| m.dev.topo.path.clone());
        let label = format!("usb{}-{path} audio", self.bus);
        let inputs: Vec<String> = f.inputs.iter().map(|t| format!("{t:#06x}")).collect();
        log::info!(
            "uac: {label}: ADC {:x}.{:02x}, control if{}, input terminals [{}], {} capture settings",
            f.bcd_adc >> 8,
            f.bcd_adc & 0xff,
            f.ac_iface,
            inputs.join(" "),
            f.capture.len()
        );
        if f.uac2 {
            log::warn!("uac: {label}: USB Audio Class 2, not supported");
            return;
        }
        for a in &f.capture {
            log::info!(
                "uac: {label}: if{} alt {}: {} ch, {}-bit in {} bytes, rates {:?}{}, endpoint {:02x} {} bytes every {} intervals",
                a.iface,
                a.alt,
                a.channels,
                a.bits,
                a.subframe,
                a.rates,
                if a.continuous { " (range)" } else { "" },
                a.ep.addr,
                a.ep.mps(),
                1 << (a.ep.interval.max(1) - 1)
            );
        }
        if crate::boot::cmdline().split_ascii_whitespace().any(|w| w == "usb.isoch_selftest=1")
            && let Some(p) = choose(&f.playback).cloned()
        {
            self.isoch_selftest(media, &p, &label);
        }
        let Some(a) = choose(&f.capture).cloned() else {
            log::info!("uac: {label}: no PCM capture setting");
            return;
        };
        let rate = alt_rate(&a);
        if let Some(mut m) = self.media[media].take() {
            self.control_quiet(&mut m.dev, 0x01, 11, 0, a.iface as u16, 0, true);
            self.media[media] = Some(m);
        }
        let parent = iface_dir(f.ac_iface);
        let longname = format!("{name} at usb-{}-{path}, high speed", self.pci_dir.rsplit('/').next().unwrap_or(""));
        let src = UacSource { ctrl: self.index, media, alt: a, rate, label: label.clone() };
        let short: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).take(15).collect();
        match alsa::register(if short.is_empty() { "Device" } else { &short }, name, &longname, "USB-Audio", &parent, Box::new(src)) {
            Some(c) => log::info!("uac: {label}: \"{name}\" is card {} (/dev/snd/pcmC{}D0c), {rate} Hz", c.index, c.index),
            None => log::warn!("uac: {label}: not registered"),
        }
    }
}

impl Xhci {
    /// Stream silence to a playback setting for 300 ms; log what completed.
    fn isoch_selftest(&mut self, media: usize, a: &AsAlt, label: &str) {
        let Some(mut m) = self.media[media].take() else { return };
        let rate = alt_rate(a);
        let bytes = (rate / 1000 * a.channels as u32 * a.subframe as u32) as usize;
        let p = IsochParams { iface: a.iface, alt: a.alt, desc: a.ep, ioc_every: 1, sentinel: false, queue_ms: 32, label: format!("uac: {label} out") };
        let started = self.isoch_start(&mut m.dev, &mut m.enabled, p, Box::new(Silence(bytes)));
        if let Err(code) = started {
            log::warn!("uac: {label}: isoch OUT self-test: cannot start (completion code {code})");
            self.media[media] = Some(m);
            return;
        }
        let t0 = crate::time::monotonic_ns();
        self.wait_event(300, |_| false);
        let ms = (crate::time::monotonic_ns() - t0) / 1_000_000;
        let dci = super::dci_of(a.ep.addr);
        let s = self.isochs.iter().find(|e| e.slot == m.dev.slot && e.dci == dci).map(|e| e.stats).unwrap_or_default();
        self.isoch_stop(&mut m.dev, &mut m.enabled, a.iface, a.ep.addr);
        self.media[media] = Some(m);
        log::info!(
            "uac: {label}: isoch OUT self-test: {} TDs in {ms} ms ({} bytes each), {} errors, {} ring underruns, last code {}",
            s.tds,
            bytes,
            s.errors,
            s.overruns,
            s.last_error
        );
    }
}

#[test_case]
fn uac_parse_ugreen_mic() {
    // The UGREEN Camera 4K's audio function (lsusb -v): AC interface 2
    // (UAC 1.00, a microphone input terminal), AS interface 3 alt 1:
    // PCM, 2 channels, 16-bit, 48 kHz, endpoint 83, 192 bytes,
    // bInterval 4, sampling frequency control.
    let cfg: &[u8] = &[
        9, 2, 0, 0, 4, 1, 0, 0x80, 250, //
        8, 11, 2, 2, 1, 0, 0, 4, //
        9, 4, 2, 0, 0, 1, 1, 0, 0, //
        9, 0x24, 1, 0x00, 0x01, 0x27, 0, 1, 3, //
        12, 0x24, 2, 1, 0x01, 0x02, 0, 1, 3, 0, 0, 0, //
        9, 0x24, 6, 2, 1, 2, 3, 0, 0, //
        9, 0x24, 3, 3, 0x01, 0x01, 0, 2, 0, //
        9, 4, 3, 0, 0, 1, 2, 0, 0, //
        9, 4, 3, 1, 1, 1, 2, 0, 0, //
        7, 0x24, 1, 3, 0, 1, 0, //
        11, 0x24, 2, 1, 2, 2, 16, 1, 0x80, 0xbb, 0x00, //
        9, 5, 0x83, 1, 0xc0, 0, 4, 0, 0, //
        7, 0x25, 1, 1, 0, 0, 0,
    ];
    let f = parse(cfg).unwrap();
    assert_eq!((f.ac_iface, f.bcd_adc, f.uac2), (2, 0x100, false));
    assert_eq!(f.inputs, alloc::vec![0x0201]);
    assert_eq!(f.capture.len(), 1);
    let a = &f.capture[0];
    assert_eq!((a.iface, a.alt, a.channels, a.subframe, a.bits), (3, 1, 2, 2, 16));
    assert_eq!(a.rates, alloc::vec![48000]);
    assert!(a.freq_ctl && !a.continuous);
    assert_eq!((a.ep.addr, a.ep.mps(), a.ep.interval), (0x83, 192, 4));
    assert_eq!(choose(&f.capture).map(|a| a.alt), Some(1));
    // 1 ms at 48 kHz stereo 16-bit is 192 bytes: the packet size.
    assert_eq!(super::isoch::intervals_per_sec(3, 4), 1000);
    let mut b = a.clone();
    b.rates = alloc::vec![8000, 44100];
    assert_eq!(alt_rate(&b), 44100);
    b.rates = alloc::vec![8000, 96000];
    b.continuous = true;
    assert_eq!(alt_rate(&b), 48000);
}
