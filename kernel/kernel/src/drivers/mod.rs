//! Device drivers.
//!
//! Now: the COM1 serial console. Planned: virtio (blk, net, gpu, input,
//! console) over PCI, then the Android character devices (binder, ashmem /
//! memfd backing, dma-buf heaps, sync_file) which live here as well.

#[macro_use]
pub mod serial;
