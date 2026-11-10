//! Device drivers.
//!
//! Now: the COM1 serial console, binder (+ binderfs), the PCI bus,
//! virtio-blk, virtio-net, virtio-gpu (with DRM/KMS on top; without
//! one, the boot framebuffer drives the same DRM device) and
//! virtio-input (with evdev and uinput on top), dma-bufs, the system
//! dma-heap and sync files; real-hardware-class controllers: NVMe and
//! AHCI disks, xHCI USB with HID boot keyboards and mice, the Realtek
//! RTL8125 2.5 GbE card; HD Audio (ALSA PCM and control devices).
//! Planned: more
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
pub mod nvme;
pub mod pci;
pub mod rtl8125;
pub mod sound;
pub mod usb;
pub mod virtio;

/// Probe buses and devices (after the VFS exists, so device nodes can be
/// created).
pub fn init() {
    crate::net::inet::init();
    dma_buf::init();
    pci::init();
    virtio::init();
    rtl8125::init();
    nvme::init();
    ahci::init();
    usb::init();
    sound::init();
    drm::register_boot_framebuffer();
    crate::net::inet::configure_static();
}
