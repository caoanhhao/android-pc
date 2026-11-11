//! Shared building blocks for the android-pc vendor HALs written in Rust.
//!
//! A HAL binary typically:
//!
//! 1. calls [`init_logging`] with its log tag,
//! 2. probes its hardware through [`sysfs`] or [`devfs`],
//! 3. wraps its AIDL implementation in a binder object and hands it to
//!    [`Service::serve`], which registers it with servicemanager and serves calls.
//!
//! Client callbacks (`I*Callback` interfaces) are kept in [`Callbacks`], hotplug and
//! state changes arrive through [`uevent`], and [`procstat`] measures CPU usage.
//!
//! Failures are reported as [`Error`], which converts into a [`binder::Status`] so AIDL
//! methods can use `?` on library calls.

mod callbacks;
pub mod devfs;
mod error;
mod logging;
pub mod procstat;
mod service;
pub mod sysfs;
pub mod uevent;

pub use callbacks::Callbacks;
pub use error::{Error, Result};
pub use logging::init_logging;
pub use service::{Registration, Service};
