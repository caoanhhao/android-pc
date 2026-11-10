//! E9: PC platform devices.
//!
//! - /dev/rtc0 (the CMOS RTC): RTC_RD_TIME agrees with CLOCK_REALTIME
//!   (the wall clock comes from it at boot), sysfs (name, since_epoch,
//!   hctosys, the class link), RTC_SET_TIME moves the hardware clock (an
//!   hour ahead, read back, restored), RTC_ALM_SET/READ round-trips,
//!   RTC_WKALM_SET two seconds ahead fires: read() returns RTC_AF |
//!   RTC_IRQF and the alarm is then off (one-shot), RTC_UIE_ON gives an
//!   update interrupt within ~1 s.
//! - The AC power supply: type Mains, online 1, its uevent variables.
//! - cpufreq: one policy per online CPU with cur/min/max frequencies, the
//!   cpuN/cpufreq link, topology core_id.
//! - Thermal: thermal_zone0 (x86_pkg_temp) and hwmon coretemp with sane
//!   temperatures when the CPU has digital sensors; skipped in QEMU.
//! - The ACPI power button input device ("Power Button", BUS_HOST,
//!   KEY_POWER). With `e9.pwrbtn` on the command line (run-qemu.sh
//!   --hw-test) the test prints "E9-PWRBTN-READY" and the host presses the
//!   button with the monitor's `system_powerdown`: KEY_POWER down and up.
//!
//! Without the Rust kernel the cases are skipped.
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("e9_platform", main);

const RTC_AIE_OFF: u32 = 0x7002;
const RTC_UIE_ON: u32 = 0x7003;
const RTC_UIE_OFF: u32 = 0x7004;
const RTC_ALM_SET: u32 = 0x4024_7007;
const RTC_ALM_READ: u32 = 0x8024_7008;
const RTC_RD_TIME: u32 = 0x8024_7009;
const RTC_SET_TIME: u32 = 0x4024_700a;
const RTC_WKALM_SET: u32 = 0x4028_700f;
const RTC_WKALM_RD: u32 = 0x8028_7010;
const RTC_AF: u64 = 0x20;
const RTC_UF: u64 = 0x10;
const RTC_IRQF: u64 = 0x80;

const EV_KEY: u16 = 1;
const KEY_POWER: u16 = 116;

/// struct rtc_time as nine ints.
type RtcTime = [i32; 9];

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

fn to_epoch(t: &RtcTime) -> i64 {
    days_from_civil(t[5] as i64 + 1900, t[4] as i64 + 1, t[3] as i64) * 86_400 + (t[2] * 3600 + t[1] * 60 + t[0]) as i64
}

fn from_epoch(s: i64) -> RtcTime {
    let z = s.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    let sec = s.rem_euclid(86_400) as i32;
    [sec % 60, sec / 60 % 60, sec / 3600, d as i32, m as i32 - 1, (y - 1900) as i32, 0, 0, 0]
}

fn realtime() -> i64 {
    let mut ts = Timespec::default();
    clock_gettime(CLOCK_REALTIME, &mut ts);
    ts.sec
}

fn rd_time(fd: i32) -> Option<i64> {
    let mut t: RtcTime = [0; 9];
    (ioctl(fd, RTC_RD_TIME, t.as_mut_ptr() as usize) == 0).then(|| to_epoch(&t))
}

/// A small file's contents without the trailing newline.
fn read_str(path: &Path, buf: &mut [u8; 512]) -> usize {
    let n = read_file(path.as_ptr(), buf);
    if n < 0 { 0 } else { trim(&buf[..n as usize]).len() }
}

fn parse(b: &[u8]) -> Option<i64> {
    let (neg, d) = match b.first() {
        Some(b'-') => (true, &b[1..]),
        _ => (false, b),
    };
    if d.is_empty() || !d.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v = d.iter().fold(0i64, |v, c| v * 10 + (c - b'0') as i64);
    Some(if neg { -v } else { v })
}

fn sys_int(path: &Path) -> Option<i64> {
    let mut b = [0u8; 512];
    let n = read_str(path, &mut b);
    parse(&b[..n])
}

fn rtc() {
    let fd = open(c"/dev/rtc0".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0);
    if !check!(fd >= 0, "rtc_open", "{}", errname(fd)) {
        return;
    }
    let fd = fd as i32;
    let t = rd_time(fd);
    let now = realtime();
    check!(t.is_some_and(|t| (t - now).abs() <= 2), "rtc_rd_time_matches_realtime", "rtc {:?} realtime {}", t, now);

    let base = cpath!("/sys/class/rtc/rtc0");
    let mut b = [0u8; 512];
    let n = read_str(&cpath!("{}/name", Bytes(base.as_bytes())), &mut b);
    check!(b[..n].starts_with(b"rtc_cmos"), "rtc_sysfs_name", "{}", Bytes(&b[..n]));
    let e = sys_int(&cpath!("{}/since_epoch", Bytes(base.as_bytes())));
    check!(e.is_some_and(|e| (e - now).abs() <= 2), "rtc_sysfs_since_epoch", "{:?} vs {}", e, now);
    check_eq!(sys_int(&cpath!("{}/hctosys", Bytes(base.as_bytes()))), Some(1), "rtc_sysfs_hctosys");
    let n = read_str(&cpath!("{}/dev", Bytes(base.as_bytes())), &mut b);
    check!(&b[..n] == b"252:0", "rtc_sysfs_dev", "{}", Bytes(&b[..n]));

    // The hardware clock an hour ahead, then back.
    let ahead = from_epoch(now + 3600);
    let r = ioctl(fd, RTC_SET_TIME, ahead.as_ptr() as usize);
    let back = rd_time(fd);
    check!(r == 0 && back.is_some_and(|t| (t - (now + 3600)).abs() <= 2), "rtc_set_time", "set {} read {:?}", errname(r), back.map(|t| t - now));
    check!((realtime() - now).abs() <= 2, "rtc_set_time_leaves_system_clock");
    let restore = from_epoch(realtime());
    ioctl(fd, RTC_SET_TIME, restore.as_ptr() as usize);
    let t = rd_time(fd);
    check!(t.is_some_and(|t| (t - realtime()).abs() <= 2), "rtc_restored", "{:?}", t.map(|t| t - realtime()));
    let mut bad = from_epoch(now);
    bad[4] = 12;
    check_err!(ioctl(fd, RTC_SET_TIME, bad.as_ptr() as usize), EINVAL, "rtc_set_time_invalid");

    // Alarm registers: hour:minute:second round-trip.
    let mut alm: RtcTime = [56, 34, 12, 0, 0, 0, 0, 0, 0];
    let r = ioctl(fd, RTC_ALM_SET, alm.as_ptr() as usize);
    alm = [0; 9];
    let r2 = ioctl(fd, RTC_ALM_READ, alm.as_mut_ptr() as usize);
    check!(r == 0 && r2 == 0 && alm[..3] == [56, 34, 12], "rtc_alm_set_read", "{} {} {:?}", errname(r), errname(r2), &alm[..6]);

    // A wake alarm two seconds ahead fires once.
    let now = rd_time(fd).unwrap_or(0);
    let mut wk = [0u8; 40];
    wk[0] = 1;
    let at = from_epoch(now + 2);
    for (i, v) in at.iter().enumerate() {
        wk[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    let r = ioctl(fd, RTC_WKALM_SET, wk.as_ptr() as usize);
    let mut rd = [0u8; 40];
    ioctl(fd, RTC_WKALM_RD, rd.as_mut_ptr() as usize);
    let got = i32::from_le_bytes(rd[4..8].try_into().unwrap());
    check!(r == 0 && rd[0] == 1 && got == at[0], "rtc_wkalm_set_rd", "{} enabled {} sec {} want {}", errname(r), rd[0], got, at[0]);
    let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
    let pr = poll(&mut p, 5000);
    let mut data = 0u64;
    let n = read(fd, unsafe { core::slice::from_raw_parts_mut(&mut data as *mut u64 as *mut u8, 8) });
    let fired_at = rd_time(fd).unwrap_or(0);
    check!(
        pr == 1 && n == 8 && data & (RTC_AF | RTC_IRQF) == RTC_AF | RTC_IRQF && data >> 8 >= 1,
        "rtc_alarm_fires",
        "poll {} read {} data {:#x}",
        pr,
        n,
        data
    );
    check!(fired_at >= now + 2 && fired_at <= now + 4, "rtc_alarm_on_time", "{} s after setting", fired_at - now);
    rd = [0; 40];
    ioctl(fd, RTC_WKALM_RD, rd.as_mut_ptr() as usize);
    check!(rd[0] == 0, "rtc_alarm_one_shot", "still enabled");
    ioctl(fd, RTC_AIE_OFF, 0);

    // Update interrupts: one per second.
    let r = ioctl(fd, RTC_UIE_ON, 0);
    let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
    let pr = poll(&mut p, 3000);
    data = 0;
    let n = read(fd, unsafe { core::slice::from_raw_parts_mut(&mut data as *mut u64 as *mut u8, 8) });
    ioctl(fd, RTC_UIE_OFF, 0);
    check!(r == 0 && pr == 1 && n == 8 && data & RTC_UF != 0, "rtc_update_interrupt", "{} poll {} data {:#x}", errname(r), pr, data);
    close(fd);
}

fn power_supply() {
    let base = "/sys/class/power_supply/AC";
    let mut b = [0u8; 512];
    let n = read_str(&cpath!("{}/type", base), &mut b);
    check!(&b[..n] == b"Mains", "ac_type_mains", "{}", Bytes(&b[..n]));
    check_eq!(sys_int(&cpath!("{}/online", base)), Some(1), "ac_online");
    let n = read_file(cpath!("{}/uevent", base).as_ptr(), &mut b);
    let ev = &b[..n.max(0) as usize];
    check!(
        contains(ev, b"POWER_SUPPLY_NAME=AC") && contains(ev, b"POWER_SUPPLY_ONLINE=1") && contains(ev, b"POWER_SUPPLY_TYPE=Mains"),
        "ac_uevent",
        "{}",
        Bytes(ev)
    );
}

fn contains(h: &[u8], n: &[u8]) -> bool {
    h.windows(n.len()).any(|w| w == n)
}

fn online_cpus() -> i64 {
    let mut b = [0u8; 512];
    let n = read_str(&cpath!("/sys/devices/system/cpu/online"), &mut b);
    match b[..n].iter().position(|&c| c == b'-') {
        Some(i) => parse(&b[i + 1..n]).unwrap_or(0) + 1,
        None => 1,
    }
}

fn cpufreq() {
    let cpus = online_cpus();
    let mut ok = 0;
    for i in 0..cpus {
        let p = cpath!("/sys/devices/system/cpu/cpu{}/cpufreq", i);
        let cur = sys_int(&cpath!("{}/scaling_cur_freq", Bytes(p.as_bytes())));
        let min = sys_int(&cpath!("{}/cpuinfo_min_freq", Bytes(p.as_bytes())));
        let max = sys_int(&cpath!("{}/cpuinfo_max_freq", Bytes(p.as_bytes())));
        if let (Some(cur), Some(min), Some(max)) = (cur, min, max)
            && cur > 0
            && min > 0
            && max >= min
        {
            ok += 1;
        } else {
            println!("# cpu{}: cur {:?} min {:?} max {:?}", i, cur, min, max);
        }
    }
    check!(ok == cpus, "cpufreq_policies", "{} of {} CPUs", ok, cpus);
    let mut b = [0u8; 512];
    let n = read_str(&cpath!("/sys/devices/system/cpu/cpufreq/policy0/scaling_driver"), &mut b);
    check!(n > 0, "cpufreq_driver", "none");
    println!("# cpufreq driver {} cpu0 {:?} kHz", Bytes(&b[..n]), sys_int(&cpath!("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")));
    check!(sys_int(&cpath!("/sys/devices/system/cpu/cpu0/topology/core_id")).is_some(), "cpu_topology_core_id");
}

fn thermal() {
    let mut b = [0u8; 512];
    let n = read_str(&cpath!("/sys/class/thermal/thermal_zone0/type"), &mut b);
    if n == 0 {
        skip("thermal", "no digital thermal sensor (QEMU)");
        return;
    }
    check!(&b[..n] == b"x86_pkg_temp", "thermal_zone_type", "{}", Bytes(&b[..n]));
    let t = sys_int(&cpath!("/sys/class/thermal/thermal_zone0/temp"));
    check!(t.is_some_and(|t| (1000..=130_000).contains(&t)), "thermal_zone_temp", "{:?}", t);
    let n = read_str(&cpath!("/sys/class/hwmon/hwmon0/name"), &mut b);
    check!(&b[..n] == b"coretemp", "hwmon_coretemp", "{}", Bytes(&b[..n]));
    let t = sys_int(&cpath!("/sys/class/hwmon/hwmon0/temp2_input"));
    check!(t.is_some_and(|t| (1000..=130_000).contains(&t)), "hwmon_core_temp", "{:?}", t);
}

fn eioc_read(nr: u32, size: usize) -> u32 {
    2 << 30 | (size as u32) << 16 | (b'E' as u32) << 8 | nr
}

/// The event node of the input device named `name`.
fn find_input(name: &[u8]) -> Option<i32> {
    for i in 0..32 {
        let p = cpath!("/dev/input/event{}", i);
        let fd = open(p.as_ptr(), O_RDONLY | O_NONBLOCK | O_CLOEXEC, 0);
        if fd < 0 {
            continue;
        }
        let fd = fd as i32;
        let mut n = [0u8; 64];
        ioctl(fd, eioc_read(0x06, 64), n.as_mut_ptr() as usize);
        if cfield(&n) == name {
            return Some(fd);
        }
        close(fd);
    }
    None
}

fn cmdline_has(word: &[u8]) -> bool {
    let mut b = [0u8; 2048];
    let n = read_file(c"/proc/cmdline".as_ptr() as *const u8, &mut b);
    b[..n.max(0) as usize].split(|c| c.is_ascii_whitespace()).any(|w| w == word)
}

/// Wait for KEY_POWER down then up on `fd`.
pub fn wait_power_key(fd: i32, ms: i64) -> (bool, bool) {
    let end = now_ns() + ms * 1_000_000;
    let (mut down, mut up) = (false, false);
    while !up {
        let left = (end - now_ns()) / 1_000_000;
        if left <= 0 {
            break;
        }
        let mut p = [Pollfd { fd, events: POLLIN, revents: 0 }];
        poll(&mut p, left as i32);
        let mut e = [0u8; 24 * 16];
        let r = read(fd, &mut e);
        for ev in e[..r.max(0) as usize].chunks(24) {
            let ty = u16::from_le_bytes([ev[16], ev[17]]);
            let code = u16::from_le_bytes([ev[18], ev[19]]);
            let value = i32::from_le_bytes(ev[20..24].try_into().unwrap());
            if (ty, code) == (EV_KEY, KEY_POWER) {
                if value == 1 {
                    down = true;
                } else if value == 0 && down {
                    up = true;
                }
            }
        }
    }
    (down, up)
}

fn power_button() {
    let Some(fd) = find_input(b"Power Button") else {
        check!(false, "power_button_device", "no \"Power Button\" input device");
        return;
    };
    let mut id = [0u16; 4];
    ioctl(fd, eioc_read(0x02, 8), id.as_mut_ptr() as usize);
    check!(id[0] == 0x19 && id[2] == 1, "power_button_id", "{:x?}", id);
    let mut keys = [0u8; 96];
    ioctl(fd, eioc_read(0x21, 96), keys.as_mut_ptr() as usize);
    check!(keys[(KEY_POWER / 8) as usize] & (1 << (KEY_POWER % 8)) != 0, "power_button_key_power");
    if !cmdline_has(b"e9.pwrbtn") {
        skip("power_button_press", "no host to press it (run-qemu.sh --hw-test)");
        close(fd);
        return;
    }
    println!("E9-PWRBTN-READY");
    let (down, up) = wait_power_key(fd, 30_000);
    check!(down && up, "power_button_press", "down {} up {}", down, up);
    close(fd);
}

fn main() {
    if !is_rust_kernel() {
        skip("all", "Rust kernel only");
        return;
    }
    rtc();
    power_supply();
    cpufreq();
    thermal();
    power_button();
}
