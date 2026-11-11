//! Shared building blocks for the android-pc vendor HALs written in Rust.
//!
//! A HAL binary typically:
//!
//! 1. calls [`init_logging`] with its log tag,
//! 2. probes its hardware through [`sysfs`] or [`devfs`],
//! 3. wraps its AIDL implementation in a binder object and hands it to
//!    [`Service::serve`], which registers it with servicemanager and serves calls.
//!
//! Failures are reported as [`Error`], which converts into a [`binder::Status`] so AIDL
//! methods can use `?` on library calls.

pub mod devfs;
mod error;
mod logging;
mod service;
pub mod sysfs;

pub use error::{Error, Result};
pub use logging::init_logging;
pub use service::{Registration, Service};
