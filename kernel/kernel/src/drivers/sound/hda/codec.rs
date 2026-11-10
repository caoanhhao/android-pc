//! HD Audio codecs: the widget graph of the audio function group and a
//! generic output parser (Linux's `hda_generic` idea, much reduced).
//!
//! Every widget's capabilities, connection list, pin capabilities and
//! pin configuration default (the board's description of each jack:
//! connectivity, location, device, color) are read. Output pins the
//! firmware marks as connected (Line Out, Speaker, Headphone; HDMI/DP
//! pins of digital codecs) are routed to an audio output converter (DAC)
//! by a depth-first search through mixers and selectors, preferring DACs
//! other pins already use, so on a Realtek ALC897 the rear Line Out
//! (0x14) and the front headphone (0x1b) both reach DAC 0x02 through
//! mixer 0x0c and play the same stream (no automute: both always play).
//! Along a path connection selects are set, mixer inputs other than the
//! path are muted (the analog loopback), amplifiers are unmuted at 0 dB,
//! pins get OUT_EN (+HP_EN for headphones) and EAPD. The volume is the
//! first amplifier with steps from the DAC side of each path.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::Controller;

// Verbs (12-bit verb << 8 | 8-bit payload, or 4-bit verb << 16 | 16-bit).
pub const GET_PARAMETER: u32 = 0xf00;
const GET_CONN_LIST: u32 = 0xf02;
const SET_CONN_SELECT: u32 = 0x701;
const SET_POWER_STATE: u32 = 0x705;
const GET_POWER_STATE: u32 = 0xf05;
pub const SET_CHANNEL_STREAMID: u32 = 0x706;
const SET_PIN_CTL: u32 = 0x707;
const GET_PIN_SENSE: u32 = 0xf09;
const EXEC_PIN_SENSE: u32 = 0x709;
const SET_EAPD: u32 = 0x70c;
const SET_DIGI_CONVERT_1: u32 = 0x70d;
const GET_SUBSYSTEM_ID: u32 = 0xf20;
const GET_CONFIG_DEFAULT: u32 = 0xf1c;
const SET_HDMI_DIP_INDEX: u32 = 0x730;
const SET_HDMI_DIP_DATA: u32 = 0x731;
const SET_HDMI_DIP_XMIT: u32 = 0x732;
const SET_CVT_CHAN_COUNT: u32 = 0x72d;
const SET_AMP: u32 = 0x3;
pub const SET_FORMAT: u32 = 0x2;

// Parameters.
const P_VENDOR_ID: u32 = 0x00;
const P_REVISION: u32 = 0x02;
const P_NODE_COUNT: u32 = 0x04;
const P_FG_TYPE: u32 = 0x05;
const P_WIDGET_CAPS: u32 = 0x09;
const P_PCM: u32 = 0x0a;
const P_PIN_CAPS: u32 = 0x0c;
const P_IN_AMP: u32 = 0x0d;
const P_CONN_LEN: u32 = 0x0e;
const P_OUT_AMP: u32 = 0x12;

pub const WT_AUDIO_OUT: u8 = 0;
const WT_AUDIO_IN: u8 = 1;
const WT_MIXER: u8 = 2;
const WT_SELECTOR: u8 = 3;
pub const WT_PIN: u8 = 4;

const WC_IN_AMP: u32 = 1 << 1;
const WC_OUT_AMP: u32 = 1 << 2;
const WC_AMP_OVERRIDE: u32 = 1 << 3;
const WC_FORMAT_OVERRIDE: u32 = 1 << 4;
const WC_CONN_LIST: u32 = 1 << 8;
const WC_DIGITAL: u32 = 1 << 9;
const WC_POWER: u32 = 1 << 10;

const PIN_TRIGGER: u32 = 1 << 1;
const PIN_DETECT: u32 = 1 << 2;
const PIN_HP_DRIVE: u32 = 1 << 3;
const PIN_OUT: u32 = 1 << 4;
const PIN_HDMI: u32 = 1 << 7;
const PIN_EAPD: u32 = 1 << 16;
const PIN_DP: u32 = 1 << 24;

const PINCTL_OUT_EN: u32 = 0x40;
const PINCTL_HP_EN: u32 = 0x80;

/// Default device field of a pin configuration.
const DEV_LINE_OUT: u32 = 0;
const DEV_SPEAKER: u32 = 1;
const DEV_HP_OUT: u32 = 2;
const DEV_DIGITAL_OUT: u32 = 5;

#[derive(Clone, Debug, Default)]
pub struct Widget {
    pub nid: u16,
    pub caps: u32,
    pub conns: Vec<u16>,
    pub pincaps: u32,
    pub config: u32,
    pub in_amp: u32,
    pub out_amp: u32,
    /// Supported sizes/rates (param 0x0a) of a converter.
    pub pcm: u32,
}

impl Widget {
    pub fn ty(&self) -> u8 {
        ((self.caps >> 20) & 0xf) as u8
    }

    fn steps(amp: u32) -> u32 {
        (amp >> 8) & 0x7f
    }

    fn offset(amp: u32) -> u32 {
        amp & 0x7f
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutKind {
    LineOut,
    Speaker,
    Headphone,
    Hdmi,
}

/// A routed output: pin first, DAC last.
#[derive(Clone, Debug)]
pub struct OutPath {
    pub kind: OutKind,
    pub nodes: Vec<u16>,
    /// The widget (and amp direction: true = output) carrying the volume.
    pub vol: Option<(u16, bool, usize)>,
}

impl OutPath {
    pub fn pin(&self) -> u16 {
        self.nodes[0]
    }

    pub fn dac(&self) -> u16 {
        *self.nodes.last().unwrap()
    }
}

pub struct Codec {
    pub cad: u8,
    pub vendor: u32,
    pub revision: u32,
    pub subsystem: u32,
    pub afg: u16,
    afg_pcm: u32,
    afg_in_amp: u32,
    afg_out_amp: u32,
    pub widgets: Vec<Widget>,
    pub outputs: Vec<OutPath>,
}

/// "Realtek", "NVIDIA", ... for the log and the card name.
pub fn vendor_name(vendor: u32) -> &'static str {
    match vendor >> 16 {
        0x10ec => "Realtek",
        0x10de => "Nvidia",
        0x8086 => "Intel",
        0x1af4 => "QEMU",
        0x1002 => "ATI",
        0x14f1 => "Conexant",
        0x111d => "IDT",
        0x11d4 => "Analog Devices",
        0x1106 => "VIA",
        0x1057 => "Motorola",
        _ => "Generic",
    }
}

/// Chip name: "ALC897", "HDMI/DP", "Generic 1af4:0012".
pub fn chip_name(vendor: u32) -> String {
    match vendor >> 16 {
        0x10ec => format!("ALC{:x}", vendor & 0xffff),
        0x10de | 0x8086 | 0x1002 => String::from("HDMI/DP"),
        _ => format!("{:04x}:{:04x}", vendor >> 16, vendor & 0xffff),
    }
}

/// A pin's location for names: "Front", "Rear", "" (internal ...).
fn location(config: u32) -> &'static str {
    match (config >> 24) & 0x3f {
        0x01 => "Rear",
        0x02 => "Front",
        0x07 | 0x08 | 0x09 => "",
        _ => "",
    }
}

const COLORS: [&str; 16] = ["?", "Black", "Grey", "Blue", "Green", "Red", "Orange", "Yellow", "Purple", "Pink", "?", "?", "?", "?", "White", "Other"];
const DEVICES: [&str; 16] = [
    "Line Out", "Speaker", "HP Out", "CD", "SPDIF Out", "Digital Out", "Modem Line", "Modem Hand", "Line In", "Aux", "Mic", "Telephony", "SPDIF In",
    "Digital In", "Reserved", "Other",
];

/// Human-readable pin configuration: "Line Out Rear Green jack".
pub fn describe_config(config: u32) -> String {
    let conn = match config >> 30 {
        0 => "jack",
        1 => "none",
        2 => "fixed",
        _ => "jack+fixed",
    };
    format!("{} {} {} {}", DEVICES[((config >> 20) & 0xf) as usize], location(config), COLORS[((config >> 12) & 0xf) as usize], conn)
}

/// HDA stream format (SDnFMT / converter format) for 16- or 32-bit
/// (`bits`; 32 means 24 significant bits if that is all the converter
/// takes) samples.
pub fn stream_format(rate: u32, bits: u32, channels: u32) -> Option<u16> {
    // (rate, base 44.1k, multiplier - 1, divisor - 1)
    let (base, mult, div) = match rate {
        8000 => (0, 0, 5),
        11025 => (1, 0, 3),
        16000 => (0, 0, 2),
        22050 => (1, 0, 1),
        32000 => (0, 1, 2),
        44100 => (1, 0, 0),
        48000 => (0, 0, 0),
        88200 => (1, 1, 0),
        96000 => (0, 1, 0),
        176400 => (1, 3, 0),
        192000 => (0, 3, 0),
        _ => return None,
    };
    let b = match bits {
        8 => 0,
        16 => 1,
        20 => 2,
        24 => 3,
        32 => 4,
        _ => return None,
    };
    if channels == 0 || channels > 16 {
        return None;
    }
    Some((base << 14 | mult << 11 | div << 8 | b << 4 | (channels - 1)) as u16)
}

/// Rates of a converter's PCM parameter, from its rate bits.
pub fn pcm_rates(pcm: u32) -> Vec<u32> {
    const R: [u32; 11] = [8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000, 176400, 192000];
    R.iter().enumerate().filter(|(i, _)| pcm & (1 << i) != 0).map(|(_, r)| *r).collect()
}

impl Codec {
    /// Read codec `cad`'s audio function group; None without one.
    pub fn probe(c: &mut Controller, cad: u8) -> Option<Codec> {
        let vendor = c.param(cad, 0, P_VENDOR_ID)?;
        let revision = c.param(cad, 0, P_REVISION).unwrap_or(0);
        let nodes = c.param(cad, 0, P_NODE_COUNT)?;
        let (start, count) = ((nodes >> 16) & 0xff, nodes & 0xff);
        let afg = (start..start + count).find(|&n| c.param(cad, n as u16, P_FG_TYPE).is_some_and(|t| t & 0xff == 1))? as u16;
        let subsystem = c.verb(cad, afg, GET_SUBSYSTEM_ID << 8).unwrap_or(0);
        // Function group to D0 (and wait for it to get there).
        c.verb(cad, afg, SET_POWER_STATE << 8);
        pci_wait(100, || c.verb(cad, afg, GET_POWER_STATE << 8).is_some_and(|s| (s >> 4) & 0xf == 0));
        let mut codec = Codec {
            cad,
            vendor,
            revision,
            subsystem,
            afg,
            afg_pcm: c.param(cad, afg, P_PCM).unwrap_or(0),
            afg_in_amp: c.param(cad, afg, P_IN_AMP).unwrap_or(0),
            afg_out_amp: c.param(cad, afg, P_OUT_AMP).unwrap_or(0),
            widgets: Vec::new(),
            outputs: Vec::new(),
        };
        let nodes = c.param(cad, afg, P_NODE_COUNT).unwrap_or(0);
        let (start, count) = ((nodes >> 16) & 0xff, nodes & 0xff);
        for nid in start..start + count {
            let nid = nid as u16;
            let caps = c.param(cad, nid, P_WIDGET_CAPS).unwrap_or(0);
            let mut w = Widget { nid, caps, ..Default::default() };
            if caps & WC_CONN_LIST != 0 {
                w.conns = codec.read_conns(c, nid);
            }
            if caps & WC_POWER != 0 {
                c.verb(cad, nid, SET_POWER_STATE << 8);
            }
            let ov = caps & WC_AMP_OVERRIDE != 0;
            if caps & WC_IN_AMP != 0 {
                w.in_amp = if ov { c.param(cad, nid, P_IN_AMP).unwrap_or(0) } else { codec.afg_in_amp };
            }
            if caps & WC_OUT_AMP != 0 {
                w.out_amp = if ov { c.param(cad, nid, P_OUT_AMP).unwrap_or(0) } else { codec.afg_out_amp };
            }
            match w.ty() {
                WT_PIN => {
                    w.pincaps = c.param(cad, nid, P_PIN_CAPS).unwrap_or(0);
                    w.config = c.verb(cad, nid, GET_CONFIG_DEFAULT << 8).unwrap_or(0);
                }
                WT_AUDIO_OUT | WT_AUDIO_IN => {
                    w.pcm = if caps & WC_FORMAT_OVERRIDE != 0 { c.param(cad, nid, P_PCM).unwrap_or(0) } else { codec.afg_pcm };
                }
                _ => {}
            }
            codec.widgets.push(w);
        }
        Some(codec)
    }

    fn read_conns(&self, c: &mut Controller, nid: u16) -> Vec<u16> {
        let len = c.param(self.cad, nid, P_CONN_LEN).unwrap_or(0);
        let long = len & 0x80 != 0;
        let n = (len & 0x7f) as usize;
        let per = if long { 2 } else { 4 };
        let (bits, range) = if long { (16, 0x8000) } else { (8, 0x80) };
        let mut out: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < n && out.len() < 64 {
            let Some(v) = c.verb(self.cad, nid, GET_CONN_LIST << 8 | i as u32) else { break };
            for k in 0..per {
                if i + k >= n {
                    break;
                }
                let e = (v >> (k * bits)) & ((1 << bits) - 1);
                let id = (e & (range - 1)) as u16;
                if e & range != 0 {
                    // A range from the previous entry up to this one.
                    if let Some(&prev) = out.last() {
                        for x in prev + 1..=id.min(prev + 32) {
                            out.push(x);
                        }
                    }
                } else {
                    out.push(id);
                }
            }
            i += per;
        }
        out
    }

    pub fn widget(&self, nid: u16) -> Option<&Widget> {
        self.widgets.iter().find(|w| w.nid == nid)
    }

    /// Whether this is a digital (HDMI/DP) codec rather than analog.
    pub fn is_hdmi(&self) -> bool {
        self.widgets.iter().any(|w| w.ty() == WT_PIN && w.pincaps & (PIN_HDMI | PIN_DP) != 0)
    }

    fn search(&self, nid: u16, depth: u32, dac_ok: &dyn Fn(&Widget) -> bool, path: &mut Vec<u16>) -> bool {
        let Some(w) = self.widget(nid) else { return false };
        if w.ty() == WT_AUDIO_OUT {
            return dac_ok(w);
        }
        if depth > 6 || !(depth == 0 && w.ty() == WT_PIN || matches!(w.ty(), WT_MIXER | WT_SELECTOR)) {
            return false;
        }
        for &n in &w.conns {
            if path.contains(&n) {
                continue;
            }
            path.push(n);
            if self.search(n, depth + 1, dac_ok, path) {
                return true;
            }
            path.pop();
        }
        false
    }

    /// Choose the output pins and route each to a DAC.
    pub fn parse_outputs(&mut self) {
        let hdmi = self.is_hdmi();
        let mut pins: Vec<(OutKind, u16, u32)> = Vec::new();
        for w in &self.widgets {
            if w.ty() != WT_PIN || w.pincaps & PIN_OUT == 0 || w.config >> 30 == 1 {
                continue;
            }
            let dev = (w.config >> 20) & 0xf;
            let kind = if w.pincaps & (PIN_HDMI | PIN_DP) != 0 {
                OutKind::Hdmi
            } else {
                match dev {
                    DEV_LINE_OUT => OutKind::LineOut,
                    DEV_SPEAKER => OutKind::Speaker,
                    DEV_HP_OUT => OutKind::Headphone,
                    DEV_DIGITAL_OUT if hdmi => OutKind::Hdmi,
                    _ => continue,
                }
            };
            // Order: association, then sequence (the firmware's ranking).
            pins.push((kind, w.nid, w.config & 0xff));
        }
        if pins.is_empty() && !hdmi {
            // No usable configuration defaults: every output-capable pin.
            for w in &self.widgets {
                if w.ty() == WT_PIN && w.pincaps & PIN_OUT != 0 {
                    pins.push((OutKind::LineOut, w.nid, 0));
                }
            }
        }
        pins.sort_by_key(|p| (p.0 as u8 == OutKind::Speaker as u8, p.2, p.1));
        let mut used: Vec<u16> = Vec::new();
        for (kind, pin, _) in pins {
            let mut path = alloc::vec![pin];
            let digital = kind == OutKind::Hdmi;
            let analog_dac = |w: &Widget| (w.caps & WC_DIGITAL != 0) == digital;
            let found = (!digital && self.search(pin, 0, &|w| analog_dac(w) && used.contains(&w.nid), &mut path))
                || {
                    path.truncate(1);
                    // HDMI pins each want their own converter.
                    self.search(pin, 0, &|w| analog_dac(w) && !(digital && used.contains(&w.nid)), &mut path)
                };
            if !found {
                continue;
            }
            let dac = *path.last().unwrap();
            if !used.contains(&dac) {
                used.push(dac);
            }
            let vol = self.volume_widget(&path);
            self.outputs.push(OutPath { kind, nodes: path, vol });
        }
    }

    /// The first amplifier with gain steps from the DAC end of `path`:
    /// (nid, output amp, input index).
    fn volume_widget(&self, path: &[u16]) -> Option<(u16, bool, usize)> {
        for i in (0..path.len()).rev() {
            let w = self.widget(path[i])?;
            if w.caps & WC_OUT_AMP != 0 && Widget::steps(w.out_amp) > 0 {
                return Some((w.nid, true, 0));
            }
            if i + 1 < path.len() && w.caps & WC_IN_AMP != 0 && Widget::steps(w.in_amp) > 0 {
                let idx = w.conns.iter().position(|&n| n == path[i + 1])?;
                return Some((w.nid, false, idx));
            }
        }
        None
    }

    fn amp_caps(&self, nid: u16, out: bool) -> u32 {
        self.widget(nid).map_or(0, |w| if out { w.out_amp } else { w.in_amp })
    }

    /// Set an amplifier (both channels, or one: `ch` 0 left / 1 right).
    pub fn set_amp(&self, c: &mut Controller, nid: u16, out: bool, idx: usize, ch: Option<u32>, mute: bool, gain: u32) {
        let dir = if out { 1 << 15 } else { 1 << 14 };
        let lr = match ch {
            Some(0) => 1 << 13,
            Some(_) => 1 << 12,
            None => 3 << 12,
        };
        let payload = dir | lr | ((idx as u32 & 0xf) << 8) | (mute as u32) << 7 | (gain & 0x7f);
        c.verb(self.cad, nid, SET_AMP << 16 | payload);
    }

    /// Unmute an amplifier at 0 dB (its offset step).
    fn amp_on(&self, c: &mut Controller, nid: u16, out: bool, idx: usize) {
        let caps = self.amp_caps(nid, out);
        self.set_amp(c, nid, out, idx, None, false, Widget::offset(caps));
    }

    /// Program every routed path: selects, amps, pins, EAPD.
    pub fn activate(&self, c: &mut Controller) {
        for p in &self.outputs {
            for i in 0..p.nodes.len() {
                let Some(w) = self.widget(p.nodes[i]) else { continue };
                let next = p.nodes.get(i + 1).and_then(|n| w.conns.iter().position(|x| x == n));
                if let Some(idx) = next {
                    if w.ty() == WT_MIXER {
                        for k in 0..w.conns.len() {
                            if w.caps & WC_IN_AMP != 0 {
                                if k == idx {
                                    self.amp_on(c, w.nid, false, k);
                                } else {
                                    let off = Widget::offset(w.in_amp);
                                    self.set_amp(c, w.nid, false, k, None, true, off);
                                }
                            }
                        }
                    } else {
                        if w.conns.len() > 1 {
                            c.verb(self.cad, w.nid, SET_CONN_SELECT << 8 | idx as u32);
                        }
                        if w.caps & WC_IN_AMP != 0 {
                            self.amp_on(c, w.nid, false, idx);
                        }
                    }
                }
                if w.caps & WC_OUT_AMP != 0 {
                    self.amp_on(c, w.nid, true, 0);
                }
                if w.caps & WC_POWER != 0 {
                    c.verb(self.cad, w.nid, SET_POWER_STATE << 8);
                }
                if w.ty() == WT_PIN {
                    let mut ctl = PINCTL_OUT_EN;
                    if p.kind == OutKind::Headphone && w.pincaps & PIN_HP_DRIVE != 0 {
                        ctl |= PINCTL_HP_EN;
                    }
                    c.verb(self.cad, w.nid, SET_PIN_CTL << 8 | ctl);
                    if w.pincaps & PIN_EAPD != 0 {
                        c.verb(self.cad, w.nid, SET_EAPD << 8 | 0x02);
                    }
                }
                if w.ty() == WT_AUDIO_OUT && w.caps & WC_DIGITAL != 0 {
                    c.verb(self.cad, w.nid, SET_DIGI_CONVERT_1 << 8 | 0x01);
                }
            }
        }
    }

    /// The DACs of the routed outputs.
    pub fn dacs(&self) -> Vec<u16> {
        let mut v: Vec<u16> = Vec::new();
        for p in &self.outputs {
            if !v.contains(&p.dac()) {
                v.push(p.dac());
            }
        }
        v
    }

    /// Sizes/rates every DAC supports (param 0x0a bits).
    pub fn dac_pcm(&self) -> u32 {
        self.dacs().iter().filter_map(|&d| self.widget(d)).fold(!0, |a, w| a & w.pcm)
    }

    /// Start the converters on stream `tag` with format `fmt`.
    pub fn setup_stream(&self, c: &mut Controller, tag: u32, fmt: u16, channels: u32) {
        for d in self.dacs() {
            c.verb(self.cad, d, SET_CHANNEL_STREAMID << 8 | tag << 4);
            c.verb(self.cad, d, SET_FORMAT << 16 | fmt as u32);
        }
        if self.is_hdmi() {
            for p in &self.outputs {
                self.hdmi_infoframe(c, p.pin(), channels);
                c.verb(self.cad, p.dac(), SET_CVT_CHAN_COUNT << 8 | (channels - 1));
            }
        }
    }

    pub fn cleanup_stream(&self, c: &mut Controller) {
        for d in self.dacs() {
            c.verb(self.cad, d, SET_CHANNEL_STREAMID << 8);
        }
    }

    /// The audio InfoFrame (HDMI) / SDP (DP) of a 2-channel PCM stream.
    fn hdmi_infoframe(&self, c: &mut Controller, pin: u16, channels: u32) {
        let dp = self.widget(pin).is_some_and(|w| w.pincaps & PIN_DP != 0 && w.pincaps & PIN_HDMI == 0);
        let mut f = [0u8; 14];
        if dp {
            // DP: header 0x00, 0x84 (audio), 0x1b, 0x11 << 2 (version).
            f[..4].copy_from_slice(&[0x00, 0x84, 0x1b, 0x44]);
            f[4] = (channels - 1) as u8;
        } else {
            f[..3].copy_from_slice(&[0x84, 0x01, 0x0a]);
            f[4] = (channels - 1) as u8;
            let sum = f.iter().fold(0u8, |a, b| a.wrapping_add(*b));
            f[3] = 0u8.wrapping_sub(sum);
        }
        c.verb(self.cad, pin, SET_HDMI_DIP_INDEX << 8);
        for b in f {
            c.verb(self.cad, pin, SET_HDMI_DIP_DATA << 8 | b as u32);
        }
        c.verb(self.cad, pin, SET_HDMI_DIP_XMIT << 8 | 0xc0);
    }

    /// Jack presence of a pin (None if it cannot tell).
    pub fn jack_present(&self, c: &mut Controller, pin: u16) -> Option<bool> {
        let w = self.widget(pin)?;
        if w.pincaps & PIN_DETECT == 0 || w.config & (1 << 8) != 0 {
            return None;
        }
        if w.pincaps & PIN_TRIGGER != 0 {
            c.verb(self.cad, pin, EXEC_PIN_SENSE << 8);
        }
        c.verb(self.cad, pin, GET_PIN_SENSE << 8).map(|s| s & (1 << 31) != 0)
    }

    pub fn can_detect(&self, pin: u16) -> bool {
        self.widget(pin).is_some_and(|w| w.pincaps & PIN_DETECT != 0 && w.config & (1 << 8) == 0)
    }

    /// Volume range (steps) common to the paths' volume amps; 0 if none.
    pub fn volume_steps(&self) -> u32 {
        self.outputs
            .iter()
            .filter_map(|p| p.vol)
            .map(|(nid, out, _)| Widget::steps(self.amp_caps(nid, out)))
            .min()
            .unwrap_or(0)
    }

    /// 0 dB as a volume step, and the step size in 0.01 dB.
    pub fn volume_db(&self) -> (u32, i32) {
        let Some((nid, out, _)) = self.outputs.iter().find_map(|p| p.vol) else { return (0, 0) };
        let caps = self.amp_caps(nid, out);
        (Widget::offset(caps), (((caps >> 16) & 0x7f) as i32 + 1) * 25)
    }

    /// Apply volume (per channel steps) and mute to the paths.
    pub fn set_volume(&self, c: &mut Controller, vol: [u32; 2], mute: [bool; 2]) {
        let mut done: Vec<(u16, bool, usize)> = Vec::new();
        for p in &self.outputs {
            let Some(v) = p.vol else { continue };
            if done.contains(&v) {
                continue;
            }
            done.push(v);
            let steps = Widget::steps(self.amp_caps(v.0, v.1));
            for ch in 0..2u32 {
                self.set_amp(c, v.0, v.1, v.2, Some(ch), mute[ch as usize], vol[ch as usize].min(steps));
            }
        }
        // No volume amp: mute at the pins.
        for p in self.outputs.iter().filter(|p| p.vol.is_none()) {
            if let Some(w) = self.widget(p.pin()).filter(|w| w.caps & WC_OUT_AMP != 0) {
                for ch in 0..2u32 {
                    self.set_amp(c, w.nid, true, 0, Some(ch), mute[ch as usize], Widget::offset(w.out_amp));
                }
            }
        }
    }

    /// Log the codec and its routes (on the framebuffer console too).
    pub fn log(&self, ctrl: &str) {
        log::info!(
            "hda {ctrl}: codec {}: {} {} ({:04x}:{:04x} rev {:06x} subsys {:08x}), AFG 0x{:02x}, {} widgets",
            self.cad,
            vendor_name(self.vendor),
            chip_name(self.vendor),
            self.vendor >> 16,
            self.vendor & 0xffff,
            self.revision,
            self.subsystem,
            self.afg,
            self.widgets.len()
        );
        for w in self.widgets.iter().filter(|w| w.ty() == WT_PIN && w.config >> 30 != 1) {
            log::info!("hda {ctrl}:   pin 0x{:02x} {:08x} {} caps {:08x}", w.nid, w.config, describe_config(w.config), w.pincaps);
        }
        for p in &self.outputs {
            let route: Vec<String> = p.nodes.iter().map(|n| format!("0x{n:02x}")).collect();
            log::info!("hda {ctrl}:   {:?}: {}", p.kind, route.join(" -> "));
        }
        if self.outputs.is_empty() {
            log::warn!("hda {ctrl}:   codec {}: no output path found", self.cad);
        }
    }
}

fn pci_wait(ms: u64, f: impl FnMut() -> bool) -> bool {
    crate::drivers::pci::wait_until(ms, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn hda_stream_formats() {
        assert_eq!(stream_format(48000, 16, 2), Some(0x0011));
        assert_eq!(stream_format(44100, 16, 2), Some(0x4011));
        assert_eq!(stream_format(96000, 32, 2), Some(0x0841));
        assert_eq!(stream_format(8000, 16, 1), Some(0x0510));
        assert_eq!(stream_format(12345, 16, 2), None);
        assert_eq!(pcm_rates(0x60), [44100, 48000]);
        assert_eq!(describe_config(0x01014010), "Line Out Rear Green jack");
        assert_eq!(describe_config(0x02214020), "HP Out Front Green jack");
    }
}
