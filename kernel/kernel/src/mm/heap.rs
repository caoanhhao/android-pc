//! Kernel heap (`alloc`): small allocations come from per-size-class free
//! lists (slabs carved out of 16 KiB chunks, never returned), larger ones
//! from a linked-list allocator over one physically contiguous block
//! accessed through the direct map. The linked list costs O(free blocks)
//! per call under one lock, which made allocation-heavy paths (path
//! lookups, binder) dominate kernel time once system_server ran; a size
//! class is O(1) under its own lock.
//!
//! Interrupts are disabled while a heap lock is held, so interrupt
//! handlers (timer wake-ups pushing to the run queue) may allocate.
//! TODO: grow on demand.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use linked_list_allocator::LockedHeap;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

use super::{PAGE_SIZE, frame, paging::phys_to_virt};

/// Block sizes of the size classes (powers of two: a block is aligned to
/// its size, the chunks to 4 KiB).
const CLASSES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const CHUNK: usize = 16 << 10;

struct Class {
    /// Free blocks, linked through their first word.
    free: *mut u8,
    /// Blocks handed out (for /proc/meminfo).
    used: usize,
}

unsafe impl Send for Class {}

struct IrqSafeHeap {
    big: LockedHeap,
    classes: [Mutex<Class>; CLASSES.len()],
}

fn class_of(layout: &Layout) -> Option<usize> {
    let size = layout.size().max(layout.align());
    CLASSES.iter().position(|&c| size <= c)
}

impl IrqSafeHeap {
    /// One block of class `i`, carving a new chunk from the big heap when
    /// the free list is empty.
    unsafe fn class_alloc(&self, i: usize) -> *mut u8 {
        let mut c = self.classes[i].lock();
        if c.free.is_null() {
            let chunk = unsafe { self.big.alloc(Layout::from_size_align(CHUNK, PAGE_SIZE as usize).unwrap()) };
            if chunk.is_null() {
                return null_mut();
            }
            let size = CLASSES[i];
            for off in (0..CHUNK).step_by(size).rev() {
                let b = unsafe { chunk.add(off) };
                unsafe { (b as *mut *mut u8).write(c.free) };
                c.free = b;
            }
        }
        let b = c.free;
        c.free = unsafe { (b as *mut *mut u8).read() };
        c.used += 1;
        b
    }
}

unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| match class_of(&layout) {
            Some(i) => unsafe { self.class_alloc(i) },
            None => unsafe { self.big.alloc(layout) },
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| match class_of(&layout) {
            Some(i) => {
                let mut c = self.classes[i].lock();
                unsafe { (ptr as *mut *mut u8).write(c.free) };
                c.free = ptr;
                c.used -= 1;
            }
            None => unsafe { self.big.dealloc(ptr, layout) },
        })
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Staying inside the same block: nothing to do.
        let new = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        if let (Some(a), Some(b)) = (class_of(&layout), class_of(&new))
            && a == b
        {
            return ptr;
        }
        let p = unsafe { self.alloc(new) };
        if !p.is_null() {
            unsafe { core::ptr::copy_nonoverlapping(ptr, p, layout.size().min(new_size)) };
            unsafe { self.dealloc(ptr, layout) };
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: IrqSafeHeap = IrqSafeHeap {
    big: LockedHeap::empty(),
    classes: [const { Mutex::new(Class { free: null_mut(), used: 0 }) }; CLASSES.len()],
};

const MIN_HEAP: usize = 4 << 20;
const MAX_HEAP: usize = 256 << 20;

pub fn init() {
    let (_, free) = frame::stats();
    let size =
        (free * PAGE_SIZE as usize / 8).clamp(MIN_HEAP, MAX_HEAP) & !(PAGE_SIZE as usize - 1);
    let phys = frame::alloc_contiguous(size / PAGE_SIZE as usize, 512)
        .expect("no contiguous memory for the kernel heap");
    let virt = phys_to_virt(phys);
    unsafe { ALLOCATOR.big.lock().init(virt.as_mut_ptr(), size) };
    log::info!("heap: {} MiB at {:#x}", size >> 20, virt.as_u64());
}

/// (bytes in use, heap size) of the kernel heap.
pub fn stats() -> (usize, usize) {
    without_interrupts(|| {
        let h = ALLOCATOR.big.lock();
        // Chunks carved into size classes count as used.
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
