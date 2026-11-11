//! The error type shared by every `libpchal` API.

use std::io;
use std::path::PathBuf;

use binder::{ExceptionCode, Status, StatusCode};

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a `libpchal` call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Reading, writing or opening a file under sysfs or devfs failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory being accessed.
        path: PathBuf,
        /// The underlying OS error.
        source: io::Error,
    },
    /// A file held a value that does not parse as the expected type.
    #[error("{path}: cannot parse {value:?}")]
    Parse {
        /// The file that was read.
        path: PathBuf,
        /// Its contents, without the trailing newline.
        value: String,
    },
    /// servicemanager rejected the service registration.
    #[error("cannot register {name}: {status:?}")]
    Register {
        /// The service instance name, such as `android.hardware.light.ILights/default`.
        name: String,
        /// The binder status code servicemanager returned.
        status: StatusCode,
    },
    /// The binder thread pool returned, so the service stopped serving calls.
    #[error("binder thread pool exited")]
    ThreadPoolExited,
}

impl Error {
    /// Wraps an I/O error together with the path it happened on.
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}

impl From<Error> for Status {
    /// Reports the error to the binder client as `ILLEGAL_STATE` with the error text.
    fn from(err: Error) -> Self {
        Status::new_exception_str(ExceptionCode::ILLEGAL_STATE, Some(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_the_path() {
        let err = Error::io("/sys/class/leds/x/brightness", io::ErrorKind::NotFound.into());
        let text = err.to_string();
        assert!(text.starts_with("/sys/class/leds/x/brightness: "), "got {text:?}");
    }

    #[test]
    fn error_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Error>();
    }
}
