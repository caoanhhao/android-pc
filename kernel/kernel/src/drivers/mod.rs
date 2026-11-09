//! Device drivers.
//!
//! Now: the COM1 serial console, binder (+ binderfs), the PCI bus,
//! virtio-blk, virtio-net, virtio-gpu (with DRM/KMS on top) and
//! virtio-input (with evdev and uinput on top), dma-bufs, the system
//! dma-heap and sync files. Planned: more virtio (console).

#[macro_use]
pub mod serial;
pub mod binder;
pub mod dma_buf;
pub mod drm;
pub mod input;
pub mod pci;
pub mod virtio;

/// Probe buses and devices (after the VFS exists, so device nodes can be
/// created).
pub fn init() {
    crate::net::inet::init();
    dma_buf::init();
    pci::init();
    virtio::init();
    crate::net::inet::configure_static();
}
