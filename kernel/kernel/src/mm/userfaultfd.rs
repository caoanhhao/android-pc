//! userfaultfd: user space resolves page faults of registered ranges.
//!
//! What ART's concurrent mark-compact GC (CMC) needs, with the Linux ABI:
//! `userfaultfd(2)` (syscall 323) and `/dev/userfaultfd`
//! (USERFAULTFD_IOC_NEW) create a context for the caller's address
//! space; UFFDIO_API negotiates features (MISSING_SHMEM, MINOR_SHMEM,
//! SIGBUS, THREAD_ID, EXACT_ADDRESS, MOVE); UFFDIO_REGISTER marks VMAs
//! (private anonymous memory, or shmem for MISSING and MINOR) and
//! UFFDIO_UNREGISTER clears them.
//!
//! A fault on a registered page that has no PTE (MISSING: no page at
//! all, MINOR: the shmem object has the page but this mapping does not)
//! either raises SIGBUS (BUS_ADRERR) at once when the context asked for
//! UFFD_FEATURE_SIGBUS (ART's mode: the mutator's signal handler fills
//! the page itself), or queues a `uffd_msg` that `read` returns and
//! blocks the thread until UFFDIO_COPY / ZEROPAGE / CONTINUE / MOVE /
//! WAKE covers the address (or the range is unregistered, or the
//! context closed). Kernel accesses (copy_in/copy_out) to such a page
//! wait the same way, or fail with EFAULT in SIGBUS mode and for
//! UFFD_USER_MODE_ONLY contexts. Registrations are dropped in a fork
//! child and on the new range of an mremap (no EVENT_FORK / EVENT_REMAP);
//! MREMAP_DONTUNMAP leaves the old, now empty, range registered.
//! UFFDIO_WRITEPROTECT and the WP register mode are not implemented.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use super::paging::phys_to_virt;
use super::user::{AddressSpace, USER_END};
use super::{PAGE_SIZE, frame};
use crate::errno::*;
use crate::fs::file::{FileOps, O_CLOEXEC, O_NONBLOCK, O_RDWR, OpenFile};
use crate::fs::poll::{POLLERR, POLLIN, WaitQueue};
use crate::process::current;
use crate::sched::{self, Wake, WaitMode};

pub const UFFD_API: u64 = 0xaa;
/// userfaultfd(2) flag: only faults from user mode are handled.
pub const UFFD_USER_MODE_ONLY: u32 = 1;

pub const MODE_MISSING: u32 = 1;
pub const MODE_WP: u32 = 2;
pub const MODE_MINOR: u32 = 4;

pub const PAGEFAULT_FLAG_WRITE: u64 = 1;
pub const PAGEFAULT_FLAG_MINOR: u64 = 4;

const FEATURE_MISSING_SHMEM: u64 = 1 << 5;
const FEATURE_SIGBUS: u64 = 1 << 7;
const FEATURE_THREAD_ID: u64 = 1 << 8;
const FEATURE_MINOR_SHMEM: u64 = 1 << 10;
const FEATURE_EXACT_ADDRESS: u64 = 1 << 11;
const FEATURE_MOVE: u64 = 1 << 16;
const FEATURES: u64 =
    FEATURE_MISSING_SHMEM | FEATURE_SIGBUS | FEATURE_THREAD_ID | FEATURE_MINOR_SHMEM | FEATURE_EXACT_ADDRESS | FEATURE_MOVE;

// ioctl numbers (`_UFFDIO_*`) and the commands built from them.
const NR_REGISTER: u32 = 0x00;
const NR_UNREGISTER: u32 = 0x01;
const NR_WAKE: u32 = 0x02;
const NR_COPY: u32 = 0x03;
const NR_ZEROPAGE: u32 = 0x04;
const NR_MOVE: u32 = 0x05;
const NR_CONTINUE: u32 = 0x07;
const NR_API: u32 = 0x3f;

const fn iowr(nr: u32, size: u32) -> u32 {
    (3 << 30) | (size << 16) | (0xaa << 8) | nr
}

const fn ior(nr: u32, size: u32) -> u32 {
    (2 << 30) | (size << 16) | (0xaa << 8) | nr
}

const UFFDIO_API: u32 = iowr(NR_API, 24);
const UFFDIO_REGISTER: u32 = iowr(NR_REGISTER, 32);
const UFFDIO_UNREGISTER: u32 = ior(NR_UNREGISTER, 16);
const UFFDIO_WAKE: u32 = ior(NR_WAKE, 16);
const UFFDIO_COPY: u32 = iowr(NR_COPY, 40);
const UFFDIO_ZEROPAGE: u32 = iowr(NR_ZEROPAGE, 32);
const UFFDIO_MOVE: u32 = iowr(NR_MOVE, 40);
const UFFDIO_CONTINUE: u32 = iowr(NR_CONTINUE, 32);
/// /dev/userfaultfd: `_IO(0xAA, 0)`, argument = userfaultfd(2) flags.
pub const USERFAULTFD_IOC_NEW: u32 = 0xaa00;
/// misc minor of /dev/userfaultfd.
pub const USERFAULTFD_MINOR: u32 = 124;

const MODE_DONTWAKE: u64 = 1;
const COPY_MODE_WP: u64 = 2;
const MOVE_MODE_ALLOW_SRC_HOLES: u64 = 2;

const UFFD_EVENT_PAGEFAULT: u8 = 0x12;
const MSG_SIZE: usize = 32;

const UFFD_KEY: u64 = 0x5546_4644_0000_0000;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A thread waiting for its fault to be resolved.
struct Pending {
    id: u64,
    addr: u64,
    flags: u64,
    tid: u32,
    /// Already returned by `read`.
    read: bool,
}

struct State {
    initialized: bool,
    features: u64,
    faults: Vec<Pending>,
    released: bool,
}

pub struct UffdCtx {
    id: u64,
    mm: Weak<AddressSpace>,
    user_only: bool,
    state: Mutex<State>,
    wq: Arc<WaitQueue>,
}

/// What fills a missing page.
pub enum Fill {
    /// UFFDIO_COPY: this frame (owned by the callee from now on).
    Frame(x86_64::structures::paging::PhysFrame),
    /// UFFDIO_ZEROPAGE.
    Zero,
    /// UFFDIO_CONTINUE: the shmem object's own page.
    Continue,
}

impl UffdCtx {
    fn key(&self) -> sched::WaitKey {
        (UFFD_KEY, self.id)
    }

    /// Faults raise SIGBUS instead of waiting (UFFD_FEATURE_SIGBUS).
    pub fn sigbus(&self) -> bool {
        self.state.lock().features & FEATURE_SIGBUS != 0
    }

    /// Kernel accesses to a missing page fail instead of waiting.
    pub fn kernel_faults_fail(&self) -> bool {
        self.user_only || self.sigbus()
    }

    /// Queue a fault at `addr` and wait until it is resolved (`present`
    /// re-checks the PTE, closing the race with a fill that happened
    /// before the message was queued). False if a signal interrupted it.
    pub fn wait_fault(&self, addr: u64, flags: u64, present: impl Fn() -> bool) -> bool {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = self.state.lock();
            if st.released {
                return true;
            }
            let addr = if st.features & FEATURE_EXACT_ADDRESS != 0 { addr } else { addr & !(PAGE_SIZE - 1) };
            st.faults.push(Pending { id, addr, flags, tid: current().tid as u32, read: false });
        }
        self.wq.wake(POLLIN);
        let w = sched::wait_until(self.key(), None, WaitMode::Interruptible, || {
            !self.state.lock().faults.iter().any(|p| p.id == id) || present()
        });
        self.state.lock().faults.retain(|p| p.id != id);
        w == Wake::Ready
    }

    /// Wake the threads waiting on a page of `[start, start + len)`.
    fn resolve(&self, start: u64, len: u64) {
        let page = start & !(PAGE_SIZE - 1);
        let end = start.saturating_add(len);
        self.state.lock().faults.retain(|p| !(p.addr >= page && p.addr < end));
        sched::wake_all(self.key());
    }

    fn mm(&self) -> KResult<Arc<AddressSpace>> {
        self.mm.upgrade().ok_or(ESRCH)
    }

    fn initialized(&self) -> KResult<()> {
        if self.state.lock().initialized { Ok(()) } else { Err(EINVAL) }
    }
}

/// The open file: closing the last reference unregisters every range and
/// wakes the waiting threads (their faults are then handled normally).
struct UffdFile(Arc<UffdCtx>);

impl Drop for UffdFile {
    fn drop(&mut self) {
        let ctx = &self.0;
        {
            let mut st = ctx.state.lock();
            st.released = true;
            st.faults.clear();
        }
        sched::wake_all(ctx.key());
        if let Some(mm) = ctx.mm.upgrade() {
            mm.uffd_release(ctx);
        }
    }
}

fn rd64(b: &[u8], off: usize) -> u64 {
    u64::from_ne_bytes(b[off..off + 8].try_into().unwrap())
}

fn wr64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_ne_bytes());
}

/// Linux's validate_range: page aligned, non-empty, inside user space.
fn validate_range(start: u64, len: u64) -> KResult<u64> {
    if !start.is_multiple_of(PAGE_SIZE) || !len.is_multiple_of(PAGE_SIZE) || len == 0 || start < PAGE_SIZE {
        return Err(EINVAL);
    }
    let end = start.checked_add(len).filter(|&e| e <= USER_END).ok_or(EINVAL)?;
    Ok(end)
}

/// The result of a fill/move over a range, as the ioctls report it:
/// bytes done, else the error; partial success is EAGAIN.
fn finish(ctx: &UffdCtx, arg: u64, b: &mut [u8], field: usize, start: u64, done: u64, err: Option<Errno>, mode: u64) -> KResult<u64> {
    let v = if done > 0 { done as i64 } else { -(err.unwrap_or(EINVAL).0 as i64) };
    wr64(b, field, v as u64);
    current().mm().copy_out(arg + field as u64, &b[field..field + 8])?;
    if done > 0 && mode & MODE_DONTWAKE == 0 {
        ctx.resolve(start, done);
    }
    match err {
        None => Ok(0),
        Some(_) if done > 0 => Err(EAGAIN),
        Some(e) => Err(e),
    }
}

impl UffdFile {
    fn api(&self, arg: u64) -> KResult<u64> {
        let mm = current().mm();
        let mut b = [0u8; 24];
        mm.copy_in(arg, &mut b)?;
        let (api, features) = (rd64(&b, 0), rd64(&b, 8));
        let mut st = self.0.state.lock();
        if st.initialized || api != UFFD_API || features & !FEATURES != 0 {
            drop(st);
            mm.copy_out(arg, &[0u8; 24])?;
            return Err(EINVAL);
        }
        st.initialized = true;
        st.features = features;
        drop(st);
        wr64(&mut b, 8, FEATURES);
        wr64(&mut b, 16, (1 << NR_REGISTER) | (1 << NR_UNREGISTER) | (1 << NR_API));
        mm.copy_out(arg, &b)?;
        Ok(0)
    }

    fn register(&self, arg: u64) -> KResult<u64> {
        self.0.initialized()?;
        let cur = current().mm();
        let mut b = [0u8; 32];
        cur.copy_in(arg, &mut b)?;
        let (start, len, mode) = (rd64(&b, 0), rd64(&b, 8), rd64(&b, 16));
        if mode == 0 || mode & !((MODE_MISSING | MODE_WP | MODE_MINOR) as u64) != 0 || mode & MODE_WP as u64 != 0 {
            return Err(EINVAL);
        }
        let end = validate_range(start, len)?;
        let all_anon = self.0.mm()?.uffd_register(&self.0, start, end, mode as u32)?;
        let mut ioctls = (1u64 << NR_WAKE) | (1 << NR_COPY) | (1 << NR_ZEROPAGE) | (1 << NR_CONTINUE);
        if all_anon {
            ioctls |= 1 << NR_MOVE;
        }
        cur.copy_out(arg + 24, &ioctls.to_ne_bytes())?;
        Ok(0)
    }

    fn range(&self, arg: u64) -> KResult<(u64, u64)> {
        self.0.initialized()?;
        let mut b = [0u8; 16];
        current().mm().copy_in(arg, &mut b)?;
        let (start, len) = (rd64(&b, 0), rd64(&b, 8));
        Ok((start, validate_range(start, len)?))
    }

    fn copy(&self, arg: u64) -> KResult<u64> {
        self.0.initialized()?;
        let cur = current().mm();
        let mut b = [0u8; 40];
        cur.copy_in(arg, &mut b)?;
        let (dst, src, len, mode) = (rd64(&b, 0), rd64(&b, 8), rd64(&b, 16), rd64(&b, 24));
        if mode & !(MODE_DONTWAKE | COPY_MODE_WP) != 0 {
            return Err(EINVAL);
        }
        validate_range(dst, len)?;
        src.checked_add(len).filter(|&e| e <= USER_END).ok_or(EINVAL)?;
        if src < dst + len && dst < src + len {
            return Err(EINVAL);
        }
        let mm = self.0.mm()?;
        let mut done = 0;
        let mut err = None;
        while done < len {
            let Some(f) = frame::alloc() else {
                err = Some(ENOMEM);
                break;
            };
            let page = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(f.start_address()).as_mut_ptr::<u8>(), PAGE_SIZE as usize) };
            if mm.copy_in(src + done, page).is_err() {
                frame::free(f);
                err = Some(EFAULT);
                break;
            }
            if let Err(e) = mm.uffd_fill(&self.0, dst + done, Fill::Frame(f)) {
                err = Some(e);
                break;
            }
            done += PAGE_SIZE;
        }
        finish(&self.0, arg, &mut b, 32, dst, done, err, mode)
    }

    /// UFFDIO_ZEROPAGE and UFFDIO_CONTINUE: struct {range, mode, result}.
    fn fill(&self, arg: u64, cont: bool) -> KResult<u64> {
        self.0.initialized()?;
        let mut b = [0u8; 32];
        current().mm().copy_in(arg, &mut b)?;
        let (start, len, mode) = (rd64(&b, 0), rd64(&b, 8), rd64(&b, 16));
        if mode & !(MODE_DONTWAKE | if cont { COPY_MODE_WP } else { 0 }) != 0 {
            return Err(EINVAL);
        }
        validate_range(start, len)?;
        let mm = self.0.mm()?;
        let mut done = 0;
        let mut err = None;
        while done < len {
            if let Err(e) = mm.uffd_fill(&self.0, start + done, if cont { Fill::Continue } else { Fill::Zero }) {
                err = Some(e);
                break;
            }
            done += PAGE_SIZE;
        }
        finish(&self.0, arg, &mut b, 24, start, done, err, mode)
    }

    fn mv(&self, arg: u64) -> KResult<u64> {
        self.0.initialized()?;
        if self.0.state.lock().features & FEATURE_MOVE == 0 {
            return Err(EINVAL);
        }
        let mut b = [0u8; 40];
        current().mm().copy_in(arg, &mut b)?;
        let (dst, src, len, mode) = (rd64(&b, 0), rd64(&b, 8), rd64(&b, 16), rd64(&b, 24));
        if mode & !(MODE_DONTWAKE | MOVE_MODE_ALLOW_SRC_HOLES) != 0 {
            return Err(EINVAL);
        }
        validate_range(dst, len)?;
        validate_range(src, len)?;
        if src < dst + len && dst < src + len {
            return Err(EINVAL);
        }
        let (done, err) = self.0.mm()?.uffd_move(&self.0, dst, src, len, mode & MOVE_MODE_ALLOW_SRC_HOLES != 0);
        finish(&self.0, arg, &mut b, 32, dst, done, err, mode)
    }
}

impl FileOps for UffdFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        if buf.len() < MSG_SIZE {
            return Err(EINVAL);
        }
        self.0.initialized()?;
        let mut n = 0;
        let w = sched::wait_until(self.0.wq.key(), None, WaitMode::Interruptible, || {
            let mut st = self.0.state.lock();
            let thread_id = st.features & FEATURE_THREAD_ID != 0;
            for p in st.faults.iter_mut().filter(|p| !p.read) {
                if n + MSG_SIZE > buf.len() {
                    break;
                }
                let m = &mut buf[n..n + MSG_SIZE];
                m.fill(0);
                m[0] = UFFD_EVENT_PAGEFAULT;
                wr64(m, 8, p.flags);
                wr64(m, 16, p.addr);
                if thread_id {
                    m[24..28].copy_from_slice(&p.tid.to_ne_bytes());
                }
                p.read = true;
                n += MSG_SIZE;
            }
            n > 0 || f.nonblock()
        });
        match w {
            _ if n > 0 => Ok(n),
            Wake::Ready => Err(EAGAIN),
            _ => Err(ERESTARTSYS),
        }
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        match cmd {
            UFFDIO_API => self.api(arg),
            UFFDIO_REGISTER => self.register(arg),
            UFFDIO_UNREGISTER => {
                let (start, end) = self.range(arg)?;
                self.0.mm()?.uffd_unregister(&self.0, start, end)?;
                self.0.resolve(start, end - start);
                Ok(0)
            }
            UFFDIO_WAKE => {
                let (start, end) = self.range(arg)?;
                self.0.resolve(start, end - start);
                Ok(0)
            }
            UFFDIO_COPY => self.copy(arg),
            UFFDIO_ZEROPAGE => self.fill(arg, false),
            UFFDIO_CONTINUE => self.fill(arg, true),
            UFFDIO_MOVE => self.mv(arg),
            _ => Err(EINVAL),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        let st = self.0.state.lock();
        if !st.initialized {
            return POLLERR;
        }
        if st.faults.iter().any(|p| !p.read) { POLLIN } else { 0 }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(self.0.wq.clone())
    }

    fn anon_name(&self) -> Option<&'static str> {
        Some("[userfaultfd]")
    }

    fn fdinfo(&self, _: &OpenFile) -> String {
        let st = self.0.state.lock();
        let pending = st.faults.iter().filter(|p| p.read).count();
        let total = st.faults.len();
        format!("pending:\t{pending}\ntotal:\t{total}\nAPI:\t{UFFD_API:x}:{:x}:{:x}\n", st.features, 0)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `userfaultfd(2)` / USERFAULTFD_IOC_NEW: a new context for the
/// caller's address space.
pub fn new(flags: u32) -> KResult<Arc<OpenFile>> {
    if flags & !(O_CLOEXEC | O_NONBLOCK | UFFD_USER_MODE_ONLY) != 0 {
        return Err(EINVAL);
    }
    let ctx = Arc::new(UffdCtx {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        mm: Arc::downgrade(&current().mm()),
        user_only: flags & UFFD_USER_MODE_ONLY != 0,
        state: Mutex::new(State { initialized: false, features: 0, faults: Vec::new(), released: false }),
        wq: WaitQueue::new(),
    });
    Ok(OpenFile::new(crate::fs::anon_inode(), None, Box::new(UffdFile(ctx)), O_RDWR | (flags & O_NONBLOCK)))
}

/// /dev/userfaultfd: only the USERFAULTFD_IOC_NEW ioctl.
struct UffdDev;

impl FileOps for UffdDev {
    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        if cmd != USERFAULTFD_IOC_NEW {
            return Err(EINVAL);
        }
        let f = new(arg as u32)?;
        Ok(current().process.files().alloc(f, arg as u32 & O_CLOEXEC != 0, 0)? as u64)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn open_dev() -> KResult<Box<dyn FileOps>> {
    Ok(Box::new(UffdDev))
}
