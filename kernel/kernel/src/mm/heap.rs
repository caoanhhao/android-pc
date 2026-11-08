//! Kernel heap (`alloc`): a linked-list allocator over one physically
//! contiguous block accessed through the direct map.
//!
//! Not interrupt-safe: interrupt handlers must not allocate.
//! TODO: grow on demand and replace with a slab allocator for hot sizes.

use linked_list_allocator::LockedHeap;

use super::{PAGE_SIZE, frame, paging::phys_to_virt};

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

const MIN_HEAP: usize = 4 << 20;
const MAX_HEAP: usize = 64 << 20;

pub fn init() {
    let (_, free) = frame::stats();
    let size =
        (free * PAGE_SIZE as usize / 8).clamp(MIN_HEAP, MAX_HEAP) & !(PAGE_SIZE as usize - 1);
    let phys = frame::alloc_contiguous(size / PAGE_SIZE as usize, 512)
        .expect("no contiguous memory for the kernel heap");
    let virt = phys_to_virt(phys);
    unsafe { ALLOCATOR.lock().init(virt.as_mut_ptr(), size) };
    log::info!("heap: {} MiB at {:#x}", size >> 20, virt.as_u64());
}

#[test_case]
fn box_and_vec() {
    use alloc::{boxed::Box, vec::Vec};
    let b = Box::new(41u64);
    assert_eq!(*b + 1, 42);
    let v: Vec<u64> = (0..10_000).collect();
    assert_eq!(v.iter().sum::<u64>(), 10_000 * 9_999 / 2);
}

#[test_case]
fn large_allocation() {
    use alloc::vec;
    let v = vec![0xa5u8; 1 << 20];
    assert!(v.iter().all(|&b| b == 0xa5));
}
