//! Kernel uevents (device hotplug and state changes) from a netlink socket.
//!
//! The kernel multicasts every uevent to `NETLINK_KOBJECT_UEVENT` group 1; ueventd reads
//! the same stream, so a HAL can listen next to it, for example for `power_supply`
//! changes. SELinux: the domain needs `self:netlink_kobject_uevent_socket
//! create_socket_perms_no_ioctl`.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::{Error, Result};

/// Kernel uevents are at most a page; the kernel's own limit is `UEVENT_BUFFER_SIZE` 2048.
const RECV_BUFFER: usize = 8192;
/// Socket receive buffer: room for bursts such as many devices appearing at once.
const SOCKET_BUFFER: libc::c_int = 256 * 1024;

/// One uevent: `ACTION`, `DEVPATH`, `SUBSYSTEM` and the other `KEY=value` pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uevent {
    vars: Vec<(String, String)>,
}

impl Uevent {
    /// Parses a kernel uevent message: `action@devpath\0KEY=value\0...`.
    ///
    /// Returns `None` for messages that are not kernel uevents (udev's `libudev` format
    /// starts with `libudev\0`) or have no `ACTION`.
    pub fn parse(message: &[u8]) -> Option<Self> {
        let mut fields = message.split(|&byte| byte == 0);
        let header = fields.next()?;
        if !header.contains(&b'@') {
            return None;
        }
        let vars: Vec<(String, String)> = fields
            .filter_map(|field| {
                let field = std::str::from_utf8(field).ok()?;
                let (key, value) = field.split_once('=')?;
                Some((key.to_owned(), value.to_owned()))
            })
            .collect();
        let event = Self { vars };
        event.get("ACTION")?;
        Some(event)
    }

    /// The value of `key`, such as `POWER_SUPPLY_ONLINE`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.iter().find(|(k, _)| k == key).map(|(_, value)| value.as_str())
    }

    /// `add`, `remove`, `change`, `bind`, ...
    pub fn action(&self) -> &str {
        self.get("ACTION").expect("parse checked ACTION")
    }

    /// The device path below `/sys`, such as `/devices/.../power_supply/AC`.
    pub fn devpath(&self) -> Option<&str> {
        self.get("DEVPATH")
    }

    /// The kernel subsystem, such as `power_supply` or `thermal`.
    pub fn subsystem(&self) -> Option<&str> {
        self.get("SUBSYSTEM")
    }
}

/// A netlink socket subscribed to kernel uevents.
#[derive(Debug)]
pub struct UeventSocket {
    fd: OwnedFd,
}

impl UeventSocket {
    /// Opens the socket and joins the kernel's uevent multicast group.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Uevent`] if the socket cannot be created or bound (usually
    /// SELinux).
    pub fn open() -> Result<Self> {
        // SAFETY: socket() takes no pointers; the result is checked before use.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(uevent_error());
        }
        // SAFETY: fd is a socket this function just created and nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };

        // A larger buffer only lowers the chance of ENOBUFS; failing to set it is fine.
        // SAFETY: the option value points to a c_int that lives across the call.
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&SOCKET_BUFFER as *const libc::c_int).cast(),
                mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }

        // SAFETY: sockaddr_nl is plain data; all-zero is a valid value.
        let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        addr.nl_groups = 1;
        // SAFETY: addr is a valid sockaddr_nl and the length matches its type.
        let bound = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_nl).cast(),
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bound < 0 {
            return Err(uevent_error());
        }
        Ok(Self { fd })
    }

    /// Blocks until the next kernel uevent arrives.
    ///
    /// Messages not sent by the kernel (netlink port 0) and malformed ones are skipped.
    /// After `ENOBUFS` (events were lost) it keeps reading, so callers should re-read the
    /// state they track rather than rely on seeing every event.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Uevent`] if the socket fails.
    pub fn recv(&self) -> Result<Uevent> {
        let mut buf = vec![0_u8; RECV_BUFFER];
        loop {
            // SAFETY: sockaddr_nl is plain data; all-zero is a valid value.
            let mut from: libc::sockaddr_nl = unsafe { mem::zeroed() };
            let mut from_len = mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            // SAFETY: buf and from are valid for writes of the lengths passed.
            let len = unsafe {
                libc::recvfrom(
                    self.fd.as_raw_fd(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                    (&mut from as *mut libc::sockaddr_nl).cast(),
                    &mut from_len,
                )
            };
            if len < 0 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(libc::ENOBUFS) => {
                        log::warn!("uevents lost (ENOBUFS)");
                        continue;
                    }
                    _ => return Err(Error::Uevent { source: err }),
                }
            }
            if from.nl_pid != 0 {
                continue;
            }
            let len = usize::try_from(len).expect("recvfrom length is non-negative");
            if let Some(event) = Uevent::parse(&buf[..len]) {
                return Ok(event);
            }
        }
    }
}

fn uevent_error() -> Error {
    Error::Uevent { source: io::Error::last_os_error() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_message() {
        let message = b"change@/devices/LNXSYSTM:00/ACPI0003:00/power_supply/AC\0\
            ACTION=change\0DEVPATH=/devices/LNXSYSTM:00/ACPI0003:00/power_supply/AC\0\
            SUBSYSTEM=power_supply\0POWER_SUPPLY_NAME=AC\0POWER_SUPPLY_ONLINE=0\0SEQNUM=4242\0";
        let event = Uevent::parse(message).expect("valid uevent");
        assert_eq!(event.action(), "change");
        assert_eq!(event.subsystem(), Some("power_supply"));
        assert_eq!(event.get("POWER_SUPPLY_ONLINE"), Some("0"));
        assert_eq!(event.get("MISSING"), None);
    }

    #[test]
    fn rejects_libudev_and_incomplete_messages() {
        assert_eq!(Uevent::parse(b"libudev\0\xfe\xed\xca\xfe"), None);
        assert_eq!(Uevent::parse(b"add@/devices/x\0DEVPATH=/devices/x\0"), None);
        assert_eq!(Uevent::parse(b""), None);
    }
}
