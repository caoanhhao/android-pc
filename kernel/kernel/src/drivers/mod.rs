//! Device drivers.
//!
//! Now: the COM1 serial console, binder (+ binderfs), the PCI bus and
//! virtio-blk. Planned: more virtio (net, gpu, input, console), then the
//! other Android character devices (dma-buf heaps, sync_file).

#[macro_use]
pub mod serial;
pub mod binder;
pub mod pci;
pub mod virtio;

/// Probe buses and devices (after the VFS exists, so device nodes can be
/// created).
pub fn init() {
    pci::init();
    virtio::init();
}
