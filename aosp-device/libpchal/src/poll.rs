//! Waiting for a file descriptor with a timeout, so worker threads can check a stop flag.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Duration;

/// Waits until `fd` is readable or `timeout` passes.
///
/// Returns `Ok(true)` when a read will not block, including when the other end hung up
/// or the descriptor has an error pending (the read then reports it), and `Ok(false)` on
/// timeout. `EINTR` counts as a timeout, so callers simply loop.
///
/// # Errors
///
/// Returns the OS error if `poll(2)` itself fails (`EINVAL`, `ENOMEM`).
pub fn readable(fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool> {
    let mut pollfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `pollfd` is a valid, exclusively borrowed array of one element that lives
    // for the whole call, and `fd` is open for the lifetime of the borrow.
    let ready = unsafe { libc::poll(&mut pollfd, 1, millis) };
    match ready {
        -1 => {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(err)
            }
        }
        0 => Ok(false),
        _ => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn times_out_then_sees_data() {
        let (mut tx, rx) = UnixStream::pair().unwrap();
        assert!(!readable(rx.as_fd(), Duration::from_millis(10)).unwrap());
        tx.write_all(b"x").unwrap();
        assert!(readable(rx.as_fd(), Duration::from_millis(10)).unwrap());
    }

    #[test]
    fn hang_up_is_readable() {
        let (tx, rx) = UnixStream::pair().unwrap();
        drop(tx);
        assert!(readable(rx.as_fd(), Duration::ZERO).unwrap());
    }
}
