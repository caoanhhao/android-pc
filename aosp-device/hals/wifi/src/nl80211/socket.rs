//! A generic netlink socket that sends one request at a time and collects the replies.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::attr::{self, Attrs, Request, GENL_HDRLEN, NLMSG_DONE};

/// Receive buffer size; one message of a split wiphy dump fits in a page, other replies
/// are small.
const RECV_BUFFER: usize = 64 * 1024;

/// One generic netlink message the kernel answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply(Vec<u8>);

impl Reply {
    /// Its attributes.
    pub fn attrs(&self) -> Attrs<'_> {
        Attrs::new(self.0.get(GENL_HDRLEN..).unwrap_or_default())
    }
}

/// A `NETLINK_GENERIC` socket bound to a kernel-assigned port.
#[derive(Debug)]
pub struct GenlSocket {
    fd: OwnedFd,
    seq: u32,
}

impl GenlSocket {
    /// Opens and binds the socket.
    ///
    /// # Errors
    ///
    /// Returns the OS error of `socket` or `bind`.
    pub fn open() -> io::Result<Self> {
        // SAFETY: plain syscall; the result is checked before use.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_GENERIC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a socket this function just opened and nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        // SAFETY: sockaddr_nl is plain data; all zeroes is a valid value (port 0 = let
        // the kernel choose).
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // SAFETY: `addr` is a valid sockaddr_nl and the length matches it.
        let ret = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                std::ptr::from_ref(&addr).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, seq: 0 })
    }

    /// Sends `request` and returns the attributes of every reply message.
    ///
    /// Requests that are not dumps carry `NLM_F_ACK` (set by the caller), so both kinds
    /// end with a known message: `NLMSG_DONE` or the acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns the errno the kernel answered with, or the OS error of `send`/`recv`.
    pub fn transact(&mut self, request: Request) -> io::Result<Vec<Reply>> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let dump = request.is_dump();
        let message = request.finish(seq);
        // SAFETY: the buffer is valid for its length; the socket is connected to nothing,
        // so send() goes to the kernel (port 0), the default netlink destination.
        let sent =
            unsafe { libc::send(self.fd.as_raw_fd(), message.as_ptr().cast(), message.len(), 0) };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut replies = Vec::new();
        let mut buf = vec![0u8; RECV_BUFFER];
        loop {
            // SAFETY: `buf` is writable for its whole length.
            let len =
                unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if len < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            let datagram = &buf[..len as usize];
            for frame in attr::frames(datagram).filter(|frame| frame.seq == seq) {
                match (frame.error(), frame.kind) {
                    (Some(0), _) if !dump => return Ok(replies),
                    (Some(0), _) => {}
                    (Some(errno), _) => return Err(io::Error::from_raw_os_error(-errno)),
                    (None, NLMSG_DONE) => return Ok(replies),
                    (None, _) => replies.push(Reply(frame.payload.to_vec())),
                }
            }
        }
    }
}
