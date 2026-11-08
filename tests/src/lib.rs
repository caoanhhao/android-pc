//! `rt`: a tiny freestanding runtime for the kernel conformance tests.
//!
//! Every test binary is a static, non-PIE x86_64 ELF with no libc. It talks
//! to the kernel only through the raw `syscall` instruction, so it can run on
//! the Rust kernel long before a libc (bionic/musl) can.
//!
//! Output protocol (see tests/README.md):
//!   BEGIN <test>
//!   PASS <test> <case>
//!   FAIL <test> <case> <detail> [file:line]
//!   SKIP <test> <case> <reason>
//!   END <test> pass=<n> fail=<n>
//! Exit status 0 iff no case failed.

#![no_std]

use core::arch::{asm, global_asm};
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

pub mod nr;
pub mod sys;

pub use sys::*;

// ---------------------------------------------------------------------------
// Entry point, signal trampoline, thread-clone helper, mem* functions.

global_asm!(
    ".globl _start",
    ".type _start,@function",
    "_start:",
    "    xor ebp, ebp",
    "    mov rdi, rsp",
    "    and rsp, -16",
    "    call {start}",
    "    ud2",
    "",
    // rt_sigreturn trampoline used as sa_restorer.
    ".globl __rt_restorer",
    ".type __rt_restorer,@function",
    "__rt_restorer:",
    "    mov eax, 15",
    "    syscall",
    "    ud2",
    "",
    // isize __rt_clone(flags, stack_top, ptid, ctid, tls, fn, arg)
    // Child runs fn(arg) on the new stack, then exit(ret) (thread exit only).
    ".globl __rt_clone",
    ".type __rt_clone,@function",
    "__rt_clone:",
    "    mov r11, [rsp+8]",
    "    and rsi, -16",
    "    sub rsi, 16",
    "    mov [rsi], r9",
    "    mov [rsi+8], r11",
    "    mov r10, rcx",
    "    mov eax, 56",
    "    syscall",
    "    test rax, rax",
    "    jnz 2f",
    "    xor ebp, ebp",
    "    pop rax",
    "    pop rdi",
    "    call rax",
    "    mov edi, eax",
    "    mov eax, 60",
    "    syscall",
    "    ud2",
    "2:  ret",
    start = sym rt_start,
);

unsafe extern "C" {
    fn __rt_restorer();
    fn __rt_clone(
        flags: usize,
        stack_top: *mut u8,
        ptid: *mut i32,
        ctid: *mut i32,
        tls: usize,
        f: extern "C" fn(usize) -> i32,
        arg: usize,
    ) -> isize;
}

/// Address of the `rt_sigreturn` trampoline, for `sa_restorer`.
pub fn restorer() -> usize {
    __rt_restorer as *const () as usize
}

/// Raw `clone` that runs `f(arg)` on `stack_top` in the child and makes the
/// child call `exit` (not `exit_group`) with its return value.
pub unsafe fn clone_raw(
    flags: usize,
    stack_top: *mut u8,
    ptid: *mut i32,
    ctid: *mut i32,
    tls: usize,
    f: extern "C" fn(usize) -> i32,
    arg: usize,
) -> isize {
    unsafe { __rt_clone(flags, stack_top, ptid, ctid, tls, f, arg) }
}

// The compiler may lower these loops back into calls to themselves, so they
// are written with `rep` string instructions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    unsafe {
        asm!("rep movsb", inout("rcx") n => _, inout("rdi") dst => _, inout("rsi") src => _,
             options(nostack, preserves_flags));
    }
    dst
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    if (dst as usize).wrapping_sub(src as usize) >= n {
        return unsafe { memcpy(dst, src, n) };
    }
    // Overlapping with dst after src: copy backwards.
    unsafe {
        asm!("std", "rep movsb", "cld",
             inout("rcx") n => _,
             inout("rdi") (dst as *mut u8).wrapping_add(n).wrapping_sub(1) => _,
             inout("rsi") (src as *const u8).wrapping_add(n).wrapping_sub(1) => _,
             options(nostack));
    }
    dst
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut c_void, c: i32, n: usize) -> *mut c_void {
    unsafe {
        asm!("rep stosb", inout("rcx") n => _, inout("rdi") dst => _, in("al") c as u8,
             options(nostack, preserves_flags));
    }
    dst
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    let (a, b) = (a as *const u8, b as *const u8);
    let mut i = 0;
    while i < n {
        let (x, y) = unsafe { (core::ptr::read_volatile(a.add(i)), core::ptr::read_volatile(b.add(i))) };
        if x != y {
            return x as i32 - y as i32;
        }
        i += 1;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(s: *const core::ffi::c_char) -> usize {
    let mut n = 0;
    while unsafe { core::ptr::read_volatile(s.add(n)) } != 0 {
        n += 1;
    }
    n
}

/// The prebuilt `core` references this even with panic=abort; it is never
/// called because nothing unwinds.
#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(a: *const c_void, b: *const c_void, n: usize) -> i32 {
    unsafe { memcmp(a, b, n) }
}

// ---------------------------------------------------------------------------
// Process start: argv / envp / auxv.

static INITIAL_SP: AtomicUsize = AtomicUsize::new(0);
static MAIN_PID: AtomicI32 = AtomicI32::new(0);

unsafe extern "Rust" {
    fn __rt_test_name() -> &'static str;
    fn __rt_test_main();
}

extern "C" fn rt_start(sp: *const usize) -> ! {
    INITIAL_SP.store(sp as usize, Ordering::Relaxed);
    MAIN_PID.store(getpid(), Ordering::Relaxed);
    if argv(1) == Some(CHILD_FLAG) {
        // Re-executed helper (see `is_child_mode`): no BEGIN/END framing.
        CHILD_MODE.store(true, Ordering::Relaxed);
        unsafe { __rt_test_main() };
        exit(if FAILED.load(Ordering::Relaxed) == 0 { 0 } else { 1 })
    }
    let name = test_name();
    println!("BEGIN {}", name);
    unsafe { __rt_test_main() };
    finish()
}

/// Print the END line and exit with 0 (all passed) or 1.
pub fn finish() -> ! {
    let f = print_end();
    exit(if f == 0 { 0 } else { 1 })
}

/// Print the END line; returns the number of failed cases.
pub fn print_end() -> usize {
    let (p, f) = (PASSED.load(Ordering::Relaxed), FAILED.load(Ordering::Relaxed));
    println!("END {} pass={} fail={}", test_name(), p, f);
    f
}

/// argv[1] that marks a test binary re-executed by itself (execve tests).
pub const CHILD_FLAG: &[u8] = b"--rt-child";
static CHILD_MODE: AtomicBool = AtomicBool::new(false);

/// True when started as `<test> --rt-child ...`: main must dispatch on
/// argv[2..]; PASS lines are suppressed and the exit status is 1 if any
/// check failed.
pub fn is_child_mode() -> bool {
    CHILD_MODE.load(Ordering::Relaxed)
}

pub fn test_name() -> &'static str {
    unsafe { __rt_test_name() }
}

/// True in the process that started the test (not in forked children).
pub fn is_main_process() -> bool {
    getpid() == MAIN_PID.load(Ordering::Relaxed)
}

pub fn argc() -> usize {
    unsafe { *(INITIAL_SP.load(Ordering::Relaxed) as *const usize) }
}

fn argv_ptr() -> *const *const u8 {
    (INITIAL_SP.load(Ordering::Relaxed) + 8) as *const *const u8
}

/// argv[i] as a NUL-terminated C string pointer.
pub fn argv_raw(i: usize) -> Option<*const u8> {
    if i >= argc() {
        return None;
    }
    Some(unsafe { *argv_ptr().add(i) })
}

/// argv[i] without its terminating NUL.
pub fn argv(i: usize) -> Option<&'static [u8]> {
    argv_raw(i).map(|p| unsafe { cstr_bytes(p) })
}

pub fn envp_ptr() -> *const *const u8 {
    unsafe { argv_ptr().add(argc() + 1) }
}

pub fn envc() -> usize {
    let mut n = 0;
    unsafe {
        while !(*envp_ptr().add(n)).is_null() {
            n += 1;
        }
    }
    n
}

pub fn env(i: usize) -> Option<&'static [u8]> {
    if i >= envc() {
        return None;
    }
    Some(unsafe { cstr_bytes(*envp_ptr().add(i)) })
}

/// getauxval(): None if the type is absent.
pub fn getauxval(ty: usize) -> Option<usize> {
    unsafe {
        let mut p = envp_ptr().add(envc() + 1) as *const usize;
        loop {
            let (t, v) = (*p, *p.add(1));
            if t == 0 {
                return None;
            }
            if t == ty {
                return Some(v);
            }
            p = p.add(2);
        }
    }
}

pub const AT_PHDR: usize = 3;
pub const AT_PHENT: usize = 4;
pub const AT_PHNUM: usize = 5;
pub const AT_PAGESZ: usize = 6;
pub const AT_ENTRY: usize = 9;
pub const AT_UID: usize = 11;
pub const AT_RANDOM: usize = 25;
pub const AT_EXECFN: usize = 31;

/// Bytes of a NUL-terminated string.
pub unsafe fn cstr_bytes(p: *const u8) -> &'static [u8] {
    let mut n = 0;
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        core::slice::from_raw_parts(p, n)
    }
}

// ---------------------------------------------------------------------------
// Output.

/// Fixed-size line buffer; each line goes out in one write(2) so lines from
/// several processes/threads never interleave mid-line.
pub struct Buf<const N: usize> {
    pub buf: [u8; N],
    pub len: usize,
}

impl<const N: usize> Buf<N> {
    pub const fn new() -> Self {
        Buf { buf: [0; N], len: 0 }
    }
    pub fn push(&mut self, b: &[u8]) {
        let n = b.len().min(N - 1 - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
        self.buf[self.len] = 0;
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    /// NUL-terminated pointer (the buffer always keeps a trailing NUL).
    pub fn as_ptr(&self) -> *const u8 {
        self.buf.as_ptr()
    }
}

impl<const N: usize> Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

/// A NUL-terminated path built with format syntax: `cpath!("{}/x", dir)`.
pub type Path = Buf<256>;

#[macro_export]
macro_rules! cpath {
    ($($arg:tt)*) => {{
        let mut p = $crate::Path::new();
        let _ = core::fmt::Write::write_fmt(&mut p, format_args!($($arg)*));
        p
    }};
}

pub fn write_all(fd: i32, mut b: &[u8]) {
    while !b.is_empty() {
        let r = write(fd, b);
        if r <= 0 {
            return;
        }
        b = &b[r as usize..];
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    let mut b = Buf::<1024>::new();
    let _ = b.write_fmt(args);
    write_all(1, b.as_bytes());
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::_print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => { $crate::_print(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Printable view of raw bytes.
pub struct Bytes<'a>(pub &'a [u8]);

impl fmt::Display for Bytes<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for &c in self.0 {
            if (0x20..0x7f).contains(&c) {
                f.write_char(c as char)?;
            } else {
                write!(f, "\\x{:02x}", c)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Checks.

static PASSED: AtomicUsize = AtomicUsize::new(0);
static FAILED: AtomicUsize = AtomicUsize::new(0);

#[doc(hidden)]
pub fn _report(ok: bool, case: &str, file: &str, line: u32, detail: fmt::Arguments) -> bool {
    if ok {
        PASSED.fetch_add(1, Ordering::Relaxed);
        if is_child_mode() {
            return true;
        }
        println!("PASS {} {}", test_name(), case);
    } else {
        FAILED.fetch_add(1, Ordering::Relaxed);
        println!("FAIL {} {} {} [{}:{}]", test_name(), case, detail, file, line);
    }
    ok
}

/// `check!(cond, "case")` or `check!(cond, "case", "detail {}", x)`.
#[macro_export]
macro_rules! check {
    ($cond:expr, $case:expr) => {
        $crate::_report($cond, $case, file!(), line!(), format_args!("condition false: {}", stringify!($cond)))
    };
    ($cond:expr, $case:expr, $($fmt:tt)+) => {
        $crate::_report($cond, $case, file!(), line!(), format_args!($($fmt)+))
    };
}

/// `check_eq!(got, expected, "case")`.
#[macro_export]
macro_rules! check_eq {
    ($got:expr, $exp:expr, $case:expr) => {{
        let (g, e) = ($got, $exp);
        $crate::_report(g == e, $case, file!(), line!(), format_args!("got {:?}, expected {:?}", g, e))
    }};
}

/// Check a syscall result is >= 0; evaluates to the result.
#[macro_export]
macro_rules! check_ok {
    ($r:expr, $case:expr) => {{
        let r: isize = $r as isize;
        $crate::_report(r >= 0, $case, file!(), line!(), format_args!("returned {} ({})", r, $crate::errname(r)));
        r
    }};
}

/// Check a syscall failed with the given errno (positive constant).
#[macro_export]
macro_rules! check_err {
    ($r:expr, $errno:expr, $case:expr) => {{
        let r: isize = $r as isize;
        let e: isize = $errno as isize;
        $crate::_report(r == -e, $case, file!(), line!(),
            format_args!("returned {} ({}), expected {}", r, $crate::errname(r), $crate::errname(-e)))
    }};
}

pub fn skip(case: &str, reason: &str) {
    println!("SKIP {} {} {}", test_name(), case, reason);
}

/// Declare the test's name and entry function.
#[macro_export]
macro_rules! test_main {
    ($name:expr, $main:path) => {
        #[unsafe(no_mangle)]
        fn __rt_test_name() -> &'static str {
            $name
        }
        #[unsafe(no_mangle)]
        fn __rt_test_main() {
            $main()
        }
    };
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let loc = info.location();
    println!(
        "FAIL {} panic {} [{}:{}]",
        test_name(),
        info.message(),
        loc.map_or("?", |l| l.file()),
        loc.map_or(0, |l| l.line())
    );
    if is_main_process() && !is_child_mode() {
        FAILED.fetch_add(1, Ordering::Relaxed);
        let (p, f) = (PASSED.load(Ordering::Relaxed), FAILED.load(Ordering::Relaxed));
        println!("END {} pass={} fail={}", test_name(), p, f);
    }
    exit(101)
}

// ---------------------------------------------------------------------------
// Helpers shared by many tests.

/// Fork; the child runs `f` and exits with its return value. Returns the
/// raw wait status of the child.
pub fn run_child(f: impl FnOnce() -> i32) -> i32 {
    let pid = fork();
    if pid == 0 {
        exit(f());
    }
    assert!(pid > 0, "fork failed: {}", errname(pid));
    let mut status = 0;
    let r = wait4(pid as i32, &mut status, 0);
    assert!(r == pid, "wait4 returned {}", errname(r));
    status
}

pub fn wifexited(s: i32) -> bool {
    s & 0x7f == 0
}
pub fn wexitstatus(s: i32) -> i32 {
    (s >> 8) & 0xff
}
pub fn wifsignaled(s: i32) -> bool {
    ((s & 0x7f) + 1) as i8 >> 1 > 0
}
pub fn wtermsig(s: i32) -> i32 {
    s & 0x7f
}

/// Exit status of a child that exited normally, or 1000 + signal number.
pub fn status_code(s: i32) -> i32 {
    if wifexited(s) {
        wexitstatus(s)
    } else if wifsignaled(s) {
        1000 + wtermsig(s)
    } else {
        -1
    }
}

/// Per-test scratch directory `/tmp/ktest-<name>-<pid>` (created).
pub fn scratch_dir() -> Path {
    let p = cpath!("/tmp/ktest-{}-{}", test_name(), getpid());
    let r = mkdirat(AT_FDCWD, p.as_ptr(), 0o755);
    assert!(r == 0, "mkdir {} failed: {}", Bytes(p.as_bytes()), errname(r));
    p
}

/// Monotonic time in nanoseconds.
pub fn now_ns() -> i64 {
    let mut ts = Timespec::default();
    clock_gettime(CLOCK_MONOTONIC, &mut ts);
    ts.sec * 1_000_000_000 + ts.nsec
}

pub fn sleep_ms(ms: i64) {
    let ts = Timespec { sec: ms / 1000, nsec: (ms % 1000) * 1_000_000 };
    nanosleep(&ts, core::ptr::null_mut());
}

/// Anonymous private RW mapping; panics on failure.
pub fn alloc_pages(len: usize) -> *mut u8 {
    let r = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert!(r >= 0, "mmap failed: {}", errname(r));
    r as *mut u8
}

/// Symbolic name for a negative errno return ("OK" for >= 0).
pub fn errname(r: isize) -> &'static str {
    if r >= 0 {
        return "OK";
    }
    match (-r) as i32 {
        EPERM => "EPERM",
        ENOENT => "ENOENT",
        ESRCH => "ESRCH",
        EINTR => "EINTR",
        EIO => "EIO",
        ENXIO => "ENXIO",
        ENOTBLK => "ENOTBLK",
        ENODEV => "ENODEV",
        EROFS => "EROFS",
        ENODATA => "ENODATA",
        EBADF => "EBADF",
        ECHILD => "ECHILD",
        EAGAIN => "EAGAIN",
        ENOMEM => "ENOMEM",
        EACCES => "EACCES",
        EFAULT => "EFAULT",
        EBUSY => "EBUSY",
        EEXIST => "EEXIST",
        ENOTDIR => "ENOTDIR",
        EISDIR => "EISDIR",
        EINVAL => "EINVAL",
        EMFILE => "EMFILE",
        ENOSPC => "ENOSPC",
        ESPIPE => "ESPIPE",
        EPIPE => "EPIPE",
        ERANGE => "ERANGE",
        ENOSYS => "ENOSYS",
        ENOTEMPTY => "ENOTEMPTY",
        ETIMEDOUT => "ETIMEDOUT",
        ECONNREFUSED => "ECONNREFUSED",
        EADDRINUSE => "EADDRINUSE",
        ENOTCONN => "ENOTCONN",
        EMSGSIZE => "EMSGSIZE",
        ENOTSOCK => "ENOTSOCK",
        EOPNOTSUPP => "EOPNOTSUPP",
        _ => "E?",
    }
}

/// Parse a decimal integer (optional leading '-').
pub fn parse_int(b: &[u8]) -> Option<i64> {
    let (neg, digits) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        _ => (false, b),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for &c in digits {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as i64)?;
    }
    Some(if neg { -v } else { v })
}

/// Spawn a thread sharing the address space (bionic pthread_create flags,
/// minus CLONE_SETTLS unless `tls` is non-zero). Returns the tid; the tid
/// word is cleared and futex-woken when the thread exits (see `join`).
pub struct Thread {
    pub tid: i32,
    ctid: *mut i32,
}

pub fn spawn(f: extern "C" fn(usize) -> i32, arg: usize, tls: usize) -> Thread {
    const STACK: usize = 256 * 1024;
    let stack = alloc_pages(STACK);
    // The tid word lives at the bottom of the thread's own stack mapping.
    let ctid = stack as *mut i32;
    let mut flags = CLONE_THREAD_FLAGS;
    if tls == 0 {
        flags &= !CLONE_SETTLS;
    }
    let r = unsafe { clone_raw(flags, stack.add(STACK), ctid, ctid, tls, f, arg) };
    assert!(r > 0, "clone(thread) failed: {}", errname(r));
    Thread { tid: r as i32, ctid }
}

impl Thread {
    /// Wait until the kernel clears the tid word (CLONE_CHILD_CLEARTID).
    pub fn join(&self) {
        loop {
            let v = unsafe { core::ptr::read_volatile(self.ctid) };
            if v == 0 {
                return;
            }
            futex(self.ctid, FUTEX_WAIT, v, core::ptr::null());
        }
    }
}
