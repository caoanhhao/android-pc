//! ALSA mixer controls (`/dev/snd/controlC<card>`): the HAL turns the codec's outputs and
//! capture path on at start.
//!
//! HD Audio codecs come up muted with every volume at 0 (Linux desktops restore saved
//! levels with alsactl; Android has nothing that does). [`unmute`] switches on the output
//! and capture switches, sets output volumes to their maximum (0 dB on HDA codecs; the
//! framework applies stream volumes in software) and capture volumes to two thirds of
//! their range. Analog loopback paths that feed an input straight to the outputs
//! (`Mic Playback Switch`, `Line Playback Volume`, ...) are left alone.

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
    /// `value.integer.{min, max, step}` for integer controls.
    integer: [i64; 3],
    union_rest: [u8; 128 - 3 * 8],
    reserved: [u8; 64],
}

#[repr(C)]
struct ElemValue {
    id: ElemId,
    indirect: u32,
    /// `value.integer.value`, also used by boolean controls.
    integer: [i64; 128],
    reserved: [u8; 128],
}

const fn ioc_rw(nr: u32, size: usize) -> u32 {
    (3 << 30) | ((size as u32) << 16) | ((b'U' as u32) << 8) | nr
}

const IOCTL_ELEM_LIST: u32 = ioc_rw(0x10, size_of::<ElemList>());
const IOCTL_ELEM_INFO: u32 = ioc_rw(0x11, size_of::<ElemInfo>());
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
        let mut info = ElemInfo {
            id: ElemId::numbered(numid),
            kind: 0,
            access: 0,
            count: 0,
            owner: 0,
            integer: [0; 3],
            union_rest: [0; 128 - 3 * 8],
            reserved: [0; 64],
        };
        // SAFETY: `info` is a `struct snd_ctl_elem_info`.
        unsafe { self.ioctl(IOCTL_ELEM_INFO, &mut info) }?;
        let kind = match info.kind {
            TYPE_BOOLEAN => Kind::Boolean,
            TYPE_INTEGER => Kind::Integer,
            _ => Kind::Other,
        };
        let (min, max) = match kind {
            Kind::Boolean => (0, 1),
            Kind::Integer => (info.integer[0], info.integer[1]),
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
        })
    }

    /// Sets every value (channel) of `control` to `value`.
    ///
    /// # Errors
    ///
    /// The kernel rejects the value.
    pub fn set(&self, control: &Control, value: i64) -> io::Result<()> {
        let mut elem = ElemValue {
            id: ElemId::numbered(control.numid),
            indirect: 0,
            integer: [0; 128],
            reserved: [0; 128],
        };
        let count = (control.count as usize).min(elem.integer.len());
        elem.integer[..count].fill(value);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn control(name: &str, kind: Kind, max: i64) -> Control {
        Control { numid: 1, name: name.into(), kind, count: 2, min: 0, max, writable: true }
    }

    #[test]
    fn ioctl_numbers_match_the_kernel() {
        assert_eq!(IOCTL_ELEM_LIST, 0xc050_5510);
        assert_eq!(IOCTL_ELEM_INFO, 0xc110_5511);
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
}
