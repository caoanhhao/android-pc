//! Device drivers.
//!
//! Now: the COM1 serial console, binder (+ binderfs), the PCI bus,
//! virtio-blk, virtio-net, virtio-gpu (with DRM/KMS on top; without
//! one, the boot framebuffer drives the same DRM device) and
//! virtio-input (with evdev and uinput on top), dma-bufs, the system
//! dma-heap and sync files; real-hardware-class controllers: NVMe and
//! AHCI disks, xHCI USB with HID boot keyboards and mice, mass storage,
//! UVC cameras (V4L2) and UAC microphones (ALSA capture), the Realtek
//! RTL8125 2.5 GbE card; HD Audio (ALSA PCM and control devices); PC
//! platform devices (CMOS RTC, ACPI power button, CPU thermal/cpufreq,
//! AC power); Bluetooth controllers on xHCI USB (`bluetooth`, on
//! /dev/hvc5 for Android's HAL); a virtio-console port as an H4
//! Bluetooth link (tests). Planned: more
//! virtio (console).

#[macro_use]
pub mod serial;
pub mod ahci;
pub mod ashmem;
pub mod binder;
pub mod bluetooth;
pub mod dma_buf;
pub mod drm;
pub mod fbcon;
mod font;
pub mod hvc;
pub mod firmware;
pub mod input;
pub mod iwlwifi;
pub mod media;
pub mod nvme;
pub mod pci;
pub mod rfkill;
pub mod platform;
pub mod rtl8125;
pub mod sound;
pub mod usb;
pub mod virtio;

/// Probe buses and devices (after the VFS exists, so device nodes can be
/// created).
pub fn init() {
    crate::net::inet::init();
    dma_buf::init();
    platform::init();
    pci::init();
    virtio::init();
    rtl8125::init();
    nvme::init();
    ahci::init();
    // Sound before USB: HD Audio is card 0, USB microphones follow.
    sound::init();
    usb::init();
    media::init();
    drm::register_boot_framebuffer();
    crate::net::inet::configure_static();
    // Wi-Fi links after the Ethernet ones (configure_static picks the
    // last Ethernet card).
    iwlwifi::init();
    crate::net::wireless::init();
}
