//! Sound: ALSA cards with Linux's character device ABI, so tinyalsa (and
//! through it the unmodified AOSP audio HAL) drives them.
//!
//! A card has one control device (`/dev/snd/controlC<c>`, 116:32c) and
//! PCM devices (`/dev/snd/pcmC<c>D<d>p`, 116:32c+16+d; capture
//! `pcmC<c>D<d>c`, 116:32c+24+d), Linux's static minors. Each appears in sysfs below its parent
//! device as `sound/card<c>/<node>` (class "sound", DEVNAME=snd/<node>, so
//! ueventd creates /dev/snd/* with the `system:audio 0660` of its rules)
//! and in devtmpfs. /proc/asound has `cards`, `devices`, `pcm` and
//! `version` as Linux formats them.
//!
//! The card numbering follows probe order, so the motherboard's HD Audio
//! controller (bus 0) is card 0: the device the Cuttlefish primary HAL
//! (`StreamPrimary`: card 0, device 0, tinyalsa writei) plays to.
//!
//! [`pcm`] is the PCM ABI over a driver's [`pcm::PcmHw`], [`control`] the
//! mixer control ABI over [`control::Element`]s; [`hda`] is the Intel HD
//! Audio driver providing both. [`capture`] is the capture PCM ABI over a
//! [`capture::CaptureSource`] pushing packets (USB microphones, E8).
//!
//! Recording on Android: the same primary HAL records its "Built-In Mic"
//! from card 0 device 0 (`pcmC0D0c`). HD Audio provides no capture PCM,
//! so when a microphone card registers ([`register_capture`]) and card 0
//! has no capture device 0, card 0 gets a `pcmC0D0c` node for that
//! microphone too (the first one found; it is also on its own card). On a
//! PC with HD Audio and a USB webcam, Android plays through HD Audio and
//! records from the webcam. Sound is probed before USB, so HD Audio
//! stays card 0.

pub mod capture;
pub mod control;
pub mod hda;
pub mod pcm;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::errno::*;
use crate::fs::file::FileOps;

pub const SOUND_MAJOR: u32 = 116;
/// Minors per card (Linux's static scheme, CONFIG_SND_DYNAMIC_MINORS=n).
const MINORS_PER_CARD: u32 = 32;
const MAX_CARDS: u32 = 8;

pub struct Card {
    pub index: u32,
    /// Short id ("PCH", "NVidia"), driver name, names for CARD_INFO.
    pub id: String,
    pub driver: String,
    pub name: String,
    pub longname: String,
    pub mixername: String,
    /// The card's sysfs directory (relative to /sys).
    sysdir: String,
    pub pcms: Mutex<Vec<Arc<pcm::PcmDevice>>>,
    /// Capture devices (device number, PCM; possibly another card's).
    pub captures: Mutex<Vec<(u32, Arc<capture::CapturePcm>)>>,
    pub controls: Mutex<Vec<Arc<dyn control::Element>>>,
}

static CARDS: Mutex<Vec<Arc<Card>>> = Mutex::new(Vec::new());

/// Create card `CARDS.len()` below the sysfs device directory `parent`
/// (with its control device; PCMs and controls are added afterwards).
pub fn new_card(parent: &str, id: &str, driver: &str, name: &str, longname: &str, mixername: &str) -> Option<Arc<Card>> {
    let mut cards = CARDS.lock();
    let index = cards.len() as u32;
    if index >= MAX_CARDS {
        return None;
    }
    let sysdir = format!("{parent}/sound/card{index}");
    crate::fs::sysfs::add_device(&sysdir, "sound", "class/sound", String::new(), true);
    crate::fs::sysfs::set(&sysdir, "id", &format!("{id}\n"));
    crate::fs::sysfs::set(&sysdir, "number", &format!("{index}\n"));
    crate::fs::sysfs::symlink(&sysdir, "device", "../..");
    let card = Arc::new(Card {
        index,
        id: id.into(),
        driver: driver.into(),
        name: name.into(),
        longname: longname.into(),
        mixername: mixername.into(),
        sysdir,
        pcms: Mutex::new(Vec::new()),
        captures: Mutex::new(Vec::new()),
        controls: Mutex::new(Vec::new()),
    });
    cards.push(card.clone());
    drop(cards);
    card.add_node(&format!("controlC{index}"), index * MINORS_PER_CARD);
    Some(card)
}

impl Card {
    fn add_node(&self, name: &str, minor: u32) {
        let path = format!("{}/{name}", self.sysdir);
        crate::fs::sysfs::add_char_device(&path, "sound", name, &format!("snd/{name}"), SOUND_MAJOR, minor);
        crate::fs::sysfs::symlink(&path, "device", "..");
        devtmpfs_node(name, minor);
    }

    /// Register PCM playback device `device` driven by `hw`.
    pub fn add_pcm(self: &Arc<Self>, device: u32, id: &str, name: &str, hw: Arc<dyn pcm::PcmHw>) {
        let dev = Arc::new(pcm::PcmDevice::new(self.index, device, id, name, hw));
        let node = format!("pcmC{}D{device}p", self.index);
        self.add_node(&node, self.index * MINORS_PER_CARD + 16 + device);
        crate::fs::sysfs::set(&format!("{}/{node}", self.sysdir), "pcm_class", "generic\n");
        self.pcms.lock().push(dev);
    }

    /// Register capture device `device` recording from `pcm`.
    pub fn add_capture(&self, device: u32, pcm: Arc<capture::CapturePcm>) {
        let node = format!("pcmC{}D{device}c", self.index);
        self.add_node(&node, self.index * MINORS_PER_CARD + 24 + device);
        crate::fs::sysfs::set(&format!("{}/{node}", self.sysdir), "pcm_class", "generic\n");
        self.captures.lock().push((device, pcm));
    }

    pub fn capture(&self, device: u32) -> Option<Arc<capture::CapturePcm>> {
        self.captures.lock().iter().find(|(d, _)| *d == device).map(|(_, p)| p.clone())
    }

    pub fn add_control(&self, e: Arc<dyn control::Element>) {
        self.controls.lock().push(e);
    }

    pub fn pcm(&self, device: u32) -> Option<Arc<pcm::PcmDevice>> {
        self.pcms.lock().iter().find(|p| p.device == device).cloned()
    }
}

pub fn card(index: u32) -> Option<Arc<Card>> {
    CARDS.lock().get(index as usize).cloned()
}

/// /dev/snd/<name> in devtmpfs.
fn devtmpfs_node(name: &str, minor: u32) {
    use crate::fs::vfs::{S_IFCHR, S_IFDIR, encode_dev};
    let root = crate::fs::devtmpfs_root();
    let dir = match root.get(b"snd") {
        Some(d) => Some(d),
        None => root.add(b"snd", S_IFDIR | 0o755, 0, None).ok(),
    };
    if let Some(d) = dir {
        let _ = d.add(name.as_bytes(), S_IFCHR | 0o660, encode_dev(SOUND_MAJOR, minor), None);
    }
}

/// Open of a 116:minor node.
pub fn open(minor: u32) -> KResult<Box<dyn FileOps>> {
    let c = card(minor / MINORS_PER_CARD).ok_or(ENODEV)?;
    match minor % MINORS_PER_CARD {
        0 => Ok(Box::new(control::ControlFile::new(c))),
        m @ 16..=23 => pcm::open(c.pcm(m - 16).ok_or(ENODEV)?),
        m @ 24..=31 => capture::open(c.capture(m - 24).ok_or(ENODEV)?),
        _ => Err(ENODEV),
    }
}

/// A card with one capture PCM (device 0) recording from `source`, below
/// the sysfs device directory `parent`; card 0 gets it as its capture
/// device 0 too if it has none (see the module doc).
pub fn register_capture(parent: &str, id: &str, driver: &str, name: &str, longname: &str, source: Box<dyn capture::CaptureSource>) -> Option<Arc<Card>> {
    let card = new_card(parent, id, driver, name, longname, name)?;
    let pcm = capture::CapturePcm::new(card.index, name, source);
    let hw = pcm.format();
    card.add_capture(0, pcm.clone());
    log::info!(
        "sound: card {} \"{name}\" ({driver}): capture pcmC{}D0c, device {} Hz, {} channels, {}-bit",
        card.index,
        card.index,
        hw.rate,
        hw.channels,
        hw.bytes * 8
    );
    if card.index != 0
        && let Some(c0) = self::card(0)
        && c0.capture(0).is_none()
    {
        c0.add_capture(0, pcm);
        log::info!("sound: card 0 ({}) has no capture: pcmC0D0c records from card {} \"{name}\" (Android's microphone)", c0.name, card.index);
    }
    Some(card)
}

/// Probe the sound hardware (PCI is enumerated).
pub fn init() {
    hda::init();
}

/// /proc/asound entries (procfs lists these names).
pub const PROC_FILES: [&[u8]; 4] = [b"cards", b"devices", b"pcm", b"version"];

pub fn proc_file(i: usize) -> Vec<u8> {
    let cards = CARDS.lock().clone();
    let mut s = String::new();
    match i {
        0 if cards.is_empty() => s.push_str("--- no soundcards ---\n"),
        0 => {
            for c in &cards {
                s += &format!("{:>2} [{:<15}]: {} - {}\n                      {}\n", c.index, c.id, c.driver, c.name, c.longname);
            }
        }
        1 => {
            for c in &cards {
                s += &format!("{:>3}: [{:>2}]   : control\n", c.index * MINORS_PER_CARD, c.index);
                for p in c.pcms.lock().iter() {
                    s += &format!("{:>3}: [{:>2}-{:>2}]: digital audio playback\n", c.index * MINORS_PER_CARD + 16 + p.device, c.index, p.device);
                }
                for (d, _) in c.captures.lock().iter() {
                    s += &format!("{:>3}: [{:>2}-{:>2}]: digital audio capture\n", c.index * MINORS_PER_CARD + 24 + d, c.index, d);
                }
            }
        }
        2 => {
            for c in &cards {
                for p in c.pcms.lock().iter() {
                    s += &format!("{:02}-{:02}: {} : {} : playback 1\n", c.index, p.device, p.id, p.name);
                }
                for (d, p) in c.captures.lock().iter() {
                    s += &format!("{:02}-{:02}: USB Audio : {} : capture 1\n", c.index, d, p.name);
                }
            }
        }
        _ => s.push_str("Advanced Linux Sound Architecture Driver Version k6.12.0.\n"),
    }
    s.into_bytes()
}

/// At most one message per `interval_ms` through `slot` (the kernel log,
/// so also the framebuffer console); returns whether to log now.
pub fn ratelimit(slot: &AtomicU64, interval_ms: u64) -> bool {
    let now = crate::time::uptime_ms().max(1);
    let last = slot.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < interval_ms {
        return false;
    }
    slot.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok()
}
