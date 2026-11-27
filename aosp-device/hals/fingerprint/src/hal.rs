//! `IFingerprint`/`ISession` over [`engine`](crate::engine): one side-key sensor, STRONG.
//!
//! Each enroll or authenticate runs on its own thread that holds the device lock for its whole
//! life, so a new operation starts only after the previous one cancelled the TA cleanly.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use android_hardware_biometrics_common::aidl::android::hardware::biometrics::common::{
    CommonProps::CommonProps,
    ComponentInfo::ComponentInfo,
    ICancellationSignal::{BnCancellationSignal, ICancellationSignal},
    OperationContext::OperationContext,
    SensorStrength::SensorStrength,
};
use android_hardware_biometrics_fingerprint::aidl::android::hardware::biometrics::fingerprint::{
    AcquiredInfo::AcquiredInfo,
    Error::Error,
    FingerprintSensorType::FingerprintSensorType,
    IFingerprint::IFingerprint,
    ISession::{BnSession, ISession},
    ISessionCallback::ISessionCallback,
    PointerContext::PointerContext,
    SensorLocation::SensorLocation,
    SensorProps::SensorProps,
};
use android_hardware_keymaster::aidl::android::hardware::keymaster::{
    HardwareAuthToken::HardwareAuthToken, HardwareAuthenticatorType::HardwareAuthenticatorType,
    Timestamp::Timestamp,
};
use binder::{BinderFeatures, Interface, Strong};
use log::{error, info, warn};

use gts7l_fingerprint::engine::{self, CancelHandle, Device, Event, STOREPATH};
use gts7l_fingerprint::securefp::{EnrollProgress, SecureFp};
use gts7l_fingerprint::store::{Store, MAX_SLOTS};

const SENSOR_ID: i32 = 0;
/// Failed attempts in a row before a timed lockout, and before a permanent one.
const LOCKOUT_TIMED_AFTER: u32 = 5;
const LOCKOUT_PERMANENT_AFTER: u32 = 20;
const LOCKOUT_DURATION: Duration = Duration::from_secs(30);

/// Where the power key's sensor sits, in pixels of the panel's natural (portrait, 1600x2560)
/// orientation: on the right long edge, near the top. The same numbers are in the framework
/// overlay's `config_sfps_sensor_props`.
pub const SENSOR_X: i32 = 1600;
pub const SENSOR_Y: i32 = 560;
pub const SENSOR_RADIUS: i32 = 90;

fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    loop {
        // SAFETY: fills a live 8-byte buffer.
        let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), 0) };
        let v = u64::from_ne_bytes(b);
        if n == 8 && v != 0 && v as i64 > 0 {
            return v;
        }
    }
}

fn uptime_ms() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: writes one timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1000 + ts.tv_nsec / 1_000_000
}

/// Lockout state, shared by all sessions (the sensor is one).
#[derive(Default)]
struct Lockout {
    failures: AtomicU32,
    until: Mutex<Option<Instant>>,
}

pub struct Fingerprint {
    root: PathBuf,
    /// Started on first use and held for the process's life (see [`Device`]).
    ta: Arc<OnceLock<Arc<SecureFp>>>,
    device: Arc<Mutex<()>>,
    lockout: Arc<Lockout>,
}

impl Fingerprint {
    pub fn new() -> Fingerprint {
        Fingerprint { root: PathBuf::from(STOREPATH), ta: Arc::default(), device: Arc::default(), lockout: Arc::default() }
    }
}

impl Interface for Fingerprint {}

impl IFingerprint for Fingerprint {
    fn getSensorProps(&self) -> binder::Result<Vec<SensorProps>> {
        Ok(vec![SensorProps {
            commonProps: CommonProps {
                sensorId: SENSOR_ID,
                sensorStrength: SensorStrength::STRONG,
                maxEnrollmentsPerUser: MAX_SLOTS as i32,
                componentInfo: vec![ComponentInfo {
                    componentId: "fingerprintSensor".into(),
                    hardwareVersion: "goodix/gw36t1".into(),
                    firmwareVersion: "securefp".into(),
                    serialNumber: String::new(),
                    softwareVersion: "gts7l-rust".into(),
                }],
            },
            sensorType: FingerprintSensorType::POWER_BUTTON,
            sensorLocations: vec![SensorLocation {
                displayId: 0,
                sensorLocationX: SENSOR_X,
                sensorLocationY: SENSOR_Y,
                sensorRadius: SENSOR_RADIUS,
                ..Default::default()
            }],
            supportsNavigationGestures: false,
            supportsDetectInteraction: false,
            halHandlesDisplayTouches: false,
            halControlsIllumination: false,
            touchDetectionParameters: None,
        }])
    }

    fn createSession(
        &self,
        sensor_id: i32,
        user_id: i32,
        cb: &Strong<dyn ISessionCallback>,
    ) -> binder::Result<Strong<dyn ISession>> {
        info!("createSession sensor {sensor_id} user {user_id}");
        let session = Session {
            user: user_id,
            store: Arc::new(Store::new(&self.root, user_id)),
            cb: cb.clone(),
            ta: self.ta.clone(),
            device: self.device.clone(),
            lockout: self.lockout.clone(),
            challenges: Mutex::default(),
            op: Mutex::default(),
        };
        Ok(BnSession::new_binder(session, BinderFeatures::default()))
    }
}

/// The running operation's cancellation, filled once its thread opened the device.
#[derive(Default)]
struct Op {
    requested: AtomicBool,
    handle: Mutex<Option<CancelHandle>>,
}

impl Op {
    fn cancel(&self) {
        self.requested.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.lock().unwrap().as_ref() {
            h.cancel();
        }
    }
}

struct Cancel(Arc<Op>);

impl Interface for Cancel {}

impl ICancellationSignal for Cancel {
    fn cancel(&self) -> binder::Result<()> {
        self.0.cancel();
        Ok(())
    }
}

struct Session {
    user: i32,
    store: Arc<Store>,
    cb: Strong<dyn ISessionCallback>,
    ta: Arc<OnceLock<Arc<SecureFp>>>,
    device: Arc<Mutex<()>>,
    lockout: Arc<Lockout>,
    challenges: Mutex<HashSet<i64>>,
    /// The latest operation; `close` cancels it.
    op: Mutex<Arc<Op>>,
}

/// Logs a failed callback; the framework may be gone, and nothing else can be done about it.
fn report(r: binder::Result<()>) {
    if let Err(e) = r {
        warn!("callback: {e}");
    }
}

fn error_of(e: &engine::Error) -> Error {
    match e {
        engine::Error::Cancelled => Error::CANCELED,
        engine::Error::Sensor(_) => Error::HW_UNAVAILABLE,
        engine::Error::Ta(_) => Error::UNABLE_TO_PROCESS,
    }
}

/// Remaining captures for `onEnrollmentProgress`, from the TA's counts or its percentage.
fn remaining(p: &EnrollProgress) -> i32 {
    if p.percentage >= 100 {
        return 0;
    }
    if p.total > p.count {
        return (p.total - p.count) as i32;
    }
    if p.count > 0 && p.percentage > 0 {
        let per = p.percentage as f32 / p.count as f32;
        return (((100 - p.percentage) as f32 / per).ceil() as i32).max(1);
    }
    1
}

impl Session {
    /// Starts `body` on a worker thread that owns the device for its whole run.
    fn spawn(
        &self,
        body: impl FnOnce(&mut Device, &dyn ISessionCallback) -> engine::Result<()> + Send + 'static,
    ) -> binder::Result<Strong<dyn ICancellationSignal>> {
        let op = Arc::new(Op::default());
        let prev = std::mem::replace(&mut *self.op.lock().unwrap(), op.clone());
        prev.cancel();
        let (device, cb, user, cancel_op) = (self.device.clone(), self.cb.clone(), self.user, op.clone());
        let ta_cell = self.ta.clone();
        thread::spawn(move || {
            let _owner = device.lock().unwrap_or_else(|e| e.into_inner());
            if op.requested.load(Ordering::SeqCst) {
                report(cb.onError(Error::CANCELED, 0));
                return;
            }
            // Under the device lock, so the TA is started once.
            let ta = match ta_cell.get() {
                Some(ta) => Ok(ta.clone()),
                None => SecureFp::start().map(|ta| ta_cell.get_or_init(|| Arc::new(ta)).clone()).map_err(engine::Error::from),
            };
            let result = ta.and_then(|ta| Device::open(ta, &user.to_string())).and_then(|(mut dev, handle)| {
                *op.handle.lock().unwrap() = Some(handle.clone());
                if op.requested.load(Ordering::SeqCst) {
                    handle.cancel();
                }
                body(&mut dev, &*cb)
            });
            *op.handle.lock().unwrap() = None;
            if let Err(e) = result {
                if !matches!(e, engine::Error::Cancelled) {
                    error!("operation failed: {e}");
                }
                report(cb.onError(error_of(&e), 0));
            }
        });
        Ok(BnCancellationSignal::new_binder(Cancel(cancel_op), BinderFeatures::default()))
    }

    fn locked_out(&self) -> bool {
        let failures = self.lockout.failures.load(Ordering::SeqCst);
        if failures >= LOCKOUT_PERMANENT_AFTER {
            report(self.cb.onLockoutPermanent());
            return true;
        }
        let mut until = self.lockout.until.lock().unwrap();
        match *until {
            Some(t) if t > Instant::now() => {
                report(self.cb.onLockoutTimed((t - Instant::now()).as_millis() as i64));
                true
            }
            Some(_) => {
                *until = None;
                false
            }
            None => false,
        }
    }

    fn do_enroll(&self, hat: &HardwareAuthToken) -> binder::Result<Strong<dyn ICancellationSignal>> {
        let known = hat.challenge != 0 && self.challenges.lock().unwrap().contains(&hat.challenge);
        let slot = self.store.free_slot();
        let (store, sid) = (self.store.clone(), hat.userId as u64);
        let refused = if !known || hat.userId == 0 {
            warn!("enroll: auth token for an unknown challenge or no user");
            Some(Error::UNABLE_TO_PROCESS)
        } else if slot.is_none() {
            Some(Error::NO_SPACE)
        } else {
            None
        };
        if let Some(err) = refused {
            report(self.cb.onError(err, 0));
            let op = Arc::new(Op::default());
            return Ok(BnCancellationSignal::new_binder(Cancel(op), BinderFeatures::default()));
        }
        let slot = slot.unwrap();
        self.spawn(move |dev, cb| {
            let mut on_event = |e: Event| match e {
                Event::FingerDown => {}
                Event::BadImage(why) => report(cb.onAcquired(AcquiredInfo::PARTIAL, why as i32)),
                Event::Progress(p) => {
                    report(cb.onAcquired(AcquiredInfo::GOOD, 0));
                    if p.percentage < 100 {
                        report(cb.onEnrollmentProgress(slot as i32, remaining(&p)));
                    }
                }
                Event::NoMatch(_) => {}
            };
            let template = dev.enroll(&store.ta_user(), slot, &mut on_event)?;
            store.save_template(slot, &template)?;
            store.set_sid(sid)?;
            store.set_authenticator_id(random_u64())?;
            info!("enrolled finger {slot}");
            report(cb.onEnrollmentProgress(slot as i32, 0));
            Ok(())
        })
    }

    fn do_authenticate(&self, operation_id: i64) -> binder::Result<Strong<dyn ICancellationSignal>> {
        let templates = self.store.templates().unwrap_or_default();
        if templates.is_empty() || self.locked_out() {
            if templates.is_empty() {
                report(self.cb.onError(Error::UNABLE_TO_PROCESS, 0));
            }
            let op = Arc::new(Op::default());
            return Ok(BnCancellationSignal::new_binder(Cancel(op), BinderFeatures::default()));
        }
        let (store, lockout) = (self.store.clone(), self.lockout.clone());
        self.spawn(move |dev, cb| {
            let locked = Cell::new(false);
            let mut on_event = |e: Event| match e {
                Event::FingerDown => {}
                Event::BadImage(why) => report(cb.onAcquired(AcquiredInfo::PARTIAL, why as i32)),
                Event::Progress(_) => {}
                Event::NoMatch(_) => {
                    report(cb.onAcquired(AcquiredInfo::GOOD, 0));
                    report(cb.onAuthenticationFailed());
                    let failures = lockout.failures.fetch_add(1, Ordering::SeqCst) + 1;
                    if failures >= LOCKOUT_PERMANENT_AFTER {
                        report(cb.onLockoutPermanent());
                        locked.set(true);
                    } else if failures % LOCKOUT_TIMED_AFTER == 0 {
                        *lockout.until.lock().unwrap() = Some(Instant::now() + LOCKOUT_DURATION);
                        report(cb.onLockoutTimed(LOCKOUT_DURATION.as_millis() as i64));
                        locked.set(true);
                    }
                }
            };
            // A lockout ends the operation: the engine stops at the next cancellation check.
            let result = dev.identify(&store.ta_user(), &templates, operation_id as u64, &mut on_event, &|| locked.get());
            let (m, _updated) = match result {
                Err(engine::Error::Cancelled) if locked.get() => return Ok(()),
                r => r?,
            };
            lockout.failures.store(0, Ordering::SeqCst);
            let hat = HardwareAuthToken {
                challenge: operation_id,
                userId: store.sid() as i64,
                authenticatorId: store.authenticator_id() as i64,
                authenticatorType: HardwareAuthenticatorType::FINGERPRINT,
                timestamp: Timestamp { milliSeconds: uptime_ms() },
                mac: vec![0; 32],
            };
            info!("finger {} matched, score {}", m.bio_idx, m.score);
            report(cb.onAcquired(AcquiredInfo::GOOD, 0));
            report(cb.onAuthenticationSucceeded(m.bio_idx as i32, &hat));
            Ok(())
        })
    }
}

impl Interface for Session {}

#[allow(non_snake_case)]
impl ISession for Session {
    fn generateChallenge(&self) -> binder::Result<()> {
        let challenge = random_u64() as i64;
        self.challenges.lock().unwrap().insert(challenge);
        report(self.cb.onChallengeGenerated(challenge));
        Ok(())
    }

    fn revokeChallenge(&self, challenge: i64) -> binder::Result<()> {
        self.challenges.lock().unwrap().remove(&challenge);
        report(self.cb.onChallengeRevoked(challenge));
        Ok(())
    }

    fn enroll(&self, hat: &HardwareAuthToken) -> binder::Result<Strong<dyn ICancellationSignal>> {
        self.do_enroll(hat)
    }

    fn authenticate(&self, operation_id: i64) -> binder::Result<Strong<dyn ICancellationSignal>> {
        self.do_authenticate(operation_id)
    }

    fn detectInteraction(&self) -> binder::Result<Strong<dyn ICancellationSignal>> {
        // Not offered (supportsDetectInteraction is false).
        report(self.cb.onError(Error::UNABLE_TO_PROCESS, 0));
        let op = Arc::new(Op::default());
        Ok(BnCancellationSignal::new_binder(Cancel(op), BinderFeatures::default()))
    }

    fn enumerateEnrollments(&self) -> binder::Result<()> {
        let ids: Vec<i32> = self.store.slots().into_iter().map(|s| s as i32).collect();
        report(self.cb.onEnrollmentsEnumerated(&ids));
        Ok(())
    }

    fn removeEnrollments(&self, ids: &[i32]) -> binder::Result<()> {
        for &id in ids {
            if let Err(e) = self.store.remove(id as u32) {
                error!("remove {id}: {e}");
                report(self.cb.onError(Error::UNABLE_TO_REMOVE, 0));
                return Ok(());
            }
        }
        if self.store.slots().is_empty() {
            let _ = self.store.set_authenticator_id(0);
        }
        report(self.cb.onEnrollmentsRemoved(ids));
        Ok(())
    }

    fn getAuthenticatorId(&self) -> binder::Result<()> {
        report(self.cb.onAuthenticatorIdRetrieved(self.store.authenticator_id() as i64));
        Ok(())
    }

    fn invalidateAuthenticatorId(&self) -> binder::Result<()> {
        let id = random_u64();
        if let Err(e) = self.store.set_authenticator_id(id) {
            error!("authenticator id: {e}");
        }
        report(self.cb.onAuthenticatorIdInvalidated(id as i64));
        Ok(())
    }

    fn resetLockout(&self, _hat: &HardwareAuthToken) -> binder::Result<()> {
        self.lockout.failures.store(0, Ordering::SeqCst);
        *self.lockout.until.lock().unwrap() = None;
        report(self.cb.onLockoutCleared());
        Ok(())
    }

    fn close(&self) -> binder::Result<()> {
        self.op.lock().unwrap().cancel();
        report(self.cb.onSessionClosed());
        Ok(())
    }

    fn onPointerDown(&self, _: i32, _: i32, _: i32, _: f32, _: f32) -> binder::Result<()> {
        Ok(())
    }

    fn onPointerUp(&self, _: i32) -> binder::Result<()> {
        Ok(())
    }

    fn onUiReady(&self) -> binder::Result<()> {
        Ok(())
    }

    fn authenticateWithContext(
        &self,
        operation_id: i64,
        _: &OperationContext,
    ) -> binder::Result<Strong<dyn ICancellationSignal>> {
        self.do_authenticate(operation_id)
    }

    fn enrollWithContext(
        &self,
        hat: &HardwareAuthToken,
        _: &OperationContext,
    ) -> binder::Result<Strong<dyn ICancellationSignal>> {
        self.do_enroll(hat)
    }

    fn detectInteractionWithContext(&self, _: &OperationContext) -> binder::Result<Strong<dyn ICancellationSignal>> {
        self.detectInteraction()
    }

    fn onPointerDownWithContext(&self, _: &PointerContext) -> binder::Result<()> {
        Ok(())
    }

    fn onPointerUpWithContext(&self, _: &PointerContext) -> binder::Result<()> {
        Ok(())
    }

    fn onContextChanged(&self, _: &OperationContext) -> binder::Result<()> {
        Ok(())
    }

    fn onPointerCancelWithContext(&self, _: &PointerContext) -> binder::Result<()> {
        Ok(())
    }

    fn setIgnoreDisplayTouches(&self, _: bool) -> binder::Result<()> {
        Ok(())
    }
}
