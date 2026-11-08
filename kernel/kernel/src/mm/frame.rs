//! Physical frame allocator: one bit per 4 KiB frame (1 = in use), plus a
//! reference count per frame so user pages can be shared between address
//! spaces (copy-on-write fork). `alloc` hands out a frame with count 1,
//! `get` adds a reference and `free` drops one; the frame goes back to the
//! pool when the count reaches 0.
//!
//! Only USABLE memory is handed out. Bootloader-reclaimable memory stays
//! reserved because Limine's responses (and the APs' initial page tables)
//! live there.

use limine::memmap::{Entry, MEMMAP_USABLE};
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::structures::paging::{FrameAllocator, PhysFrame, Size4KiB};

use super::PAGE_SIZE;
use super::paging::phys_to_virt;

struct BitmapAllocator {
    bitmap: &'static mut [u64],
    /// Reference count of each frame (0 for free frames).
    refs: &'static mut [u32],
    /// Number of frames covered by the bitmap.
    frames: usize,
    free: usize,
    usable: usize,
    /// Word index where the next search starts.
    hint: usize,
}

impl BitmapAllocator {
    fn is_used(&self, i: usize) -> bool {
        self.bitmap[i / 64] & (1 << (i % 64)) != 0
    }

    fn set_used(&mut self, i: usize) {
        debug_assert!(!self.is_used(i));
        self.bitmap[i / 64] |= 1 << (i % 64);
        self.refs[i] = 1;
        self.free -= 1;
    }

    fn set_free(&mut self, i: usize) {
        self.bitmap[i / 64] &= !(1 << (i % 64));
        self.refs[i] = 0;
        self.free += 1;
    }

    fn check(&self, i: usize) {
        assert!(
            i < self.frames && self.is_used(i) && self.refs[i] > 0,
            "reference to free frame {:#x}",
            i as u64 * PAGE_SIZE
        );
    }

    fn get(&mut self, i: usize) {
        self.check(i);
        self.refs[i] += 1;
    }

    fn put(&mut self, i: usize) {
        self.check(i);
        self.refs[i] -= 1;
        if self.refs[i] == 0 {
            self.set_free(i);
        }
    }

    fn alloc(&mut self) -> Option<usize> {
        let words = self.bitmap.len();
        for n in 0..words {
            let w = (self.hint + n) % words;
            if self.bitmap[w] != u64::MAX {
                let i = w * 64 + (!self.bitmap[w]).trailing_zeros() as usize;
                if i >= self.frames {
                    continue;
                }
                self.set_used(i);
                self.hint = w;
                return Some(i);
            }
        }
        None
    }

    /// First fit for `count` frames starting at a multiple of `align` frames.
    fn alloc_contiguous(&mut self, count: usize, align: usize) -> Option<usize> {
        let mut start = 0;
        while start + count <= self.frames {
            match (start..start + count).find(|&i| self.is_used(i)) {
                Some(used) => start = (used + 1).next_multiple_of(align),
                None => {
                    (start..start + count).for_each(|i| self.set_used(i));
                    return Some(start);
                }
            }
        }
        None
    }
}

static ALLOCATOR: Mutex<Option<BitmapAllocator>> = Mutex::new(None);

pub fn init(memmap: &[&Entry]) {
    let usable = || memmap.iter().filter(|e| e.type_ == MEMMAP_USABLE);
    let end = usable()
        .map(|e| e.base + e.length)
        .max()
        .expect("no usable memory");
    let frames = (end / PAGE_SIZE) as usize;
    let words = frames.div_ceil(64);
    let bitmap_bytes = (words * 8) as u64;
    let refs_bytes = (frames * 4) as u64;
    let meta_frames = (bitmap_bytes + refs_bytes).div_ceil(PAGE_SIZE) as usize;

    let home = usable()
        .find(|e| e.length >= bitmap_bytes + refs_bytes && e.base != 0)
        .expect("no room for the frame bitmap");
    let meta = phys_to_virt(PhysAddr::new(home.base));
    let bitmap = unsafe { core::slice::from_raw_parts_mut(meta.as_mut_ptr(), words) };
    bitmap.fill(u64::MAX);
    let refs =
        unsafe { core::slice::from_raw_parts_mut((meta + bitmap_bytes).as_mut_ptr(), frames) };
    refs.fill(1);

    let mut a = BitmapAllocator {
        bitmap,
        refs,
        frames,
        free: 0,
        usable: 0,
        hint: 0,
    };
    for e in usable() {
        let first = e.base.div_ceil(PAGE_SIZE) as usize;
        let last = ((e.base + e.length) / PAGE_SIZE) as usize;
        for i in first..last {
            a.set_free(i);
        }
        a.usable += last - first;
    }
    // Keep the bitmap, the reference counts and the null frame reserved.
    let home_first = (home.base / PAGE_SIZE) as usize;
    for i in home_first..home_first + meta_frames {
        a.set_used(i);
    }
    if !a.is_used(0) {
        a.set_used(0);
    }
    log::info!(
        "frames: {} usable ({} MiB), bitmap + refcounts {} KiB at {:#x}",
        a.usable,
        (a.usable as u64 * PAGE_SIZE) >> 20,
        (bitmap_bytes + refs_bytes) / 1024,
        home.base
    );
    *ALLOCATOR.lock() = Some(a);
}

fn with<R>(f: impl FnOnce(&mut BitmapAllocator) -> R) -> R {
    f(ALLOCATOR
        .lock()
        .as_mut()
        .expect("frame allocator not initialised"))
}

fn to_frame(i: usize) -> PhysFrame {
    PhysFrame::containing_address(PhysAddr::new(i as u64 * PAGE_SIZE))
}

/// Allocate one frame (contents undefined).
pub fn alloc() -> Option<PhysFrame> {
    with(|a| a.alloc()).map(to_frame)
}

/// Allocate one zero-filled frame.
pub fn alloc_zeroed() -> Option<PhysFrame> {
    let frame = alloc()?;
    unsafe {
        core::ptr::write_bytes(
            phys_to_virt(frame.start_address()).as_mut_ptr::<u8>(),
            0,
            PAGE_SIZE as usize,
        )
    };
    Some(frame)
}

/// Allocate `count` physically contiguous frames aligned to `align` frames.
pub fn alloc_contiguous(count: usize, align: usize) -> Option<PhysAddr> {
    with(|a| a.alloc_contiguous(count, align.max(1))).map(|i| PhysAddr::new(i as u64 * PAGE_SIZE))
}

fn index(frame: PhysFrame) -> usize {
    (frame.start_address().as_u64() / PAGE_SIZE) as usize
}

/// Drop one reference; the frame is freed when none are left.
pub fn free(frame: PhysFrame) {
    with(|a| a.put(index(frame)))
}

/// Add a reference to an allocated frame.
pub fn get(frame: PhysFrame) {
    with(|a| a.get(index(frame)))
}

/// Number of references to an allocated frame.
pub fn refcount(frame: PhysFrame) -> u32 {
    with(|a| {
        let i = index(frame);
        a.check(i);
        a.refs[i]
    })
}

/// (usable frames, free frames).
pub fn stats() -> (usize, usize) {
    with(|a| (a.usable, a.free))
}

/// Adapter for the `x86_64` crate's page table code (hands out zeroed frames).
pub struct GlobalFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        alloc_zeroed()
    }
}

#[test_case]
fn alloc_and_free() {
    let (_, free_before) = stats();
    let a = alloc().unwrap();
    let b = alloc().unwrap();
    assert_ne!(a, b);
    assert_eq!(stats().1, free_before - 2);
    free(a);
    free(b);
    assert_eq!(stats().1, free_before);
}

#[test_case]
fn shared_frame_freed_on_last_reference() {
    let (_, free_before) = stats();
    let f = alloc().unwrap();
    get(f);
    assert_eq!(refcount(f), 2);
    free(f);
    assert_eq!(refcount(f), 1);
    assert_eq!(stats().1, free_before - 1);
    free(f);
    assert_eq!(stats().1, free_before);
}

#[test_case]
fn zeroed_frame_is_zero() {
    let f = alloc_zeroed().unwrap();
    let p = phys_to_virt(f.start_address()).as_ptr::<u64>();
    let words = unsafe { core::slice::from_raw_parts(p, 512) };
    assert!(words.iter().all(|&w| w == 0));
    free(f);
}

#[test_case]
fn contiguous_alignment() {
    let p = alloc_contiguous(16, 512).unwrap();
    assert_eq!(p.as_u64() % (512 * PAGE_SIZE), 0);
    for i in 0..16 {
        free(PhysFrame::containing_address(p + i * PAGE_SIZE));
    }
}
