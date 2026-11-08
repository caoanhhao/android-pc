//! The serial console as a tty (/dev/console, /dev/tty, /dev/ttyS0): a
//! subset of Linux's N_TTY line discipline on COM1.
//!
//! Input: the BSP's timer tick drains the UART receive FIFO (there is no
//! IOAPIC routing yet, so no RX interrupt) and feeds each byte through the
//! line discipline in interrupt context:
//! - iflag ISTRIP, IGNCR, ICRNL, INLCR;
//! - ISIG: VINTR/VQUIT/VSUSP send SIGINT/SIGQUIT/SIGTSTP to the foreground
//!   process group (from process context: see [`deliver_signals`]);
//! - ICANON: line editing with VERASE, VKILL, VWERASE (IEXTEN), VEOF,
//!   VEOL/VEOL2; reads return at most one line, ^D on an empty line is EOF;
//! - non-canonical reads honour VMIN/VTIME;
//! - ECHO, ECHOE, ECHOK, ECHOKE, ECHONL, ECHOCTL.
//!
//! Output: OPOST with ONLCR. Job control is minimal: TIOCSPGRP/TIOCGPGRP
//! keep the foreground group, there is no SIGTTIN/SIGTTOU.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use super::poll::{POLLIN, POLLRDNORM, WRITE_EVENTS, WaitQueue};
use crate::errno::*;
use crate::sched::{self, Wake, WaitMode};
use crate::sync::IrqLock;

// termios layout: iflag, oflag, cflag, lflag (u32 each), line, cc[19].
const IFLAG: usize = 0;
const OFLAG: usize = 4;
const LFLAG: usize = 12;
const CC: usize = 17;

const ISTRIP: u32 = 0o40;
const INLCR: u32 = 0o100;
const IGNCR: u32 = 0o200;
const ICRNL: u32 = 0o400;
const OPOST: u32 = 0o1;
const ONLCR: u32 = 0o4;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ECHOE: u32 = 0o20;
const ECHOK: u32 = 0o40;
const ECHONL: u32 = 0o100;
const NOFLSH: u32 = 0o200;
const ECHOCTL: u32 = 0o1000;
const ECHOKE: u32 = 0o4000;
const IEXTEN: u32 = 0o100000;

const VINTR: usize = 0;
const VQUIT: usize = 1;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;
const VTIME: usize = 5;
const VMIN: usize = 6;
const VSUSP: usize = 10;
const VEOL: usize = 11;
const VWERASE: usize = 14;
const VEOL2: usize = 16;

/// Linux N_TTY_BUF_SIZE.
const BUF_SIZE: usize = 4096;

const SIGINT: u32 = 2;
const SIGQUIT: u32 = 3;
const SIGTSTP: u32 = 20;

/// Kernel `struct termios` (36 bytes) as Linux sets up a serial console.
fn default_termios() -> [u8; 36] {
    let mut t = [0u8; 36];
    let iflag: u32 = ICRNL | 0o2000; // IXON
    let oflag: u32 = OPOST | ONLCR;
    let cflag: u32 = 0o17 | 0o60 | 0o200 | 0o2000; // B38400 | CS8 | CREAD | HUPCL
    let lflag: u32 = ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN;
    for (i, v) in [iflag, oflag, cflag, lflag].iter().enumerate() {
        t[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    // VINTR VQUIT VERASE VKILL VEOF VTIME VMIN VSWTC VSTART VSTOP VSUSP
    // VEOL VREPRINT VDISCARD VWERASE VLNEXT
    let cc = [3u8, 0x1c, 0x7f, 0x15, 4, 0, 1, 0, 0x11, 0x13, 0x1a, 0, 0x12, 0x0f, 0x17, 0x16];
    t[CC..CC + cc.len()].copy_from_slice(&cc);
    t
}

struct State {
    termios: [u8; 36],
    winsize: [u8; 8],
    pgrp: i32,
    /// Session the console is the controlling tty of (TIOCSCTTY).
    sid: i32,
    /// Canonical mode: completed lines (an empty one is an EOF from ^D).
    lines: VecDeque<Vec<u8>>,
    /// Canonical mode: the line being edited.
    line: Vec<u8>,
    /// Non-canonical mode: bytes ready to read.
    raw: VecDeque<u8>,
}

impl State {
    fn flag(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.termios[off..off + 4].try_into().unwrap())
    }
    fn cc(&self, i: usize) -> u8 {
        self.termios[CC + i]
    }
    fn canon(&self) -> bool {
        self.flag(LFLAG) & ICANON != 0
    }
    /// Is `c` the (enabled, non-zero) control character `i`?
    fn is_cc(&self, c: u8, i: usize) -> bool {
        c != 0 && self.cc(i) == c
    }

    fn available(&self) -> usize {
        if self.canon() { self.lines.iter().map(|l| l.len()).sum() } else { self.raw.len() }
    }

    fn readable(&self) -> bool {
        if self.canon() { !self.lines.is_empty() } else { !self.raw.is_empty() }
    }

    fn flush_input(&mut self) {
        self.lines.clear();
        self.line.clear();
        self.raw.clear();
    }

    /// Switch between canonical and raw mode: pending input is kept.
    fn set_termios(&mut self, t: [u8; 36]) {
        let was = self.canon();
        self.termios = t;
        match (was, self.canon()) {
            (true, false) => {
                for l in self.lines.drain(..) {
                    self.raw.extend(l);
                }
                self.raw.extend(self.line.drain(..));
            }
            (false, true) if !self.raw.is_empty() => {
                let v: Vec<u8> = self.raw.drain(..).collect();
                self.lines.push_back(v);
            }
            _ => {}
        }
    }

    fn output(&self, out: &mut Vec<u8>, data: &[u8]) {
        let onlcr = self.flag(OFLAG) & (OPOST | ONLCR) == OPOST | ONLCR;
        for &b in data {
            if b == b'\n' && onlcr {
                out.push(b'\r');
            }
            out.push(b);
        }
    }

    /// Echo `c` (ECHOCTL shows control characters as ^X).
    fn echo(&self, out: &mut Vec<u8>, c: u8) {
        let lflag = self.flag(LFLAG);
        if lflag & ECHO == 0 {
            if c == b'\n' && lflag & ECHONL != 0 && self.canon() {
                self.output(out, b"\n");
            }
            return;
        }
        if lflag & ECHOCTL != 0 && (c < 0x20 || c == 0x7f) && c != b'\t' && c != b'\n' {
            out.extend_from_slice(&[b'^', c ^ 0x40]);
        } else {
            self.output(out, &[c]);
        }
    }

    /// Erase the last character of the line being edited.
    fn erase_one(&mut self, out: &mut Vec<u8>) -> bool {
        let Some(c) = self.line.pop() else {
            return false;
        };
        let lflag = self.flag(LFLAG);
        if lflag & ECHO != 0 && lflag & ECHOE != 0 {
            let w = if lflag & ECHOCTL != 0 && (c < 0x20 || c == 0x7f) && c != b'\t' { 2 } else { 1 };
            for _ in 0..w {
                out.extend_from_slice(b"\x08 \x08");
            }
        }
        true
    }

    /// Process one input byte; `sig` collects the signal to raise.
    fn input(&mut self, mut c: u8, out: &mut Vec<u8>, sig: &mut u32) {
        let iflag = self.flag(IFLAG);
        let lflag = self.flag(LFLAG);
        if iflag & ISTRIP != 0 {
            c &= 0x7f;
        }
        if c == b'\r' {
            if iflag & IGNCR != 0 {
                return;
            }
            if iflag & ICRNL != 0 {
                c = b'\n';
            }
        } else if c == b'\n' && iflag & INLCR != 0 {
            c = b'\r';
        }
        if lflag & ISIG != 0 {
            let s = if self.is_cc(c, VINTR) {
                SIGINT
            } else if self.is_cc(c, VQUIT) {
                SIGQUIT
            } else if self.is_cc(c, VSUSP) {
                SIGTSTP
            } else {
                0
            };
            if s != 0 {
                if lflag & NOFLSH == 0 {
                    self.flush_input();
                }
                self.echo(out, c);
                *sig |= 1 << s;
                return;
            }
        }
        if !self.canon() {
            if self.raw.len() < BUF_SIZE {
                self.raw.push_back(c);
                self.echo(out, c);
            }
            return;
        }
        if self.is_cc(c, VERASE) {
            self.erase_one(out);
        } else if self.is_cc(c, VWERASE) && lflag & IEXTEN != 0 {
            while self.line.last().is_some_and(|b| *b == b' ' || *b == b'\t') {
                self.erase_one(out);
            }
            while self.line.last().is_some_and(|b| *b != b' ' && *b != b'\t') {
                self.erase_one(out);
            }
        } else if self.is_cc(c, VKILL) {
            if lflag & ECHOKE != 0 && lflag & ECHOE != 0 {
                while self.erase_one(out) {}
            } else {
                self.line.clear();
                self.echo(out, c);
                if lflag & ECHOK != 0 && lflag & ECHO != 0 {
                    self.output(out, b"\n");
                }
            }
        } else if self.is_cc(c, VEOF) {
            // The line so far is readable as is; on an empty line, EOF.
            let l = core::mem::take(&mut self.line);
            self.lines.push_back(l);
        } else if c == b'\n' || self.is_cc(c, VEOL) || self.is_cc(c, VEOL2) {
            self.line.push(c);
            self.echo(out, c);
            let l = core::mem::take(&mut self.line);
            self.lines.push_back(l);
        } else if self.line.len() < BUF_SIZE - 1 {
            self.line.push(c);
            self.echo(out, c);
        }
    }
}

static TTY: IrqLock<Option<State>> = IrqLock::new(None);
/// Signals (bit per number) raised by input, for the foreground group.
static PENDING_SIGS: AtomicU32 = AtomicU32::new(0);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = TTY.lock();
    let s = g.get_or_insert_with(|| {
        let mut winsize = [0u8; 8];
        winsize[0..2].copy_from_slice(&25u16.to_le_bytes());
        winsize[2..4].copy_from_slice(&80u16.to_le_bytes());
        State {
            termios: default_termios(),
            winsize,
            pgrp: 1,
            sid: 1,
            lines: VecDeque::new(),
            line: Vec::new(),
            raw: VecDeque::new(),
        }
    });
    f(s)
}

pub fn wait_queue() -> Arc<WaitQueue> {
    static WQ: spin::Once<Arc<WaitQueue>> = spin::Once::new();
    WQ.call_once(WaitQueue::new).clone()
}

/// From the BSP's timer interrupt: feed received bytes to the line
/// discipline.
pub fn poll_input() {
    let mut buf = [0u8; 64];
    let mut n = 0;
    while n < buf.len()
        && let Some(b) = crate::drivers::serial::read_byte()
    {
        buf[n] = b;
        n += 1;
    }
    if n == 0 {
        return;
    }
    let mut echo = Vec::new();
    let mut sig = 0;
    let readable = with(|s| {
        for &b in &buf[..n] {
            s.input(b, &mut echo, &mut sig);
        }
        s.readable()
    });
    if !echo.is_empty() {
        crate::drivers::serial::write_raw(&echo);
    }
    if sig != 0 {
        PENDING_SIGS.fetch_or(sig, Ordering::SeqCst);
    }
    if readable {
        wait_queue().wake(POLLIN | POLLRDNORM);
    }
}

/// Send the signals raised by ^C/^\/^Z to the foreground process group.
/// Called in process context (return to user mode, idle loop).
pub fn deliver_signals() {
    if PENDING_SIGS.load(Ordering::Relaxed) == 0 {
        return;
    }
    let sigs = PENDING_SIGS.swap(0, Ordering::SeqCst);
    let pgrp = with(|s| s.pgrp);
    if pgrp <= 0 {
        return;
    }
    let targets: Vec<u32> = {
        let t = crate::process::TABLE.lock();
        t.procs.iter().filter(|(_, n)| n.pgid == pgrp as u32 && n.zombie.is_none()).map(|(&p, _)| p).collect()
    };
    for sig in [SIGINT, SIGQUIT, SIGTSTP] {
        if sigs & (1 << sig) != 0 {
            let info = crate::process::signal::SigInfo::user(sig, crate::process::signal::SI_KERNEL, 0);
            for &p in &targets {
                let _ = crate::process::signal::send_process(p, sig, info);
            }
        }
    }
}

pub fn signals_pending() -> bool {
    PENDING_SIGS.load(Ordering::Relaxed) != 0
}

pub fn read(buf: &mut [u8], nonblock: bool) -> KResult<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let (canon, vmin, vtime) = with(|s| (s.canon(), s.cc(VMIN) as usize, s.cc(VTIME) as u64));
    if canon {
        return wait_for(1, None, nonblock).map(|_| take(buf));
    }
    // Non-canonical: VMIN bytes (capped by the buffer) and/or VTIME tenths
    // of a second, counted from the call when VMIN is 0 and from the first
    // byte otherwise.
    let timeout = || (vtime > 0).then(|| crate::time::monotonic_ns() + vtime * 100_000_000);
    match (vmin, vtime) {
        (0, 0) => Ok(take(buf)),
        (0, _) => wait_for(1, timeout(), nonblock).map(|_| take(buf)),
        (_, 0) => wait_for(vmin.min(buf.len()), None, nonblock).map(|_| take(buf)),
        _ => {
            wait_for(1, None, nonblock)?;
            wait_for(vmin.min(buf.len()), timeout(), false).map(|_| take(buf))
        }
    }
}

/// Block until `want` bytes (canonical: a line) are readable or the
/// deadline passes (then fewer may be).
fn wait_for(want: usize, deadline: Option<u64>, nonblock: bool) -> KResult<()> {
    let mut seen = 0;
    let w = sched::wait_until(wait_queue().key(), deadline, WaitMode::Interruptible, || {
        seen = with(|s| if !s.readable() { 0 } else if s.canon() { usize::MAX } else { s.available() });
        seen >= want || nonblock
    });
    match w {
        Wake::Ready if seen == 0 => Err(EAGAIN),
        Wake::Ready | Wake::Timeout => Ok(()),
        Wake::Interrupted => Err(ERESTARTSYS),
    }
}

/// Copy out what is readable: one line at most in canonical mode.
fn take(buf: &mut [u8]) -> usize {
    with(|s| {
        if s.canon() {
            let Some(mut l) = s.lines.pop_front() else {
                return 0;
            };
            let n = l.len().min(buf.len());
            buf[..n].copy_from_slice(&l[..n]);
            if n < l.len() {
                l.drain(..n);
                s.lines.push_front(l);
            }
            n
        } else {
            let n = s.raw.len().min(buf.len());
            for (i, b) in s.raw.drain(..n).enumerate() {
                buf[i] = b;
            }
            n
        }
    })
}

pub fn write(data: &[u8]) -> usize {
    let mut out = Vec::with_capacity(data.len() + 16);
    with(|s| s.output(&mut out, data));
    crate::drivers::serial::write_raw(&out);
    data.len()
}

pub fn poll() -> u32 {
    WRITE_EVENTS | if with(|s| s.readable()) { POLLIN | POLLRDNORM } else { 0 }
}

// ---------------------------------------------------------------------------
// ioctls

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TCSBRK: u32 = 0x5409;
const TCXONC: u32 = 0x540a;
const TCFLSH: u32 = 0x540b;
const TIOCSCTTY: u32 = 0x540e;
const TIOCGPGRP: u32 = 0x540f;
const TIOCSPGRP: u32 = 0x5410;
const TIOCOUTQ: u32 = 0x5411;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;
const FIONREAD: u32 = 0x541b;
const TIOCNOTTY: u32 = 0x5422;
const TIOCGSID: u32 = 0x5429;
const TCSBRKP: u32 = 0x5425;

pub fn ioctl(cmd: u32, arg: u64) -> KResult<u64> {
    let mm = crate::process::current().mm();
    match cmd {
        TCGETS => {
            let t = with(|s| s.termios);
            mm.copy_out(arg, &t)?;
        }
        TCSETS | TCSETSW | TCSETSF => {
            let mut t = [0u8; 36];
            mm.copy_in(arg, &mut t)?;
            with(|s| {
                if cmd == TCSETSF {
                    s.flush_input();
                }
                s.set_termios(t);
            });
            // Canonical <-> raw may have made input readable.
            wait_queue().wake(POLLIN);
        }
        TIOCGWINSZ => {
            let w = with(|s| s.winsize);
            mm.copy_out(arg, &w)?;
        }
        TIOCSWINSZ => {
            let mut w = [0u8; 8];
            mm.copy_in(arg, &mut w)?;
            with(|s| s.winsize = w);
        }
        TIOCGPGRP => {
            let p = with(|s| s.pgrp);
            mm.copy_out(arg, &p.to_le_bytes())?;
        }
        TIOCSPGRP => {
            let mut b = [0u8; 4];
            mm.copy_in(arg, &mut b)?;
            let pgrp = i32::from_le_bytes(b);
            if pgrp <= 0 {
                return Err(EINVAL);
            }
            with(|s| s.pgrp = pgrp);
        }
        TIOCGSID => {
            let sid = with(|s| s.sid);
            mm.copy_out(arg, &sid.to_le_bytes())?
        }
        // Become the controlling tty of the caller's session, its process
        // group in the foreground (a shell started after setsid, like
        // init's console service, waits until it is).
        TIOCSCTTY => {
            let me = crate::process::current().pid();
            let (pgid, sid) = {
                let t = crate::process::TABLE.lock();
                let n = t.procs.get(&me).ok_or(ESRCH)?;
                (n.pgid as i32, n.sid as i32)
            };
            with(|s| {
                s.pgrp = pgid;
                s.sid = sid;
            });
        }
        FIONREAD => {
            let n = with(|s| s.available()) as i32;
            mm.copy_out(arg, &n.to_le_bytes())?;
        }
        TIOCOUTQ => mm.copy_out(arg, &0i32.to_le_bytes())?,
        TCFLSH => match arg {
            0 | 2 => with(|s| s.flush_input()),
            1 => {}
            _ => return Err(EINVAL),
        },
        // Output is synchronous: drain and flow control are no-ops.
        TCSBRK | TCSBRKP | TCXONC | TIOCNOTTY => {}
        _ => return Err(ENOTTY),
    }
    Ok(0)
}
