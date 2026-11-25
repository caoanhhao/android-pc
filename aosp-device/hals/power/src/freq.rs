//! CPU performance profiles applied through the generic cpufreq limits, for CPUs without
//! `intel_pstate` (ARM SoCs such as the Galaxy Tab S7's SM8250 on schedutil).
//!
//! The governor (schedutil) keeps picking the frequency; each profile only moves the
//! `scaling_min_freq` floor and the `scaling_max_freq` cap of every policy under
//! `/sys/devices/system/cpu/cpufreq`, as fractions of the limits the kernel booted with,
//! rounded to a frequency the policy offers.
//!
//! This module only reads and writes sysfs and knows nothing about AIDL; its tests run with
//! the whole HAL's (`android.hardware.power-service.pc_test`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{debug, info, warn};

use crate::pstate::Profile;

/// Floor while a boost or app launch runs, as a fraction of the boot maximum: the cores
/// start at half speed instead of ramping up from idle.
const BOOST_FLOOR: f64 = 0.5;
/// Floor while a game runs.
const GAME_FLOOR: f64 = 0.3;
/// Cap in sustained and fixed performance modes: a clock the SoC can hold without
/// thermal throttling, so performance stays steady.
const SUSTAINED_CAP: f64 = 0.8;
/// Cap for battery saver and screen off.
const POWER_SAVE_CAP: f64 = 0.6;

/// One cpufreq policy (a cluster of cores sharing a clock).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Policy {
    dir: PathBuf,
    /// Frequencies the policy offers in kHz, ascending; empty if it does not list them.
    available: Vec<u32>,
    /// `scaling_min_freq` and `scaling_max_freq` at startup, in kHz.
    boot: (u32, u32),
    /// What was last written (or read at startup).
    current: (u32, u32),
}

impl Policy {
    /// The highest offered frequency at or below `fraction` of the boot maximum, but not
    /// below the boot minimum.
    fn at(&self, fraction: f64) -> u32 {
        let (min, max) = self.boot;
        // kHz values fit easily in f64; the result is at most `max`.
        let wanted = (f64::from(max) * fraction) as u32;
        let offered = self.available.iter().rev().find(|&&freq| freq <= wanted).copied();
        offered.unwrap_or(wanted).clamp(min, max)
    }

    /// The `(min, max)` limits for `profile`.
    fn limits(&self, profile: Profile) -> (u32, u32) {
        let (min, max) = self.boot;
        match profile {
            Profile::Balanced => (min, max),
            Profile::Boost => (self.at(BOOST_FLOOR), max),
            Profile::Game => (self.at(GAME_FLOOR), max),
            Profile::Sustained => (min, self.at(SUSTAINED_CAP)),
            Profile::Fixed => {
                let fixed = self.at(SUSTAINED_CAP);
                (fixed, fixed)
            }
            Profile::PowerSave | Profile::Idle => (min, self.at(POWER_SAVE_CAP)),
        }
    }
}

/// The cpufreq policies of this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FreqLimits {
    policies: Vec<Policy>,
}

impl FreqLimits {
    /// Probes `cpufreq_dir` (`/sys/devices/system/cpu/cpufreq`) and remembers the current
    /// limits as the balanced ones.
    pub fn discover(cpufreq_dir: &Path) -> Self {
        let mut policies: Vec<Policy> = pchal::sysfs::entries(cpufreq_dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|dir| {
                dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("policy"))
            })
            .filter_map(|dir| {
                let boot = (
                    read::<u32>(&dir.join("scaling_min_freq"))?,
                    read::<u32>(&dir.join("scaling_max_freq"))?,
                );
                let mut available: Vec<u32> =
                    read::<String>(&dir.join("scaling_available_frequencies"))
                        .map(|list| {
                            list.split_whitespace().filter_map(|f| f.parse().ok()).collect()
                        })
                        .unwrap_or_default();
                available.sort_unstable();
                Some(Policy { dir, available, boot, current: boot })
            })
            .collect();
        policies.sort_by(|a, b| a.dir.cmp(&b.dir));
        for policy in &policies {
            info!(
                "cpufreq {}: {}-{} kHz, {} frequencies",
                policy.dir.display(),
                policy.boot.0,
                policy.boot.1,
                policy.available.len()
            );
        }
        Self { policies }
    }

    /// Whether there is no policy to steer.
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Writes the limits of `profile` that differ from what was last written. Failures are
    /// logged (once per file); the limit is retried on the next change.
    pub fn apply(&mut self, profile: Profile) {
        for policy in &mut self.policies {
            let (min, max) = policy.limits(profile);
            // A higher cap goes first so that a higher floor fits below it, and a lower
            // floor first so that a lower cap stays above it.
            let raise_max = max > policy.current.1;
            if raise_max && write(&policy.dir.join("scaling_max_freq"), max) {
                policy.current.1 = max;
            }
            if min != policy.current.0 && write(&policy.dir.join("scaling_min_freq"), min) {
                policy.current.0 = min;
            }
            if max != policy.current.1 && write(&policy.dir.join("scaling_max_freq"), max) {
                policy.current.1 = max;
            }
        }
    }
}

fn read<T: std::str::FromStr>(path: &Path) -> Option<T> {
    pchal::sysfs::read_optional(path).unwrap_or_else(|err| {
        debug!("{err}");
        None
    })
}

/// Writes a limit. A failure (usually missing permissions) is logged once per file, since
/// boosts retry the same writes many times a second.
fn write(path: &Path, value: u32) -> bool {
    static FAILED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    match pchal::sysfs::write(path, value) {
        Ok(()) => true,
        Err(err) => {
            let mut failed = FAILED.lock().expect("failed-writes lock poisoned");
            if !failed.iter().any(|known| known == path) {
                warn!("{err} (not logged again)");
                failed.push(path.to_owned());
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn write_fixture(path: &Path, value: &str) {
        fs::create_dir_all(path.parent().expect("fixture has a parent")).expect("create dir");
        fs::write(path, format!("{value}\n")).expect("write fixture");
    }

    fn limits(cpufreq: &Path, policy: &str) -> (String, String) {
        let read = |name: &str| {
            fs::read_to_string(cpufreq.join(policy).join(name)).expect("read").trim().to_owned()
        };
        (read("scaling_min_freq"), read("scaling_max_freq"))
    }

    /// The SM8250's three policies (A55, A77, A77 prime), abridged frequency tables.
    fn sm8250(root: &Path) -> PathBuf {
        let cpufreq = root.join("cpufreq");
        for (policy, freqs) in [
            ("policy0", "300000 403200 518400 614400 902400 1075200 1401600 1612800 1804800"),
            ("policy4", "710400 844800 1075200 1305600 1555200 1766400 2073600 2419200"),
            ("policy7", "844800 1075200 1401600 1766400 2265600 2457600 2841600 3091200"),
        ] {
            let dir = cpufreq.join(policy);
            let list: Vec<&str> = freqs.split(' ').collect();
            write_fixture(&dir.join("scaling_available_frequencies"), freqs);
            write_fixture(&dir.join("scaling_min_freq"), list[0]);
            write_fixture(&dir.join("scaling_max_freq"), list[list.len() - 1]);
        }
        cpufreq
    }

    #[test]
    fn profiles_move_floor_and_cap() {
        let root = tempfile::tempdir().expect("create temp dir");
        let cpufreq = sm8250(root.path());
        let mut cpu = FreqLimits::discover(&cpufreq);
        assert!(!cpu.is_empty());

        cpu.apply(Profile::Boost);
        // Half of 1804800 is 902400, which the A55s offer.
        assert_eq!(limits(&cpufreq, "policy0"), ("902400".into(), "1804800".into()));
        // Half of 3091200 is 1545600: the next lower offered frequency.
        assert_eq!(limits(&cpufreq, "policy7"), ("1401600".into(), "3091200".into()));

        cpu.apply(Profile::Fixed);
        // 80 % of 2419200 is 1935360.
        assert_eq!(limits(&cpufreq, "policy4"), ("1766400".into(), "1766400".into()));

        cpu.apply(Profile::PowerSave);
        assert_eq!(limits(&cpufreq, "policy4"), ("710400".into(), "1305600".into()));

        cpu.apply(Profile::Balanced);
        for (policy, boot) in
            [("policy0", ("300000", "1804800")), ("policy7", ("844800", "3091200"))]
        {
            assert_eq!(limits(&cpufreq, policy), (boot.0.into(), boot.1.into()));
        }
    }

    #[test]
    fn without_a_frequency_table_the_fraction_is_written() {
        let root = tempfile::tempdir().expect("create temp dir");
        let dir = root.path().join("cpufreq/policy0");
        write_fixture(&dir.join("scaling_min_freq"), "400000");
        write_fixture(&dir.join("scaling_max_freq"), "2000000");
        let mut cpu = FreqLimits::discover(&root.path().join("cpufreq"));
        cpu.apply(Profile::Sustained);
        assert_eq!(limits(&root.path().join("cpufreq"), "policy0").1, "1600000");
    }

    #[test]
    fn no_cpufreq_is_empty() {
        let root = tempfile::tempdir().expect("create temp dir");
        let mut cpu = FreqLimits::discover(&root.path().join("missing"));
        assert!(cpu.is_empty());
        cpu.apply(Profile::Boost);
    }
}
