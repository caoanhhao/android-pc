//! Device drivers.
//!
//! Now: the COM1 serial console and binder (+ binderfs). Planned: virtio
//! (blk, net, gpu, input, console) over PCI, then the other Android
//! character devices (ashmem / memfd backing, dma-buf heaps, sync_file).

#[macro_use]
pub mod serial;
pub mod binder;
