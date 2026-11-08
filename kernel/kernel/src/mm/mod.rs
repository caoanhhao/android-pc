//! Memory management: physical frames, kernel page tables, kernel heap,
//! kernel stacks, user address spaces and shmem objects (the page cache of
//! RAM-backed files, also behind MAP_SHARED anonymous memory).

pub mod frame;
pub mod heap;
pub mod kstack;
pub mod paging;
pub mod shmem;
pub mod user;

use crate::boot;

pub const PAGE_SIZE: u64 = 4096;

pub fn init() {
    let memmap = boot::memory_map();
    paging::set_hhdm_offset(boot::hhdm_offset());

    log::info!("memory map ({} entries):", memmap.len());
    for e in memmap {
        log::info!(
            "  {:#014x}-{:#014x} {:>10} KiB  {}",
            e.base,
            e.base + e.length,
            e.length / 1024,
            boot::memmap_type_name(e.type_)
        );
    }

    frame::init(memmap);
    paging::init(memmap);
    heap::init();

    let (total, free) = frame::stats();
    log::info!(
        "memory: {} MiB usable, {} MiB free after heap",
        (total * PAGE_SIZE as usize) >> 20,
        (free * PAGE_SIZE as usize) >> 20
    );
}
