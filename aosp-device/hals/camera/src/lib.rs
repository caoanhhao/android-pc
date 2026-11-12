//! The hardware-independent half of the android-pc camera HAL: V4L2 capture, image
//! conversion, JPEG, camera metadata and the camera description Android sees.
//!
//! Everything here builds and is unit-tested on the host; the binder side (provider,
//! device, session) and gralloc live in the HAL binary.

pub mod image;
pub mod jpeg;
pub mod metadata;
pub mod sensor;
pub mod tags;
pub mod v4l2;
