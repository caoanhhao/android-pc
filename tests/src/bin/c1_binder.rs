//! C1: binder IPC — binderfs (mount, BINDER_CTL_ADD), open/mmap/ioctl,
//! separate contexts, a context manager reached through handle 0, sync
//! and oneway calls, sender pid/euid, binder objects passed between three
//! processes (and back to their owner), nested calls, fd passing both
//! ways, scatter-gather buffers with fd arrays, BR_SPAWN_LOOPER and
//! several looper threads, death notifications when a child exits,
//! BR_DEAD_REPLY / BR_FAILED_REPLY and extended errors.
//!
//! Processes: the test (client), a context manager child and a server
//! child. The children report through their exit status and through
//! replies; only the main process prints results.
//!
//! The host kernel's binder needs root to mount binderfs, so off the Rust
//! kernel the whole test is skipped.
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use rt::*;

rt::test_main!("c1_binder", main);

// ---------------------------------------------------------------------------
// ABI (include/uapi/linux/android/binder.h, binderfs.h)

const fn ioc(dir: u32, ty: u8, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | ((ty as u32) << 8) | nr
}
const fn ioc_size(cmd: u32) -> usize {
    ((cmd >> 16) & 0x3fff) as usize
}

const BINDER_WRITE_READ: u32 = ioc(3, b'b', 1, 48);
const BINDER_SET_MAX_THREADS: u32 = ioc(1, b'b', 5, 4);
const BINDER_SET_CONTEXT_MGR: u32 = ioc(1, b'b', 7, 4);
const BINDER_THREAD_EXIT: u32 = ioc(1, b'b', 8, 4);
const BINDER_VERSION: u32 = ioc(3, b'b', 9, 4);
const BINDER_GET_NODE_DEBUG_INFO: u32 = ioc(3, b'b', 11, 24);
const BINDER_GET_NODE_INFO_FOR_REF: u32 = ioc(3, b'b', 12, 24);
const BINDER_SET_CONTEXT_MGR_EXT: u32 = ioc(1, b'b', 13, 24);
const BINDER_ENABLE_ONEWAY_SPAM_DETECTION: u32 = ioc(1, b'b', 16, 4);
const BINDER_GET_EXTENDED_ERROR: u32 = ioc(3, b'b', 17, 12);
const BINDER_CTL_ADD: u32 = ioc(3, b'b', 1, 264);

const BR_TRANSACTION_SEC_CTX: u32 = ioc(2, b'r', 2, 72);
const BR_TRANSACTION: u32 = ioc(2, b'r', 2, 64);
const BR_REPLY: u32 = ioc(2, b'r', 3, 64);
const BR_DEAD_REPLY: u32 = ioc(0, b'r', 5, 0);
const BR_TRANSACTION_COMPLETE: u32 = ioc(0, b'r', 6, 0);
const BR_INCREFS: u32 = ioc(2, b'r', 7, 16);
const BR_ACQUIRE: u32 = ioc(2, b'r', 8, 16);
const BR_RELEASE: u32 = ioc(2, b'r', 9, 16);
const BR_DECREFS: u32 = ioc(2, b'r', 10, 16);
const BR_NOOP: u32 = ioc(0, b'r', 12, 0);
const BR_SPAWN_LOOPER: u32 = ioc(0, b'r', 13, 0);
const BR_DEAD_BINDER: u32 = ioc(2, b'r', 15, 8);
const BR_CLEAR_DEATH_NOTIFICATION_DONE: u32 = ioc(2, b'r', 16, 8);
const BR_FAILED_REPLY: u32 = ioc(0, b'r', 17, 0);

const BC_TRANSACTION: u32 = ioc(1, b'c', 0, 64);
const BC_REPLY: u32 = ioc(1, b'c', 1, 64);
const BC_FREE_BUFFER: u32 = ioc(1, b'c', 3, 8);
const BC_INCREFS: u32 = ioc(1, b'c', 4, 4);
const BC_ACQUIRE: u32 = ioc(1, b'c', 5, 4);
const BC_RELEASE: u32 = ioc(1, b'c', 6, 4);
const BC_DECREFS: u32 = ioc(1, b'c', 7, 4);
const BC_INCREFS_DONE: u32 = ioc(1, b'c', 8, 16);
const BC_ACQUIRE_DONE: u32 = ioc(1, b'c', 9, 16);
const BC_REGISTER_LOOPER: u32 = ioc(0, b'c', 11, 0);
const BC_ENTER_LOOPER: u32 = ioc(0, b'c', 12, 0);
const BC_REQUEST_DEATH_NOTIFICATION: u32 = ioc(1, b'c', 14, 12);
const BC_CLEAR_DEATH_NOTIFICATION: u32 = ioc(1, b'c', 15, 12);
const BC_DEAD_BINDER_DONE: u32 = ioc(1, b'c', 16, 8);
const BC_TRANSACTION_SG: u32 = ioc(1, b'c', 17, 72);

const fn pack(a: u8, b: u8, c: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | 0x85
}
const TYPE_BINDER: u32 = pack(b's', b'b', b'*');
const TYPE_HANDLE: u32 = pack(b's', b'h', b'*');
const TYPE_FD: u32 = pack(b'f', b'd', b'*');
const TYPE_FDA: u32 = pack(b'f', b'd', b'a');
const TYPE_PTR: u32 = pack(b'p', b't', b'*');
const FLAG_ACCEPTS_FDS: u32 = 0x100;
const FLAG_TXN_SECURITY_CTX: u32 = 0x1000;
const BUFFER_FLAG_HAS_PARENT: u32 = 1;
const TF_ONE_WAY: u32 = 0x01;
const TF_ACCEPT_FDS: u32 = 0x10;

const MAP_SIZE: usize = 128 * 1024;
const MOUNT: usize = 165;

// Service protocol of the test.
const SVC_ECHO: u32 = 7;
const SM_ADD: u32 = 1;
const SM_GET: u32 = 2;
const SM_INFO: u32 = 3;
const ECHO: u32 = 10;
const ONEWAY: u32 = 11;
const COUNT: u32 = 12;
const FDS: u32 = 13;
const CALLBACK: u32 = 14;
const PASS_BACK: u32 = 15;
const SG: u32 = 16;
const SLOW: u32 = 17;
const PING_CB: u32 = 20;
const EXIT: u32 = 99;

const CLIENT_PTR: u64 = 0x3000;
const CLIENT_COOKIE: u64 = 0x3001;
const SERVER_PTR: u64 = 0x1000;
const SERVER_COOKIE: u64 = 0x2000;

fn ioctl(fd: i32, cmd: u32, arg: usize) -> isize {
    unsafe { syscall!(16, fd, cmd, arg) }
}

fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Fixed-size little-endian byte builder.
struct Buf<const N: usize> {
    b: [u8; N],
    n: usize,
}

impl<const N: usize> Buf<N> {
    fn new() -> Self {
        Buf { b: [0; N], n: 0 }
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.b[self.n..self.n + v.len()].copy_from_slice(v);
        self.n += v.len();
        self
    }
    /// flat_binder_object / binder_fd_object (24 bytes).
    fn obj(&mut self, ty: u32, flags: u32, val: u64, cookie: u64) -> &mut Self {
        self.u32(ty).u32(flags).u64(val).u64(cookie)
    }
    fn get(&self) -> &[u8] {
        &self.b[..self.n]
    }
}

/// A received binder_transaction_data.
#[derive(Clone, Copy, Default)]
struct Tr {
    target: u64,
    cookie: u64,
    code: u32,
    flags: u32,
    pid: u32,
    euid: u32,
    data: u64,
    data_size: u64,
    offs: u64,
    offs_size: u64,
    secctx: u64,
}

impl Tr {
    fn bytes(&self) -> &'static [u8] {
        if self.data_size == 0 {
            return &[];
        }
        unsafe { core::slice::from_raw_parts(self.data as *const u8, self.data_size as usize) }
    }
    fn offsets(&self, i: usize) -> u64 {
        unsafe { *(self.offs as *const u64).add(i) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BErr {
    Dead,
    Failed,
    Ioctl,
    Timeout,
}

enum Ev {
    Txn(Tr),
    Reply(Tr),
    Complete,
    Dead(u64),
    ClearDone(u64),
    Fail(BErr),
}

/// One binder thread's connection: pending commands and a read buffer.
struct Conn {
    fd: i32,
    out: Buf<1024>,
    rbuf: [u8; 256],
    rpos: usize,
    rlen: usize,
    spawn: u32,
    increfs: u32,
    acquires: u32,
    /// Pointers of BR_RELEASE / BR_DECREFS seen.
    released: [u64; 8],
    nrel: usize,
}

impl Conn {
    fn new(fd: i32) -> Conn {
        Conn { fd, out: Buf::new(), rbuf: [0; 256], rpos: 0, rlen: 0, spawn: 0, increfs: 0, acquires: 0, released: [0; 8], nrel: 0 }
    }

    /// BINDER_WRITE_READ with the pending commands (and a read if asked).
    fn flush(&mut self, read: bool) -> isize {
        let mut bwr = [0u64; 6];
        bwr[0] = self.out.n as u64;
        bwr[2] = self.out.b.as_ptr() as u64;
        if read {
            bwr[3] = self.rbuf.len() as u64;
            bwr[5] = self.rbuf.as_mut_ptr() as u64;
        }
        let r = ioctl(self.fd, BINDER_WRITE_READ, bwr.as_mut_ptr() as usize);
        let wc = bwr[1] as usize;
        self.out.b.copy_within(wc..self.out.n, 0);
        self.out.n -= wc;
        if read && r >= 0 {
            self.rpos = 0;
            self.rlen = bwr[4] as usize;
        }
        r
    }

    fn next(&mut self) -> Ev {
        self.next_timeout(-1)
    }

    /// Next interesting return. NOOP, SPAWN_LOOPER and the node reference
    /// returns are handled here (INCREFS/ACQUIRE are acknowledged).
    /// `ms` >= 0 polls the fd first when nothing is buffered.
    fn next_timeout(&mut self, ms: i32) -> Ev {
        loop {
            if self.rpos >= self.rlen {
                if ms >= 0 && self.out.n == 0 {
                    let mut p = [Pollfd { fd: self.fd, events: EPOLLIN as i16, revents: 0 }];
                    if poll(&mut p, ms) == 0 {
                        return Ev::Fail(BErr::Timeout);
                    }
                }
                let r = self.flush(true);
                if r == -(EINTR as isize) {
                    continue;
                }
                if r < 0 {
                    return Ev::Fail(BErr::Ioctl);
                }
                continue;
            }
            let cmd = rd32(&self.rbuf, self.rpos);
            let a = self.rpos + 4;
            self.rpos = a + ioc_size(cmd);
            let b = &self.rbuf;
            match cmd {
                BR_NOOP => {}
                BR_SPAWN_LOOPER => self.spawn += 1,
                BR_INCREFS | BR_ACQUIRE => {
                    let (ptr, cookie) = (rd64(b, a), rd64(b, a + 8));
                    if cmd == BR_INCREFS {
                        self.increfs += 1;
                        self.out.u32(BC_INCREFS_DONE);
                    } else {
                        self.acquires += 1;
                        self.out.u32(BC_ACQUIRE_DONE);
                    }
                    self.out.u64(ptr).u64(cookie);
                }
                BR_RELEASE | BR_DECREFS => {
                    let ptr = rd64(b, a);
                    self.released[self.nrel % 8] = ptr;
                    self.nrel += 1;
                }
                BR_TRANSACTION_COMPLETE => return Ev::Complete,
                BR_TRANSACTION | BR_TRANSACTION_SEC_CTX | BR_REPLY => {
                    let t = Tr {
                        target: rd64(b, a),
                        cookie: rd64(b, a + 8),
                        code: rd32(b, a + 16),
                        flags: rd32(b, a + 20),
                        pid: rd32(b, a + 24),
                        euid: rd32(b, a + 28),
                        data_size: rd64(b, a + 32),
                        offs_size: rd64(b, a + 40),
                        data: rd64(b, a + 48),
                        offs: rd64(b, a + 56),
                        secctx: if cmd == BR_TRANSACTION_SEC_CTX { rd64(b, a + 64) } else { 0 },
                    };
                    return if cmd == BR_REPLY { Ev::Reply(t) } else { Ev::Txn(t) };
                }
                BR_DEAD_REPLY => return Ev::Fail(BErr::Dead),
                BR_FAILED_REPLY => return Ev::Fail(BErr::Failed),
                BR_DEAD_BINDER => return Ev::Dead(rd64(b, a)),
                BR_CLEAR_DEATH_NOTIFICATION_DONE => return Ev::ClearDone(rd64(b, a)),
                _ => {
                    println!("# unexpected binder return {:#x}", cmd);
                    return Ev::Fail(BErr::Ioctl);
                }
            }
        }
    }

    fn released(&self, ptr: u64) -> usize {
        self.released.iter().take(self.nrel.min(8)).filter(|&&p| p == ptr).count()
    }

    fn tr(&mut self, cmd: u32, handle: u32, code: u32, flags: u32, data: &[u8], offs: &[u64]) {
        self.out.u32(cmd).u64(handle as u64).u64(0).u32(code).u32(flags).u32(0).u32(0);
        self.out.u64(data.len() as u64).u64((offs.len() * 8) as u64);
        self.out.u64(data.as_ptr() as u64).u64(offs.as_ptr() as u64);
    }

    /// A transaction; returns the reply (default Tr for oneway calls).
    /// Calls arriving meanwhile (nested) go to `nested`.
    fn call_with(&mut self, handle: u32, code: u32, flags: u32, data: &[u8], offs: &[u64], nested: &mut dyn FnMut(&mut Conn, &Tr)) -> Result<Tr, BErr> {
        self.tr(BC_TRANSACTION, handle, code, flags, data, offs);
        // Flush now, while `data` and `offs` are alive.
        if self.flush(false) < 0 {
            return Err(BErr::Ioctl);
        }
        loop {
            match self.next() {
                Ev::Complete if flags & TF_ONE_WAY != 0 => return Ok(Tr::default()),
                Ev::Reply(t) => return Ok(t),
                Ev::Txn(t) => nested(self, &t),
                Ev::Fail(e) => return Err(e),
                _ => {}
            }
        }
    }

    fn call(&mut self, handle: u32, code: u32, flags: u32, data: &[u8], offs: &[u64]) -> Result<Tr, BErr> {
        self.call_with(handle, code, flags, data, offs, &mut |_, _| {})
    }

    fn free(&mut self, t: &Tr) {
        self.out.u32(BC_FREE_BUFFER).u64(t.data);
    }

    /// Free the request buffer and send the reply.
    fn reply(&mut self, req: &Tr, flags: u32, data: &[u8], offs: &[u64]) {
        self.free(req);
        self.tr(BC_REPLY, 0, 0, flags, data, offs);
        self.flush(false);
    }

    fn cmd_handle(&mut self, cmd: u32, h: u32) {
        self.out.u32(cmd).u32(h);
    }
}

/// Open a binder device and map its buffer; returns the fd.
fn open_binder(path: &core::ffi::CStr) -> i32 {
    let fd = open(path.as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0);
    if fd < 0 {
        return fd as i32;
    }
    let m = mmap(0, MAP_SIZE, PROT_READ, MAP_PRIVATE, fd as i32, 0);
    if m < 0 {
        return m as i32;
    }
    fd as i32
}

fn reply_u32s(c: &mut Conn, req: &Tr, vals: &[u32]) {
    let mut b = Buf::<64>::new();
    for &v in vals {
        b.u32(v);
    }
    c.reply(req, 0, b.get(), &[]);
}

const DEV: &core::ffi::CStr = c"/dev/binderfs/c1test";

// ---------------------------------------------------------------------------
// Context manager (child): a tiny service manager.

fn service_manager() -> i32 {
    let fd = open_binder(DEV);
    if fd < 0 {
        return 101;
    }
    let mut fbo = Buf::<24>::new();
    fbo.obj(TYPE_BINDER, FLAG_TXN_SECURITY_CTX, 0, 0);
    if ioctl(fd, BINDER_SET_CONTEXT_MGR_EXT, fbo.b.as_ptr() as usize) < 0 {
        return 102;
    }
    let mut c = Conn::new(fd);
    c.out.u32(BC_ENTER_LOOPER);
    let mut services = [(0u32, 0u32); 4];
    let mut bad = 0u32;
    loop {
        let t = match c.next() {
            Ev::Txn(t) => t,
            Ev::Fail(_) => return 103,
            _ => continue,
        };
        // Transactions to a TXN_SECURITY_CTX node carry a context string.
        let ctx_ok = t.secctx != 0 && unsafe { core::slice::from_raw_parts(t.secctx as *const u8, 4) } == b"u:r:";
        if !ctx_ok {
            bad |= 1;
        }
        let d = t.bytes();
        match t.code {
            SM_ADD => {
                // name, pad, flat_binder_object (a handle for us).
                if t.offs_size != 8 || t.offsets(0) != 8 || rd32(d, 8) != TYPE_HANDLE {
                    bad |= 2;
                }
                let h = rd32(d, 16);
                c.cmd_handle(BC_ACQUIRE, h);
                if let Some(slot) = services.iter_mut().find(|s| s.0 == 0) {
                    *slot = (rd32(d, 0), h);
                }
                reply_u32s(&mut c, &t, &[0, t.pid]);
            }
            SM_GET => {
                let name = rd32(d, 0);
                match services.iter().find(|s| s.0 == name && name != 0) {
                    Some(&(_, h)) => {
                        let mut b = Buf::<32>::new();
                        b.u32(0).u32(0).obj(TYPE_HANDLE, 0, h as u64, 0);
                        c.reply(&t, 0, b.get(), &[8]);
                    }
                    None => reply_u32s(&mut c, &t, &[1]),
                }
            }
            SM_INFO => {
                let name = rd32(d, 0);
                let h = services.iter().find(|s| s.0 == name).map_or(0, |s| s.1);
                let mut info = [0u32; 6];
                info[0] = h;
                let r = ioctl(fd, BINDER_GET_NODE_INFO_FOR_REF, info.as_mut_ptr() as usize);
                reply_u32s(&mut c, &t, &[r as u32, info[1], info[2]]);
            }
            EXIT => {
                reply_u32s(&mut c, &t, &[bad]);
                return bad as i32;
            }
            _ => reply_u32s(&mut c, &t, &[0xffff_ffff]),
        }
    }
}

// ---------------------------------------------------------------------------
// Server (child): one service object, a looper thread pool.

static S_FD: AtomicU32 = AtomicU32::new(0);
static S_BAD: AtomicU32 = AtomicU32::new(0);
static ONEWAY_SEEN: AtomicU32 = AtomicU32::new(0);
static ONEWAY_ORDER_BAD: AtomicU32 = AtomicU32::new(0);
static SPAWNED: AtomicU32 = AtomicU32::new(0);

fn server() -> i32 {
    let fd = open_binder(DEV);
    if fd < 0 {
        return 101;
    }
    S_FD.store(fd as u32, Ordering::Relaxed);
    let two = 2u32;
    let one = 1u32;
    if ioctl(fd, BINDER_SET_MAX_THREADS, &two as *const u32 as usize) < 0
        || ioctl(fd, BINDER_ENABLE_ONEWAY_SPAM_DETECTION, &one as *const u32 as usize) < 0
    {
        return 102;
    }
    let mut c = Conn::new(fd);
    // Register with the context manager (retry until it exists).
    let mut b = Buf::<32>::new();
    b.u32(SVC_ECHO).u32(0).obj(TYPE_BINDER, FLAG_ACCEPTS_FDS, SERVER_PTR, SERVER_COOKIE);
    let t0 = now_ns();
    let r = loop {
        match c.call(0, SM_ADD, 0, b.get(), &[8]) {
            Err(BErr::Dead) if now_ns() - t0 < 10_000_000_000 => sleep_ms(10),
            r => break r,
        }
    };
    let Ok(rep) = r else { return 103 };
    if rep.data_size != 8 || rd32(rep.bytes(), 4) != getpid() as u32 {
        return 104;
    }
    c.free(&rep);
    // The node's first strong ref was reported to the sending thread.
    if c.increfs == 0 || c.acquires == 0 {
        return 105;
    }
    c.out.u32(BC_ENTER_LOOPER);
    serve(&mut c)
}

extern "C" fn server_looper(_: usize) -> i32 {
    let mut c = Conn::new(S_FD.load(Ordering::Relaxed) as i32);
    c.out.u32(BC_REGISTER_LOOPER);
    serve(&mut c)
}

fn serve(c: &mut Conn) -> i32 {
    loop {
        while c.spawn > 0 {
            c.spawn -= 1;
            SPAWNED.fetch_add(1, Ordering::Relaxed);
            spawn(server_looper, 0, 0);
        }
        let t = match c.next() {
            Ev::Txn(t) => t,
            Ev::Fail(_) => return 110,
            _ => continue,
        };
        if t.target != SERVER_PTR || t.cookie != SERVER_COOKIE {
            S_BAD.fetch_or(1, Ordering::Relaxed);
        }
        let d = t.bytes();
        match t.code {
            ECHO => {
                let mut b = Buf::<256>::new();
                b.u32(t.pid).u32(t.euid).bytes(&d[..d.len().min(200)]);
                c.reply(&t, 0, b.get(), &[]);
            }
            ONEWAY => {
                if t.flags & TF_ONE_WAY == 0 || t.pid != 0 {
                    S_BAD.fetch_or(2, Ordering::Relaxed);
                }
                if rd32(d, 0) != ONEWAY_SEEN.load(Ordering::Relaxed) {
                    ONEWAY_ORDER_BAD.store(1, Ordering::Relaxed);
                }
                ONEWAY_SEEN.fetch_add(1, Ordering::Relaxed);
                c.free(&t);
            }
            COUNT => {
                let v = [ONEWAY_SEEN.load(Ordering::Relaxed), ONEWAY_ORDER_BAD.load(Ordering::Relaxed), SPAWNED.load(Ordering::Relaxed), S_BAD.load(Ordering::Relaxed)];
                reply_u32s(c, &t, &v);
            }
            FDS => serve_fds(c, &t),
            CALLBACK => {
                // Call the client's object back while it waits for us.
                let ok = t.offs_size == 8 && rd32(d, 0) == TYPE_HANDLE;
                let h = rd32(d, 8);
                let arg = 41u32.to_le_bytes();
                let res = match c.call(h, PING_CB, 0, &arg, &[]) {
                    Ok(r) => {
                        let v = if r.data_size >= 4 { rd32(r.bytes(), 0) } else { 0 };
                        c.free(&r);
                        v
                    }
                    Err(_) => 0,
                };
                reply_u32s(c, &t, &[ok as u32, res]);
            }
            PASS_BACK => {
                // Return the client's own object: it arrives as a binder.
                // The request buffer holds our ref: free it after replying.
                let mut b = Buf::<24>::new();
                b.obj(TYPE_HANDLE, 0, rd32(d, 8) as u64, 0);
                c.tr(BC_REPLY, 0, 0, 0, b.get(), &[0]);
                c.free(&t);
                c.flush(false);
            }
            SG => serve_sg(c, &t),
            SLOW => {
                sleep_ms(300);
                reply_u32s(c, &t, &[gettid() as u32]);
            }
            EXIT => {
                let bad = S_BAD.load(Ordering::Relaxed);
                reply_u32s(c, &t, &[bad]);
                exit(bad as i32);
            }
            _ => reply_u32s(c, &t, &[0xffff_ffff]),
        }
    }
}

/// FDS: read the passed fd, reply with a pipe of our own.
fn serve_fds(c: &mut Conn, t: &Tr) {
    let d = t.bytes();
    let mut got = [0u8; 16];
    let mut n = 0;
    if t.offs_size == 8 && rd32(d, 0) == TYPE_FD {
        let fd = rd32(d, 8) as i32;
        n = read(fd, &mut got).max(0) as usize;
        close(fd);
    }
    let ok = &got[..n] == b"hello-fd";
    let mut p = [0i32; 2];
    pipe2(&mut p, 0);
    write(p[1], b"pong");
    let mut b = Buf::<32>::new();
    b.obj(TYPE_FD, 0, p[0] as u64, 0).u32(ok as u32);
    c.reply(t, 0, b.get(), &[0]);
    // The reply holds its own reference to the pipe.
    close(p[0]);
    close(p[1]);
}

/// SG: two linked buffers (child pointer patched into the parent) and an
/// fd array inside a third buffer.
fn serve_sg(c: &mut Conn, t: &Tr) {
    let d = t.bytes();
    let mut bad = 0u32;
    if t.offs_size != 32 || rd32(d, 0) != TYPE_PTR || rd32(d, 40) != TYPE_PTR || rd32(d, 120) != TYPE_FDA {
        bad |= 1;
    }
    let (outer, inner) = (rd64(d, 8), rd64(d, 48));
    let in_buffer = |a: u64| a >= t.data && a < t.data + MAP_SIZE as u64;
    if bad == 0 && (!in_buffer(outer) || !in_buffer(inner)) {
        bad |= 2;
    }
    let mut fds = [-1i32; 2];
    if bad == 0 {
        let o = unsafe { core::slice::from_raw_parts(outer as *const u8, 16) };
        let i = unsafe { core::slice::from_raw_parts(inner as *const u8, 32) };
        if rd64(o, 0) != inner || rd64(o, 8) != 0x5ca7_7e12 {
            bad |= 4;
        }
        if &i[..19] != b"scatter-gather-data" {
            bad |= 8;
        }
        let fa = unsafe { core::slice::from_raw_parts(rd64(d, 88) as *const u8, 8) };
        fds = [rd32(fa, 0) as i32, rd32(fa, 4) as i32];
        for (k, &fd) in fds.iter().enumerate() {
            let mut b = [0u8; 4];
            if read(fd, &mut b) != 3 || b[..3] != *[b"fd0", b"fd1"][k] {
                bad |= 16;
            }
        }
    }
    // Fds from an fd array are closed when the buffer is freed.
    c.free(t);
    c.flush(false);
    if fds[0] >= 0 && (fcntl(fds[0], F_GETFD, 0) != -(EBADF as isize) || fcntl(fds[1], F_GETFD, 0) != -(EBADF as isize)) {
        bad |= 32;
    }
    let v = bad.to_le_bytes();
    c.tr(BC_REPLY, 0, 0, 0, &v, &[]);
    c.flush(false);
}

// ---------------------------------------------------------------------------
// Client (the test process)

static SLOW_TIDS: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
static CLIENT_FD: AtomicU32 = AtomicU32::new(0);
static HANDLE: AtomicUsize = AtomicUsize::new(0);

extern "C" fn slow_caller(i: usize) -> i32 {
    let mut c = Conn::new(CLIENT_FD.load(Ordering::Relaxed) as i32);
    if let Ok(r) = c.call(HANDLE.load(Ordering::Relaxed) as u32, SLOW, 0, &[], &[]) {
        SLOW_TIDS[i].store(rd32(r.bytes(), 0), Ordering::Relaxed);
        c.free(&r);
        c.flush(false);
    }
    ioctl(c.fd, BINDER_THREAD_EXIT, 0);
    0
}

fn is_rust_kernel() -> bool {
    let mut u = [0u8; 65 * 6];
    unsafe { syscall!(63, u.as_mut_ptr()) };
    u[65 * 2..65 * 3].windows(10).any(|w| w == b"android-pc")
}

fn main() {
    if !is_rust_kernel() {
        skip("all", "binder tests need the Rust kernel (binderfs needs root here)");
        return;
    }
    binderfs();
    contexts();
    ipc();
}

fn binderfs() {
    let r = mkdirat(AT_FDCWD, c"/dev/binderfs".as_ptr() as *const u8, 0o755);
    check!(r == 0 || r == -(EEXIST as isize), "mkdir_binderfs", "returned {}", errname(r));
    let r = unsafe { syscall!(MOUNT, c"binder".as_ptr(), c"/dev/binderfs".as_ptr(), c"binder".as_ptr(), 0, c"stats=global".as_ptr()) };
    check_ok!(r, "mount_binderfs");
    let mut st = Stat::default();
    for (name, case) in [(c"/dev/binderfs/binder", "node_binder"), (c"/dev/binderfs/hwbinder", "node_hwbinder"), (c"/dev/binderfs/vndbinder", "node_vndbinder"), (c"/dev/binderfs/binder-control", "node_control")] {
        let r = fstatat(AT_FDCWD, name.as_ptr() as *const u8, &mut st, 0);
        check!(r == 0 && st.mode & S_IFMT == S_IFCHR, case, "stat {} mode {:o}", errname(r), st.mode);
    }

    let ctl = open(c"/dev/binderfs/binder-control".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0) as i32;
    let mut dev = [0u8; 264];
    dev[..6].copy_from_slice(b"c1test");
    let r = ioctl(ctl, BINDER_CTL_ADD, dev.as_mut_ptr() as usize);
    let (major, minor) = (rd32(&dev, 256), rd32(&dev, 260));
    let r2 = fstatat(AT_FDCWD, DEV.as_ptr() as *const u8, &mut st, 0);
    let rdev_ok = st.rdev == (((minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)) as u64);
    check!(r == 0 && r2 == 0 && major != 0 && rdev_ok, "ctl_add_device", "ioctl {} stat {} major {} minor {}", errname(r), errname(r2), major, minor);
    check_err!(ioctl(ctl, BINDER_CTL_ADD, dev.as_mut_ptr() as usize), EEXIST, "ctl_add_exists");
    close(ctl);

    let fd = open(c"/dev/binderfs/binder".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0) as i32;
    let mut v = 0i32;
    let r = ioctl(fd, BINDER_VERSION, &mut v as *mut i32 as usize);
    check!(r == 0 && v == 8, "version", "returned {} version {}", errname(r), v);
    check_err!(mmap(0, MAP_SIZE, PROT_READ | PROT_WRITE, MAP_PRIVATE, fd, 0), EPERM, "mmap_writable_eperm");
    let m = mmap(0, MAP_SIZE, PROT_READ, MAP_PRIVATE, fd, 0);
    check_ok!(m, "mmap_readonly");
    check_err!(mmap(0, MAP_SIZE, PROT_READ, MAP_PRIVATE, fd, 0), EBUSY, "mmap_twice_ebusy");
    check_err!(mprotect(m as *mut u8, MAP_SIZE, PROT_READ | PROT_WRITE), EACCES, "mprotect_write_eacces");
    // A new thread's first read returns at once, the next one would block.
    let nb = open(c"/dev/binderfs/binder".as_ptr() as *const u8, O_RDWR | O_CLOEXEC | O_NONBLOCK, 0) as i32;
    let mut rb = [0u8; 128];
    let mut bwr = [0u64, 0, 0, 128, 0, rb.as_mut_ptr() as u64];
    let r1 = ioctl(nb, BINDER_WRITE_READ, bwr.as_mut_ptr() as usize);
    let first_noop = bwr[4] == 4 && rd32(&rb, 0) == BR_NOOP;
    bwr[4] = 0;
    let r2 = ioctl(nb, BINDER_WRITE_READ, bwr.as_mut_ptr() as usize);
    check!(r1 == 0 && first_noop && r2 == -(EAGAIN as isize), "nonblock_read", "first {} (noop {}), second {}", errname(r1), first_noop, errname(r2));
    close(nb);
    munmap(m as *mut u8, MAP_SIZE);
    close(fd);
}

fn contexts() {
    let path = |p: &core::ffi::CStr| open(p.as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0) as i32;
    let a = path(c"/dev/binderfs/binder");
    let b = path(c"/dev/binderfs/binder");
    let h = path(c"/dev/binderfs/hwbinder");
    check_ok!(ioctl(a, BINDER_SET_CONTEXT_MGR, 0), "context_mgr_set");
    check_err!(ioctl(b, BINDER_SET_CONTEXT_MGR, 0), EBUSY, "context_mgr_busy");
    check_ok!(ioctl(h, BINDER_SET_CONTEXT_MGR, 0), "context_separate_device");
    // Only the context manager may ask about refs.
    let mut info = [0u32; 6];
    check_err!(ioctl(b, BINDER_GET_NODE_INFO_FOR_REF, info.as_mut_ptr() as usize), EPERM, "node_info_not_mgr");
    info[0] = 1;
    check_err!(ioctl(a, BINDER_GET_NODE_INFO_FOR_REF, info.as_mut_ptr() as usize), EINVAL, "node_info_bad_handle");
    close(a);
    check_ok!(ioctl(b, BINDER_SET_CONTEXT_MGR, 0), "context_mgr_released_on_close");
    close(b);
    close(h);
}

fn ipc() {
    let sm = fork();
    if sm == 0 {
        exit(service_manager());
    }
    let srv = fork();
    if srv == 0 {
        exit(server());
    }
    let fd = open_binder(DEV);
    check!(fd >= 0, "client_open", "returned {}", errname(fd as isize));
    CLIENT_FD.store(fd as u32, Ordering::Relaxed);
    let mut c = Conn::new(fd);

    // Look the service up through handle 0 (retry until registered).
    let t0 = now_ns();
    let name = SVC_ECHO.to_le_bytes();
    let mut hs = 0;
    while now_ns() - t0 < 10_000_000_000 {
        match c.call(0, SM_GET, 0, &name, &[]) {
            Ok(r) => {
                let d = r.bytes();
                if rd32(d, 0) == 0 && r.offs_size == 8 && rd32(d, 8) == TYPE_HANDLE {
                    hs = rd32(d, 16);
                    // Keep the ref past the reply buffer.
                    c.cmd_handle(BC_ACQUIRE, hs);
                    c.cmd_handle(BC_INCREFS, hs);
                    c.free(&r);
                    break;
                }
                c.free(&r);
            }
            Err(BErr::Dead) => {}
            Err(e) => {
                println!("# SM_GET: {:?}", e);
                break;
            }
        }
        sleep_ms(10);
    }
    check!(hs != 0, "lookup_via_handle0", "no handle for the service");
    HANDLE.store(hs as usize, Ordering::Relaxed);

    // Synchronous call: data round trip, sender pid and euid.
    let msg = b"hello binder";
    match c.call(hs, ECHO, 0, msg, &[]) {
        Ok(r) => {
            let d = r.bytes();
            check!(d.len() == 8 + msg.len() && &d[8..] == msg, "sync_call", "reply of {} bytes", d.len());
            check_eq!(rd32(d, 0), getpid() as u32, "sender_pid");
            check_eq!(rd32(d, 4), 0, "sender_euid");
            c.free(&r);
        }
        Err(e) => {
            check!(false, "sync_call", "{:?}", e);
        }
    }

    // The context manager sees the strong refs of the context manager
    // itself and of this client on the service node.
    match c.call(0, SM_INFO, 0, &name, &[]) {
        Ok(r) => {
            let d = r.bytes();
            check!(rd32(d, 0) == 0 && rd32(d, 4) >= 2, "node_info_for_ref", "ioctl {} strong {} weak {}", rd32(d, 0) as i32, rd32(d, 4), rd32(d, 8));
            c.free(&r);
        }
        Err(e) => {
            check!(false, "node_info_for_ref", "{:?}", e);
        }
    }

    // Oneway calls: complete at once, delivered in order.
    let mut all = true;
    for i in 0..5u32 {
        all &= c.call(hs, ONEWAY, TF_ONE_WAY, &i.to_le_bytes(), &[]).is_ok();
    }
    check!(all, "oneway_complete");
    let mut counts = [0u32; 4];
    let t0 = now_ns();
    while now_ns() - t0 < 5_000_000_000 {
        if let Ok(r) = c.call(hs, COUNT, 0, &[], &[]) {
            let d = r.bytes();
            counts = [rd32(d, 0), rd32(d, 4), rd32(d, 8), rd32(d, 12)];
            c.free(&r);
        }
        if counts[0] >= 5 {
            break;
        }
        sleep_ms(10);
    }
    check_eq!(counts[0], 5, "oneway_delivered");
    check_eq!(counts[1], 0, "oneway_in_order");

    // Fd passing: to the server, and back in the reply.
    let mut p = [0i32; 2];
    pipe2(&mut p, 0);
    write(p[1], b"hello-fd");
    close(p[1]);
    let mut b = Buf::<24>::new();
    b.obj(TYPE_FD, 0, p[0] as u64, 0);
    match c.call(hs, FDS, TF_ACCEPT_FDS, b.get(), &[0]) {
        Ok(r) => {
            let d = r.bytes();
            check_eq!(rd32(d, 24), 1, "fd_to_server");
            let rfd = rd32(d, 8) as i32;
            let mut got = [0u8; 8];
            let n = read(rfd, &mut got);
            check!(rd32(d, 0) == TYPE_FD && n == 4 && &got[..4] == b"pong", "fd_in_reply", "type {:#x} read {}", rd32(d, 0), n);
            check_eq!(fcntl(rfd, F_GETFD, 0), FD_CLOEXEC as isize, "fd_installed_cloexec");
            close(rfd);
            c.free(&r);
        }
        Err(e) => {
            check!(false, "fd_to_server", "{:?}", e);
        }
    }
    close(p[0]);

    // A binder object of ours, called back by the server while we wait
    // (nested call routed to this thread).
    let mut b = Buf::<24>::new();
    b.obj(TYPE_BINDER, 0, CLIENT_PTR, CLIENT_COOKIE);
    let mut cb_ok = false;
    let r = c.call_with(hs, CALLBACK, 0, b.get(), &[0], &mut |c, t| {
        cb_ok = t.code == PING_CB && t.target == CLIENT_PTR && t.cookie == CLIENT_COOKIE && t.pid == srv as u32;
        let v = rd32(t.bytes(), 0) + 1;
        reply_u32s(c, t, &[v]);
    });
    match r {
        Ok(r) => {
            let d = r.bytes();
            check!(rd32(d, 0) == 1, "binder_arrives_as_handle");
            check!(cb_ok && rd32(d, 4) == 42, "nested_callback", "callback {} result {}", cb_ok, rd32(d, 4));
            c.free(&r);
        }
        Err(e) => {
            check!(false, "nested_callback", "{:?}", e);
        }
    }
    check!(c.increfs >= 1 && c.acquires >= 1, "own_node_acquired", "increfs {} acquires {}", c.increfs, c.acquires);
    let mut dbg = [0u64; 3];
    let r = ioctl(fd, BINDER_GET_NODE_DEBUG_INFO, dbg.as_mut_ptr() as usize);
    check!(r == 0 && dbg[0] == CLIENT_PTR && dbg[1] == CLIENT_COOKIE, "node_debug_info", "returned {} ptr {:#x}", errname(r), dbg[0]);

    // Our object, passed to the server and back, is a binder again.
    let mut b = Buf::<24>::new();
    b.obj(TYPE_BINDER, 0, CLIENT_PTR, CLIENT_COOKIE);
    match c.call(hs, PASS_BACK, 0, b.get(), &[0]) {
        Ok(r) => {
            let d = r.bytes();
            check!(rd32(d, 0) == TYPE_BINDER && rd64(d, 8) == CLIENT_PTR && rd64(d, 16) == CLIENT_COOKIE, "binder_back_to_owner", "type {:#x} ptr {:#x}", rd32(d, 0), rd64(d, 8));
            c.free(&r);
        }
        Err(e) => {
            check!(false, "binder_back_to_owner", "{:?}", e);
        }
    }

    sg_call(&mut c, hs);

    // Two threads calling at once are served by two looper threads.
    let t1 = spawn(slow_caller, 0, 0);
    let t2 = spawn(slow_caller, 1, 0);
    t1.join();
    t2.join();
    let (a, b2) = (SLOW_TIDS[0].load(Ordering::Relaxed), SLOW_TIDS[1].load(Ordering::Relaxed));
    check!(a != 0 && b2 != 0 && a != b2, "parallel_loopers", "tids {} {}", a, b2);
    if let Ok(r) = c.call(hs, COUNT, 0, &[], &[]) {
        let d = r.bytes();
        check!(rd32(d, 8) >= 1, "spawn_looper", "spawned {}", rd32(d, 8));
        check_eq!(rd32(d, 12), 0, "server_saw_valid_targets");
        c.free(&r);
    }

    death(&mut c, hs, srv as i32);

    // The context manager checked every request's security context.
    match c.call(0, EXIT, 0, &[], &[]) {
        Ok(r) => {
            check_eq!(rd32(r.bytes(), 0), 0, "context_manager_secctx");
            c.free(&r);
            c.flush(false);
        }
        Err(e) => {
            check!(false, "context_manager_secctx", "{:?}", e);
        }
    }
    let mut st = 0;
    wait4(sm as i32, &mut st, 0);
    check_eq!(status_code(st), 0, "context_manager_exit");
    close(fd);
}

/// BC_TRANSACTION_SG: parent buffer with a pointer to a child buffer, and
/// an fd array inside a third buffer.
fn sg_call(c: &mut Conn, hs: u32) {
    let inner = *b"scatter-gather-data.............";
    let mut outer = [0u64; 2];
    outer[0] = 0; // patched by the kernel to the child's copy
    outer[1] = 0x5ca7_7e12;
    let mut p0 = [0i32; 2];
    let mut p1 = [0i32; 2];
    pipe2(&mut p0, 0);
    pipe2(&mut p1, 0);
    write(p0[1], b"fd0");
    write(p1[1], b"fd1");
    let fdbuf = [p0[0] as u32, p1[0] as u32];
    let mut b = Buf::<160>::new();
    b.u32(TYPE_PTR).u32(0).u64(outer.as_ptr() as u64).u64(16).u64(0).u64(0);
    b.u32(TYPE_PTR).u32(BUFFER_FLAG_HAS_PARENT).u64(inner.as_ptr() as u64).u64(32).u64(0).u64(0);
    b.u32(TYPE_PTR).u32(0).u64(fdbuf.as_ptr() as u64).u64(8).u64(0).u64(0);
    b.u32(TYPE_FDA).u32(0).u64(2).u64(2).u64(0);
    let offs = [0u64, 40, 80, 120];
    let data = b.get();
    c.out.u32(BC_TRANSACTION_SG).u64(hs as u64).u64(0).u32(SG).u32(TF_ACCEPT_FDS).u32(0).u32(0);
    c.out.u64(data.len() as u64).u64(32).u64(data.as_ptr() as u64).u64(offs.as_ptr() as u64);
    c.out.u64(16 + 32 + 8);
    c.flush(false);
    let res = loop {
        match c.next() {
            Ev::Reply(r) => break Ok(r),
            Ev::Fail(e) => break Err(e),
            _ => {}
        }
    };
    match res {
        Ok(r) => {
            let bad = rd32(r.bytes(), 0);
            check!(bad & 0xf == 0, "scatter_gather", "server check bits {:#x}", bad);
            check!(bad & 16 == 0, "fd_array");
            check!(bad & 32 == 0, "fd_array_closed_on_free");
            c.free(&r);
        }
        Err(e) => {
            check!(false, "scatter_gather", "{:?}", e);
        }
    }
    for fd in [p0[0], p0[1], p1[0], p1[1]] {
        close(fd);
    }
}

/// Death notification when the server process exits, then calls on the
/// dead handle, clearing, and releasing it.
fn death(c: &mut Conn, hs: u32, srv: i32) {
    c.out.u32(BC_REQUEST_DEATH_NOTIFICATION).u32(hs).u64(0xdead);
    match c.call(hs, EXIT, 0, &[], &[]) {
        Ok(r) => {
            check_eq!(rd32(r.bytes(), 0), 0, "server_ok_before_exit");
            c.free(&r);
        }
        Err(e) => {
            check!(false, "server_ok_before_exit", "{:?}", e);
        }
    }
    let mut st = 0;
    wait4(srv, &mut st, 0);
    check_eq!(status_code(st), 0, "server_exit");
    // Death notifications are process work: be a looper to get them.
    c.out.u32(BC_ENTER_LOOPER);
    let mut dead = false;
    loop {
        match c.next_timeout(5000) {
            Ev::Dead(0xdead) => {
                dead = true;
                break;
            }
            Ev::Fail(_) => break,
            _ => {}
        }
    }
    check!(dead, "death_notification");
    c.out.u32(BC_DEAD_BINDER_DONE).u64(0xdead);
    c.out.u32(BC_CLEAR_DEATH_NOTIFICATION).u32(hs).u64(0xdead);
    let mut cleared = false;
    loop {
        match c.next_timeout(5000) {
            Ev::ClearDone(0xdead) => {
                cleared = true;
                break;
            }
            Ev::Fail(_) => break,
            _ => {}
        }
    }
    check!(cleared, "clear_death_notification_done");
    check_eq!(c.call(hs, ECHO, 0, &[], &[]).err(), Some(BErr::Dead), "call_dead_binder");
    // The server's refs on our object went away with it.
    let t0 = now_ns();
    while c.released(CLIENT_PTR) < 2 && now_ns() - t0 < 2_000_000_000 {
        if let Ev::Fail(_) = c.next_timeout(100) {
            continue;
        }
    }
    check!(c.released(CLIENT_PTR) >= 2, "own_node_released", "{} release/decrefs", c.released(CLIENT_PTR));

    // Dropping our last references removes the handle.
    c.cmd_handle(BC_RELEASE, hs);
    c.cmd_handle(BC_DECREFS, hs);
    check_eq!(c.call(hs, ECHO, 0, &[], &[]).err(), Some(BErr::Failed), "released_handle_fails");
    let mut ee = [0u32; 3];
    let r = ioctl(c.fd, BINDER_GET_EXTENDED_ERROR, ee.as_mut_ptr() as usize);
    check!(r == 0 && ee[1] == BR_FAILED_REPLY && ee[2] as i32 == -EINVAL, "extended_error", "command {:#x} param {}", ee[1], ee[2] as i32);
    let r = ioctl(c.fd, BINDER_GET_EXTENDED_ERROR, ee.as_mut_ptr() as usize);
    check!(r == 0 && ee[2] == 0, "extended_error_reset");
}
