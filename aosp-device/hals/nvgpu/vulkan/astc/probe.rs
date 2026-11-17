// Copyright 2026 The Android PC Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Call statistics for the Vulkan entry points an app can poll or block in, logged every 2 s
//! while an app submits work (`vulkan.nvidia: calls/2s ...`) when `debug.nvvk.stats` is 1 (read
//! when the process first counts a call): how often each was called, how long
//! it took in total and how often it answered `VK_NOT_READY` / `VK_TIMEOUT`. For finding what an
//! app waits on when it renders slowly with an idle CPU (Phase L4, Where Winds Meet on Vulkan).

use ash::vk;
use std::ffi::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const NAMES: [&str; 9] = [
    "vkGetFenceStatus",
    "vkResetFences",
    "vkGetEventStatus",
    "vkGetQueryPoolResults",
    "vkGetSemaphoreCounterValue",
    "vkInvalidateMappedMemoryRanges",
    "vkAcquireImageANDROID",
    "vkQueueSignalReleaseImageANDROID",
    "vkQueueSubmit",
];
const FENCE_STATUS: usize = 0;
const RESET_FENCES: usize = 1;
const EVENT_STATUS: usize = 2;
const QUERY_RESULTS: usize = 3;
const SEMAPHORE_VALUE: usize = 4;
const INVALIDATE: usize = 5;
const ACQUIRE: usize = 6;
const RELEASE: usize = 7;
/// Counted by the queue_submit hook in lib.rs.
pub(crate) const SUBMIT: usize = 8;

struct Stat {
    calls: AtomicU64,
    nanos: AtomicU64,
    not_ready: AtomicU64,
}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: Stat = Stat {
    calls: AtomicU64::new(0),
    nanos: AtomicU64::new(0),
    not_ready: AtomicU64::new(0),
};
static STATS: [Stat; NAMES.len()] = [ZERO; NAMES.len()];

extern "C" {
    fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
}

/// Whether the system property `name` is "1".
pub(crate) fn prop_is_one(name: &std::ffi::CStr) -> bool {
    let mut value = [0 as c_char; 92]; // PROP_VALUE_MAX
                                       // SAFETY: a NUL-terminated name and a PROP_VALUE_MAX buffer, as bionic requires.
    let n = unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
    n == 1 && value[0] == b'1' as c_char
}

/// Whether `debug.nvvk.stats` is 1 (read once per process).
fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| prop_is_one(c"debug.nvvk.stats"))
}

/// Counts one call that started at `start` and returned `r`, and logs the totals every 2 s.
pub(crate) fn record(i: usize, start: Instant, r: vk::Result) {
    if !enabled() {
        return;
    }
    let s = &STATS[i];
    s.calls.fetch_add(1, Ordering::Relaxed);
    s.nanos
        .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if r == vk::Result::NOT_READY || r == vk::Result::TIMEOUT {
        s.not_ready.fetch_add(1, Ordering::Relaxed);
    }
    static LAST: OnceLock<Mutex<Instant>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(Instant::now()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if last.elapsed().as_secs() < 2 {
        return;
    }
    *last = Instant::now();
    let mut line = String::from("calls/2s");
    for (name, s) in NAMES.iter().zip(STATS.iter()) {
        let calls = s.calls.swap(0, Ordering::Relaxed);
        let nanos = s.nanos.swap(0, Ordering::Relaxed);
        let not_ready = s.not_ready.swap(0, Ordering::Relaxed);
        if calls > 0 {
            line += &format!(" {name}={calls}/{}ms/{not_ready}nr", nanos / 1_000_000);
        }
    }
    crate::log(2, &line);
}

/// The next entry point of `name` (the HAL's or NVIDIA's), resolved once on the first device:
/// NVIDIA's entry points and the HAL's are the same for every device.
fn next(slot: &OnceLock<usize>, device: vk::Device, name: &str) -> usize {
    *slot.get_or_init(|| crate::resolve(device, &[name]))
}

macro_rules! probe {
    ($i:expr, $name:literal, $ty:ty, $device:expr, ($($arg:expr),*)) => {{
        static NEXT: OnceLock<usize> = OnceLock::new();
        let p = next(&NEXT, $device, $name);
        if p == 0 {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
        let start = Instant::now();
        // SAFETY: p is the entry point of this name, whose type is $ty; the app's arguments are
        // passed on unchanged.
        let r = unsafe { crate::fn_ptr::<$ty>(p)($($arg),*) };
        record($i, start, r);
        r
    }};
}

unsafe extern "system" fn get_fence_status(device: vk::Device, fence: vk::Fence) -> vk::Result {
    probe!(
        FENCE_STATUS,
        "vkGetFenceStatus",
        vk::PFN_vkGetFenceStatus,
        device,
        (device, fence)
    )
}

unsafe extern "system" fn reset_fences(
    device: vk::Device,
    count: u32,
    fences: *const vk::Fence,
) -> vk::Result {
    probe!(
        RESET_FENCES,
        "vkResetFences",
        vk::PFN_vkResetFences,
        device,
        (device, count, fences)
    )
}

unsafe extern "system" fn get_event_status(device: vk::Device, event: vk::Event) -> vk::Result {
    probe!(
        EVENT_STATUS,
        "vkGetEventStatus",
        vk::PFN_vkGetEventStatus,
        device,
        (device, event)
    )
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn get_query_pool_results(
    device: vk::Device,
    pool: vk::QueryPool,
    first: u32,
    count: u32,
    size: usize,
    data: *mut c_void,
    stride: vk::DeviceSize,
    flags: vk::QueryResultFlags,
) -> vk::Result {
    probe!(
        QUERY_RESULTS,
        "vkGetQueryPoolResults",
        vk::PFN_vkGetQueryPoolResults,
        device,
        (device, pool, first, count, size, data, stride, flags)
    )
}

unsafe extern "system" fn get_semaphore_counter_value(
    device: vk::Device,
    semaphore: vk::Semaphore,
    value: *mut u64,
) -> vk::Result {
    probe!(
        SEMAPHORE_VALUE,
        "vkGetSemaphoreCounterValue",
        vk::PFN_vkGetSemaphoreCounterValue,
        device,
        (device, semaphore, value)
    )
}

unsafe extern "system" fn invalidate_mapped_memory_ranges(
    device: vk::Device,
    count: u32,
    ranges: *const vk::MappedMemoryRange,
) -> vk::Result {
    probe!(
        INVALIDATE,
        "vkInvalidateMappedMemoryRanges",
        vk::PFN_vkInvalidateMappedMemoryRanges,
        device,
        (device, count, ranges)
    )
}

unsafe extern "system" fn acquire_image_android(
    device: vk::Device,
    image: vk::Image,
    fd: c_int,
    semaphore: vk::Semaphore,
    fence: vk::Fence,
) -> vk::Result {
    probe!(
        ACQUIRE,
        "vkAcquireImageANDROID",
        vk::PFN_vkAcquireImageANDROID,
        device,
        (device, image, fd, semaphore, fence)
    )
}

unsafe extern "system" fn queue_signal_release_image_android(
    queue: vk::Queue,
    count: u32,
    waits: *const vk::Semaphore,
    image: vk::Image,
    fd: *mut c_int,
) -> vk::Result {
    // SAFETY: a queue the HAL has seen.
    let device = unsafe { crate::nvvk_queue_device(queue) };
    probe!(
        RELEASE,
        "vkQueueSignalReleaseImageANDROID",
        vk::PFN_vkQueueSignalReleaseImageANDROID,
        device,
        (queue, count, waits, image, fd)
    )
}

/// This module's entry point of a Vulkan function, or null.
pub(crate) fn proc(name: &str) -> *const c_void {
    match name {
        "vkGetFenceStatus" => get_fence_status as vk::PFN_vkGetFenceStatus as *const c_void,
        "vkResetFences" => reset_fences as vk::PFN_vkResetFences as *const c_void,
        "vkGetEventStatus" => get_event_status as vk::PFN_vkGetEventStatus as *const c_void,
        "vkGetQueryPoolResults" => {
            get_query_pool_results as vk::PFN_vkGetQueryPoolResults as *const c_void
        }
        "vkGetSemaphoreCounterValue" | "vkGetSemaphoreCounterValueKHR" => {
            get_semaphore_counter_value as vk::PFN_vkGetSemaphoreCounterValue as *const c_void
        }
        "vkInvalidateMappedMemoryRanges" => {
            invalidate_mapped_memory_ranges as vk::PFN_vkInvalidateMappedMemoryRanges
                as *const c_void
        }
        "vkAcquireImageANDROID" => {
            acquire_image_android as vk::PFN_vkAcquireImageANDROID as *const c_void
        }
        "vkQueueSignalReleaseImageANDROID" => {
            queue_signal_release_image_android as vk::PFN_vkQueueSignalReleaseImageANDROID
                as *const c_void
        }
        _ => std::ptr::null(),
    }
}
