//! /dev/hvc1../dev/hvc17 (229:N): stand-ins for the virtio-console ports
//! Cuttlefish wires to its host daemons (oemlock, keymint/gatekeeper
//! fifos, bluetooth, GNSS, sensors, NFC, UWB, ...). There is no host side
//! here: the terminals open, take termios settings (the HALs switch them
//! to raw mode), discard what is written and never have anything to read.
//! That lets those HALs register their services, so system_server's
//! `waitForDeclaredService` calls return; requests that need an answer
//! from the host block (or fail with EAGAIN when non-blocking).

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::any::Any;
use spin::{Mutex, Once};

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLOUT, POLLWRNORM, WaitQueue};
use crate::sched::{self, WaitMode};

pub const HVC_MAJOR: u32 = 229;
/// hvc1..hvc17 (hvc0 is the Cuttlefish kernel console, unused here).
pub const HVC_PORTS: u32 = 18;

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TCFLSH: u32 = 0x540b;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;

/// struct termios (36 bytes): 38400 8N1, ICANON | ECHO like a fresh tty.
const INITIAL_TERMIOS: [u8; 36] = {
    let mut t = [0u8; 36];
    // c_iflag ICRNL | IXON, c_oflag OPOST | ONLCR, c_cflag B38400 | CS8 | CREAD | HUPCL,
    // c_lflag ISIG | ICANON | ECHO | ECHOE | ECHOK | IEXTEN | ECHOCTL | ECHOKE
    let (i, o, c, l): (u32, u32, u32, u32) = (0o2400, 0o5, 0o2277, 0o105073);
    let (ib, ob, cb, lb) = (i.to_le_bytes(), o.to_le_bytes(), c.to_le_bytes(), l.to_le_bytes());
    let mut k = 0;
    while k < 4 {
        t[k] = ib[k];
        t[4 + k] = ob[k];
        t[8 + k] = cb[k];
        t[12 + k] = lb[k];
        k += 1;
    }
    t
};

struct Port {
    termios: Mutex<[u8; 36]>,
    winsize: Mutex<[u8; 8]>,
    wq: Arc<WaitQueue>,
}

fn port(minor: u32) -> &'static Port {
    static PORTS: Once<alloc::vec::Vec<Port>> = Once::new();
    &PORTS.call_once(|| {
        (0..HVC_PORTS)
            .map(|_| Port { termios: Mutex::new(INITIAL_TERMIOS), winsize: Mutex::new([0; 8]), wq: WaitQueue::new() })
            .collect()
    })[minor as usize]
}

struct Hvc(u32);

impl FileOps for Hvc {
    fn read(&self, f: &OpenFile, _: &mut u64, _buf: &mut [u8]) -> KResult<usize> {
        if f.nonblock() {
            return Err(EAGAIN);
        }
        // Nothing ever arrives from the (absent) host.
        sched::wait_until(port(self.0).wq.key(), None, WaitMode::Interruptible, || false);
        Err(ERESTARTSYS)
    }

    fn write(&self, _: &OpenFile, _: &mut u64, data: &[u8]) -> KResult<usize> {
        Ok(data.len())
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        let mm = crate::process::current().mm();
        let p = port(self.0);
        match cmd {
            TCGETS => mm.copy_out(arg, &*p.termios.lock()).map(|_| 0),
            TCSETS | TCSETSW | TCSETSF => {
                let mut t = [0u8; 36];
                mm.copy_in(arg, &mut t)?;
                *p.termios.lock() = t;
                Ok(0)
            }
            TCFLSH => Ok(0),
            TIOCGWINSZ => mm.copy_out(arg, &*p.winsize.lock()).map(|_| 0),
            TIOCSWINSZ => {
                let mut w = [0u8; 8];
                mm.copy_in(arg, &mut w)?;
                *p.winsize.lock() = w;
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        POLLOUT | POLLWRNORM
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(port(self.0).wq.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn open(minor: u32) -> KResult<Box<dyn FileOps>> {
    if minor == 0 || minor >= HVC_PORTS {
        return Err(ENXIO);
    }
    Ok(Box::new(Hvc(minor)))
}
