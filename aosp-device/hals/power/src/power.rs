//! The IPower implementation: hints become a [`Profile`] that a worker thread applies.

use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use android_hardware_power::aidl::android::hardware::power::{
    Boost::Boost,
    ChannelConfig::ChannelConfig,
    CompositionData::CompositionData,
    CompositionUpdate::CompositionUpdate,
    CpuHeadroomParams::CpuHeadroomParams,
    CpuHeadroomResult::CpuHeadroomResult,
    GpuHeadroomParams::GpuHeadroomParams,
    GpuHeadroomResult::GpuHeadroomResult,
    IPower::IPower,
    IPowerHintSession::IPowerHintSession,
    Mode::Mode,
    SessionConfig::SessionConfig,
    SessionTag::SessionTag,
    SupportInfo::{HeadroomSupportInfo::HeadroomSupportInfo, SupportInfo},
};
use binder::{ExceptionCode, Interface, Status, Strong};
use log::{debug, info};
use pchal::procstat::CpuTimes;

use crate::pstate::Profile;

/// Modes this HAL acts on. FIXED_PERFORMANCE is required (VTS `hasFixedPerformance`),
/// even on machines where it changes nothing.
const SUPPORTED_MODES: [Mode; 9] = [
    Mode::LOW_POWER,
    Mode::SUSTAINED_PERFORMANCE,
    Mode::FIXED_PERFORMANCE,
    Mode::LAUNCH,
    Mode::INTERACTIVE,
    Mode::DEVICE_IDLE,
    Mode::DISPLAY_INACTIVE,
    Mode::GAME,
    Mode::GAME_LOADING,
];
/// Boosts this HAL acts on. Display, camera and audio boosts matter on phones whose
/// cores sleep deeply; HWP ramps a PC's cores fast enough.
const SUPPORTED_BOOSTS: [Boost; 1] = [Boost::INTERACTION];
/// INTERACTION boost length when the caller passes 0 or less ("default").
const DEFAULT_BOOST: Duration = Duration::from_millis(200);
/// Upper bound for a requested boost, so a bogus duration cannot pin the CPUs.
const MAX_BOOST: Duration = Duration::from_secs(5);
/// Bounds of the CPU headroom calculation window, as reported in SupportInfo.
const HEADROOM_WINDOW_MS: (i32, i32) = (50, 10_000);

/// The hints Android has sent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hints {
    /// Active modes, one bit per `Mode` value (all are below 64).
    modes: u64,
    boost_until: Option<Instant>,
}

impl Default for Hints {
    fn default() -> Self {
        // The screen is on until Android says otherwise.
        Self { modes: bit(Mode::INTERACTIVE.0), boost_until: None }
    }
}

impl Hints {
    fn has(&self, mode: Mode) -> bool {
        self.modes & bit(mode.0) != 0
    }

    /// The profile to apply at `now`, highest priority first: explicit benchmark and
    /// battery-saver modes, then screen off, then performance requests.
    fn profile(&self, now: Instant) -> Profile {
        let boosting = self.boost_until.is_some_and(|until| until > now);
        if self.has(Mode::FIXED_PERFORMANCE) {
            Profile::Fixed
        } else if self.has(Mode::LOW_POWER) {
            Profile::PowerSave
        } else if !self.has(Mode::INTERACTIVE)
            || self.has(Mode::DEVICE_IDLE)
            || self.has(Mode::DISPLAY_INACTIVE)
        {
            Profile::Idle
        } else if self.has(Mode::SUSTAINED_PERFORMANCE) {
            Profile::Sustained
        } else if boosting || self.has(Mode::LAUNCH) || self.has(Mode::GAME_LOADING) {
            Profile::Boost
        } else if self.has(Mode::GAME) {
            Profile::Game
        } else {
            Profile::Balanced
        }
    }
}

fn bit(value: i32) -> u64 {
    u32::try_from(value).ok().and_then(|shift| 1_u64.checked_shl(shift)).unwrap_or(0)
}

fn bitset<T: Copy>(items: &[T], value: impl Fn(T) -> i32) -> i64 {
    let bits = items.iter().fold(0_u64, |bits, &item| bits | bit(value(item)));
    i64::from_ne_bytes(bits.to_ne_bytes())
}

/// The power HAL. Cloning shares the same state with the worker thread.
#[derive(Debug, Clone)]
pub struct Power {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    hints: Mutex<Hints>,
    changed: Condvar,
}

impl Interface for Power {}

impl Power {
    /// A HAL with no hints received yet.
    pub fn new() -> Self {
        Self { inner: Arc::default() }
    }

    /// Starts the thread that applies the hints' profile with `apply`.
    ///
    /// # Errors
    ///
    /// Returns the error from spawning the thread.
    pub fn spawn_worker(
        &self,
        mut apply: impl FnMut(Profile) + Send + 'static,
    ) -> std::io::Result<()> {
        let inner = Arc::clone(&self.inner);
        thread::Builder::new().name("cpufreq".into()).spawn(move || {
            let mut hints = inner.hints.lock().expect("hints lock poisoned");
            let mut applied = None;
            loop {
                let now = Instant::now();
                let profile = hints.profile(now);
                if applied != Some(profile) {
                    debug!("profile {profile:?}");
                    apply(profile);
                    applied = Some(profile);
                }
                // Sleep until the next hint, or until the running boost ends.
                hints = match hints.boost_until.filter(|&until| until > now) {
                    Some(until) => {
                        inner
                            .changed
                            .wait_timeout(hints, until - now)
                            .expect("hints lock poisoned")
                            .0
                    }
                    None => inner.changed.wait(hints).expect("hints lock poisoned"),
                };
            }
        })?;
        Ok(())
    }

    fn update(&self, change: impl FnOnce(&mut Hints)) {
        let mut hints = self.inner.hints.lock().expect("hints lock poisoned");
        change(&mut hints);
        self.inner.changed.notify_one();
    }
}

impl Default for Power {
    fn default() -> Self {
        Self::new()
    }
}

impl IPower for Power {
    fn setMode(&self, mode: Mode, enabled: bool) -> binder::Result<()> {
        if !SUPPORTED_MODES.contains(&mode) {
            return Ok(());
        }
        debug!("setMode {mode:?} {enabled}");
        self.update(|hints| {
            if enabled {
                hints.modes |= bit(mode.0);
            } else {
                hints.modes &= !bit(mode.0);
            }
        });
        Ok(())
    }

    fn isModeSupported(&self, mode: Mode) -> binder::Result<bool> {
        Ok(SUPPORTED_MODES.contains(&mode))
    }

    fn setBoost(&self, boost: Boost, duration_ms: i32) -> binder::Result<()> {
        if !SUPPORTED_BOOSTS.contains(&boost) {
            return Ok(());
        }
        let duration = u64::try_from(duration_ms)
            .ok()
            .filter(|&ms| ms > 0)
            .map_or(DEFAULT_BOOST, Duration::from_millis)
            .min(MAX_BOOST);
        let until = Instant::now() + duration;
        self.update(|hints| hints.boost_until = hints.boost_until.max(Some(until)));
        Ok(())
    }

    fn isBoostSupported(&self, boost: Boost) -> binder::Result<bool> {
        Ok(SUPPORTED_BOOSTS.contains(&boost))
    }

    fn createHintSession(
        &self,
        _tgid: i32,
        _uid: i32,
        _thread_ids: &[i32],
        _duration_nanos: i64,
    ) -> binder::Result<Strong<dyn IPowerHintSession>> {
        // ADPF hint sessions are optional; without them the framework sends no session
        // hints and getHintSessionPreferredRate is never asked for.
        Err(unsupported())
    }

    fn getHintSessionPreferredRate(&self) -> binder::Result<i64> {
        Err(unsupported())
    }

    fn createHintSessionWithConfig(
        &self,
        _tgid: i32,
        _uid: i32,
        _thread_ids: &[i32],
        _duration_nanos: i64,
        _tag: SessionTag,
        _config: &mut SessionConfig,
    ) -> binder::Result<Strong<dyn IPowerHintSession>> {
        Err(unsupported())
    }

    fn getSessionChannel(&self, _tgid: i32, _uid: i32) -> binder::Result<ChannelConfig> {
        Err(unsupported())
    }

    fn closeSessionChannel(&self, _tgid: i32, _uid: i32) -> binder::Result<()> {
        Ok(())
    }

    fn getSupportInfo(&self) -> binder::Result<SupportInfo> {
        Ok(SupportInfo {
            usesSessions: false,
            boosts: bitset(&SUPPORTED_BOOSTS, |boost| boost.0),
            modes: bitset(&SUPPORTED_MODES, |mode| mode.0),
            sessionHints: 0,
            sessionModes: 0,
            sessionTags: 0,
            headroom: HeadroomSupportInfo {
                isCpuSupported: true,
                isGpuSupported: false,
                cpuMinIntervalMillis: 0,
                cpuMinCalculationWindowMillis: HEADROOM_WINDOW_MS.0,
                cpuMaxCalculationWindowMillis: HEADROOM_WINDOW_MS.1,
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn getCpuHeadroom(&self, params: &CpuHeadroomParams) -> binder::Result<CpuHeadroomResult> {
        // Global headroom only: the idle share of all CPUs over the requested window,
        // measured from now (the calling binder thread waits for the window).
        let window_ms =
            params.calculationWindowMillis.clamp(HEADROOM_WINDOW_MS.0, HEADROOM_WINDOW_MS.1);
        let window = Duration::from_millis(u64::try_from(window_ms).expect("window is positive"));
        let before = CpuTimes::read()?;
        thread::sleep(window);
        let after = CpuTimes::read()?;
        let usage = after.usage_since(&before).unwrap_or(0.0);
        Ok(CpuHeadroomResult::GlobalHeadroom((100.0 - usage).clamp(0.0, 100.0)))
    }

    fn getGpuHeadroom(&self, _params: &GpuHeadroomParams) -> binder::Result<GpuHeadroomResult> {
        Err(unsupported())
    }

    fn sendCompositionData(&self, _data: &[CompositionData]) -> binder::Result<()> {
        // SupportInfo marks composition data unsupported, so SurfaceFlinger sends none.
        Ok(())
    }

    fn sendCompositionUpdate(&self, _update: &CompositionUpdate) -> binder::Result<()> {
        Ok(())
    }
}

fn unsupported() -> Status {
    Status::new_exception(ExceptionCode::UNSUPPORTED_OPERATION, None)
}

/// Logs the supported hints once at startup.
pub fn log_support() {
    info!("modes {SUPPORTED_MODES:?}, boosts {SUPPORTED_BOOSTS:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_priorities() {
        let now = Instant::now();
        let mut hints = Hints::default();
        assert_eq!(hints.profile(now), Profile::Balanced);

        hints.modes |= bit(Mode::GAME.0);
        assert_eq!(hints.profile(now), Profile::Game);
        hints.boost_until = Some(now + Duration::from_millis(100));
        assert_eq!(hints.profile(now), Profile::Boost);
        assert_eq!(hints.profile(now + Duration::from_secs(1)), Profile::Game, "boost expired");

        hints.modes &= !bit(Mode::INTERACTIVE.0);
        assert_eq!(hints.profile(now), Profile::Idle);
        hints.modes |= bit(Mode::LOW_POWER.0);
        assert_eq!(hints.profile(now), Profile::PowerSave);
        hints.modes |= bit(Mode::FIXED_PERFORMANCE.0);
        assert_eq!(hints.profile(now), Profile::Fixed);
    }

    #[test]
    fn support_info_matches_queries() -> binder::Result<()> {
        let power = Power::new();
        let info = power.getSupportInfo()?;
        for value in -1..=20 {
            let mode = Mode(value);
            let in_mask = (0..64).contains(&value) && (info.modes >> value) & 1 == 1;
            assert_eq!(power.isModeSupported(mode)?, in_mask, "{mode:?}");
            let boost = Boost(value);
            let in_mask = (0..64).contains(&value) && (info.boosts >> value) & 1 == 1;
            assert_eq!(power.isBoostSupported(boost)?, in_mask, "{boost:?}");
        }
        assert!(power.isModeSupported(Mode::FIXED_PERFORMANCE)?);
        Ok(())
    }

    #[test]
    fn headroom_is_a_percentage() -> binder::Result<()> {
        let params = CpuHeadroomParams { calculationWindowMillis: 50, ..Default::default() };
        let CpuHeadroomResult::GlobalHeadroom(headroom) = Power::new().getCpuHeadroom(&params)?;
        assert!((0.0..=100.0).contains(&headroom), "{headroom}");
        Ok(())
    }

    #[test]
    fn bits_ignore_out_of_range_values() {
        assert_eq!(bit(-1), 0);
        assert_eq!(bit(64), 0);
        assert_eq!(bit(3), 8);
    }
}
