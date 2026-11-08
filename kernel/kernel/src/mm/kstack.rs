//! Kernel stacks for tasks: mapped in the kernel half with an unmapped
//! guard page below each, so an overflow faults instead of corrupting
//! memory.
//!
//! Virtual slots are never reused, so a freed stack needs no TLB shootdown:
//! a stale translation on another CPU can only be reached through an
//! address nobody uses again (2^40 bytes of slots are plenty).

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::VirtAddr;
use x86_64::structures::paging::PageTableFlags as F;

use super::{PAGE_SIZE, frame, paging};

pub const STACK_SIZE: u64 = 64 * 1024;
const SLOT_SIZE: u64 = STACK_SIZE + PAGE_SIZE;
const AREA_START: u64 = 0xffff_e000_1000_0000;
const AREA_END: u64 = 0xffff_f000_0000_0000;

static NEXT_SLOT: AtomicU64 = AtomicU64::new(AREA_START);

pub struct KernelStack {
    base: u64,
}

impl KernelStack {
    pub fn new() -> Option<Self> {
        let slot = NEXT_SLOT.fetch_add(SLOT_SIZE, Ordering::Relaxed);
        assert!(slot + SLOT_SIZE <= AREA_END, "kernel stack area exhausted");
        let base = slot + PAGE_SIZE;
        for off in (0..STACK_SIZE).step_by(PAGE_SIZE as usize) {
            let Some(f) = frame::alloc() else {
                for done in (0..off).step_by(PAGE_SIZE as usize) {
                    frame::free(paging::unmap_page(VirtAddr::new(base + done)));
                }
                return None;
            };
            paging::map_page(
                VirtAddr::new(base + off),
                f,
                F::PRESENT | F::WRITABLE | F::NO_EXECUTE | F::GLOBAL,
            );
        }
        Some(Self { base })
    }

    /// 16-byte aligned initial stack pointer.
    pub fn top(&self) -> u64 {
        self.base + STACK_SIZE
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        for off in (0..STACK_SIZE).step_by(PAGE_SIZE as usize) {
            frame::free(paging::unmap_page(VirtAddr::new(self.base + off)));
        }
    }
}

#[test_case]
fn kernel_stack_alloc_free() {
    let s = KernelStack::new().unwrap();
    let (_, free_before) = frame::stats();
    let p = (s.top() - 8) as *mut u64;
    unsafe { p.write_volatile(0x1234) };
    assert_eq!(unsafe { p.read_volatile() }, 0x1234);
    // The guard page below the stack is not mapped.
    assert_eq!(paging::translate(VirtAddr::new(s.top() - STACK_SIZE - 8)), None);
    drop(s);
    assert_eq!(frame::stats().1, free_before + (STACK_SIZE / PAGE_SIZE) as usize);
}
