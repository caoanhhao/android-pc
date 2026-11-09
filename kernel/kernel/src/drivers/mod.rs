//! Device drivers.
//!
//! Now: the COM1 serial console, binder (+ binderfs), the PCI bus,
//! virtio-blk and virtio-net. Planned: more virtio (gpu, input, console), then the
//! other Android character devices (dma-buf heaps, sync_file).

#[macro_use]
pub mod serial;
pub mod binder;
pub mod pci;
pub mod virtio;

/// Probe buses and devices (after the VFS exists, so device nodes can be
/// created).
pub fn init() {
    crate::net::inet::init();
    pci::init();
    virtio::init();
    crate::net::inet::configure_static();
}
