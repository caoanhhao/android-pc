//! The ALSA control ABI (`/dev/snd/controlC*`, `SNDRV_CTL_IOCTL_*`), as
//! tinyalsa's mixer and tinypcminfo use it: CARD_INFO, ELEM_LIST,
//! ELEM_INFO, ELEM_READ/WRITE, TLV_READ (dB scales), SUBSCRIBE_EVENTS
//! (accepted; no events are queued), PCM_NEXT_DEVICE, PCM_INFO.
//!
//! Elements are the driver's [`Element`]s in registration order; numid
//! is the position + 1.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::Card;
use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};

pub const IFACE_CARD: u32 = 0;
pub const IFACE_MIXER: u32 = 2;

const TYPE_BOOLEAN: u32 = 1;
const TYPE_INTEGER: u32 = 2;

const ACCESS_READ: u32 = 1;
const ACCESS_WRITE: u32 = 2;
const ACCESS_VOLATILE: u32 = 4;
const ACCESS_TLV_READ: u32 = 0x10;

/// dB scale TLV (SNDRV_CTL_TLVT_DB_SCALE).
pub const TLV_DB_SCALE: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Boolean,
    Integer { min: i64, max: i64 },
}

/// A mixer control.
pub trait Element: Send + Sync {
    fn name(&self) -> &str;
    fn iface(&self) -> u32 {
        IFACE_MIXER
    }
    fn kind(&self) -> Kind;
    /// Values (channels).
    fn count(&self) -> u32;
    fn writable(&self) -> bool {
        true
    }
    /// Changes by itself (jack state).
    fn volatile(&self) -> bool {
        false
    }
    fn get(&self) -> Vec<i64>;
    fn put(&self, v: &[i64]) -> KResult<()>;
    /// The TLV words (type, length, payload...).
    fn tlv(&self) -> Option<Vec<u32>> {
        None
    }
}

pub struct ControlFile {
    card: Arc<Card>,
    subscribed: AtomicBool,
}

impl ControlFile {
    pub fn new(card: Arc<Card>) -> ControlFile {
        ControlFile { card, subscribed: AtomicBool::new(false) }
    }

    /// The element an snd_ctl_elem_id names: by numid, else by
    /// (iface, name, index).
    fn find(&self, id: &[u8]) -> KResult<(u32, Arc<dyn Element>)> {
        let numid = u32::from_le_bytes(id[0..4].try_into().unwrap());
        let ctls = self.card.controls.lock();
        if numid != 0 {
            return ctls.get(numid as usize - 1).map(|e| (numid, e.clone())).ok_or(ENOENT);
        }
        let iface = u32::from_le_bytes(id[4..8].try_into().unwrap());
        let name = &id[16..60];
        let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(44)];
        let index = u32::from_le_bytes(id[60..64].try_into().unwrap());
        if index != 0 {
            return Err(ENOENT);
        }
        ctls.iter()
            .position(|e| e.iface() == iface && e.name().as_bytes() == name)
            .map(|i| (i as u32 + 1, ctls[i].clone()))
            .ok_or(ENOENT)
    }
}

fn copy_out(arg: u64, b: &[u8]) -> KResult<()> {
    crate::process::current().mm().copy_out(arg, b)
}

fn copy_in(arg: u64, b: &mut [u8]) -> KResult<()> {
    crate::process::current().mm().copy_in(arg, b)
}

fn put_str(b: &mut [u8], s: &str) {
    let n = s.len().min(b.len() - 1);
    b[..n].copy_from_slice(&s.as_bytes()[..n]);
    b[n..].fill(0);
}

/// struct snd_ctl_elem_id (64 bytes).
fn elem_id(numid: u32, e: &dyn Element) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[0..4].copy_from_slice(&numid.to_le_bytes());
    b[4..8].copy_from_slice(&e.iface().to_le_bytes());
    put_str(&mut b[16..60], e.name());
    b
}

impl FileOps for ControlFile {
    fn read(&self, _: &OpenFile, _: &mut u64, _: &mut [u8]) -> KResult<usize> {
        // No events are ever queued.
        Err(EAGAIN)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if (cmd >> 8) & 0xff != b'U' as u32 {
            return Err(ENOTTY);
        }
        let c = &self.card;
        match cmd & 0xff {
            // PVERSION: 2.0.9
            0x00 => copy_out(arg, &0x00020009i32.to_le_bytes()).map(|_| 0),
            // CARD_INFO (376 bytes)
            0x01 => {
                let mut b = [0u8; 376];
                b[0..4].copy_from_slice(&(c.index as i32).to_le_bytes());
                put_str(&mut b[8..24], &c.id);
                put_str(&mut b[24..40], &c.driver);
                put_str(&mut b[40..72], &c.name);
                put_str(&mut b[72..152], &c.longname);
                put_str(&mut b[168..248], &c.mixername);
                put_str(&mut b[248..376], &String::from("HDA:"));
                copy_out(arg, &b).map(|_| 0)
            }
            // ELEM_LIST: offset, space, used, count, pids
            0x10 => {
                let mut b = [0u8; 24];
                copy_in(arg, &mut b)?;
                let offset = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
                let space = u32::from_le_bytes(b[4..8].try_into().unwrap()) as usize;
                let pids = u64::from_le_bytes(b[16..24].try_into().unwrap());
                let ctls = c.controls.lock().clone();
                let mut used = 0usize;
                for (i, e) in ctls.iter().enumerate().skip(offset).take(space) {
                    copy_out(pids + used as u64 * 64, &elem_id(i as u32 + 1, e.as_ref()))?;
                    used += 1;
                }
                b[8..12].copy_from_slice(&(used as u32).to_le_bytes());
                b[12..16].copy_from_slice(&(ctls.len() as u32).to_le_bytes());
                copy_out(arg, &b[..16]).map(|_| 0)
            }
            // ELEM_INFO (272 bytes)
            0x11 => {
                let mut b = [0u8; 272];
                copy_in(arg, &mut b)?;
                let (numid, e) = self.find(&b[..64])?;
                b.fill(0);
                b[..64].copy_from_slice(&elem_id(numid, e.as_ref()));
                let mut access = ACCESS_READ;
                if e.writable() {
                    access |= ACCESS_WRITE;
                }
                if e.volatile() {
                    access |= ACCESS_VOLATILE;
                }
                if e.tlv().is_some() {
                    access |= ACCESS_TLV_READ;
                }
                let (ty, min, max) = match e.kind() {
                    Kind::Boolean => (TYPE_BOOLEAN, 0, 1),
                    Kind::Integer { min, max } => (TYPE_INTEGER, min, max),
                };
                b[64..68].copy_from_slice(&ty.to_le_bytes());
                b[68..72].copy_from_slice(&access.to_le_bytes());
                b[72..76].copy_from_slice(&e.count().to_le_bytes());
                b[76..80].copy_from_slice(&(-1i32).to_le_bytes());
                b[80..88].copy_from_slice(&min.to_le_bytes());
                b[88..96].copy_from_slice(&max.to_le_bytes());
                b[96..104].copy_from_slice(&1i64.to_le_bytes());
                copy_out(arg, &b).map(|_| 0)
            }
            // ELEM_READ / ELEM_WRITE (1224 bytes; values at 72, longs)
            n @ (0x12 | 0x13) => {
                let mut b = alloc::vec![0u8; 1224];
                copy_in(arg, &mut b)?;
                let (numid, e) = self.find(&b[..64])?;
                if n == 0x13 {
                    if !e.writable() {
                        return Err(EPERM);
                    }
                    let (lo, hi) = match e.kind() {
                        Kind::Boolean => (0, 1),
                        Kind::Integer { min, max } => (min, max),
                    };
                    let vals: Vec<i64> = (0..e.count() as usize)
                        .map(|i| i64::from_le_bytes(b[72 + 8 * i..80 + 8 * i].try_into().unwrap()))
                        .collect();
                    if vals.iter().any(|v| *v < lo || *v > hi) {
                        return Err(EINVAL);
                    }
                    e.put(&vals)?;
                }
                let id = elem_id(numid, e.as_ref());
                b[..64].copy_from_slice(&id);
                for (i, v) in e.get().iter().enumerate().take(128) {
                    b[72 + 8 * i..80 + 8 * i].copy_from_slice(&v.to_le_bytes());
                }
                copy_out(arg, &b).map(|_| 0)
            }
            // ELEM_LOCK / ELEM_UNLOCK
            0x14 | 0x15 => Ok(0),
            // SUBSCRIBE_EVENTS: 1 on, 0 off, -1 query
            0x16 => {
                let mut v = [0u8; 4];
                copy_in(arg, &mut v)?;
                match i32::from_le_bytes(v) {
                    x if x < 0 => copy_out(arg, &(self.subscribed.load(Ordering::Relaxed) as i32).to_le_bytes()).map(|_| 0),
                    x => {
                        self.subscribed.store(x > 0, Ordering::Relaxed);
                        Ok(0)
                    }
                }
            }
            // TLV_READ: struct snd_ctl_tlv { numid, length, tlv[] }
            0x1a => {
                let mut h = [0u8; 8];
                copy_in(arg, &mut h)?;
                let numid = u32::from_le_bytes(h[0..4].try_into().unwrap());
                let len = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
                let e = c.controls.lock().get((numid as usize).wrapping_sub(1)).cloned().ok_or(ENOENT)?;
                let tlv = e.tlv().ok_or(ENXIO)?;
                let bytes: Vec<u8> = tlv.iter().flat_map(|w| w.to_le_bytes()).collect();
                if bytes.len() > len {
                    return Err(ENOMEM);
                }
                copy_out(arg + 8, &bytes).map(|_| 0)
            }
            // HWDEP_NEXT_DEVICE, RAWMIDI_NEXT_DEVICE: none
            0x20 | 0x40 => copy_out(arg, &(-1i32).to_le_bytes()).map(|_| 0),
            // PCM_NEXT_DEVICE
            0x30 => {
                let mut v = [0u8; 4];
                copy_in(arg, &mut v)?;
                let cur = i32::from_le_bytes(v);
                let caps: Vec<i32> = c.captures.lock().iter().map(|(d, _)| *d as i32).collect();
                let next = c.pcms.lock().iter().map(|p| p.device as i32).chain(caps).filter(|&d| d > cur).min().unwrap_or(-1);
                copy_out(arg, &next.to_le_bytes()).map(|_| 0)
            }
            // PCM_INFO
            0x31 => {
                let mut b = [0u8; 288];
                copy_in(arg, &mut b)?;
                let device = u32::from_le_bytes(b[0..4].try_into().unwrap());
                let subdevice = u32::from_le_bytes(b[4..8].try_into().unwrap());
                let stream = i32::from_le_bytes(b[8..12].try_into().unwrap());
                if subdevice != 0 {
                    return Err(ENOENT);
                }
                if stream == 1 {
                    let p = c.capture(device).ok_or(ENXIO)?;
                    return copy_out(arg, &p.info()).map(|_| 0);
                }
                if stream != 0 {
                    return Err(ENOENT);
                }
                let p = c.pcm(device).ok_or(ENXIO)?;
                copy_out(arg, &super::pcm::pcm_info(&p)).map(|_| 0)
            }
            // PCM_PREFER_SUBDEVICE
            0x32 => Ok(0),
            // POWER_STATE: D0
            0xd1 => copy_out(arg, &0i32.to_le_bytes()).map(|_| 0),
            _ => Err(ENOTTY),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        0
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
