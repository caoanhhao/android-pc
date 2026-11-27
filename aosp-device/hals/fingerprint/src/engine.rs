//! Enrollment and matching on the real sensor: the `securefp` command sequence of the stock
//! HAL's `FPBAuthService::enroll`/`identify`, with the capture loop driven by the TA's fpops.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use log::{debug, info, warn};

use crate::securefp::{self, fpop, EnrollProgress, Match, SecureFp, Step, STATUS_BAD_IMAGE};
use crate::sensor::{Canceller, Sensor, Wake};

/// The directory the TA mixes into its template keys; the stock HAL's storage root.
pub const STOREPATH: &str = "/data/vendor/biometrics";

/// TA answers in a row that ask for no finger wait before we give up. A TA that keeps
/// answering at once once reset the SoC when called back in a tight loop.
const MAX_IMMEDIATE: u32 = 16;

#[derive(Debug, thiserror::Error)]
/// Why an operation failed.
pub enum Error {
    #[error(transparent)]
    /// The TA refused a command or answered unexpectedly.
    Ta(#[from] securefp::Error),
    #[error("sensor: {0}")]
    /// `/dev/goodix_fp` failed.
    Sensor(#[from] std::io::Error),
    #[error("cancelled")]
    /// The operation was cancelled.
    Cancelled,
}

/// Result of an operation.
pub type Result<T> = std::result::Result<T, Error>;

/// What a running operation reports as it goes.
#[derive(Debug)]
pub enum Event {
    /// The TA saw a finger arrive.
    FingerDown,
    /// A capture was rejected; the user should try again.
    BadImage(u32),
    /// A capture was accepted into the enrollment.
    Progress(EnrollProgress),
    /// No enrolled finger matched; matching goes on.
    NoMatch(Step),
}

/// The sensor and the TA for one operation. Dropping it cancels a parked capture and powers
/// the sensor off; the TA stays loaded.
///
/// `securefp` is started once per boot and never shut down: a second start in the same boot,
/// even after a clean Cancel and shutdown, reset the SoC (docs/gts7l.md §25). The kernel driver
/// keeps an app running while any client holds it, so the HAL holds one [`SecureFp`] for its
/// whole life.
pub struct Device {
    sensor: Sensor,
    ta: Arc<SecureFp>,
    cancelled: Arc<AtomicBool>,
    /// A capture command was answered with a wait the TA still expects us to finish.
    parked: bool,
}

/// Cancels a running operation from another thread.
#[derive(Clone)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
    waker: Arc<Canceller>,
}

impl CancelHandle {
    /// Cancels the operation; a running finger wait returns at once.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.waker.cancel();
    }
}

impl Device {
    /// Powers the sensor for an operation of user `user` on `ta`.
    pub fn open(ta: Arc<SecureFp>, user: &str) -> Result<(Device, CancelHandle)> {
        let sensor = Sensor::open()?;
        sensor.power_on()?;
        let flag = Arc::new(AtomicBool::new(false));
        let handle = CancelHandle { flag: flag.clone(), waker: Arc::new(sensor.canceller()?) };
        let dev = Device { sensor, ta, cancelled: flag, parked: false };
        debug!("securefp started, app id {}", dev.ta().app_id());
        dev.ta().type_check()?;
        dev.ta().send_storepath(user, STOREPATH)?;
        Ok((dev, handle))
    }

    fn ta(&self) -> &SecureFp {
        &self.ta
    }

    fn check_cancel(&self) -> Result<()> {
        if self.cancelled.load(Ordering::SeqCst) { Err(Error::Cancelled) } else { Ok(()) }
    }

    /// Runs one capture command until the TA answers fpop 0 (or the operation is cancelled).
    /// `waited` counts finger waits, so callers can tell a fresh capture from a repeat.
    fn capture(
        &mut self,
        mut step: impl FnMut(&SecureFp) -> securefp::Result<Step>,
        on_event: &mut dyn FnMut(Event),
        waited: &mut u32,
    ) -> Result<Step> {
        let mut immediate = 0;
        loop {
            self.check_cancel()?;
            let s = step(self.ta())?;
            debug!("step {s:x?}");
            self.parked = s.fpop != fpop::DONE;
            match s.fpop {
                fpop::DONE => return Ok(s),
                fpop::WAIT_INTERRUPT | fpop::WAIT_FINGER_UP => {
                    match self.sensor.wait_finger(None)? {
                        Wake::Interrupt => {}
                        Wake::Cancelled | Wake::Timeout => return Err(Error::Cancelled),
                    }
                    *waited += 1;
                    immediate = 0;
                    continue;
                }
                fpop::FINGER_DOWN => on_event(Event::FingerDown),
                fpop::FINGER_DOWN_2 => {}
                fpop::RESET | fpop::RESET_2 => self.sensor.reset()?,
                other => {
                    warn!("unknown fpop {other}, step {s:x?}");
                    return Err(securefp::Error::Protocol("unknown fpop").into());
                }
            }
            immediate += 1;
            if immediate > MAX_IMMEDIATE {
                return Err(securefp::Error::Protocol("TA keeps answering without a finger wait").into());
            }
        }
    }

    /// Enrolls one finger into slot `bio_idx` (1..4). Returns the encrypted template.
    ///
    /// As the stock `FPBAuthService::enroll`, every capture starts with `Prepare` and
    /// `Enroll_Init`: without them `Enroll_Do` keeps answering the last capture's result instead
    /// of waiting for a new touch. The TA keeps the progress across them until `Enroll_Final`.
    pub fn enroll(&mut self, user: &str, bio_idx: u32, on_event: &mut dyn FnMut(Event)) -> Result<Vec<u8>> {
        let mut idle = 0;
        let mut template = Vec::new();
        let last = loop {
            self.ta().prepare()?;
            self.ta().enroll_init(user, bio_idx, STOREPATH)?;
            let mut waited = 0;
            let s = self.capture(
                |ta| {
                    let (s, data) = ta.enroll_do()?;
                    if !data.is_empty() {
                        template = data;
                    }
                    Ok(s)
                },
                on_event,
                &mut waited,
            )?;
            idle = if waited == 0 { idle + 1 } else { 0 };
            if idle > 2 {
                return Err(securefp::Error::Protocol("Enroll_Do finished captures without a finger").into());
            }
            let p = EnrollProgress::from(&s);
            match s.status {
                0 if p.percentage >= 100 => break p,
                0 => on_event(Event::Progress(p)),
                STATUS_BAD_IMAGE => on_event(Event::BadImage(p.result)),
                status => return Err(securefp::Error::Status { cmd: 3, status }.into()),
            }
        };
        on_event(Event::Progress(last));
        self.ta().enroll_final()?;
        if template.is_empty() {
            return Err(securefp::Error::Protocol("enrollment finished without a template").into());
        }
        info!("enrolled slot {bio_idx}, template {} bytes", template.len());
        Ok(template)
    }

    /// Waits for a finger and matches it against `templates` (the user's template files
    /// concatenated). Bad captures and non-matching fingers are reported and matching goes on;
    /// returns on the first match, or with [`Error::Cancelled`] once `stop` says so after a
    /// reported attempt (a lockout).
    pub fn identify(
        &mut self,
        user: &str,
        templates: &[u8],
        challenge: u64,
        on_event: &mut dyn FnMut(Event),
        stop: &dyn Fn() -> bool,
    ) -> Result<(Match, Vec<u8>)> {
        let mut idle = 0;
        loop {
            self.ta().prepare()?;
            self.ta().identify_init(user, templates, challenge)?;
            let mut hat = None;
            let mut waited = 0;
            let s = self.capture(
                |ta| {
                    let (s, h) = ta.identify_do()?;
                    if h.is_some() {
                        hat = h;
                    }
                    Ok(s)
                },
                on_event,
                &mut waited,
            );
            let s = s?;
            idle = if waited == 0 { idle + 1 } else { 0 };
            if idle > 2 {
                return Err(securefp::Error::Protocol("Identify_Do finished without a finger").into());
            }
            let (score, bio_idx) = (s.words[2], s.words[3]);
            info!("identify: status {:#x} score {score} idx {bio_idx} words {:x?}", s.status, s.words);
            let updated = self.ta().identify_final().unwrap_or_default();
            match s.status {
                0 if score != 0 && bio_idx != 0 && bio_idx != 0xFF => {
                    return Ok((Match { score, bio_idx, hat }, updated));
                }
                STATUS_BAD_IMAGE => on_event(Event::BadImage(s.words[1])),
                _ => on_event(Event::NoMatch(s)),
            }
            if stop() {
                return Err(Error::Cancelled);
            }
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.parked {
            if let Err(e) = self.ta.cancel() {
                warn!("cancel: {e}");
            }
        }
        if let Err(e) = self.sensor.power_off() {
            warn!("sensor off: {e}");
        }
    }
}
