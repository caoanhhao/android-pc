//! Device drivers.
//!
//! Now: the COM1 serial console, binder (+ binderfs), the PCI bus,
//! virtio-blk, virtio-net, virtio-gpu (with DRM/KMS on top; without
//! one, the boot framebuffer drives the same DRM device) and
//! virtio-input (with evdev and uinput on top), dma-bufs, the system
//! dma-heap and sync files; real-hardware-class controllers: NVMe and
//! AHCI disks, xHCI USB with HID boot keyboards and mice, mass storage,
//! UVC cameras (V4L2) and UAC microphones (ALSA capture). Planned: more
//! virtio (console).

#[macro_use]
pub mod serial;
pub mod ahci;
pub mod ashmem;
pub mod binder;
pub mod dma_buf;
pub mod drm;
pub mod fbcon;
mod font;
pub mod hvc;
pub mod input;
pub mod media;
pub mod nvme;
pub mod pci;
pub mod usb;
pub mod virtio;

/// Probe buses and devices (after the VFS exists, so device nodes can be
/// created).
pub fn init() {
    crate::net::inet::init();
    dma_buf::init();
    pci::init();
    virtio::init();
    nvme::init();
    ahci::init();
    usb::init();
    media::init();
    drm::register_boot_framebuffer();
    crate::net::inet::configure_static();
}
