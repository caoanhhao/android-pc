//! CPU time counters from `/proc/stat`, for CPU usage over an interval.

use std::path::Path;

use crate::{Error, Result};

/// The kernel's file of CPU time counters.
pub const PROC_STAT: &str = "/proc/stat";

/// Aggregate CPU time of all CPUs since boot, in clock ticks (`USER_HZ`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CpuTimes {
    /// Ticks spent running anything: user, nice, system, irq, softirq, steal.
    pub busy: u64,
    /// `busy` plus idle and iowait.
    pub total: u64,
}

impl CpuTimes {
    /// Reads the aggregate `cpu` line of `/proc/stat`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the file cannot be read and [`Error::Parse`] if it has no
    /// valid `cpu` line.
    pub fn read() -> Result<Self> {
        Self::read_from(PROC_STAT)
    }

    /// Like [`CpuTimes::read`], from another file (tests use fixtures).
    ///
    /// # Errors
    ///
    /// As for [`CpuTimes::read`].
    pub fn read_from(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|err| Error::io(path, err))?;
        let line = text.lines().find(|line| line.starts_with("cpu ")).unwrap_or_default();
        Self::parse(line).ok_or_else(|| Error::Parse { path: path.to_owned(), value: line.into() })
    }

    /// Parses a `cpu  user nice system idle iowait irq softirq steal guest guest_nice` line.
    ///
    /// Guest time is already counted in user and nice, so it is not added again.
    pub fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split_whitespace();
        if !fields.next()?.starts_with("cpu") {
            return None;
        }
        let values: Vec<u64> =
            fields.take(8).map(str::parse).collect::<std::result::Result<_, _>>().ok()?;
        // Kernels since 2.6.33 have all eight; older fields default to zero.
        let field = |index: usize| values.get(index).copied().unwrap_or(0);
        if values.len() < 4 {
            return None;
        }
        let idle = field(3) + field(4);
        let busy = field(0) + field(1) + field(2) + field(5) + field(6) + field(7);
        Some(Self { busy, total: busy + idle })
    }

    /// The share of time the CPUs were busy between `earlier` and `self`, in percent
    /// (0.0..=100.0). `None` if no time passed.
    pub fn usage_since(&self, earlier: &Self) -> Option<f32> {
        let total = self.total.checked_sub(earlier.total).filter(|&total| total > 0)?;
        let busy = self.busy.saturating_sub(earlier.busy).min(total);
        // Tick counts over a few seconds fit f64 exactly; the ratio only needs f32.
        Some((busy as f64 * 100.0 / total as f64) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aggregate_line() {
        let times = CpuTimes::parse("cpu  100 5 50 800 20 3 2 0 0 0").expect("valid line");
        assert_eq!(times, CpuTimes { busy: 160, total: 980 });
        assert_eq!(CpuTimes::parse("intr 1 2 3"), None);
        assert_eq!(CpuTimes::parse("cpu 1 2"), None);
    }

    #[test]
    fn usage_between_samples() {
        let before = CpuTimes { busy: 100, total: 1000 };
        let after = CpuTimes { busy: 150, total: 1200 };
        assert_eq!(after.usage_since(&before), Some(25.0));
        assert_eq!(before.usage_since(&before), None);
    }

    #[test]
    fn reads_fixture_file() -> Result<()> {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("stat");
        std::fs::write(&path, "cpu  1 1 1 7 0 0 0 0 0 0\ncpu0 1 1 1 7 0 0 0 0 0 0\nintr 0\n")
            .expect("write fixture");
        assert_eq!(CpuTimes::read_from(&path)?, CpuTimes { busy: 3, total: 10 });
        Ok(())
    }
}
