//! CPU performance profiles applied through `intel_pstate`.
//!
//! With hardware P-states (HWP) the CPU picks its own frequency; software steers it with
//! the energy/performance preference (`energy_performance_preference`, per policy under
//! `/sys/devices/system/cpu/cpufreq/policy*`) and global limits in
//! `/sys/devices/system/cpu/intel_pstate` (`min_perf_pct`, `max_perf_pct`, `no_turbo`).
//! Android's power hints map to a [`Profile`], and each profile to [`Settings`] relative
//! to the values the kernel booted with.
//!
//! Without `intel_pstate` in active mode (AMD, `acpi-cpufreq`, Cuttlefish) every setting
//! is unavailable and the hints change nothing.
//!
//! This module only reads and writes sysfs and knows nothing about AIDL, so its tests run
//! on the host (`android.hardware.power-service.pc_pstate_test`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{debug, info, warn};

/// `min_perf_pct` while a boost or app launch runs: the cores start at half of their
/// maximum performance instead of ramping up from idle.
pub const BOOST_MIN_PERF_PCT: u32 = 50;

/// What the CPUs should optimise for, from the hints Android sends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Profile {
    /// The boot defaults.
    #[default]
    Balanced,
    /// Touch, app launch, game loading: fast response for a short time.
    Boost,
    /// A game in the foreground.
    Game,
    /// Sustained performance mode: no turbo, so clocks do not drop when it heats up.
    Sustained,
    /// Fixed performance mode (benchmarks, tests): fixed clocks without turbo.
    Fixed,
    /// Battery saver.
    PowerSave,
    /// Screen off or device idle.
    Idle,
}

/// Values for the `intel_pstate` knobs. `None` leaves a knob unchanged (or marks it
/// unavailable in the boot defaults).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Settings {
    /// `energy_performance_preference` for every policy.
    pub epp: Option<String>,
    /// `intel_pstate/min_perf_pct`.
    pub min_perf_pct: Option<u32>,
    /// `intel_pstate/max_perf_pct`.
    pub max_perf_pct: Option<u32>,
    /// `intel_pstate/no_turbo`.
    pub no_turbo: Option<bool>,
}

/// The cpufreq policies and `intel_pstate` knobs of this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuFreq {
    policies: Vec<PathBuf>,
    pstate_dir: Option<PathBuf>,
    available_epp: Vec<String>,
    defaults: Settings,
    current: Settings,
}

impl CpuFreq {
    /// Probes `cpufreq_dir` (`/sys/devices/system/cpu/cpufreq`) and `pstate_dir`
    /// (`/sys/devices/system/cpu/intel_pstate`), and remembers the current values as the
    /// balanced defaults.
    pub fn discover(cpufreq_dir: &Path, pstate_dir: &Path) -> Self {
        let status = read::<String>(&pstate_dir.join("status"));
        let active = status.as_deref() == Some("active");
        let policies: Vec<PathBuf> = if active {
            pchal::sysfs::entries(cpufreq_dir)
                .unwrap_or_default()
                .into_iter()
                .filter(|dir| {
                    dir.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("policy"))
                })
                .filter(|dir| dir.join("energy_performance_preference").exists())
                .collect()
        } else {
            Vec::new()
        };
        let available_epp: Vec<String> = policies
            .first()
            .and_then(|policy| {
                read::<String>(&policy.join("energy_performance_available_preferences"))
            })
            .map(|list| list.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        let pstate_dir = active.then(|| pstate_dir.to_owned());
        let knob = |name: &str| pstate_dir.as_ref().and_then(|dir| read::<u32>(&dir.join(name)));
        let defaults = Settings {
            epp: policies
                .first()
                .and_then(|policy| read(&policy.join("energy_performance_preference"))),
            min_perf_pct: knob("min_perf_pct"),
            max_perf_pct: knob("max_perf_pct"),
            no_turbo: knob("no_turbo").map(|value| value != 0),
        };
        match &status {
            Some(status) if active => info!(
                "intel_pstate {status}: {} policies, EPP {:?} of {:?}, defaults {defaults:?}",
                policies.len(),
                defaults.epp,
                available_epp
            ),
            Some(status) => info!("intel_pstate is {status}, power hints are ignored"),
            None => info!("no intel_pstate, power hints are ignored"),
        }
        Self { policies, pstate_dir, available_epp, current: defaults.clone(), defaults }
    }

    /// The settings for `profile`, from the boot defaults. Knobs this machine lacks stay
    /// `None`, and an EPP value the CPU does not offer is replaced by the nearest one.
    pub fn settings(&self, profile: Profile) -> Settings {
        let d = &self.defaults;
        let epp = |wanted: &str| self.epp(wanted).or_else(|| d.epp.clone());
        let mut settings = d.clone();
        match profile {
            Profile::Balanced => {}
            Profile::Boost => {
                settings.epp = epp("performance");
                settings.min_perf_pct = d.min_perf_pct.map(|min| min.max(BOOST_MIN_PERF_PCT));
            }
            Profile::Game => settings.epp = epp("performance"),
            Profile::Sustained => settings.no_turbo = d.no_turbo.map(|_| true),
            Profile::Fixed => {
                settings.epp = epp("performance");
                settings.no_turbo = d.no_turbo.map(|_| true);
                settings.min_perf_pct = d.max_perf_pct;
            }
            Profile::PowerSave => {
                settings.epp = epp("power");
                settings.no_turbo = d.no_turbo.map(|_| true);
            }
            Profile::Idle => settings.epp = epp("power"),
        }
        settings
    }

    /// Writes the knobs of `target` that differ from what was last written. Failures are
    /// logged (once per file); the knob is retried on the next change.
    pub fn apply(&mut self, target: &Settings) {
        let Some(dir) = self.pstate_dir.clone() else { return };
        // A higher max_perf_pct goes first so that a higher min_perf_pct fits below it.
        let raise_max = target.max_perf_pct > self.current.max_perf_pct;
        if raise_max {
            self.write_pct(&dir, Limit::Max, target.max_perf_pct);
        }
        if target.no_turbo != self.current.no_turbo {
            if let Some(no_turbo) = target.no_turbo {
                if write(&dir.join("no_turbo"), u32::from(no_turbo)) {
                    self.current.no_turbo = Some(no_turbo);
                }
            }
        }
        self.write_pct(&dir, Limit::Min, target.min_perf_pct);
        if !raise_max {
            self.write_pct(&dir, Limit::Max, target.max_perf_pct);
        }
        if target.epp != self.current.epp {
            if let Some(epp) = &target.epp {
                let written = self.policies.iter().fold(true, |ok, policy| {
                    write(&policy.join("energy_performance_preference"), epp) && ok
                });
                if written {
                    self.current.epp = Some(epp.clone());
                }
            }
        }
    }

    fn write_pct(&mut self, dir: &Path, limit: Limit, value: Option<u32>) {
        let (name, current) = match limit {
            Limit::Min => ("min_perf_pct", &mut self.current.min_perf_pct),
            Limit::Max => ("max_perf_pct", &mut self.current.max_perf_pct),
        };
        if value == *current {
            return;
        }
        if let Some(value) = value {
            if write(&dir.join(name), value) {
                *current = Some(value);
            }
        }
    }

    /// `wanted` if the CPU offers it, else the nearest offered preference towards
    /// balanced, else `None`.
    fn epp(&self, wanted: &str) -> Option<String> {
        let fallbacks: &[&str] = match wanted {
            "performance" => &["performance", "balance_performance"],
            "power" => &["power", "balance_power"],
            other => return self.available_epp.iter().find(|epp| *epp == other).cloned(),
        };
        fallbacks
            .iter()
            .find_map(|epp| self.available_epp.iter().find(|offered| offered == epp))
            .cloned()
    }
}

/// The two `intel_pstate` performance limits.
#[derive(Debug, Clone, Copy)]
enum Limit {
    Min,
    Max,
}

fn read<T: std::str::FromStr>(path: &Path) -> Option<T> {
    pchal::sysfs::read_optional(path).unwrap_or_else(|err| {
        debug!("{err}");
        None
    })
}

/// Writes a knob. A failure (usually missing permissions) is logged once per file, since
/// boosts retry the same writes many times a second.
fn write(path: &Path, value: impl std::fmt::Display) -> bool {
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

    fn read_fixture(path: &Path) -> String {
        fs::read_to_string(path).expect("read fixture").trim().to_owned()
    }

    /// An i7-13700K-like machine: 2 policies, HWP with every EPP value.
    fn machine(root: &Path) -> (PathBuf, PathBuf) {
        let cpufreq = root.join("cpufreq");
        let pstate = root.join("intel_pstate");
        for policy in ["policy0", "policy1"] {
            let dir = cpufreq.join(policy);
            write_fixture(&dir.join("energy_performance_preference"), "balance_performance");
            write_fixture(
                &dir.join("energy_performance_available_preferences"),
                "default performance balance_performance balance_power power",
            );
        }
        write_fixture(&pstate.join("status"), "active");
        write_fixture(&pstate.join("min_perf_pct"), "13");
        write_fixture(&pstate.join("max_perf_pct"), "100");
        write_fixture(&pstate.join("no_turbo"), "0");
        (cpufreq, pstate)
    }

    #[test]
    fn profiles_map_to_settings() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (cpufreq, pstate) = machine(root.path());
        let cpu = CpuFreq::discover(&cpufreq, &pstate);
        assert_eq!(cpu.defaults.epp.as_deref(), Some("balance_performance"));

        assert_eq!(cpu.settings(Profile::Balanced), cpu.defaults);
        let boost = cpu.settings(Profile::Boost);
        assert_eq!(boost.epp.as_deref(), Some("performance"));
        assert_eq!(boost.min_perf_pct, Some(BOOST_MIN_PERF_PCT));
        let fixed = cpu.settings(Profile::Fixed);
        assert_eq!((fixed.min_perf_pct, fixed.no_turbo), (Some(100), Some(true)));
        let save = cpu.settings(Profile::PowerSave);
        assert_eq!((save.epp.as_deref(), save.no_turbo), (Some("power"), Some(true)));
    }

    #[test]
    fn apply_writes_changes_and_restores() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (cpufreq, pstate) = machine(root.path());
        let mut cpu = CpuFreq::discover(&cpufreq, &pstate);

        cpu.apply(&cpu.settings(Profile::Boost));
        assert_eq!(
            read_fixture(&cpufreq.join("policy1/energy_performance_preference")),
            "performance"
        );
        assert_eq!(read_fixture(&pstate.join("min_perf_pct")), "50");

        cpu.apply(&cpu.settings(Profile::Balanced));
        assert_eq!(
            read_fixture(&cpufreq.join("policy0/energy_performance_preference")),
            "balance_performance"
        );
        assert_eq!(read_fixture(&pstate.join("min_perf_pct")), "13");
        assert_eq!(read_fixture(&pstate.join("no_turbo")), "0");
    }

    #[test]
    fn missing_epp_values_fall_back() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (cpufreq, pstate) = machine(root.path());
        for policy in ["policy0", "policy1"] {
            write_fixture(
                &cpufreq.join(policy).join("energy_performance_available_preferences"),
                "balance_performance balance_power",
            );
        }
        let cpu = CpuFreq::discover(&cpufreq, &pstate);
        assert_eq!(cpu.settings(Profile::Game).epp.as_deref(), Some("balance_performance"));
        assert_eq!(cpu.settings(Profile::Idle).epp.as_deref(), Some("balance_power"));
    }

    #[test]
    fn passive_or_missing_pstate_does_nothing() {
        let root = tempfile::tempdir().expect("create temp dir");
        let (cpufreq, pstate) = machine(root.path());
        write_fixture(&pstate.join("status"), "passive");
        let mut cpu = CpuFreq::discover(&cpufreq, &pstate);
        assert_eq!(&cpu.defaults, &Settings::default());
        cpu.apply(&cpu.settings(Profile::Boost));
        assert_eq!(
            read_fixture(&cpufreq.join("policy0/energy_performance_preference")),
            "balance_performance"
        );

        let none = CpuFreq::discover(&root.path().join("x"), &root.path().join("y"));
        assert_eq!(none.settings(Profile::Fixed), Settings::default());
    }
}
