//! /dev/uhid (misc 10:239): HID devices implemented in user space. This
//! is how Android's Bluetooth stack (its HID host, for Bluetooth
//! keyboards, mice and remotes) creates input devices.
//!
//! Writes are `struct uhid_event`s: UHID_CREATE2 (or the legacy
//! UHID_CREATE, whose descriptor is behind a pointer) gives the device's
//! name, ids and report descriptor; the descriptor is parsed
//! ([`super::hidrd`]) into an input device's capabilities, and each
//! UHID_INPUT2 (UHID_INPUT) report becomes its events. UHID_DESTROY or
//! closing the file removes the device. Reads return the events of the
//! "kernel" side: UHID_START then UHID_OPEN right after creation (the
//! stack holds reports back until the device is opened), UHID_CLOSE and
//! UHID_STOP on destruction. Output, feature and get/set-report traffic
//! is not generated (LEDs stay off).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;

use super::hidrd::Decoder;
use super::{Caps, EV_SYN, InputDev, SYN_REPORT};
use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{READ_EVENTS, WRITE_EVENTS, WaitQueue};
use crate::sched::{self, Wake, WaitMode};

pub const UHID_MINOR: u32 = 239;

const UHID_CREATE: u32 = 0;
const UHID_DESTROY: u32 = 1;
const UHID_START: u32 = 2;
const UHID_STOP: u32 = 3;
const UHID_OPEN: u32 = 4;
const UHID_CLOSE: u32 = 5;
const UHID_INPUT: u32 = 8;
const UHID_GET_REPORT_REPLY: u32 = 10;
const UHID_CREATE2: u32 = 11;
const UHID_INPUT2: u32 = 12;
const UHID_SET_REPORT_REPLY: u32 = 14;

/// sizeof(struct uhid_event): the type and the largest request
/// (uhid_create2_req).
pub const EVENT_SIZE: usize = 4 + 128 + 64 + 64 + 2 + 2 + 4 * 4 + 4096;
const DATA_MAX: usize = 4096;
/// UHID_DEV_NUMBERED_{FEATURE,OUTPUT,INPUT}_REPORTS.
const DEV_NUMBERED: u64 = 7;

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

struct Dev {
    input: Arc<InputDev>,
    decoder: Decoder,
}

struct St {
    dev: Option<Dev>,
    /// Events for `read`, each EVENT_SIZE bytes.
    out: VecDeque<Vec<u8>>,
}

pub struct UhidFile {
    st: Mutex<St>,
    wq: Arc<WaitQueue>,
}

pub fn open() -> KResult<Box<dyn FileOps>> {
    Ok(Box::new(UhidFile { st: Mutex::new(St { dev: None, out: VecDeque::new() }), wq: WaitQueue::new() }))
}

fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn event(ty: u32, payload: &[u8]) -> Vec<u8> {
    let mut e = alloc::vec![0u8; EVENT_SIZE];
    e[..4].copy_from_slice(&ty.to_le_bytes());
    e[4..4 + payload.len()].copy_from_slice(payload);
    e
}

impl UhidFile {
    fn push(&self, st: &mut St, e: Vec<u8>) {
        st.out.push_back(e);
        self.wq.wake(READ_EVENTS);
    }

    /// Make the device: ids, names and the report descriptor.
    fn create(&self, names: [String; 3], ids: [u32; 4], rd: &[u8]) -> KResult<()> {
        let mut st = self.st.lock();
        if st.dev.is_some() {
            return Err(EALREADY);
        }
        let [name, phys, uniq] = names;
        let [bus, vendor, product, version] = ids;
        let decoder = Decoder::new(rd);
        let mut caps = Caps::new(&name);
        caps.phys = phys;
        caps.uniq = uniq;
        caps.id = [bus as u16, vendor as u16, product as u16, version as u16];
        let evs = decoder.capabilities();
        for &(t, c) in &evs {
            caps.ev.set(t as usize, true);
            caps.codes[t as usize].set(c as usize, true);
        }
        let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let parent = format!("devices/virtual/misc/uhid/{:04X}:{:04X}:{:04X}.{n:04X}", bus as u16, vendor as u16, product as u16);
        let input = super::register(caps, &parent, None)?;
        log::info!(
            "uhid: \"{name}\" {:04x}:{:04x}:{:04x}: {} report fields, {} event codes, input{}",
            bus as u16,
            vendor as u16,
            product as u16,
            decoder.desc.fields.len(),
            evs.len(),
            input.index
        );
        let flags = if decoder.desc.numbered { DEV_NUMBERED } else { 0 };
        st.dev = Some(Dev { input, decoder });
        self.push(&mut st, event(UHID_START, &flags.to_le_bytes()));
        self.push(&mut st, event(UHID_OPEN, &[]));
        Ok(())
    }

    fn destroy(&self) -> KResult<()> {
        let mut st = self.st.lock();
        let d = st.dev.take().ok_or(EINVAL)?;
        d.input.unregister();
        self.push(&mut st, event(UHID_CLOSE, &[]));
        self.push(&mut st, event(UHID_STOP, &[]));
        Ok(())
    }

    fn input(&self, report: &[u8]) -> KResult<()> {
        let mut st = self.st.lock();
        let d = st.dev.as_mut().ok_or(EINVAL)?;
        let input = d.input.clone();
        let mut any = false;
        d.decoder.report(report, &mut |t, c, v| {
            any = true;
            input.event(t, c, v)
        });
        if any {
            input.event(EV_SYN, SYN_REPORT, 0);
        }
        Ok(())
    }
}

impl Drop for UhidFile {
    fn drop(&mut self) {
        if let Some(d) = self.st.lock().dev.take() {
            d.input.unregister();
        }
    }
}

impl FileOps for UhidFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        let mut result = Err(EAGAIN);
        let wake = sched::wait_until(self.wq.key(), None, WaitMode::Interruptible, || {
            let mut st = self.st.lock();
            if let Some(e) = st.out.pop_front() {
                let n = buf.len().min(e.len());
                buf[..n].copy_from_slice(&e[..n]);
                result = Ok(n);
                return true;
            }
            f.nonblock()
        });
        match wake {
            Wake::Ready => result,
            _ => Err(ERESTARTSYS),
        }
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        if data.len() < 4 {
            return Err(EINVAL);
        }
        let mut e = alloc::vec![0u8; EVENT_SIZE];
        let n = data.len().min(EVENT_SIZE);
        e[..n].copy_from_slice(&data[..n]);
        let p = &e[4..];
        match rd32(&e, 0) {
            UHID_CREATE2 => {
                let names = [cstr(&p[..128]), cstr(&p[128..192]), cstr(&p[192..256])];
                let rd_size = rd16(p, 256) as usize;
                if rd_size > DATA_MAX {
                    return Err(EINVAL);
                }
                let ids = [rd16(p, 258) as u32, rd32(p, 260), rd32(p, 264), rd32(p, 268)];
                self.create(names, ids, &p[276..276 + rd_size])?;
            }
            UHID_CREATE => {
                // name, phys, uniq, then a user pointer to the descriptor.
                let names = [cstr(&p[..128]), cstr(&p[128..192]), cstr(&p[192..256])];
                let ptr = u64::from_le_bytes(p[256..264].try_into().unwrap());
                let rd_size = rd16(p, 264) as usize;
                if rd_size > DATA_MAX {
                    return Err(EINVAL);
                }
                let mut rd = alloc::vec![0u8; rd_size];
                crate::process::current().mm().copy_in(ptr, &mut rd)?;
                let ids = [rd16(p, 266) as u32, rd32(p, 268), rd32(p, 272), rd32(p, 276)];
                self.create(names, ids, &rd)?;
            }
            UHID_DESTROY => self.destroy()?,
            UHID_INPUT2 => {
                let size = (rd16(p, 0) as usize).min(DATA_MAX);
                self.input(&p[2..2 + size])?;
            }
            UHID_INPUT => {
                let size = (rd16(p, DATA_MAX) as usize).min(DATA_MAX);
                self.input(&p[..size])?;
            }
            // Answers to requests we never make.
            UHID_GET_REPORT_REPLY | UHID_SET_REPORT_REPLY => {}
            _ => return Err(EOPNOTSUPP),
        }
        Ok(data.len())
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        WRITE_EVENTS | if self.st.lock().out.is_empty() { 0 } else { READ_EVENTS }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
