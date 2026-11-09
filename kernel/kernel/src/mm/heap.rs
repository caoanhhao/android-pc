//! Kernel heap (`alloc`): a linked-list allocator over one physically
//! contiguous block accessed through the direct map.
//!
//! Interrupts are disabled while the heap lock is held, so interrupt
//! handlers (timer wake-ups pushing to the run queue) may allocate.
//! TODO: grow on demand and replace with a slab allocator for hot sizes.

use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;
use x86_64::instructions::interrupts::without_interrupts;

use super::{PAGE_SIZE, frame, paging::phys_to_virt};

struct IrqSafeHeap(LockedHeap);

unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| unsafe { self.0.alloc(layout) })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

#[global_allocator]
static ALLOCATOR: IrqSafeHeap = IrqSafeHeap(LockedHeap::empty());

const MIN_HEAP: usize = 4 << 20;
const MAX_HEAP: usize = 64 << 20;

pub fn init() {
    let (_, free) = frame::stats();
    let size =
        (free * PAGE_SIZE as usize / 8).clamp(MIN_HEAP, MAX_HEAP) & !(PAGE_SIZE as usize - 1);
    let phys = frame::alloc_contiguous(size / PAGE_SIZE as usize, 512)
        .expect("no contiguous memory for the kernel heap");
    let virt = phys_to_virt(phys);
    unsafe { ALLOCATOR.0.lock().init(virt.as_mut_ptr(), size) };
    log::info!("heap: {} MiB at {:#x}", size >> 20, virt.as_u64());
}

/// (bytes in use, heap size) of the kernel heap.
pub fn stats() -> (usize, usize) {
    without_interrupts(|| {
        let h = ALLOCATOR.0.lock();
        (h.used(), h.size())
    })
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
