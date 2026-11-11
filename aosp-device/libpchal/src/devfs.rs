//! Opening device nodes under `/dev`.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::{Error, Result};

/// How a device node is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Access {
    /// `O_RDONLY`.
    Read,
    /// `O_RDWR`.
    ReadWrite,
}

/// Opens a device node such as `/dev/video0` with `O_CLOEXEC` and `O_NOCTTY`, so a tty
/// never becomes the HAL's controlling terminal.
///
/// Further flags (`O_NONBLOCK`, ...) can be added with `fcntl` on the returned file.
///
/// # Errors
///
/// Returns [`Error::Io`] if the node does not exist or the process lacks permission
/// (Unix mode or SELinux).
pub fn open(path: impl AsRef<Path>, access: Access) -> Result<File> {
    let path = path.as_ref();
    OpenOptions::new()
        .read(true)
        .write(access == Access::ReadWrite)
        // std already sets O_CLOEXEC.
        .custom_flags(libc::O_NOCTTY)
        .open(path)
        .map_err(|err| Error::io(path, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_node_reports_path() {
        let err = open("/dev/does-not-exist", Access::Read).unwrap_err();
        assert!(err.to_string().starts_with("/dev/does-not-exist: "), "got {err}");
    }
}
