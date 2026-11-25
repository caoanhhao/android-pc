//! ALSA mixer controls (`/dev/snd/controlC<card>`): the HAL turns the codec's outputs and
//! capture path on at start, and an HDMI card's `IEC958 Playback Switch`es (Linux desktops
//! switch them on through alsactl or PulseAudio's HDMI path) when it plays to HDMI; it reads
//! the HDMI/DP jack controls to find the PCM a monitor is on.
//!
//! HD Audio codecs come up muted with every volume at 0 (Linux desktops restore saved
//! levels with alsactl; Android has nothing that does). [`unmute`] switches on the output
//! and capture switches, sets output volumes to their maximum (0 dB on HDA codecs; the
//! framework applies stream volumes in software) and capture volumes to two thirds of
//! their range. Analog loopback paths that feed an input straight to the outputs
//! (`Mic Playback Switch`, `Line Playback Volume`, ...) are left alone.
//!
//! SoC cards (ASoC on a DSP) have no such controls: their routes from the PCMs to the
//! codecs are mixer switches and muxes named by the board. A board lists them in a setup
//! file of `<control> = <value>` lines ([`parse_setup`]), which [`apply_setup`] writes at
//! start.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use log::{info, warn};

/// `SNDRV_CTL_ELEM_TYPE_*` of the controls the HAL changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Boolean,
    Integer,
    Enumerated,
    Other,
}

/// One mixer control (`snd_ctl_elem_info`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    numid: u32,
    pub name: String,
    pub kind: Kind,
    /// Values (channels) of the control.
    pub count: u32,
    pub min: i64,
    pub max: i64,
    pub writable: bool,
    /// Item names of an enumerated control.
    pub items: Vec<String>,
}

/// Words in a playback control's name that mean an input routed to the outputs.
const LOOPBACK_SOURCES: [&str; 9] =
    ["Mic", "Line Playback", "CD", "Aux", "Beep", "PC Speaker", "Phone", "Video", "Loopback"];

/// The value `unmute` gives a control, `None` to leave it as it is.
pub fn default_level(control: &Control) -> Option<i64> {
    let name = control.name.as_str();
    let playback = name.ends_with(" Playback Switch") || name.ends_with(" Playback Volume");
    let capture = name.starts_with("Capture Switch") || name.starts_with("Capture Volume");
    if !control.writable || control.max <= control.min {
        return None;
    }
    if playback && LOOPBACK_SOURCES.iter().any(|source| name.contains(source)) {
        return None;
    }
    match control.kind {
        Kind::Boolean if playback || capture => Some(1),
        Kind::Integer if playback => Some(control.max),
        Kind::Integer if capture => Some(control.min + (control.max - control.min) * 2 / 3),
        _ => None,
    }
}

// Kernel ABI (include/uapi/sound/asound.h), 64-bit layouts (`long` is i64).

const IFACE_MIXER: i32 = 2;
const TYPE_BOOLEAN: i32 = 1;
const TYPE_INTEGER: i32 = 2;
const TYPE_ENUMERATED: i32 = 3;
const ACCESS_WRITE: u32 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct ElemId {
    numid: u32,
    iface: i32,
    device: u32,
    subdevice: u32,
    name: [u8; 44],
    index: u32,
}

impl ElemId {
    fn numbered(numid: u32) -> Self {
        Self { numid, iface: 0, device: 0, subdevice: 0, name: [0; 44], index: 0 }
    }

    fn name(&self) -> String {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        String::from_utf8_lossy(&self.name[..end]).into_owned()
    }
}

#[repr(C)]
struct ElemList {
    offset: u32,
    space: u32,
    used: u32,
    count: u32,
    pids: *mut ElemId,
    reserved: [u8; 50],
}

#[repr(C)]
struct ElemInfo {
    id: ElemId,
    kind: i32,
    access: u32,
    count: u32,
    owner: i32,
    /// `value`: `integer.{min, max, step}` (i64) for integer controls,
    /// `enumerated.{items, item, name[64]}` (u32, u32, chars) for enumerated ones.
    value: [u8; 128],
    reserved: [u8; 64],
}

impl ElemInfo {
    fn numbered(numid: u32) -> Self {
        Self {
            id: ElemId::numbered(numid),
            kind: 0,
            access: 0,
            count: 0,
            owner: 0,
            value: [0; 128],
            reserved: [0; 64],
        }
    }

    fn i64_at(&self, offset: usize) -> i64 {
        i64::from_ne_bytes(self.value[offset..offset + 8].try_into().unwrap())
    }

    fn u32_at(&self, offset: usize) -> u32 {
        u32::from_ne_bytes(self.value[offset..offset + 4].try_into().unwrap())
    }

    /// `value.enumerated.name`.
    fn item_name(&self) -> String {
        let name = &self.value[8..72];
        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
        String::from_utf8_lossy(&name[..end]).into_owned()
    }
}

#[repr(C)]
struct ElemValue {
    id: ElemId,
    indirect: u32,
    /// `value.integer.value`, also used by boolean controls. Enumerated controls use the
    /// same bytes as `value.enumerated.item` (u32), see [`ElemValue::set_item`].
    integer: [i64; 128],
    reserved: [u8; 128],
}

// `set_item` packs two u32 items into each i64.
const _: () = assert!(cfg!(target_endian = "little"), "enumerated items assume little endian");

impl ElemValue {
    fn numbered(numid: u32) -> Self {
        Self { id: ElemId::numbered(numid), indirect: 0, integer: [0; 128], reserved: [0; 128] }
    }

    /// `value.enumerated.item[index]`.
    fn item(&self, index: usize) -> u32 {
        (self.integer[index / 2] >> (32 * (index % 2))) as u32
    }

    fn set_item(&mut self, index: usize, item: u32) {
        let shift = 32 * (index % 2);
        let word = &mut self.integer[index / 2];
        *word = (*word & !(0xffff_ffff_i64 << shift)) | (i64::from(item) << shift);
    }
}

const fn ioc_rw(nr: u32, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | ((b'U' as u32) << 8) | nr
}

const IOCTL_ELEM_LIST: u32 = ioc_rw(0x10, size_of::<ElemList>());
const IOCTL_ELEM_INFO: u32 = ioc_rw(0x11, size_of::<ElemInfo>());
const IOCTL_ELEM_READ: u32 = ioc_rw(0x12, size_of::<ElemValue>());
const IOCTL_ELEM_WRITE: u32 = ioc_rw(0x13, size_of::<ElemValue>());

const _: () = assert!(size_of::<ElemId>() == 64, "struct snd_ctl_elem_id");
const _: () = assert!(size_of::<ElemList>() == 80, "struct snd_ctl_elem_list");
const _: () = assert!(size_of::<ElemInfo>() == 272, "struct snd_ctl_elem_info");
const _: () = assert!(size_of::<ElemValue>() == 1224, "struct snd_ctl_elem_value");

/// The control device of one card.
pub struct Mixer {
    file: File,
}

impl Mixer {
    /// Opens `/dev/snd/controlC<card>`.
    ///
    /// # Errors
    ///
    /// The node does not exist or cannot be opened.
    pub fn open(card: u32) -> io::Result<Self> {
        let path = PathBuf::from(format!("/dev/snd/controlC{card}"));
        let file =
            pchal::devfs::open(path, pchal::devfs::Access::ReadWrite).map_err(io::Error::other)?;
        Ok(Self { file })
    }

    /// `ioctl(2)` on the control device.
    ///
    /// # Safety
    ///
    /// `arg` must point to the structure `request` encodes, valid for the kernel to read
    /// and write during the call.
    unsafe fn ioctl<T>(&self, request: u32, arg: &mut T) -> io::Result<()> {
        // SAFETY: the caller guarantees `arg` matches `request`; the fd is open.
        let ret = unsafe {
            libc::ioctl(self.file.as_raw_fd(), request as _, (arg as *mut T).cast::<libc::c_void>())
        };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Every control of the card.
    ///
    /// # Errors
    ///
    /// The kernel rejects the list or info requests.
    pub fn controls(&self) -> io::Result<Vec<Control>> {
        let mut list = ElemList {
            offset: 0,
            space: 0,
            used: 0,
            count: 0,
            pids: std::ptr::null_mut(),
            reserved: [0; 50],
        };
        // SAFETY: `list` is a `struct snd_ctl_elem_list` with no id buffer (space 0).
        unsafe { self.ioctl(IOCTL_ELEM_LIST, &mut list) }?;
        let mut ids = vec![ElemId::numbered(0); list.count as usize];
        list.space = list.count;
        list.pids = ids.as_mut_ptr();
        // SAFETY: `pids` points to `space` ids, which the kernel fills.
        unsafe { self.ioctl(IOCTL_ELEM_LIST, &mut list) }?;
        ids.truncate(list.used as usize);
        ids.iter().map(|id| self.info(id.numid)).collect()
    }

    fn info(&self, numid: u32) -> io::Result<Control> {
        let mut info = ElemInfo::numbered(numid);
        // SAFETY: `info` is a `struct snd_ctl_elem_info`.
        unsafe { self.ioctl(IOCTL_ELEM_INFO, &mut info) }?;
        let kind = match info.kind {
            TYPE_BOOLEAN => Kind::Boolean,
            TYPE_INTEGER => Kind::Integer,
            TYPE_ENUMERATED => Kind::Enumerated,
            _ => Kind::Other,
        };
        let mut items = Vec::new();
        if kind == Kind::Enumerated {
            for item in 0..info.u32_at(0) {
                let mut named = ElemInfo::numbered(numid);
                named.value[4..8].copy_from_slice(&item.to_ne_bytes());
                // SAFETY: `named` is a `struct snd_ctl_elem_info` asking for item `item`.
                unsafe { self.ioctl(IOCTL_ELEM_INFO, &mut named) }?;
                items.push(named.item_name());
            }
        }
        let (min, max) = match kind {
            Kind::Boolean => (0, 1),
            Kind::Integer => (info.i64_at(0), info.i64_at(8)),
            Kind::Enumerated => (0, items.len() as i64 - 1),
            Kind::Other => (0, 0),
        };
        Ok(Control {
            numid,
            name: info.id.name(),
            kind,
            count: info.count,
            min,
            max,
            writable: info.access & ACCESS_WRITE != 0 && info.id.iface == IFACE_MIXER,
            items,
        })
    }

    /// The first value (channel) of `control`.
    ///
    /// # Errors
    ///
    /// The kernel rejects the read.
    pub fn get(&self, control: &Control) -> io::Result<i64> {
        let mut elem = ElemValue::numbered(control.numid);
        // SAFETY: `elem` is a `struct snd_ctl_elem_value`.
        unsafe { self.ioctl(IOCTL_ELEM_READ, &mut elem) }?;
        Ok(match control.kind {
            Kind::Enumerated => i64::from(elem.item(0)),
            _ => elem.integer[0],
        })
    }

    /// Sets every value (channel) of `control` to `value`.
    ///
    /// # Errors
    ///
    /// The kernel rejects the value.
    pub fn set(&self, control: &Control, value: i64) -> io::Result<()> {
        let mut elem = ElemValue::numbered(control.numid);
        if control.kind == Kind::Enumerated {
            let item = u32::try_from(value).map_err(io::Error::other)?;
            for index in 0..(control.count as usize).min(2 * elem.integer.len()) {
                elem.set_item(index, item);
            }
        } else {
            let count = (control.count as usize).min(elem.integer.len());
            elem.integer[..count].fill(value);
        }
        // SAFETY: `elem` is a `struct snd_ctl_elem_value`.
        unsafe { self.ioctl(IOCTL_ELEM_WRITE, &mut elem) }
    }
}

/// Switches on the outputs and the capture path of `card` (see the module docs).
pub fn unmute(card: u32) {
    let result = Mixer::open(card).and_then(|mixer| {
        for control in mixer.controls()? {
            let Some(value) = default_level(&control) else { continue };
            match mixer.set(&control, value) {
                Ok(()) => info!("card {card}: {} = {value}", control.name),
                Err(err) => warn!("card {card}: cannot set {}: {err}", control.name),
            }
        }
        Ok(())
    });
    if let Err(err) = result {
        warn!("card {card}: mixer: {err}");
    }
}

/// One `<control> = <value>` line of a board's mixer setup file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub control: String,
    pub value: String,
}

/// Parses a mixer setup file: one `<control> = <value>` per line, `#` starts a comment.
/// Control names may contain spaces but no `=`. Malformed lines are skipped.
pub fn parse_setup(text: &str) -> Vec<Setting> {
    text.lines()
        .map(|line| line.split_once('#').map_or(line, |(code, _)| code).trim())
        .filter_map(|line| {
            let (control, value) = line.split_once('=')?;
            let (control, value) = (control.trim(), value.trim());
            (!control.is_empty() && !value.is_empty())
                .then(|| Setting { control: control.to_owned(), value: value.to_owned() })
        })
        .collect()
}

/// The raw value `value` means for `control`: a number, `On`/`Off` for switches, or an
/// item name (or index) of an enumerated control. `None`: not a value of this control.
pub fn resolve(control: &Control, value: &str) -> Option<i64> {
    let number = value.parse::<i64>().ok();
    let raw = match control.kind {
        Kind::Boolean if value.eq_ignore_ascii_case("on") => Some(1),
        Kind::Boolean if value.eq_ignore_ascii_case("off") => Some(0),
        Kind::Enumerated => {
            control.items.iter().position(|item| item == value).map(|index| index as i64).or(number)
        }
        Kind::Other => None,
        _ => number,
    }?;
    (control.min..=control.max).contains(&raw).then_some(raw)
}

/// Writes `settings` to `card`'s controls. Unknown controls and values are logged and
/// skipped.
pub fn apply_setup(card: u32, settings: &[Setting]) {
    let result = Mixer::open(card).and_then(|mixer| {
        let controls = mixer.controls()?;
        for setting in settings {
            let Some(control) = controls.iter().find(|c| c.name == setting.control) else {
                warn!("card {card}: no control {:?}", setting.control);
                continue;
            };
            let Some(value) = resolve(control, &setting.value) else {
                warn!("card {card}: {:?} is not a value of {:?}", setting.value, control.name);
                continue;
            };
            match mixer.set(control, value) {
                Ok(()) => info!("card {card}: {} = {}", control.name, setting.value),
                Err(err) => warn!("card {card}: cannot set {}: {err}", control.name),
            }
        }
        Ok(())
    });
    if let Err(err) = result {
        warn!("card {card}: mixer setup: {err}");
    }
}

/// The PCM devices of `card` whose `HDMI/DP,pcm=<N> Jack` is on: the codec has a monitor
/// with a valid ELD on that PCM's pin. Errors are logged and mean no jack.
pub fn hdmi_jacks(card: u32) -> Vec<u32> {
    let result = Mixer::open(card).and_then(|mixer| {
        let mut devices = Vec::new();
        for control in mixer.controls()? {
            let Some(device) = crate::cards::hdmi_jack_device(&control.name) else { continue };
            let on = mixer.get(&control)? != 0;
            info!("card {card}: {} = {}", control.name, if on { "on" } else { "off" });
            if on {
                devices.push(device);
            }
        }
        Ok(devices)
    });
    result.unwrap_or_else(|err| {
        warn!("card {card}: jacks: {err}");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(name: &str, kind: Kind, max: i64) -> Control {
        Control {
            numid: 1,
            name: name.into(),
            kind,
            count: 2,
            min: 0,
            max,
            writable: true,
            items: Vec::new(),
        }
    }

    #[test]
    fn ioctl_numbers_match_the_kernel() {
        assert_eq!(IOCTL_ELEM_LIST, 0xc050_5510);
        assert_eq!(IOCTL_ELEM_INFO, 0xc110_5511);
        assert_eq!(IOCTL_ELEM_READ, 0xc4c8_5512);
        assert_eq!(IOCTL_ELEM_WRITE, 0xc4c8_5513);
    }

    #[test]
    fn unmutes_outputs_and_capture() {
        assert_eq!(default_level(&control("Master Playback Switch", Kind::Boolean, 1)), Some(1));
        assert_eq!(default_level(&control("Master Playback Volume", Kind::Integer, 87)), Some(87));
        assert_eq!(
            default_level(&control("Line Out Playback Volume", Kind::Integer, 87)),
            Some(87)
        );
        assert_eq!(default_level(&control("Headphone Playback Switch", Kind::Boolean, 1)), Some(1));
        assert_eq!(default_level(&control("Capture Switch", Kind::Boolean, 1)), Some(1));
        assert_eq!(default_level(&control("Capture Volume", Kind::Integer, 63)), Some(42));
    }

    #[test]
    fn leaves_loopback_and_other_controls() {
        for name in [
            "Front Mic Playback Switch",
            "Rear Mic Playback Volume",
            "Line Playback Switch",
            "Beep Playback Volume",
        ] {
            assert_eq!(default_level(&control(name, Kind::Boolean, 1)), None, "{name}");
        }
        assert_eq!(default_level(&control("Front Mic Boost Volume", Kind::Integer, 3)), None);
        assert_eq!(default_level(&control("Auto-Mute Mode", Kind::Other, 0)), None);
        let read_only =
            Control { writable: false, ..control("Master Playback Switch", Kind::Boolean, 1) };
        assert_eq!(default_level(&read_only), None);
    }

    #[test]
    fn enumerated_items_pack_into_the_value_words() {
        let mut elem = ElemValue::numbered(1);
        elem.set_item(0, 3);
        elem.set_item(1, 7);
        elem.set_item(2, 1);
        assert_eq!(elem.integer[0], 7 << 32 | 3);
        assert_eq!((elem.item(0), elem.item(1), elem.item(2)), (3, 7, 1));
        elem.set_item(1, 2);
        assert_eq!((elem.item(0), elem.item(1)), (3, 2));
    }

    #[test]
    fn parses_setup_lines() {
        let text = "# Speakers\nPRIMARY_TDM_RX_0 Audio Mixer MultiMedia1 = On\n\n  VA DMIC MUX0 =DMIC3 # sub mic\nbroken line\n= 1\n";
        let settings = parse_setup(text);
        assert_eq!(
            settings,
            vec![
                Setting {
                    control: "PRIMARY_TDM_RX_0 Audio Mixer MultiMedia1".into(),
                    value: "On".into()
                },
                Setting { control: "VA DMIC MUX0".into(), value: "DMIC3".into() },
            ]
        );
    }

    #[test]
    fn resolves_values_by_kind() {
        let switch = control("X Mixer Y", Kind::Boolean, 1);
        assert_eq!(resolve(&switch, "On"), Some(1));
        assert_eq!(resolve(&switch, "off"), Some(0));
        assert_eq!(resolve(&switch, "2"), None);
        let volume = Control { min: 0, ..control("FL Digital PCM Volume", Kind::Integer, 913) };
        assert_eq!(resolve(&volume, "817"), Some(817));
        assert_eq!(resolve(&volume, "loud"), None);
        let items: Vec<String> = ["ZERO", "DMIC0", "DMIC1"].map(String::from).to_vec();
        let mux = Control { items, ..control("VA DMIC MUX0", Kind::Enumerated, 2) };
        assert_eq!(resolve(&mux, "DMIC1"), Some(2));
        assert_eq!(resolve(&mux, "1"), Some(1));
        assert_eq!(resolve(&mux, "DMIC7"), None);
    }
}
