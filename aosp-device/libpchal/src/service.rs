//! Registering a binder service with servicemanager and serving its calls.

use std::convert::Infallible;

use binder::{ProcessState, SpIBinder};

use crate::{Error, Result};

/// How servicemanager manages the service process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Registration {
    /// init starts the service at boot and it runs until shutdown.
    #[default]
    Persistent,
    /// servicemanager starts the service on the first client request and init stops it
    /// once it has no clients. The init `.rc` entry needs `interface aidl <name>`,
    /// `oneshot` and `disabled`.
    Lazy,
}

/// A binder service instance waiting to be registered.
///
/// ```no_run
/// # fn run(binder: binder::SpIBinder) -> pchal::Result<()> {
/// let Err(err) = pchal::Service::new("android.hardware.light.ILights", "default").serve(binder);
/// # Err(err) }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    name: String,
    registration: Registration,
    extra_threads: u32,
}

impl Service {
    /// Describes the instance `<descriptor>/<instance>`, for example
    /// `android.hardware.light.ILights/default`, as named in the VINTF manifest.
    pub fn new(descriptor: &str, instance: &str) -> Self {
        Self {
            name: format!("{descriptor}/{instance}"),
            registration: Registration::default(),
            extra_threads: 0,
        }
    }

    /// The full instance name registered with servicemanager.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sets how the service is started and stopped.
    #[must_use]
    pub fn registration(mut self, registration: Registration) -> Self {
        self.registration = registration;
        self
    }

    /// Serves calls on `threads` binder threads in addition to the main thread.
    ///
    /// The default of 0 handles every call on the main thread, one at a time.
    #[must_use]
    pub fn extra_threads(mut self, threads: u32) -> Self {
        self.extra_threads = threads;
        self
    }

    /// Registers `binder` under this name and serves binder calls on the calling thread.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Register`] if servicemanager refuses the registration (usually a
    /// missing VINTF manifest entry or SELinux `add` permission), and
    /// [`Error::ThreadPoolExited`] if the binder thread pool ever stops. It never returns
    /// otherwise.
    pub fn serve(self, binder: SpIBinder) -> Result<Infallible> {
        ProcessState::set_thread_pool_max_thread_count(self.extra_threads);
        let registered = match self.registration {
            Registration::Persistent => binder::add_service(&self.name, binder),
            Registration::Lazy => binder::register_lazy_service(&self.name, binder),
        };
        registered.map_err(|status| Error::Register { name: self.name.clone(), status })?;
        log::info!("registered {} ({:?})", self.name, self.registration);

        if self.extra_threads > 0 {
            ProcessState::start_thread_pool();
        }
        ProcessState::join_thread_pool();
        Err(Error::ThreadPoolExited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_joins_descriptor_and_instance() {
        let service = Service::new("android.hardware.light.ILights", "default");
        assert_eq!(service.name(), "android.hardware.light.ILights/default");
        assert_eq!(service.registration, Registration::Persistent);
    }
}
