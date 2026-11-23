//! Route the MAX77705 USB data switch to the SoC, as Samsung's MUIC driver does at boot.
//!
//! Without it the USB D+/D- lines stay on the MUIC and the host never sees the gadget.
//! The MUIC answers at 0x25 on the same I2C bus as the MAX77705 PMIC (0x66), which is
//! found through sysfs so the bus number does not matter. Reimplementation of
//! gts7xl-mainline's `tools/usbroute.c`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::process::ExitCode;

const PMIC_COMPATIBLE: &str = "maxim,max77705";
const MUIC_ADDR: u64 = 0x25;
/// From `<linux/i2c-dev.h>`: use this slave address even if a driver owns it.
const I2C_SLAVE_FORCE: i32 = 0x0706;
/// Control register 1: switch D+/D- to USB; then control register 2 (0x41) cleared.
const ROUTE: [&[u8]; 2] = [&[0x21, 0x06, 0x09], &[0x41, 0x00]];

extern "C" {
    fn ioctl(fd: i32, request: i32, ...) -> i32;
}

/// Returns the number of the I2C adapter that carries the MAX77705.
fn find_bus() -> io::Result<u32> {
    for entry in fs::read_dir("/sys/bus/i2c/devices")? {
        let path = entry?.path();
        let compatible = fs::read(path.join("of_node/compatible")).unwrap_or_default();
        let matches = compatible
            .split(|&b| b == 0)
            .any(|c| c == PMIC_COMPATIBLE.as_bytes());
        if !matches {
            continue;
        }
        // Client directories are named "<bus>-<addr>", e.g. "0-0066".
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if let Some(bus) = name.split_once('-').and_then(|(b, _)| b.parse().ok()) {
            return Ok(bus);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "no maxim,max77705 on any I2C bus"))
}

fn route() -> io::Result<u32> {
    let bus = find_bus()?;
    let dev: File = OpenOptions::new().read(true).write(true).open(format!("/dev/i2c-{bus}"))?;
    // SAFETY: plain integer ioctl on an open i2c-dev file descriptor.
    if unsafe { ioctl(dev.as_raw_fd(), I2C_SLAVE_FORCE, MUIC_ADDR) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut dev = dev;
    for msg in ROUTE {
        dev.write_all(msg)?;
    }
    Ok(bus)
}

fn main() -> ExitCode {
    match route() {
        Ok(bus) => {
            println!("usbroute ok (i2c-{bus})");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("usbroute: {e}");
            ExitCode::FAILURE
        }
    }
}
