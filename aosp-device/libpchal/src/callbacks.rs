//! Client callbacks registered with a HAL, such as `IHealthInfoCallback`.

use std::sync::Mutex;

use binder::{ExceptionCode, FromIBinder, SpIBinder, Status, StatusCode, Strong};

/// The callbacks clients registered, each with a filter `F` (for example the temperature
/// type a thermal callback asked for; `()` when there is none).
///
/// A callback is identified by its binder object, so registering the same object twice
/// fails, as the AIDL HAL contracts require. Callbacks whose process died are dropped the
/// next time [`Callbacks::notify`] reaches them.
pub struct Callbacks<I: FromIBinder + ?Sized, F = ()> {
    entries: Mutex<Vec<Entry<I, F>>>,
}

struct Entry<I: FromIBinder + ?Sized, F> {
    binder: SpIBinder,
    callback: Strong<I>,
    filter: F,
}

impl<I: FromIBinder + ?Sized, F: Clone> Callbacks<I, F> {
    /// An empty list.
    pub fn new() -> Self {
        Self { entries: Mutex::new(Vec::new()) }
    }

    /// The number of registered callbacks.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no callback is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Registers `callback` with `filter`.
    ///
    /// # Errors
    ///
    /// Returns `EX_ILLEGAL_ARGUMENT` if the same binder object is already registered.
    pub fn add(&self, callback: &Strong<I>, filter: F) -> binder::Result<()> {
        let binder = callback.as_binder();
        let mut entries = self.lock();
        if entries.iter().any(|entry| entry.binder == binder) {
            return Err(illegal_argument("callback already registered"));
        }
        entries.push(Entry { binder, callback: callback.clone(), filter });
        Ok(())
    }

    /// Unregisters `callback`.
    ///
    /// # Errors
    ///
    /// Returns `EX_ILLEGAL_ARGUMENT` if it was not registered.
    pub fn remove(&self, callback: &Strong<I>) -> binder::Result<()> {
        let binder = callback.as_binder();
        let mut entries = self.lock();
        let len = entries.len();
        entries.retain(|entry| entry.binder != binder);
        if entries.len() == len {
            return Err(illegal_argument("callback not registered"));
        }
        Ok(())
    }

    /// Calls `call` for every callback whose filter `wants` accepts, outside the lock.
    ///
    /// Callbacks that fail with `DEAD_OBJECT` are unregistered; other failures are
    /// logged and the callback stays.
    pub fn notify(&self, wants: impl Fn(&F) -> bool, call: impl Fn(&I) -> binder::Result<()>) {
        // Strong and SpIBinder clones only take a reference, so the list is copied out
        // and no client call runs under the lock.
        let targets: Vec<(SpIBinder, Strong<I>)> = self
            .lock()
            .iter()
            .filter(|entry| wants(&entry.filter))
            .map(|entry| (entry.binder.clone(), entry.callback.clone()))
            .collect();

        let mut dead = Vec::new();
        for (binder, callback) in targets {
            match call(&callback) {
                Ok(()) => {}
                Err(status) if status.transaction_error() == StatusCode::DEAD_OBJECT => {
                    dead.push(binder);
                }
                Err(status) => log::warn!("callback failed: {status:?}"),
            }
        }
        if !dead.is_empty() {
            log::info!("dropping {} dead callback(s)", dead.len());
            self.lock().retain(|entry| !dead.contains(&entry.binder));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry<I, F>>> {
        self.entries.lock().expect("callback list lock poisoned")
    }
}

impl<I: FromIBinder + ?Sized, F: Clone> Default for Callbacks<I, F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: FromIBinder + ?Sized, F> std::fmt::Debug for Callbacks<I, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.entries.lock().map(|entries| entries.len()).unwrap_or_default();
        f.debug_struct("Callbacks").field("len", &len).finish()
    }
}

fn illegal_argument(message: &str) -> Status {
    Status::new_exception_str(ExceptionCode::ILLEGAL_ARGUMENT, Some(message))
}
