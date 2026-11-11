//! Reading and writing sysfs attributes.
//!
//! sysfs attributes hold one value followed by a newline. These helpers strip the newline
//! on read and attach the path to every error.

use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::{Error, Result};

/// Reads an attribute as text, without its trailing newline.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read or is not UTF-8.
pub fn read_string(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let mut value = fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
    let len = value.trim_end_matches('\n').len();
    value.truncate(len);
    Ok(value)
}

/// Reads an attribute and parses it, for example as a `u32`.
///
/// Surrounding whitespace is ignored.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read and [`Error::Parse`] if its contents
/// do not parse as `T`.
pub fn read<T: FromStr>(path: impl AsRef<Path>) -> Result<T> {
    let path = path.as_ref();
    let value = read_string(path)?;
    value.trim().parse().map_err(|_| Error::Parse { path: path.to_owned(), value })
}

/// Writes `value` to an attribute.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be opened or the kernel rejects the value.
pub fn write(path: impl AsRef<Path>, value: impl Display) -> Result<()> {
    let path = path.as_ref();
    fs::write(path, value.to_string()).map_err(|err| Error::io(path, err))
}

/// Lists the entries of a sysfs directory such as `/sys/class/leds`, sorted by name.
///
/// A missing directory means the kernel has no such class and yields an empty list.
///
/// # Errors
///
/// Returns [`Error::Io`] if the directory exists but cannot be read.
pub fn entries(dir: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
    let dir = dir.as_ref();
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(dir, err)),
    };
    let mut paths: Vec<PathBuf> = read_dir
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()
        .map_err(|err| Error::io(dir, err))?;
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_strips_newline_and_parses() -> Result<()> {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("max_brightness");
        fs::write(&path, "255\n").expect("write fixture");

        assert_eq!(read_string(&path)?, "255");
        assert_eq!(read::<u32>(&path)?, 255);
        Ok(())
    }

    #[test]
    fn parse_error_keeps_value() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("brightness");
        fs::write(&path, "bright\n").expect("write fixture");

        match read::<u32>(&path) {
            Err(Error::Parse { value, .. }) => assert_eq!(value, "bright"),
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn write_then_read_round_trips() -> Result<()> {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("brightness");
        write(&path, 42)?;
        assert_eq!(read::<u32>(&path)?, 42);
        Ok(())
    }

    #[test]
    fn entries_are_sorted_and_missing_dir_is_empty() -> Result<()> {
        let dir = tempfile::tempdir().expect("create temp dir");
        for name in ["b", "a", "c"] {
            fs::create_dir(dir.path().join(name)).expect("create fixture dir");
        }
        let names: Vec<_> =
            entries(dir.path())?.iter().map(|p| p.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        assert!(entries(dir.path().join("missing"))?.is_empty());
        Ok(())
    }
}
