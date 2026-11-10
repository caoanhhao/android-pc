//! The PC's CMOS real-time clock (MC146818 compatible, ports 0x70/0x71)
//! as Linux's rtc-cmos: the wall clock at boot and /dev/rtc0.
//!
//! The clock counts in BCD or binary and in 12- or 24-hour mode as the
//! firmware set register B; reads retry until two consecutive reads agree
//! (no update in between). The century comes from the CMOS register the
//! FADT names, else years 70-99 are 19xx. The RTC keeps UTC (as Linux and
//! Android assume); `rtc_offset=SECONDS` on the command line says it keeps
//! UTC + SECONDS instead (a PC whose Windows keeps local time).
//!
//! /dev/rtc0 (252:0, class "rtc", below the "PNP0b00" device at
//! `devices/pnp0/00:01`) has Linux's ioctls: RTC_RD_TIME/RTC_SET_TIME
//! (the hardware clock only, never the system time), the alarm
//! (RTC_ALM_READ/SET: hour:minute:second in CMOS registers 1/3/5, plus
//! day/month when the FADT has those registers; RTC_WKALM_RD/SET with
//! the full date and enable bit; RTC_AIE_ON/OFF) and update interrupts
//! (RTC_UIE_ON/OFF). Nothing here takes interrupts: while an alarm or
//! update interrupt is enabled the BSP's timer tick polls register C, and
//! a read() of the node returns `count << 8 | flags` (RTC_AF, RTC_UF,
//! RTC_IRQF) as on Linux. Like rtc-cmos the alarm is one-shot, and an
//! alarm more than a day ahead only fires once its date has come.
//! sysfs has the usual attributes (name, date, time, since_epoch,
//! hctosys, max_user_freq, wakealarm).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use spin::{Lazy, Mutex, Once};
use x86_64::instructions::port::Port;

use crate::errno::*;
use crate::fs::file::{FileOps, OpenFile};
use crate::fs::poll::{POLLIN, READ_EVENTS, WaitQueue};
use crate::sched::{self, Wake, WaitMode};
use crate::sync::IrqLock;

pub const RTC_MAJOR: u32 = 252;
const SYSDIR: &str = "devices/pnp0/00:01/rtc/rtc0";

const RTC_AIE_ON: u32 = 0x7001;
const RTC_AIE_OFF: u32 = 0x7002;
const RTC_UIE_ON: u32 = 0x7003;
const RTC_UIE_OFF: u32 = 0x7004;
const RTC_PIE_ON: u32 = 0x7005;
const RTC_PIE_OFF: u32 = 0x7006;
const RTC_ALM_SET: u32 = 0x4024_7007;
const RTC_ALM_READ: u32 = 0x8024_7008;
const RTC_RD_TIME: u32 = 0x8024_7009;
const RTC_SET_TIME: u32 = 0x4024_700a;
const RTC_IRQP_READ: u32 = 0x8008_700b;
const RTC_IRQP_SET: u32 = 0x4008_700c;
const RTC_WKALM_SET: u32 = 0x4028_700f;
const RTC_WKALM_RD: u32 = 0x8028_7010;

/// read() flags.
const RTC_IRQF: u64 = 0x80;
const RTC_AF: u64 = 0x20;
const RTC_UF: u64 = 0x10;

const REG_A: u8 = 0x0a;
const REG_B: u8 = 0x0b;
const REG_C: u8 = 0x0c;
/// Register A: update in progress.
const UIP: u8 = 0x80;
/// Register B bits.
const SET: u8 = 0x80;
const AIE: u8 = 0x20;
const UIE: u8 = 0x10;
const DM_BINARY: u8 = 0x04;
const HOUR_24: u8 = 0x02;
/// Register C flags.
const AF: u8 = 0x20;
const UF: u8 = 0x10;

/// Calendar time (UTC unless `rtc_offset`): month 1-12, full year.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Tm {
    pub year: i64,
    pub mon: u32,
    pub mday: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's
/// days_from_civil).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Tm {
    pub fn to_epoch(&self) -> i64 {
        days_from_civil(self.year, self.mon, self.mday) * 86_400 + (self.hour * 3600 + self.min * 60 + self.sec) as i64
    }

    pub fn from_epoch(t: i64) -> Tm {
        let (year, mon, mday) = civil_from_days(t.div_euclid(86_400));
        let s = t.rem_euclid(86_400) as u32;
        Tm { year, mon, mday, hour: s / 3600, min: s / 60 % 60, sec: s % 60 }
    }

    fn valid(&self) -> bool {
        (1..=12).contains(&self.mon)
            && self.mday >= 1
            && self.mday <= days_in_month(self.year, self.mon)
            && self.hour < 24
            && self.min < 60
            && self.sec < 60
    }

    /// Linux's struct rtc_time: nine ints (tm_mon 0-11, tm_year - 1900,
    /// wday, yday, isdst 0).
    fn to_user(self) -> [u8; 36] {
        let days = days_from_civil(self.year, self.mon, self.mday);
        let yday = days - days_from_civil(self.year, 1, 1);
        let wday = (days + 4).rem_euclid(7);
        let v = [
            self.sec as i32,
            self.min as i32,
            self.hour as i32,
            self.mday as i32,
            self.mon as i32 - 1,
            (self.year - 1900) as i32,
            wday as i32,
            yday as i32,
            0,
        ];
        let mut b = [0u8; 36];
        for (i, x) in v.iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
        }
        b
    }

    fn from_user(b: &[u8]) -> Tm {
        let f = |i: usize| i32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        Tm {
            sec: f(0) as u32,
            min: f(1) as u32,
            hour: f(2) as u32,
            mday: f(3) as u32,
            mon: (f(4) + 1) as u32,
            year: f(5) as i64 + 1900,
        }
    }
}

/// What the firmware and the FADT say about the chip.
struct Chip {
    binary: bool,
    h24: bool,
    century: u8,
    day_alarm: u8,
    month_alarm: u8,
}

static CHIP: Once<Chip> = Once::new();
/// Serializes index/data port pairs (the timer tick polls register C).
static CMOS: IrqLock<()> = IrqLock::new(());
/// The RTC keeps UTC + OFFSET seconds.
static OFFSET: AtomicI64 = AtomicI64::new(0);
/// Interrupts enabled through the ioctls.
static AIE_ON: AtomicBool = AtomicBool::new(false);
static UIE_ON: AtomicBool = AtomicBool::new(false);
/// Pending read() data: count << 8 | flags.
static IRQ_DATA: AtomicU64 = AtomicU64::new(0);
static IRQ_FREQ: AtomicU64 = AtomicU64::new(64);
/// The alarm's full time (UTC epoch seconds) as last set.
static ALARM: Mutex<Option<i64>> = Mutex::new(None);
static WQ: Lazy<Arc<WaitQueue>> = Lazy::new(WaitQueue::new);

/// Raw CMOS access; the caller holds `CMOS`.
fn rd(reg: u8) -> u8 {
    unsafe {
        Port::<u8>::new(0x70).write(reg);
        Port::<u8>::new(0x71).read()
    }
}

fn wr(reg: u8, v: u8) {
    unsafe {
        Port::<u8>::new(0x70).write(reg);
        Port::<u8>::new(0x71).write(v);
    }
}

fn from_bcd(v: u8) -> u32 {
    (v >> 4) as u32 * 10 + (v & 0xf) as u32
}

fn to_bcd(v: u32) -> u8 {
    ((v / 10) << 4 | v % 10) as u8
}

impl Chip {
    fn dec(&self, v: u8) -> u32 {
        if self.binary { v as u32 } else { from_bcd(v) }
    }

    fn enc(&self, v: u32) -> u8 {
        if self.binary { v as u8 } else { to_bcd(v) }
    }

    fn dec_hour(&self, v: u8) -> u32 {
        if self.h24 {
            return self.dec(v);
        }
        let h = self.dec(v & 0x7f) % 12;
        if v & 0x80 != 0 { h + 12 } else { h }
    }

    fn enc_hour(&self, h: u32) -> u8 {
        if self.h24 {
            return self.enc(h);
        }
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        self.enc(h12) | if h >= 12 { 0x80 } else { 0 }
    }

    /// The current time in the RTC's own zone; the caller holds `CMOS`.
    fn read_raw(&self) -> Option<Tm> {
        let snap = || {
            for _ in 0..100_000 {
                if rd(REG_A) & UIP == 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            [rd(0), rd(2), rd(4), rd(7), rd(8), rd(9), if self.century != 0 { rd(self.century) } else { 0 }]
        };
        let mut a = snap();
        for _ in 0..10 {
            let b = snap();
            if a == b {
                let yy = self.dec(a[5]) as i64;
                let year = if self.century != 0 && self.dec(a[6]) >= 19 {
                    self.dec(a[6]) as i64 * 100 + yy
                } else if yy < 70 {
                    2000 + yy
                } else {
                    1900 + yy
                };
                let tm = Tm { year, mon: self.dec(a[4]), mday: self.dec(a[3]), hour: self.dec_hour(a[2]), min: self.dec(a[1]), sec: self.dec(a[0]) };
                return tm.valid().then_some(tm);
            }
            a = b;
        }
        None
    }

    fn write_raw(&self, tm: &Tm) {
        let b = rd(REG_B);
        wr(REG_B, b | SET);
        wr(0, self.enc(tm.sec));
        wr(2, self.enc(tm.min));
        wr(4, self.enc_hour(tm.hour));
        let days = days_from_civil(tm.year, tm.mon, tm.mday);
        wr(6, self.enc(((days + 4).rem_euclid(7) + 1) as u32));
        wr(7, self.enc(tm.mday));
        wr(8, self.enc(tm.mon));
        wr(9, self.enc((tm.year % 100) as u32));
        if self.century != 0 {
            wr(self.century, self.enc((tm.year / 100) as u32));
        }
        wr(REG_B, b & !SET);
    }
}

fn chip() -> KResult<&'static Chip> {
    CHIP.get().ok_or(ENODEV)
}

/// Current time of the RTC as UTC epoch seconds.
pub fn now() -> Option<i64> {
    let c = CHIP.get()?;
    let _g = CMOS.lock();
    Some(c.read_raw()?.to_epoch() - OFFSET.load(Ordering::Relaxed))
}

/// Set the RTC to UTC epoch seconds `t`.
pub fn set(t: i64) -> KResult<()> {
    let c = chip()?;
    let tm = Tm::from_epoch(t + OFFSET.load(Ordering::Relaxed));
    if !(1970..2100).contains(&tm.year) || (c.century == 0 && tm.year >= 2070) {
        return Err(EINVAL);
    }
    let _g = CMOS.lock();
    c.write_raw(&tm);
    Ok(())
}

/// Probe the chip (after ACPI): None when the FADT says there is no CMOS
/// RTC or it reads garbage. Returns the wall clock (UTC epoch seconds).
pub fn probe() -> Option<i64> {
    let a = crate::arch::x86_64::acpi::get();
    if a.is_some_and(|a| a.boot_arch & (1 << 5) != 0) {
        log::info!("rtc: the FADT says there is no CMOS RTC");
        return None;
    }
    if let Some(off) = crate::boot::cmdline().split_ascii_whitespace().find_map(|w| w.strip_prefix("rtc_offset=")?.parse::<i64>().ok()) {
        OFFSET.store(off, Ordering::Relaxed);
    }
    let b = {
        let _g = CMOS.lock();
        rd(REG_B)
    };
    if b == 0xff {
        log::info!("rtc: no CMOS RTC (register B reads 0xff)");
        return None;
    }
    let c = Chip {
        binary: b & DM_BINARY != 0,
        h24: b & HOUR_24 != 0,
        century: a.map_or(0, |a| a.century).min(0x7f),
        day_alarm: a.map_or(0, |a| a.day_alarm).min(0x7f),
        month_alarm: a.map_or(0, |a| a.month_alarm).min(0x7f),
    };
    let tm = {
        let _g = CMOS.lock();
        c.read_raw()
    };
    let Some(tm) = tm else {
        log::info!("rtc: CMOS RTC reads an invalid date");
        return None;
    };
    let off = OFFSET.load(Ordering::Relaxed);
    log::info!(
        "rtc: CMOS RTC {:04}-{:02}-{:02} {:02}:{:02}:{:02}{} ({}, {}-hour{}{})",
        tm.year,
        tm.mon,
        tm.mday,
        tm.hour,
        tm.min,
        tm.sec,
        if off == 0 { String::from(" UTC") } else { format!(" (UTC{off:+} s)") },
        if c.binary { "binary" } else { "BCD" },
        if c.h24 { 24 } else { 12 },
        if c.century != 0 { format!(", century reg {:#x}", c.century) } else { String::new() },
        if c.day_alarm != 0 { ", date alarm" } else { "" }
    );
    CHIP.call_once(|| c);
    Some(tm.to_epoch() - off)
}

/// /dev/rtc0 and its sysfs device (after the VFS exists).
pub fn init() {
    use crate::fs::sysfs;
    if CHIP.get().is_none() {
        return;
    }
    sysfs::add_device("devices/pnp0/00:01", "pnp", "bus/pnp", String::from("MODALIAS=acpi:PNP0B00:\n"), false);
    sysfs::set("devices/pnp0/00:01", "id", "PNP0b00\n");
    sysfs::add_char_device(SYSDIR, "rtc", "rtc0", "rtc0", RTC_MAJOR, 0);
    sysfs::set(SYSDIR, "name", "rtc_cmos 00:01\n");
    sysfs::set(SYSDIR, "hctosys", "1\n");
    sysfs::set(SYSDIR, "max_user_freq", "64\n");
    sysfs::set_fn(SYSDIR, "since_epoch", || now().map_or(String::new(), |t| format!("{t}\n")));
    sysfs::set_fn(SYSDIR, "date", || now().map_or(String::new(), |t| {
        let tm = Tm::from_epoch(t);
        format!("{:04}-{:02}-{:02}\n", tm.year, tm.mon, tm.mday)
    }));
    sysfs::set_fn(SYSDIR, "time", || now().map_or(String::new(), |t| {
        let tm = Tm::from_epoch(t);
        format!("{:02}:{:02}:{:02}\n", tm.hour, tm.min, tm.sec)
    }));
    sysfs::set_fn(SYSDIR, "wakealarm", || match *ALARM.lock() {
        Some(t) if AIE_ON.load(Ordering::Relaxed) => format!("{t}\n"),
        _ => String::from("\n"),
    });
    let root = crate::fs::devtmpfs_root();
    let _ = root.add(b"rtc0", crate::fs::vfs::S_IFCHR | 0o600, crate::fs::vfs::encode_dev(RTC_MAJOR, 0), None);
}

/// Program the alarm registers for `t` (UTC epoch seconds) and remember
/// it; enable or disable the alarm interrupt.
fn set_alarm(t: i64, enable: bool) -> KResult<()> {
    let c = chip()?;
    let tm = Tm::from_epoch(t + OFFSET.load(Ordering::Relaxed));
    {
        let _g = CMOS.lock();
        let b = rd(REG_B);
        wr(REG_B, b & !AIE);
        wr(1, c.enc(tm.sec));
        wr(3, c.enc(tm.min));
        wr(5, c.enc_hour(tm.hour));
        if c.day_alarm != 0 {
            wr(c.day_alarm, c.enc(tm.mday));
        }
        if c.month_alarm != 0 {
            wr(c.month_alarm, c.enc(tm.mon));
        }
        rd(REG_C);
    }
    *ALARM.lock() = Some(t);
    aie(enable);
    Ok(())
}

/// The alarm registers' hour:minute:second (RTC zone).
fn read_alarm_hms() -> KResult<(u32, u32, u32)> {
    let c = chip()?;
    let _g = CMOS.lock();
    Ok((c.dec_hour(rd(5)), c.dec(rd(3)), c.dec(rd(1))))
}

/// The alarm as a full UTC time: the remembered one if it still matches
/// the registers, else the next time the registers' hour:minute:second
/// comes (what Linux's rtc core reports for a CMOS alarm).
fn alarm_time() -> KResult<i64> {
    let (h, m, s) = read_alarm_hms()?;
    let off = OFFSET.load(Ordering::Relaxed);
    if let Some(t) = *ALARM.lock() {
        let tm = Tm::from_epoch(t + off);
        if (tm.hour, tm.min, tm.sec) == (h, m, s) {
            return Ok(t);
        }
    }
    if h >= 24 || m >= 60 || s >= 60 {
        return Err(EINVAL);
    }
    let now = now().ok_or(EIO)? + off;
    let day = now.div_euclid(86_400) * 86_400;
    let mut t = day + (h * 3600 + m * 60 + s) as i64;
    if t <= now {
        t += 86_400;
    }
    Ok(t - off)
}

fn aie(on: bool) {
    AIE_ON.store(on, Ordering::Relaxed);
    update_reg_b();
}

fn update_reg_b() {
    let _g = CMOS.lock();
    let b = rd(REG_B) & !(AIE | UIE);
    let mut v = b;
    if AIE_ON.load(Ordering::Relaxed) {
        v |= AIE;
    }
    if UIE_ON.load(Ordering::Relaxed) {
        v |= UIE;
    }
    // Stale flags must not fire a just-enabled interrupt.
    rd(REG_C);
    wr(REG_B, v);
}

/// BSP timer tick: while an interrupt is enabled, poll register C.
pub fn tick() {
    let (aie_on, uie_on) = (AIE_ON.load(Ordering::Relaxed), UIE_ON.load(Ordering::Relaxed));
    if !aie_on && !uie_on {
        return;
    }
    let c = {
        let _g = CMOS.lock();
        rd(REG_C)
    };
    let mut flags = 0;
    if uie_on && c & UF != 0 {
        flags |= RTC_UF;
    }
    if aie_on && c & AF != 0 {
        // The registers only hold hour:minute:second: an alarm days ahead
        // waits for its date.
        let due = ALARM.try_lock().map(|a| *a).unwrap_or(None);
        if due.is_none_or(|t| now().is_none_or(|n| n >= t)) {
            flags |= RTC_AF;
            AIE_ON.store(false, Ordering::Relaxed);
            let _g = CMOS.lock();
            let b = rd(REG_B);
            wr(REG_B, b & !AIE);
        }
    }
    if flags != 0 {
        let old = IRQ_DATA.load(Ordering::Relaxed);
        IRQ_DATA.store((((old >> 8) + 1) << 8) | (old & 0xff) | flags | RTC_IRQF, Ordering::Relaxed);
        WQ.wake(POLLIN);
    }
}

struct RtcFile;

pub fn open() -> KResult<Box<dyn FileOps>> {
    chip()?;
    Ok(Box::new(RtcFile))
}

fn put(arg: u64, b: &[u8]) -> KResult<u64> {
    crate::process::current().mm().copy_out(arg, b)?;
    Ok(0)
}

fn get<const N: usize>(arg: u64) -> KResult<[u8; N]> {
    let mut b = [0u8; N];
    crate::process::current().mm().copy_in(arg, &mut b)?;
    Ok(b)
}

impl FileOps for RtcFile {
    fn read(&self, f: &OpenFile, _: &mut u64, buf: &mut [u8]) -> KResult<usize> {
        if buf.len() < 4 {
            return Err(EINVAL);
        }
        let mut data = 0;
        let w = sched::wait_until(WQ.key(), None, WaitMode::Interruptible, || {
            data = IRQ_DATA.swap(0, Ordering::Relaxed);
            data != 0 || f.nonblock()
        });
        if data != 0 {
            let n = if buf.len() >= 8 { 8 } else { 4 };
            buf[..n].copy_from_slice(&data.to_le_bytes()[..n]);
            return Ok(n);
        }
        match w {
            Wake::Interrupted => Err(ERESTARTSYS),
            _ => Err(EAGAIN),
        }
    }

    fn ioctl(&self, _: &OpenFile, cmd: u32, arg: u64) -> KResult<u64> {
        match cmd {
            RTC_RD_TIME => put(arg, &Tm::from_epoch(now().ok_or(EIO)?).to_user()),
            RTC_SET_TIME => {
                let tm = Tm::from_user(&get::<36>(arg)?);
                if !tm.valid() {
                    return Err(EINVAL);
                }
                log::info!("rtc: set to {:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", tm.year, tm.mon, tm.mday, tm.hour, tm.min, tm.sec);
                set(tm.to_epoch())?;
                Ok(0)
            }
            RTC_ALM_READ => put(arg, &Tm::from_epoch(alarm_time()?).to_user()),
            RTC_ALM_SET => {
                // Only hour:minute:second count: the next such time.
                let tm = Tm::from_user(&get::<36>(arg)?);
                if tm.hour >= 24 || tm.min >= 60 || tm.sec >= 60 {
                    return Err(EINVAL);
                }
                let now = now().ok_or(EIO)?;
                let day = now.div_euclid(86_400) * 86_400;
                let mut t = day + (tm.hour * 3600 + tm.min * 60 + tm.sec) as i64;
                if t <= now {
                    t += 86_400;
                }
                set_alarm(t, AIE_ON.load(Ordering::Relaxed))?;
                Ok(0)
            }
            RTC_WKALM_SET => {
                // struct rtc_wkalrm: enabled, pending, rtc_time (at 4).
                let b = get::<40>(arg)?;
                let tm = Tm::from_user(&b[4..]);
                if !tm.valid() {
                    return Err(EINVAL);
                }
                set_alarm(tm.to_epoch(), b[0] != 0)?;
                Ok(0)
            }
            RTC_WKALM_RD => {
                let mut b = [0u8; 40];
                b[0] = AIE_ON.load(Ordering::Relaxed) as u8;
                b[1] = (IRQ_DATA.load(Ordering::Relaxed) & RTC_AF != 0) as u8;
                b[4..].copy_from_slice(&Tm::from_epoch(alarm_time()?).to_user());
                put(arg, &b)
            }
            RTC_AIE_ON | RTC_AIE_OFF => {
                chip()?;
                aie(cmd == RTC_AIE_ON);
                Ok(0)
            }
            RTC_UIE_ON | RTC_UIE_OFF => {
                chip()?;
                UIE_ON.store(cmd == RTC_UIE_ON, Ordering::Relaxed);
                update_reg_b();
                Ok(0)
            }
            RTC_PIE_ON | RTC_PIE_OFF => Err(EINVAL),
            RTC_IRQP_READ => put(arg, &IRQ_FREQ.load(Ordering::Relaxed).to_le_bytes()),
            RTC_IRQP_SET => {
                if !(2..=8192).contains(&arg) || !arg.is_power_of_two() {
                    return Err(EINVAL);
                }
                IRQ_FREQ.store(arg, Ordering::Relaxed);
                Ok(0)
            }
            _ => Err(ENOTTY),
        }
    }

    fn poll(&self, _: &OpenFile) -> u32 {
        if IRQ_DATA.load(Ordering::Relaxed) != 0 { READ_EVENTS } else { 0 }
    }

    fn wait_queue(&self, _: &OpenFile) -> Option<Arc<WaitQueue>> {
        Some(WQ.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[test_case]
fn rtc_calendar() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    assert_eq!(civil_from_days(11_017), (2000, 3, 1));
    let tm = Tm { year: 2026, mon: 11, mday: 10, hour: 13, min: 5, sec: 59 };
    assert_eq!(Tm::from_epoch(tm.to_epoch()), tm);
    assert_eq!(tm.to_epoch(), 1_794_315_959);
    // 2026-11-10 is a Tuesday (wday 2), day 313 of the year.
    let u = tm.to_user();
    assert_eq!(i32::from_le_bytes(u[24..28].try_into().unwrap()), 2);
    assert_eq!(i32::from_le_bytes(u[28..32].try_into().unwrap()), 313);
    assert_eq!(Tm::from_user(&u), tm);
    assert_eq!(from_bcd(0x59), 59);
    assert_eq!(to_bcd(47), 0x47);
}
