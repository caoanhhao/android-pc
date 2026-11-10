//! Media devices (E8): V4L2 video capture nodes ([`v4l2`]), fed by USB
//! video class cameras (`drivers::usb::uvc`) or, with `testcam=1` on the
//! kernel command line, a built-in test camera ([`testdev`]) that sends
//! the same UVC payloads, and a test microphone (a capture card of
//! `drivers::sound`).

pub mod testdev;
pub mod v4l2;

pub use v4l2::VIDEO_MAJOR;

/// Register the test devices if asked for (after the USB devices, so a
/// real camera is video0 and a real microphone card 0).
pub fn init() {
    if crate::boot::cmdline().split_ascii_whitespace().any(|w| w == "testcam=1") {
        testdev::register();
    }
}
