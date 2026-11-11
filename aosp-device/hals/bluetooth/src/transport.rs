//! Links to the Bluetooth controller: the kernel's HCI user channel (a USB or UART
//! controller the kernel already set up, such as Intel AX2xx on btusb) or a raw H4 UART
//! (Cuttlefish's virtual controller on /dev/hvc5).

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use log::{info, warn};
use pchal::devfs::{self, Access};

use crate::mgmt;
use crate::packet::{H4Decoder, Packet, PacketType};

/// How often to retry a user channel bind while the kernel is still setting up the
/// controller (firmware download), which the kernel answers with `EBUSY`.
const BIND_RETRY: Duration = Duration::from_millis(100);
/// How long one `Read Controller Index List` round waits for an answer.
const MGMT_ROUND: Duration = Duration::from_millis(500);

/// Where the controller is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// HCI user channel on controller `hciN` (or the first one above it).
    UserChannel { controller: u16 },
    /// H4 over a serial device.
    Uart { path: PathBuf },
}

impl Transport {
    /// Opens the link, waiting up to `timeout` for the controller to show up.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::NotFound`] when no controller appeared in time, or the
    /// OS error from opening the socket or device.
    pub fn open(&self, timeout: Duration) -> io::Result<(Sender, Receiver)> {
        let (file, decoder) = match self {
            Self::UserChannel { controller } => (open_user_channel(*controller, timeout)?, None),
            Self::Uart { path } => (open_uart(path)?, Some(H4Decoder::new())),
        };
        let sender = Sender(file.try_clone()?);
        Ok((sender, Receiver { file, decoder }))
    }
}

/// The sending half of a link. Each packet goes out in one `write`, so concurrent
/// senders never interleave on a datagram socket; callers serialize sends on a UART.
#[derive(Debug)]
pub struct Sender(File);

impl Sender {
    /// Sends `data` as one H4 frame of type `kind`.
    ///
    /// # Errors
    ///
    /// Returns the OS error of the write (`ENODEV` once a USB controller is unplugged).
    pub fn send(&self, kind: PacketType, data: &[u8]) -> io::Result<()> {
        (&self.0).write_all(&Packet::frame(kind, data))
    }
}

/// The receiving half of a link, owned by the reader thread.
#[derive(Debug)]
pub struct Receiver {
    file: File,
    /// `Some` on a byte stream (UART); a user channel delivers one frame per read.
    decoder: Option<H4Decoder>,
}

impl Receiver {
    /// Returns the next packet, or `None` after `timeout` without one. Malformed frames
    /// are logged and skipped.
    ///
    /// # Errors
    ///
    /// Returns the OS error of the read, or [`io::ErrorKind::UnexpectedEof`] once the
    /// controller or the other end of the UART is gone.
    pub fn recv(&mut self, timeout: Duration) -> io::Result<Option<Packet>> {
        if let Some(packet) = self.decode() {
            return Ok(Some(packet));
        }
        if !pchal::poll::readable(self.file.as_fd(), timeout)? {
            return Ok(None);
        }
        let mut buf = [0; 4096];
        let len = match self.file.read(&mut buf) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(len) => len,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => return Ok(None),
            Err(err) => return Err(err),
        };
        match &mut self.decoder {
            Some(decoder) => {
                decoder.push(&buf[..len]);
                Ok(self.decode())
            }
            None => Ok(Packet::from_frame(&buf[..len])
                .inspect_err(|err| warn!("dropping a {len}-byte frame: {err}"))
                .ok()),
        }
    }

    fn decode(&mut self) -> Option<Packet> {
        let decoder = self.decoder.as_mut()?;
        decoder.next_packet().inspect_err(|err| warn!("UART out of sync: {err}")).ok().flatten()
    }
}

/// `BTPROTO_HCI` from <linux/net/bluetooth/bluetooth.h>.
const BTPROTO_HCI: libc::c_int = 1;
/// `HCI_DEV_NONE` from <linux/net/bluetooth/hci.h>.
const HCI_DEV_NONE: u16 = 0xffff;

/// HCI socket channels from <linux/net/bluetooth/hci_sock.h>.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    /// Exclusive raw access to one controller; the kernel's own stack lets go of it.
    User = 1,
    /// The management interface.
    Control = 3,
}

/// `struct sockaddr_hci`.
#[repr(C)]
struct SockaddrHci {
    family: libc::sa_family_t,
    dev: u16,
    channel: u16,
}

fn hci_socket(dev: u16, channel: Channel) -> io::Result<File> {
    // SAFETY: plain system call without pointers.
    let fd = unsafe {
        libc::socket(libc::AF_BLUETOOTH, libc::SOCK_RAW | libc::SOCK_CLOEXEC, BTPROTO_HCI)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by socket(2) and is owned by nothing else.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let addr = SockaddrHci {
        family: libc::AF_BLUETOOTH as libc::sa_family_t,
        dev,
        channel: channel as u16,
    };
    let addr_len = std::mem::size_of::<SockaddrHci>() as libc::socklen_t;
    // SAFETY: `addr` is a valid `sockaddr_hci` of `addr_len` bytes that outlives the call.
    let bound = unsafe {
        libc::bind(fd.as_raw_fd(), std::ptr::from_ref(&addr).cast::<libc::sockaddr>(), addr_len)
    };
    if bound < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(File::from(fd))
}

/// Waits for controller `wanted` (or the next one above it) through the management
/// channel and returns its index.
fn wait_for_controller(wanted: u16, timeout: Duration) -> io::Result<u16> {
    let mut control = hci_socket(HCI_DEV_NONE, Channel::Control)?;
    let deadline = Instant::now() + timeout;
    let mut buf = [0; 1024];
    loop {
        control.write_all(&mgmt::READ_INDEX_LIST)?;
        let round_end = deadline.min(Instant::now() + MGMT_ROUND);
        while let Some(left) = round_end.checked_duration_since(Instant::now()) {
            if !pchal::poll::readable(control.as_fd(), left)? {
                break;
            }
            let len = control.read(&mut buf)?;
            if let Some(index) = mgmt::controller_from_event(&buf[..len], wanted) {
                return Ok(index);
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no Bluetooth controller"));
        }
    }
}

fn open_user_channel(wanted: u16, timeout: Duration) -> io::Result<File> {
    let deadline = Instant::now() + timeout;
    let index = wait_for_controller(wanted, timeout)?;
    loop {
        match hci_socket(index, Channel::User) {
            Ok(socket) => {
                info!("hci{index}: user channel open");
                return Ok(socket);
            }
            Err(err)
                if matches!(err.raw_os_error(), Some(libc::EBUSY | libc::EALREADY))
                    && Instant::now() < deadline =>
            {
                thread::sleep(BIND_RETRY);
            }
            Err(err) => return Err(err),
        }
    }
}

fn open_uart(path: &Path) -> io::Result<File> {
    // Keeps the path in the message.
    let file = devfs::open(path, Access::ReadWrite).map_err(io::Error::other)?;
    make_raw(&file)?;
    // Drop whatever a previous session left in the device.
    let mut buf = [0; 4096];
    while pchal::poll::readable(file.as_fd(), Duration::ZERO)? {
        if (&file).read(&mut buf)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
    }
    Ok(file)
}

/// Puts a tty into raw mode, so H4 bytes pass unchanged.
fn make_raw(file: &File) -> io::Result<()> {
    // SAFETY: an all-zero termios is a valid value, and tcgetattr overwrites it.
    let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: the fd is open and `attrs` is a valid, exclusive termios.
    if unsafe { libc::tcgetattr(file.as_raw_fd(), &mut attrs) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `attrs` is a valid termios.
    unsafe { libc::cfmakeraw(&mut attrs) };
    // SAFETY: the fd is open and `attrs` is a valid termios.
    if unsafe { libc::tcsetattr(file.as_raw_fd(), libc::TCSANOW, &attrs) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
