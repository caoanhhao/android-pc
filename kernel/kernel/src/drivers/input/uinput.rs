//! /dev/uinput (misc 10:223): input devices made by user space.
//!
//! Before UI_DEV_CREATE the file collects the capabilities (UI_SET_*BIT,
//! UI_DEV_SETUP + UI_ABS_SETUP, or the legacy write of a
//! `struct uinput_user_dev`); after it, input_events written to the file
//! are the device's events, and events other clients write to its event
//! node (LEDs, sounds) can be read back. Closing the file or
//! UI_DEV_DESTROY removes the device. Force feedback uploads are refused.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use spin::Mutex;

use super::{ABS_CNT, Caps, EV_ABS, EV_CNT, EVENT_SIZE, InputDev, PROP_CNT, Sink, code_count};
use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{READ_EVENTS, WRITE_EVENTS, WaitQueue};
use crate::sched::{self, Wake, WaitMode};

pub const UINPUT_MINOR: u32 = 223;

const UI_DEV_CREATE: u32 = 0x5501;
const UI_DEV_DESTROY: u32 = 0x5502;
const UI_DEV_SETUP: u32 = 0x405c_5503;
const UI_ABS_SETUP: u32 = 0x401c_5504;
const UI_GET_VERSION: u32 = 0x8004_552d;
const UI_SET_PHYS: u32 = 0x4008_556c;
const UINPUT_VERSION: u32 = 5;
/// sizeof(struct uinput_user_dev).
const USER_DEV_SIZE: usize = 80 + 8 + 4 + 4 * 4 * ABS_CNT;

/// Events written to the device's node, for the uinput reader.
struct Out {
    q: Mutex<VecDeque<(u16, u16, i32, u64)>>,
    wq: Arc<WaitQueue>,
}

struct OutSink(Arc<Out>);

impl Sink for OutSink {
    fn output(&self, ty: u16, code: u16, value: i32) {
        let mut q = self.0.q.lock();
        if q.len() < 1024 {
            q.push_back((ty, code, value, crate::time::monotonic_ns()));
        }
        drop(q);
        self.0.wq.wake(READ_EVENTS);
    }
}

struct St {
    caps: Caps,
    dev: Option<Arc<InputDev>>,
}

pub struct UinputFile {
    st: Mutex<St>,
    out: Arc<Out>,
}

pub fn open() -> KResult<Box<dyn FileOps>> {
    Ok(Box::new(UinputFile {
        st: Mutex::new(St { caps: Caps::new(""), dev: None }),
        out: Arc::new(Out { q: Mutex::new(VecDeque::new()), wq: WaitQueue::new() }),
    }))
}

fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..n]).into_owned()
}

fn rd16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn rd32(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl UinputFile {
    fn create(&self, st: &mut St) -> KResult<()> {
        if st.dev.is_some() {
            return Err(EINVAL);
        }
        if st.caps.name.is_empty() {
            return Err(EINVAL);
        }
        let dev = super::register(st.caps.clone(), "devices/virtual", Some(Box::new(OutSink(self.out.clone()))))?;
        st.dev = Some(dev);
        Ok(())
    }
}

impl FileOps for UinputFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        if self.st.lock().dev.is_none() {
            return Err(EINVAL);
        }
        if buf.len() < EVENT_SIZE {
            return Err(EINVAL);
        }
        let mut n = 0;
        let w = sched::wait_until(self.out.wq.key(), None, WaitMode::Interruptible, || {
            let mut q = self.out.q.lock();
            while n + EVENT_SIZE <= buf.len() {
                let Some((ty, code, value, ns)) = q.pop_front() else { break };
                let ns = ns as i64;
                buf[n..n + 8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
                buf[n + 8..n + 16].copy_from_slice(&((ns % 1_000_000_000) / 1000).to_le_bytes());
                buf[n + 16..n + 18].copy_from_slice(&ty.to_le_bytes());
                buf[n + 18..n + 20].copy_from_slice(&code.to_le_bytes());
                buf[n + 20..n + 24].copy_from_slice(&value.to_le_bytes());
                n += EVENT_SIZE;
            }
            n > 0 || f.nonblock()
        });
        match w {
            _ if n > 0 => Ok(n),
            Wake::Interrupted => Err(ERESTARTSYS),
            _ => Err(EAGAIN),
        }
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        let mut st = self.st.lock();
        let Some(dev) = st.dev.clone() else {
            // Legacy setup: struct uinput_user_dev.
            if data.len() < USER_DEV_SIZE {
                return Err(EINVAL);
            }
            let c = &mut st.caps;
            c.name = cstr(&data[..80]);
            for i in 0..4 {
                c.id[i] = rd16(data, 80 + 2 * i);
            }
            let base = 92;
            for a in 0..ABS_CNT {
                let max = rd32(data, base + 4 * a);
                let min = rd32(data, base + 4 * (ABS_CNT + a));
                let fuzz = rd32(data, base + 4 * (2 * ABS_CNT + a));
                let flat = rd32(data, base + 4 * (3 * ABS_CNT + a));
                c.abs[a].0 = [0, min, max, fuzz, flat, c.abs[a].0[5]];
            }
            return Ok(USER_DEV_SIZE);
        };
        drop(st);
        let evs = super::parse_events(data)?;
        for (ty, code, value) in &evs {
            dev.event(*ty, *code, *value);
        }
        Ok(evs.len() * EVENT_SIZE)
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let mm = crate::process::current().mm();
        let mut st = self.st.lock();
        match cmd {
            UI_GET_VERSION => mm.copy_out(arg, &UINPUT_VERSION.to_le_bytes()).map(|_| 0),
            UI_DEV_CREATE => self.create(&mut st).map(|_| 0),
            UI_DEV_DESTROY => {
                let d = st.dev.take().ok_or(EINVAL)?;
                d.unregister();
                Ok(0)
            }
            UI_DEV_SETUP => {
                if st.dev.is_some() {
                    return Err(EINVAL);
                }
                let mut b = [0u8; 92];
                mm.copy_in(arg, &mut b)?;
                for i in 0..4 {
                    st.caps.id[i] = rd16(&b, 2 * i);
                }
                st.caps.name = cstr(&b[8..88]);
                Ok(0)
            }
            UI_ABS_SETUP => {
                if st.dev.is_some() {
                    return Err(EINVAL);
                }
                let mut b = [0u8; 28];
                mm.copy_in(arg, &mut b)?;
                let code = rd16(&b, 0) as usize;
                if code >= ABS_CNT {
                    return Err(ERANGE);
                }
                for i in 0..6 {
                    st.caps.abs[code].0[i] = rd32(&b, 4 + 4 * i);
                }
                Ok(0)
            }
            UI_SET_PHYS => {
                let p = mm.read_cstr(arg, 1024)?;
                st.caps.phys = String::from_utf8_lossy(&p).into_owned();
                Ok(0)
            }
            _ if cmd & 0xffff_ff00 == 0x4004_5500 && (100..=110).contains(&(cmd & 0xff)) => {
                if st.dev.is_some() {
                    return Err(EINVAL);
                }
                let v = arg as usize;
                let nr = cmd & 0xff;
                let ty = match nr {
                    100 => {
                        if v >= EV_CNT {
                            return Err(EINVAL);
                        }
                        st.caps.ev.set(v, true);
                        return Ok(0);
                    }
                    110 => {
                        if v >= PROP_CNT {
                            return Err(EINVAL);
                        }
                        st.caps.props.set(v, true);
                        return Ok(0);
                    }
                    101 => super::EV_KEY,
                    102 => super::EV_REL,
                    103 => EV_ABS,
                    104 => super::EV_MSC,
                    105 => super::EV_LED,
                    106 => super::EV_SND,
                    107 => super::EV_FF,
                    109 => super::EV_SW,
                    _ => return Err(EINVAL),
                };
                if v >= code_count(ty) {
                    return Err(EINVAL);
                }
                st.caps.codes[ty as usize].set(v, true);
                Ok(0)
            }
            // UI_GET_SYSNAME(len)
            _ if cmd & 0xc000_ffff == 0x8000_552c => {
                let d = st.dev.as_ref().ok_or(ENOENT)?;
                let len = ((cmd >> 16) & 0x3fff) as usize;
                let mut s = format!("input{}", d.index).into_bytes();
                s.push(0);
                let n = len.min(s.len());
                mm.copy_out(arg, &s[..n])?;
                Ok(n as u64)
            }
            _ => Err(EINVAL),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let mut m = WRITE_EVENTS;
        if !self.out.q.lock().is_empty() {
            m |= READ_EVENTS;
        }
        m
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.out.wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for UinputFile {
    fn drop(&mut self) {
        if let Some(d) = self.st.lock().dev.take() {
            d.unregister();
        }
    }
}
