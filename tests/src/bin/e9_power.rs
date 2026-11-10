//! E9: restart and power off through ACPI, on QEMU (run-qemu.sh
//! --power-test, `e9.reboot` on the command line; otherwise skipped).
//!
//! First boot: the CMOS alarm registers do not hold the marker time
//! (23:59:58). The test prints "E9-POWER-READY", the host presses the
//! power button (`system_powerdown`), the test sees KEY_POWER, stores the
//! marker in the alarm registers (CMOS RAM survives a reset) and calls
//! reboot(RESTART): the kernel resets through the FADT reset register and
//! QEMU (without -no-reboot) boots again.
//! Second boot: the marker is there, so the machine really restarted;
//! the test clears it and passes, ktest-init ends with reboot(POWER_OFF)
//! and ACPI S5 must make QEMU exit with status 0 (tools/power-test.py
//! checks two boots, the log lines and the exit status).
#![no_std]
#![no_main]

use rt::*;

rt::test_main!("e9_power", main);

const RTC_ALM_SET: u32 = 0x4024_7007;
const RTC_ALM_READ: u32 = 0x8024_7008;
const RTC_AIE_OFF: u32 = 0x7002;
const EV_KEY: u16 = 1;
const KEY_POWER: u16 = 116;
const MARKER: [i32; 3] = [58, 59, 23];

fn eioc_read(nr: u32, size: usize) -> u32 {
    2 << 30 | (size as u32) << 16 | (b'E' as u32) << 8 | nr
}

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

fn set_alarm(fd: i32, hms: [i32; 3]) -> isize {
    let t: [i32; 9] = [hms[0], hms[1], hms[2], 0, 0, 0, 0, 0, 0];
    ioctl(fd, RTC_ALM_SET, t.as_ptr() as usize)
}

fn main() {
    if !is_rust_kernel() || !cmdline_has(b"e9.reboot") {
        skip("all", "run-qemu.sh --power-test only");
        return;
    }
    let fd = open(c"/dev/rtc0".as_ptr() as *const u8, O_RDWR | O_CLOEXEC, 0);
    if !check!(fd >= 0, "rtc_open", "{}", errname(fd)) {
        return;
    }
    let fd = fd as i32;
    ioctl(fd, RTC_AIE_OFF, 0);
    let mut t = [0i32; 9];
    let r = ioctl(fd, RTC_ALM_READ, t.as_mut_ptr() as usize);
    check_ok!(r, "rtc_alm_read");
    if t[..3] == MARKER {
        println!("E9-REBOOTED");
        check!(true, "restarted");
        check_ok!(set_alarm(fd, [0, 0, 0]), "marker_cleared");
        close(fd);
        return;
    }
    let Some(btn) = find_input(b"Power Button") else {
        check!(false, "power_button_device", "no \"Power Button\" input device");
        return;
    };
    println!("E9-POWER-READY");
    let end = now_ns() + 30_000_000_000;
    let (mut down, mut up) = (false, false);
    while !up && now_ns() < end {
        let mut p = [Pollfd { fd: btn, events: POLLIN, revents: 0 }];
        poll(&mut p, ((end - now_ns()) / 1_000_000).max(1) as i32);
        let mut e = [0u8; 24 * 16];
        let n = read(btn, &mut e);
        for ev in e[..n.max(0) as usize].chunks(24) {
            let ty = u16::from_le_bytes([ev[16], ev[17]]);
            let code = u16::from_le_bytes([ev[18], ev[19]]);
            let value = i32::from_le_bytes(ev[20..24].try_into().unwrap());
            if (ty, code, value) == (EV_KEY, KEY_POWER, 1) {
                down = true;
            }
            if (ty, code, value) == (EV_KEY, KEY_POWER, 0) && down {
                up = true;
            }
        }
    }
    if !check!(down && up, "power_button_press", "down {} up {}", down, up) {
        return;
    }
    check_ok!(set_alarm(fd, MARKER), "marker_set");
    println!("E9-REBOOTING");
    // LINUX_REBOOT_MAGIC1/2, LINUX_REBOOT_CMD_RESTART.
    let r = unsafe { syscall!(169, 0xfee1deadusize, 672274793usize, 0x01234567usize, 0) };
    check!(false, "reboot_returned", "{}", errname(r));
}
