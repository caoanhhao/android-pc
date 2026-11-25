//! IUsbGadget: the framework's USB function choice (adb, MTP, PTP, MIDI, RNDIS/NCM
//! tethering, charging only) applied to the configfs gadget.
//!
//! Until the framework's first request the gadget is the one init built at boot
//! (init.usb.configfs.rc, driven by `sys.usb.config`), so adb works without the framework.
//! The first request takes it over: `vendor.usb.gadget.hal=1` makes the device's init
//! script set `sys.usb.configfs=2`, which turns the stock triggers off (only init and
//! system_server may set that property).
//!
//! A FunctionFS function (adb, MTP, PTP) can bind only once its daemon wrote the
//! descriptors, and the kernel unbinds the whole gadget when such a daemon closes its
//! endpoint 0 (adbd restarting for `adb root`, for one). A monitor thread watches the
//! FunctionFS mounts and binds the gadget again whenever everything it needs is ready.

use std::ffi::CString;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use android_hardware_usb_gadget::aidl::android::hardware::usb::gadget::{
    IUsbGadget::IUsbGadget, IUsbGadgetCallback::IUsbGadgetCallback, Status::Status,
    UsbSpeed::UsbSpeed,
};
use binder::{Interface, Strong};
use log::{error, info, warn};

use crate::configfs::Gadget;
use crate::functions::{self, Plan};
use crate::props;

/// How long a request waits for its FunctionFS daemons before answering anyway; the
/// monitor binds the gadget once they are ready. The framework gives up after 2.5 s.
const FFS_WAIT: Duration = Duration::from_millis(2000);
/// The monitor re-checks this often even without an inotify event.
const MONITOR_INTERVAL: Duration = Duration::from_secs(30);
/// How long the host gets to notice a disconnect before the gadget comes back.
const DISCONNECT_WAIT: Duration = Duration::from_millis(100);

const UDC_CLASS: &str = "/sys/class/udc";

#[derive(Debug, Default)]
struct State {
    /// The framework has taken the gadget over from init.
    taken_over: bool,
    /// The last requested GadgetFunction bits and their plan.
    functions: i64,
    plan: Option<Plan>,
}

/// The IUsbGadget service. Cheap to clone; clones share the state.
#[derive(Clone)]
pub struct UsbGadget {
    gadget: Gadget,
    state: Arc<(Mutex<State>, Condvar)>,
}

impl UsbGadget {
    pub fn new(gadget: Gadget) -> Self {
        Self { gadget, state: Arc::default() }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.0.lock().unwrap()
    }

    /// The UDC to bind to: `sys.usb.controller`, else the only one in /sys/class/udc.
    fn controller(&self) -> Option<String> {
        props::read("sys.usb.controller").or_else(|| {
            let entries = std::fs::read_dir(UDC_CLASS).ok()?;
            entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).next()
        })
    }

    /// Takes the gadget over from init's `sys.usb.config` triggers.
    fn take_over(&self, state: &mut State) {
        if state.taken_over {
            return;
        }
        props::write("vendor.usb.gadget.hal", "1");
        if !props::wait_for("sys.usb.configfs", "2", Duration::from_secs(1)) {
            warn!("sys.usb.configfs did not become 2; init's USB triggers may still fire");
        }
        state.taken_over = true;
        info!("took the gadget over from init");
    }

    /// Binds the gadget if the current plan has functions and all of its FunctionFS
    /// daemons are ready. Returns whether the gadget is bound afterwards.
    fn bind_if_ready(&self, state: &State) -> bool {
        let Some(plan) = state.plan.as_ref().filter(|p| !p.is_empty()) else { return false };
        if self.gadget.udc().is_some() {
            return true;
        }
        if plan.ffs().any(|ffs| !self.gadget.ffs_ready(ffs)) {
            return false;
        }
        let Some(udc) = self.controller() else {
            error!("no UDC");
            return false;
        };
        match self.gadget.bind(&udc) {
            Ok(()) => {
                info!("bound {} to {udc}", plan.config_name);
                true
            }
            Err(err) => {
                error!("bind: {err}");
                false
            }
        }
    }

    /// Applies `functions`; the status for the framework's callback.
    fn set_functions(&self, functions: i64, timeout: Duration) -> Status {
        let acm = props::read_bool("persist.vendor.usb.gadget.acm", false);
        let plan = match functions::plan(functions, acm) {
            Ok(plan) => plan,
            Err(functions::Unsupported(bits)) => {
                warn!("functions {functions:#x}: {bits:#x} not supported");
                return Status::CONFIGURATION_NOT_SUPPORTED;
            }
        };

        let mut state = self.lock();
        self.take_over(&mut state);
        if let Err(err) = self.gadget.unbind() {
            error!("unbind: {err}");
            return Status::ERROR;
        }
        thread::sleep(DISCONNECT_WAIT);
        state.functions = functions;
        state.plan = None;
        if let Err(err) = self.gadget.apply(&plan) {
            error!("{}: {err}", plan.config_name);
            return Status::ERROR;
        }
        info!("functions {functions:#x}: {} (0x{:04x})", plan.config_name, plan.id_product);
        state.plan = Some(plan);
        self.state.1.notify_all();

        // Wait for the FunctionFS daemons (adbd after `adb root`, ...), then answer anyway:
        // the monitor binds when they show up.
        let deadline = Instant::now() + timeout.min(FFS_WAIT);
        while !self.bind_if_ready(&state) && !state.plan.as_ref().is_some_and(Plan::is_empty) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let plan = state.plan.as_ref();
                let missing: Vec<_> =
                    plan.into_iter().flat_map(Plan::ffs).filter(|f| !self.gadget.ffs_ready(f)).collect();
                warn!("FunctionFS {missing:?} not ready; binding when they are");
                break;
            }
            state = self.state.1.wait_timeout(state, left.min(Duration::from_millis(100))).unwrap().0;
        }
        Status::SUCCESS
    }

    /// The functions now in effect and whether they are applied (the gadget is bound).
    fn current(&self) -> (i64, bool) {
        let state = self.lock();
        let bound = self.gadget.udc().is_some();
        if state.taken_over {
            (state.functions, bound || state.plan.as_ref().is_some_and(Plan::is_empty))
        } else {
            let linked = self.gadget.linked();
            (functions::from_linked(linked.iter().map(String::as_str)), bound)
        }
    }

    fn speed(&self) -> UsbSpeed {
        let Some(udc) = self.gadget.udc() else { return UsbSpeed::UNKNOWN };
        match self.gadget.speed(&udc, std::path::Path::new(UDC_CLASS)).as_deref() {
            Some("low-speed") => UsbSpeed::LOWSPEED,
            Some("full-speed") => UsbSpeed::FULLSPEED,
            Some("high-speed") => UsbSpeed::HIGHSPEED,
            Some("super-speed") => UsbSpeed::SUPERSPEED,
            Some("super-speed-plus") => UsbSpeed::SUPERSPEED_10Gb,
            _ => UsbSpeed::UNKNOWN,
        }
    }

    /// Starts the thread that binds the gadget again after a FunctionFS daemon restarts.
    pub fn spawn_monitor(&self) {
        let this = self.clone();
        let spawned = thread::Builder::new().name("ffs-monitor".into()).spawn(move || this.monitor());
        if let Err(err) = spawned {
            error!("cannot start the FunctionFS monitor: {err}");
        }
    }

    fn monitor(&self) {
        let inotify = match Inotify::new() {
            Ok(inotify) => Some(inotify),
            Err(err) => {
                warn!("inotify: {err}; checking every {MONITOR_INTERVAL:?}");
                None
            }
        };
        let mut watched = Vec::<PathBuf>::new();
        loop {
            if let Some(inotify) = &inotify {
                for dir in self.gadget.ffs_dirs() {
                    if !watched.contains(&dir) {
                        match inotify.watch(&dir) {
                            Ok(()) => watched.push(dir),
                            Err(err) => warn!("watch {}: {err}", dir.display()),
                        }
                    }
                }
                inotify.wait(MONITOR_INTERVAL);
            } else {
                thread::sleep(MONITOR_INTERVAL);
            }
            let state = self.lock();
            if state.taken_over {
                self.bind_if_ready(&state);
                self.state.1.notify_all();
            }
        }
    }
}

impl Interface for UsbGadget {}

#[allow(non_snake_case)]
impl IUsbGadget for UsbGadget {
    fn setCurrentUsbFunctions(
        &self,
        functions: i64,
        callback: &Strong<dyn IUsbGadgetCallback>,
        timeoutMs: i64,
        transactionId: i64,
    ) -> binder::Result<()> {
        let timeout = Duration::from_millis(timeoutMs.max(0) as u64);
        let status = self.set_functions(functions, timeout);
        callback.setCurrentUsbFunctionsCb(functions, status, transactionId)
    }

    fn getCurrentUsbFunctions(
        &self,
        callback: &Strong<dyn IUsbGadgetCallback>,
        transactionId: i64,
    ) -> binder::Result<()> {
        let (functions, applied) = self.current();
        let status = if applied { Status::FUNCTIONS_APPLIED } else { Status::FUNCTIONS_NOT_APPLIED };
        callback.getCurrentUsbFunctionsCb(functions, status, transactionId)
    }

    fn getUsbSpeed(&self, callback: &Strong<dyn IUsbGadgetCallback>, transactionId: i64) -> binder::Result<()> {
        callback.getUsbSpeedCb(self.speed(), transactionId)
    }

    fn reset(&self, callback: &Strong<dyn IUsbGadgetCallback>, transactionId: i64) -> binder::Result<()> {
        let status = {
            let state = self.lock();
            match self.gadget.unbind() {
                Ok(()) => {
                    thread::sleep(DISCONNECT_WAIT);
                    if state.taken_over {
                        self.bind_if_ready(&state);
                    }
                    Status::SUCCESS
                }
                Err(err) => {
                    error!("reset: {err}");
                    Status::ERROR
                }
            }
        };
        callback.resetCb(status, transactionId)
    }
}

/// An inotify instance watching directories for files created and deleted.
struct Inotify {
    fd: OwnedFd,
}

impl Inotify {
    fn new() -> std::io::Result<Self> {
        // SAFETY: inotify_init1 takes no pointers; the result is checked.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fd was just created and is owned by nothing else.
        Ok(Self { fd: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    fn watch(&self, dir: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(dir.as_os_str().as_bytes())?;
        let mask = libc::IN_CREATE | libc::IN_DELETE | libc::IN_MOVED_TO;
        // SAFETY: path is a valid NUL-terminated string for the duration of the call.
        if unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), path.as_ptr(), mask) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Waits up to `timeout` for events and discards them; the caller re-reads the state.
    fn wait(&self, timeout: Duration) {
        if !matches!(pchal::poll::readable(self.fd.as_fd(), timeout), Ok(true)) {
            return;
        }
        let mut buf = [0_u8; 4096];
        // SAFETY: buf is valid for writes of its length. The fd is non-blocking.
        while unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
}
