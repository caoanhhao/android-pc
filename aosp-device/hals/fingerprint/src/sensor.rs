//! The Goodix sensor's normal-world side through `/dev/goodix_fp` (kernel patch 0304): power,
//! reset, the SPI clock vote TrustZone needs while it drives the bus, and the finger interrupt.
//! The sensor itself is read only by the secure app.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::thread;
use std::time::Duration;

use crate::qsee::ioctl;

const GOODIX_DEV: &str = "/dev/goodix_fp";

const GF_IOC_RESET: u32 = 0x6702;
const GF_IOC_ENABLE_IRQ: u32 = 0x6703;
const GF_IOC_DISABLE_IRQ: u32 = 0x6704;
const GF_IOC_ENABLE_SPI_CLK: u32 = 0x4004_6705;
const GF_IOC_DISABLE_SPI_CLK: u32 = 0x6706;
const GF_IOC_ENABLE_POWER: u32 = 0x6707;
const GF_IOC_DISABLE_POWER: u32 = 0x6708;

/// Why [`Sensor::wait_finger`] returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Wake {
    /// The sensor raised its interrupt.
    Interrupt,
    /// [`Canceller::cancel`] was called.
    Cancelled,
    /// The timeout passed.
    Timeout,
}

/// `/dev/goodix_fp`, plus an eventfd that ends a finger wait.
pub struct Sensor {
    file: File,
    /// Written by [`Canceller::cancel`] to end a wait.
    cancel: OwnedFd,
}

/// Ends a running [`Sensor::wait_finger`] from another thread.
pub struct Canceller(OwnedFd);

impl Canceller {
    /// Ends the current or next finger wait.
    pub fn cancel(&self) {
        let one = 1u64.to_ne_bytes();
        // SAFETY: writes 8 bytes from a live buffer to our eventfd.
        unsafe { libc::write(self.0.as_raw_fd(), one.as_ptr().cast(), one.len()) };
    }
}

impl Sensor {
    /// Opens the device.
    pub fn open() -> io::Result<Sensor> {
        let file = OpenOptions::new().read(true).write(true).open(GOODIX_DEV)?;
        // SAFETY: eventfd returns a new descriptor we own, or -1.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nothing else owns.
        let cancel = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Sensor { file, cancel })
    }

    /// A handle that ends finger waits from another thread.
    pub fn canceller(&self) -> io::Result<Canceller> {
        Ok(Canceller(self.cancel.try_clone()?))
    }

    fn io(&self, request: u32) -> io::Result<()> {
        ioctl::<u8>(&self.file, request, std::ptr::null_mut())
    }

    /// Powers the sensor up from off (so it starts from a clean reset) and votes the bus clock.
    /// The interrupt stays off while the power switches: a power cycle raises hundreds of
    /// spurious interrupts.
    pub fn power_on(&self) -> io::Result<()> {
        self.io(GF_IOC_DISABLE_IRQ)?;
        self.io(GF_IOC_DISABLE_POWER)?;
        thread::sleep(Duration::from_millis(50));
        self.io(GF_IOC_ENABLE_POWER)?;
        self.clock_on()?;
        self.drain();
        Ok(())
    }

    /// Interrupt off, clock vote dropped, power off.
    pub fn power_off(&self) -> io::Result<()> {
        self.io(GF_IOC_DISABLE_IRQ)?;
        self.io(GF_IOC_DISABLE_SPI_CLK)?;
        self.io(GF_IOC_DISABLE_POWER)
    }

    /// Pulses the reset line.
    pub fn reset(&self) -> io::Result<()> {
        self.io(GF_IOC_RESET)
    }

    fn clock_on(&self) -> io::Result<()> {
        let mut hz = 0u32; // keep the rate the driver set
        ioctl(&self.file, GF_IOC_ENABLE_SPI_CLK, &mut hz)
    }

    /// Forgets interrupts counted so far.
    fn drain(&self) {
        let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd, no wait.
        while unsafe { libc::poll(&mut pfd, 1, 0) } > 0 {
            let mut count = [0u8; 4];
            if (&self.file).read(&mut count).is_err() {
                break;
            }
        }
        self.clear_cancel();
    }

    fn clear_cancel(&self) {
        let mut v = [0u8; 8];
        // SAFETY: reads at most 8 bytes into a live buffer from our nonblocking eventfd.
        unsafe { libc::read(self.cancel.as_raw_fd(), v.as_mut_ptr().cast(), v.len()) };
    }

    /// Waits for the finger interrupt the secure app asked for, as the stock HAL does: interrupt
    /// on, the clock vote dropped while waiting, voted again before the app runs.
    pub fn wait_finger(&self, timeout: Option<Duration>) -> io::Result<Wake> {
        self.drain_irq_only();
        self.io(GF_IOC_ENABLE_IRQ)?;
        self.io(GF_IOC_DISABLE_SPI_CLK)?;
        let mut fds = [
            libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: self.cancel.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let ms = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
        let wake = loop {
            // SAFETY: two valid pollfds.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                self.clock_on()?;
                return Err(e);
            }
            if n == 0 {
                break Wake::Timeout;
            }
            if fds[1].revents != 0 {
                self.clear_cancel();
                break Wake::Cancelled;
            }
            let mut count = [0u8; 4];
            (&self.file).read_exact(&mut count)?;
            break Wake::Interrupt;
        };
        self.clock_on()?;
        Ok(wake)
    }

    /// Waits until the finger has left: no interrupt for `quiet`. A finger resting on the
    /// sensor keeps raising interrupts, and a capture started then sees a finger that is half
    /// lifted (`Enroll_Do` status 0x27, result 0x88000000, too little finger).
    pub fn wait_lift(&self, quiet: Duration) -> io::Result<Wake> {
        self.drain_irq_only();
        self.io(GF_IOC_ENABLE_IRQ)?;
        self.io(GF_IOC_DISABLE_SPI_CLK)?;
        let mut fds = [
            libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: self.cancel.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let ms = quiet.as_millis().min(i32::MAX as u128) as i32;
        let wake = loop {
            // SAFETY: two valid pollfds.
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                self.clock_on()?;
                return Err(e);
            }
            if n == 0 {
                break Wake::Timeout;
            }
            if fds[1].revents != 0 {
                self.clear_cancel();
                break Wake::Cancelled;
            }
            let mut count = [0u8; 4];
            (&self.file).read_exact(&mut count)?;
        };
        self.io(GF_IOC_DISABLE_IRQ)?;
        self.clock_on()?;
        Ok(wake)
    }

    fn drain_irq_only(&self) {
        let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        // SAFETY: one valid pollfd, no wait.
        while unsafe { libc::poll(&mut pfd, 1, 0) } > 0 {
            let mut count = [0u8; 4];
            if (&self.file).read(&mut count).is_err() {
                break;
            }
        }
    }
}
