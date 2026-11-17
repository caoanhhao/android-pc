// Copyright 2026 The Android PC Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Texture formats and shader behavior NVIDIA's desktop driver lacks, for vulkan.nvidia (Phase L4).
//!
//! Android games ship ASTC and ETC2/EAC textures and their Vulkan paths need
//! `textureCompressionASTC_LDR` (Where Winds Meet's world is ASTC, a few of its maps ETC2/EAC);
//! NVIDIA has neither. The driver gets an uncompressed image in place of every such image
//! (R8G8B8A8 UNORM or SRGB; R16(G16) UNORM/SNORM for EAC R11(G11)), and the texels are decoded
//! on the CPU: ASTC with the astc-decode crate (third_party/astc-decode), ETC2/EAC in etc.rs.
//! - `vkGetPhysicalDeviceFeatures*`: `textureCompressionASTC_LDR` and `textureCompressionETC2`
//!   are true;
//! - `vkGetPhysicalDevice*FormatProperties*`: a compressed format has what its emulated format
//!   has for sampling and transfers in optimal tiling, nothing in linear tiling or as a buffer;
//! - `vkCreateImage` / `vkCreateImageView` / `vkGetDeviceImageMemoryRequirements`: a compressed
//!   format becomes its emulated format;
//! - `vkCmdCopyBufferToImage(2)` into such an image records a copy from a staging buffer of
//!   the HAL instead, and `vkQueueSubmit(2)` decodes the app's blocks into it before
//!   submitting (also for secondary command buffers the submitted ones execute): the app may
//!   write its upload buffer up to the submit. The source buffer must be in host-visible
//!   memory (an upload buffer always is); the staging buffers live until the command buffer is
//!   begun again, reset or freed.
//!
//! Copies out of a compressed image (to a buffer, or to an image of another format) are not
//! emulated.
//!
//! With `debug.nvvk.invariant_position` 1, `vkCreateShaderModule` makes vertex shader positions
//! invariant (spirv.rs, an experiment).
//!
//! Debugging: `debug.nvvk.astc_trace` 1 logs every upload, view, copy and memory alias of a
//! compressed image (`ASTC trace: ...`), `debug.nvvk.astc_dump` 1 writes decoded regions for a
//! comparison with a reference decoder (`dump()`).
//!
//! The C++ HAL (nvvk_hal.cpp) asks `nvvk_astc_proc` first in its vkGet*ProcAddr; the functions
//! here call the HAL's own entry points (or NVIDIA's) below them through `nvvk_next_*_proc`.

mod etc;
mod probe;
mod spirv;

use ash::vk;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};

// ---- the C++ side (nvvk_hal.cpp) ----

extern "C" {
    /// The HAL's entry point (or NVIDIA's) for a device function: what an app would get
    /// without this module.
    fn nvvk_next_device_proc(device: vk::Device, name: *const c_char) -> *const c_void;
    /// The same for an instance or physical-device function.
    fn nvvk_next_instance_proc(name: *const c_char) -> *const c_void;
    /// The device of a queue the HAL has seen, or null.
    pub(crate) fn nvvk_queue_device(queue: vk::Queue) -> vk::Device;
    fn nvvk_device_memory_properties(
        device: vk::Device,
        props: *mut vk::PhysicalDeviceMemoryProperties,
    ) -> bool;
    /// 0 error, 1 warning, 2 info.
    fn nvvk_log(level: i32, msg: *const c_char);
}

pub(crate) fn log(level: i32, msg: &str) {
    if let Ok(s) = CString::new(msg) {
        // SAFETY: s is a valid C string for the call.
        unsafe { nvvk_log(level, s.as_ptr()) };
    }
}

// ---- decoding ----

/// Decodes one block of `c` and hands each texel (`c.texel_bytes()` bytes) with its position in
/// the block to `put`. False if the block is invalid (ASTC's error color is written then).
fn decode_block(c: Compressed, block: &[u8], mut put: impl FnMut(usize, usize, &[u8])) -> bool {
    let row_major = |put: &mut dyn FnMut(usize, usize, &[u8]), texels: &[&[u8]]| {
        for (i, t) in texels.iter().enumerate() {
            put(i % 4, i / 4, t);
        }
    };
    let half = |b: &[u8]| -> [u8; 8] { b.try_into().unwrap() };
    match c.codec {
        Codec::Astc => {
            let footprint = astc_decode::Footprint::new(c.bw, c.bh);
            astc_decode::astc_decode_block(block.try_into().unwrap(), footprint, |x, y, color| {
                put(x as usize, y as usize, &color)
            })
        }
        Codec::Etc2Rgb | Codec::Etc2RgbA1 => {
            let t = etc::etc2_rgb(&half(block), c.codec == Codec::Etc2RgbA1);
            row_major(&mut put, &t.each_ref().map(|t| &t[..]));
            true
        }
        Codec::Etc2Rgba => {
            let alpha = etc::eac_alpha(&half(&block[..8]));
            let mut t = etc::etc2_rgb(&half(&block[8..]), false);
            for (t, a) in t.iter_mut().zip(alpha) {
                t[3] = a;
            }
            row_major(&mut put, &t.each_ref().map(|t| &t[..]));
            true
        }
        Codec::EacR11 { signed } => {
            let t = etc::eac_r11(&half(block), signed).map(u16::to_le_bytes);
            row_major(&mut put, &t.each_ref().map(|t| &t[..]));
            true
        }
        Codec::EacRg11 { signed } => {
            let r = etc::eac_r11(&half(&block[..8]), signed);
            let g = etc::eac_r11(&half(&block[8..]), signed);
            let t: [[u8; 4]; 16] = std::array::from_fn(|i| {
                let (r, g) = (r[i].to_le_bytes(), g[i].to_le_bytes());
                [r[0], r[1], g[0], g[1]]
            });
            row_major(&mut put, &t.each_ref().map(|t| &t[..]));
            true
        }
    }
}

/// Decodes the blocks of a `w` x `h` compressed image (rows of `row_blocks` blocks) into `out`,
/// a `w` x `h` image of its emulated format with rows of `w * c.texel_bytes()` bytes. Rows of
/// blocks are decoded on up to 4 threads. sRGB textures decode to their stored (sRGB-encoded)
/// values, which the R8G8B8A8_SRGB image then converts as the compressed sRGB format would.
fn decode(c: Compressed, blocks: &[u8], row_blocks: u32, w: u32, h: u32, out: &mut [u8]) -> bool {
    let (bw, bh) = (c.bw as usize, c.bh as usize);
    let (w, h, row_blocks) = (w as usize, h as usize, row_blocks as usize);
    let (block_bytes, texel) = (c.block_bytes(), c.texel_bytes());
    let blocks_x = w.div_ceil(bw);
    let band = bh * w * texel; // the bytes one row of blocks fills
    let decode_row = |by: usize, out: &mut [u8]| -> bool {
        let mut ok = true;
        for bx in 0..blocks_x {
            let at = (by * row_blocks + bx) * block_bytes;
            let Some(block) = blocks.get(at..at + block_bytes) else {
                return false;
            };
            ok &= decode_block(c, block, |x, y, color| {
                let px = bx * bw + x;
                if px < w && (by * bh + y) < h {
                    let i = (y * w + px) * texel;
                    out[i..i + texel].copy_from_slice(color);
                }
            });
        }
        ok
    };
    let rows: Vec<(usize, &mut [u8])> = out[..w * h * texel].chunks_mut(band).enumerate().collect();
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(4);
    if threads <= 1 || rows.len() < 8 {
        return rows
            .into_iter()
            .fold(true, |ok, (by, out)| decode_row(by, out) & ok);
    }
    let per = rows.len().div_ceil(threads);
    let mut groups: Vec<Vec<(usize, &mut [u8])>> = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        if i % per == 0 {
            groups.push(Vec::new());
        }
        groups.last_mut().unwrap().push(row);
    }
    let decode_row = &decode_row;
    std::thread::scope(|scope| {
        let handles: Vec<_> = groups
            .into_iter()
            .map(|group| {
                scope.spawn(move || {
                    group
                        .into_iter()
                        .fold(true, |ok, (by, out)| decode_row(by, out) & ok)
                })
            })
            .collect();
        handles
            .into_iter()
            .fold(true, |ok, h| h.join().unwrap_or(false) & ok)
    })
}

// ---- formats ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Codec {
    Astc,
    Etc2Rgb,
    Etc2RgbA1,
    Etc2Rgba,
    EacR11 { signed: bool },
    EacRg11 { signed: bool },
}

/// A compressed format the driver lacks: ASTC LDR, ETC2 or EAC.
#[derive(Clone, Copy)]
struct Compressed {
    codec: Codec,
    bw: u32,
    bh: u32,
    srgb: bool,
}

impl Compressed {
    fn block_bytes(&self) -> usize {
        match self.codec {
            Codec::Etc2Rgb | Codec::Etc2RgbA1 | Codec::EacR11 { .. } => 8,
            _ => 16,
        }
    }

    /// The bytes of a texel of the emulated format.
    fn texel_bytes(&self) -> usize {
        match self.codec {
            Codec::EacR11 { .. } => 2,
            _ => 4,
        }
    }
}

/// ASTC block sizes in the order of the VkFormat enum (each as UNORM, then SRGB).
const BLOCKS: [(u32, u32); 14] = [
    (4, 4),
    (5, 4),
    (5, 5),
    (6, 5),
    (6, 6),
    (8, 5),
    (8, 6),
    (8, 8),
    (10, 5),
    (10, 6),
    (10, 8),
    (10, 10),
    (12, 10),
    (12, 12),
];

fn compressed_of(format: vk::Format) -> Option<Compressed> {
    let etc = |codec, srgb| {
        Some(Compressed {
            codec,
            bw: 4,
            bh: 4,
            srgb,
        })
    };
    match format {
        vk::Format::ETC2_R8G8B8_UNORM_BLOCK => return etc(Codec::Etc2Rgb, false),
        vk::Format::ETC2_R8G8B8_SRGB_BLOCK => return etc(Codec::Etc2Rgb, true),
        vk::Format::ETC2_R8G8B8A1_UNORM_BLOCK => return etc(Codec::Etc2RgbA1, false),
        vk::Format::ETC2_R8G8B8A1_SRGB_BLOCK => return etc(Codec::Etc2RgbA1, true),
        vk::Format::ETC2_R8G8B8A8_UNORM_BLOCK => return etc(Codec::Etc2Rgba, false),
        vk::Format::ETC2_R8G8B8A8_SRGB_BLOCK => return etc(Codec::Etc2Rgba, true),
        vk::Format::EAC_R11_UNORM_BLOCK => return etc(Codec::EacR11 { signed: false }, false),
        vk::Format::EAC_R11_SNORM_BLOCK => return etc(Codec::EacR11 { signed: true }, false),
        vk::Format::EAC_R11G11_UNORM_BLOCK => return etc(Codec::EacRg11 { signed: false }, false),
        vk::Format::EAC_R11G11_SNORM_BLOCK => return etc(Codec::EacRg11 { signed: true }, false),
        _ => {}
    }
    let first = vk::Format::ASTC_4X4_UNORM_BLOCK.as_raw();
    let last = vk::Format::ASTC_12X12_SRGB_BLOCK.as_raw();
    let raw = format.as_raw();
    if raw < first || raw > last {
        return None;
    }
    let i = (raw - first) as usize;
    let (bw, bh) = BLOCKS[i / 2];
    Some(Compressed {
        codec: Codec::Astc,
        bw,
        bh,
        srgb: i % 2 == 1,
    })
}

/// The format the driver gets for a compressed format; any other format unchanged.
fn emulated(format: vk::Format) -> vk::Format {
    let Some(c) = compressed_of(format) else {
        return format;
    };
    match c.codec {
        Codec::EacR11 { signed: false } => vk::Format::R16_UNORM,
        Codec::EacR11 { signed: true } => vk::Format::R16_SNORM,
        Codec::EacRg11 { signed: false } => vk::Format::R16G16_UNORM,
        Codec::EacRg11 { signed: true } => vk::Format::R16G16_SNORM,
        _ if c.srgb => vk::Format::R8G8B8A8_SRGB,
        _ => vk::Format::R8G8B8A8_UNORM,
    }
}

/// What an ASTC format may do: sampling and transfers, as its R8G8B8A8 format does.
const ASTC_FEATURES: vk::FormatFeatureFlags = vk::FormatFeatureFlags::from_raw(
    vk::FormatFeatureFlags::SAMPLED_IMAGE.as_raw()
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR.as_raw()
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_MINMAX.as_raw()
        | vk::FormatFeatureFlags::BLIT_SRC.as_raw()
        | vk::FormatFeatureFlags::TRANSFER_SRC.as_raw()
        | vk::FormatFeatureFlags::TRANSFER_DST.as_raw(),
);

/// The image usages an ASTC image may have.
const ASTC_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::SAMPLED.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw()
        | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);

fn mask_format_properties(props: &mut vk::FormatProperties) {
    props.linear_tiling_features = vk::FormatFeatureFlags::empty();
    props.optimal_tiling_features &= ASTC_FEATURES;
    props.buffer_features = vk::FormatFeatureFlags::empty();
}

// ---- the functions below this module ----

/// The HAL's entry point (or NVIDIA's) of an instance function, as `$ty`. Used inside the
/// `unsafe` blocks of the entry points: the name is NUL-terminated, and the HAL returns the
/// entry point of that name, whose type is `$ty`.
macro_rules! next_instance {
    ($name:literal, $ty:ty) => {{
        static FN: OnceLock<usize> = OnceLock::new();
        let p = *FN
            .get_or_init(|| nvvk_next_instance_proc(concat!($name, "\0").as_ptr().cast()) as usize);
        fn_ptr::<$ty>(p)
    }};
}

/// The next device-level functions this module calls, per device.
#[derive(Clone, Copy)]
struct DeviceFns {
    create_image: vk::PFN_vkCreateImage,
    destroy_image: vk::PFN_vkDestroyImage,
    create_image_view: vk::PFN_vkCreateImageView,
    get_device_image_memory_requirements: Option<vk::PFN_vkGetDeviceImageMemoryRequirements>,
    allocate_memory: vk::PFN_vkAllocateMemory,
    free_memory: vk::PFN_vkFreeMemory,
    map_memory: vk::PFN_vkMapMemory,
    unmap_memory: vk::PFN_vkUnmapMemory,
    create_buffer: vk::PFN_vkCreateBuffer,
    destroy_buffer: vk::PFN_vkDestroyBuffer,
    bind_buffer_memory: vk::PFN_vkBindBufferMemory,
    bind_buffer_memory2: Option<vk::PFN_vkBindBufferMemory2>,
    get_buffer_memory_requirements: vk::PFN_vkGetBufferMemoryRequirements,
    allocate_command_buffers: vk::PFN_vkAllocateCommandBuffers,
    free_command_buffers: vk::PFN_vkFreeCommandBuffers,
    begin_command_buffer: vk::PFN_vkBeginCommandBuffer,
    reset_command_buffer: vk::PFN_vkResetCommandBuffer,
    reset_command_pool: vk::PFN_vkResetCommandPool,
    destroy_command_pool: vk::PFN_vkDestroyCommandPool,
    cmd_copy_buffer_to_image: vk::PFN_vkCmdCopyBufferToImage,
    cmd_copy_buffer_to_image2: Option<vk::PFN_vkCmdCopyBufferToImage2>,
    cmd_copy_image: vk::PFN_vkCmdCopyImage,
    cmd_copy_image2: Option<vk::PFN_vkCmdCopyImage2>,
    cmd_blit_image: vk::PFN_vkCmdBlitImage,
    cmd_blit_image2: Option<vk::PFN_vkCmdBlitImage2>,
    cmd_copy_image_to_buffer: vk::PFN_vkCmdCopyImageToBuffer,
    cmd_copy_image_to_buffer2: Option<vk::PFN_vkCmdCopyImageToBuffer2>,
    cmd_execute_commands: vk::PFN_vkCmdExecuteCommands,
    create_shader_module: vk::PFN_vkCreateShaderModule,
    bind_image_memory: vk::PFN_vkBindImageMemory,
    get_image_memory_requirements: vk::PFN_vkGetImageMemoryRequirements,
    bind_image_memory2: Option<vk::PFN_vkBindImageMemory2>,
    queue_submit: vk::PFN_vkQueueSubmit,
    queue_submit2: Option<vk::PFN_vkQueueSubmit2>,
    wait_for_fences: vk::PFN_vkWaitForFences,
    wait_semaphores: Option<vk::PFN_vkWaitSemaphores>,
    queue_wait_idle: vk::PFN_vkQueueWaitIdle,
    device_wait_idle: vk::PFN_vkDeviceWaitIdle,
    memory_props: vk::PhysicalDeviceMemoryProperties,
}

/// `p` (an entry point from the HAL, non-zero) as the function pointer type `T`.
///
/// # Safety
/// `p` is the address of a function whose signature is `T`.
pub(crate) unsafe fn fn_ptr<T: Copy>(p: usize) -> T {
    debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<usize>());
    // SAFETY: T is a function pointer type of the same size as usize, and p is such a function
    // (the caller's contract).
    unsafe { std::mem::transmute_copy::<usize, T>(&p) }
}

pub(crate) fn resolve(device: vk::Device, names: &[&str]) -> usize {
    for name in names {
        let c = CString::new(*name).unwrap();
        // SAFETY: a valid C string; the HAL returns null for an unknown name.
        let p = unsafe { nvvk_next_device_proc(device, c.as_ptr()) };
        if !p.is_null() {
            return p as usize;
        }
    }
    0
}

impl DeviceFns {
    fn load(device: vk::Device) -> Option<DeviceFns> {
        macro_rules! req {
            ($($n:literal),+) => {{
                let p = resolve(device, &[$($n),+]);
                if p == 0 {
                    log(0, concat!("ASTC: no ", $($n, " "),+));
                    return None;
                }
                // SAFETY: the entry point of this name, whose type is the field's.
                unsafe { fn_ptr(p) }
            }};
        }
        macro_rules! opt {
            ($($n:literal),+) => {{
                let p = resolve(device, &[$($n),+]);
                (p != 0).then(|| {
                    // SAFETY: the entry point of this name, whose type is the field's.
                    unsafe { fn_ptr(p) }
                })
            }};
        }
        let mut memory_props = vk::PhysicalDeviceMemoryProperties::default();
        // SAFETY: memory_props is a valid output.
        if !unsafe { nvvk_device_memory_properties(device, &mut memory_props) } {
            return None;
        }
        Some(DeviceFns {
            create_image: req!("vkCreateImage"),
            destroy_image: req!("vkDestroyImage"),
            create_image_view: req!("vkCreateImageView"),
            get_device_image_memory_requirements: opt!(
                "vkGetDeviceImageMemoryRequirements",
                "vkGetDeviceImageMemoryRequirementsKHR"
            ),
            allocate_memory: req!("vkAllocateMemory"),
            free_memory: req!("vkFreeMemory"),
            map_memory: req!("vkMapMemory"),
            unmap_memory: req!("vkUnmapMemory"),
            create_buffer: req!("vkCreateBuffer"),
            destroy_buffer: req!("vkDestroyBuffer"),
            bind_buffer_memory: req!("vkBindBufferMemory"),
            bind_buffer_memory2: opt!("vkBindBufferMemory2", "vkBindBufferMemory2KHR"),
            get_buffer_memory_requirements: req!("vkGetBufferMemoryRequirements"),
            allocate_command_buffers: req!("vkAllocateCommandBuffers"),
            free_command_buffers: req!("vkFreeCommandBuffers"),
            begin_command_buffer: req!("vkBeginCommandBuffer"),
            reset_command_buffer: req!("vkResetCommandBuffer"),
            reset_command_pool: req!("vkResetCommandPool"),
            destroy_command_pool: req!("vkDestroyCommandPool"),
            cmd_copy_buffer_to_image: req!("vkCmdCopyBufferToImage"),
            cmd_copy_buffer_to_image2: opt!(
                "vkCmdCopyBufferToImage2",
                "vkCmdCopyBufferToImage2KHR"
            ),
            cmd_copy_image: req!("vkCmdCopyImage"),
            cmd_copy_image2: opt!("vkCmdCopyImage2", "vkCmdCopyImage2KHR"),
            cmd_blit_image: req!("vkCmdBlitImage"),
            cmd_blit_image2: opt!("vkCmdBlitImage2", "vkCmdBlitImage2KHR"),
            cmd_copy_image_to_buffer: req!("vkCmdCopyImageToBuffer"),
            cmd_copy_image_to_buffer2: opt!(
                "vkCmdCopyImageToBuffer2",
                "vkCmdCopyImageToBuffer2KHR"
            ),
            cmd_execute_commands: req!("vkCmdExecuteCommands"),
            create_shader_module: req!("vkCreateShaderModule"),
            bind_image_memory: req!("vkBindImageMemory"),
            get_image_memory_requirements: req!("vkGetImageMemoryRequirements"),
            bind_image_memory2: opt!("vkBindImageMemory2", "vkBindImageMemory2KHR"),
            queue_submit: req!("vkQueueSubmit"),
            queue_submit2: opt!("vkQueueSubmit2", "vkQueueSubmit2KHR"),
            wait_for_fences: req!("vkWaitForFences"),
            wait_semaphores: opt!("vkWaitSemaphores", "vkWaitSemaphoresKHR"),
            queue_wait_idle: req!("vkQueueWaitIdle"),
            device_wait_idle: req!("vkDeviceWaitIdle"),
            memory_props,
        })
    }
}

// ---- state ----

/// One region of an app's copy into an emulated image: its blocks in the app's buffer and where
/// the decoded texels go in the staging buffer.
#[derive(Clone)]
struct Region {
    src_offset: u64,
    row_blocks: u32,
    slice_bytes: u64,
    w: u32,
    h: u32,
    layers: u32,
    dst_offset: u64,
}

#[derive(Clone)]
struct Upload {
    src: vk::Buffer,
    astc: Compressed,
    regions: Vec<Region>,
    staging: vk::Buffer,
    staging_memory: vk::DeviceMemory,
    staging_ptr: usize,
    /// With the trace: a hash of the app's blocks when they were decoded.
    hash: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

struct CommandBuffer {
    device: vk::Device,
    pool: vk::CommandPool,
    uploads: Vec<Upload>,
    /// Secondary command buffers this one executes, whose uploads are decoded with its own.
    secondaries: Vec<vk::CommandBuffer>,
}

#[derive(Default)]
struct State {
    devices: HashMap<vk::Device, DeviceFns>,
    images: HashMap<vk::Image, Compressed>,
    buffers: HashMap<vk::Buffer, (vk::DeviceMemory, u64)>,
    mappings: HashMap<vk::DeviceMemory, usize>, // the app's pointer for offset 0
    memory_types: HashMap<vk::DeviceMemory, u32>,
    cmd_buffers: HashMap<vk::CommandBuffer, CommandBuffer>,
    /// With `debug.nvvk.astc_trace`: every image's format and flags, and its memory binding.
    traced: HashMap<vk::Image, (vk::Format, vk::ImageCreateFlags)>,
    /// With the trace: uploads recorded per compressed image, and when it was created.
    upload_counts: HashMap<vk::Image, (u32, std::time::Instant, String)>,
    /// Live image bindings per memory: (offset, size, image).
    bindings: HashMap<vk::DeviceMemory, Vec<(u64, u64, vk::Image)>>,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(State::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The next functions of a device; None if they cannot be resolved (then nothing is emulated).
fn device_fns(device: vk::Device) -> Option<DeviceFns> {
    if let Some(f) = state().devices.get(&device) {
        return Some(*f);
    }
    let f = DeviceFns::load(device)?;
    state().devices.insert(device, f);
    Some(f)
}

fn cmd_device(cb: vk::CommandBuffer) -> Option<vk::Device> {
    state().cmd_buffers.get(&cb).map(|c| c.device)
}

/// A hash of the app's blocks of an upload (FNV-1a over every region and layer), or None if
/// they cannot be read now.
fn upload_hash(u: &Upload) -> Option<u64> {
    let (memory, offset, base) = {
        let s = state();
        let &(memory, offset) = s.buffers.get(&u.src)?;
        (memory, offset, *s.mappings.get(&memory)?)
    };
    let _ = memory;
    let mut h = 0xcbf29ce484222325u64;
    for r in &u.regions {
        let len = r.slice_bytes as usize * r.layers as usize;
        // SAFETY: the app's mapped upload buffer (still bound and mapped).
        let src = unsafe {
            std::slice::from_raw_parts((base as u64 + offset + r.src_offset) as *const u8, len)
        };
        for &b in src {
            h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
    }
    Some(h)
}

fn free_uploads(device: vk::Device, f: &DeviceFns, uploads: Vec<Upload>) {
    use std::sync::atomic::Ordering;
    for u in uploads {
        if tracing() {
            let then = u.hash.load(Ordering::Relaxed);
            if then != 0 {
                if let Some(now) = upload_hash(&u) {
                    if now != then {
                        trace(|| {
                            format!(
                                "late: the blocks of an upload from buffer {:?} ({} regions, first {}x{} +{}) changed after vkQueueSubmit",
                                u.src,
                                u.regions.len(),
                                u.regions[0].w,
                                u.regions[0].h,
                                u.regions[0].src_offset
                            )
                        });
                    }
                }
            }
        }
        // SAFETY: the HAL's own staging objects, no longer used by the GPU (the app reset or
        // freed the command buffer that used them).
        unsafe {
            (f.free_memory)(device, u.staging_memory, std::ptr::null());
            (f.destroy_buffer)(device, u.staging, std::ptr::null());
        }
    }
}

/// Drops the staging buffers of command buffers that are recorded again, reset or freed.
fn release_command_buffers(cbs: &[vk::CommandBuffer], forget: bool) {
    for cb in cbs {
        let (device, uploads) = {
            let mut s = state();
            let Some(c) = s.cmd_buffers.get_mut(cb) else {
                continue;
            };
            c.secondaries.clear();
            let out = (c.device, std::mem::take(&mut c.uploads));
            if forget {
                s.cmd_buffers.remove(cb);
            }
            out
        };
        if uploads.is_empty() {
            continue;
        }
        if let Some(f) = device_fns(device) {
            free_uploads(device, &f, uploads);
        }
    }
}

fn pool_command_buffers(pool: vk::CommandPool) -> Vec<vk::CommandBuffer> {
    state()
        .cmd_buffers
        .iter()
        .filter(|(_, c)| c.pool == pool)
        .map(|(cb, _)| *cb)
        .collect()
}

fn find_memory_type(f: &DeviceFns, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
    (0..f.memory_props.memory_type_count).find(|&i| {
        bits & (1 << i) != 0
            && f.memory_props.memory_types[i as usize]
                .property_flags
                .contains(want)
    })
}

/// A host-visible, coherent, mapped transfer source of `size` bytes.
fn create_staging(
    device: vk::Device,
    f: &DeviceFns,
    size: u64,
) -> Option<(vk::Buffer, vk::DeviceMemory, usize)> {
    let info = vk::BufferCreateInfo {
        size,
        usage: vk::BufferUsageFlags::TRANSFER_SRC,
        sharing_mode: vk::SharingMode::EXCLUSIVE,
        ..Default::default()
    };
    let mut buffer = vk::Buffer::null();
    // SAFETY: valid create info and output for the device's functions.
    unsafe {
        if (f.create_buffer)(device, &info, std::ptr::null(), &mut buffer) != vk::Result::SUCCESS {
            return None;
        }
        let mut req = vk::MemoryRequirements::default();
        (f.get_buffer_memory_requirements)(device, buffer, &mut req);
        let want = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let mut memory = vk::DeviceMemory::null();
        let ok = find_memory_type(f, req.memory_type_bits, want).is_some_and(|t| {
            let alloc = vk::MemoryAllocateInfo {
                allocation_size: req.size,
                memory_type_index: t,
                ..Default::default()
            };
            (f.allocate_memory)(device, &alloc, std::ptr::null(), &mut memory)
                == vk::Result::SUCCESS
        });
        if !ok {
            (f.destroy_buffer)(device, buffer, std::ptr::null());
            return None;
        }
        let mut ptr: *mut c_void = std::ptr::null_mut();
        if (f.bind_buffer_memory)(device, buffer, memory, 0) != vk::Result::SUCCESS
            || (f.map_memory)(
                device,
                memory,
                0,
                vk::WHOLE_SIZE,
                vk::MemoryMapFlags::empty(),
                &mut ptr,
            ) != vk::Result::SUCCESS
        {
            (f.free_memory)(device, memory, std::ptr::null());
            (f.destroy_buffer)(device, buffer, std::ptr::null());
            return None;
        }
        Some((buffer, memory, ptr as usize))
    }
}

/// The parts of VkBufferImageCopy and VkBufferImageCopy2 an upload needs.
struct CopyRegion {
    buffer_offset: u64,
    buffer_row_length: u32,
    buffer_image_height: u32,
    image_subresource: vk::ImageSubresourceLayers,
    image_offset: vk::Offset3D,
    image_extent: vk::Extent3D,
}

/// Records the copy of an app's buffer into an emulated image as a copy from a staging buffer.
#[allow(clippy::too_many_arguments)]
fn record_upload(
    cb: vk::CommandBuffer,
    device: vk::Device,
    f: &DeviceFns,
    src: vk::Buffer,
    dst: vk::Image,
    layout: vk::ImageLayout,
    astc: Compressed,
    regions: &[CopyRegion],
) {
    if tracing() {
        if let Some(c) = state().upload_counts.get_mut(&dst) {
            c.0 += regions.len() as u32;
        }
        let buffer = traced_buffer(src, f);
        for r in regions {
            trace(|| {
                format!(
                    "upload cb {cb:?} buffer {src:?} ({buffer}) +{} row {} height {} -> image {dst:?} {}x{} {} at {},{},{} extent {}x{}x{}",
                    r.buffer_offset,
                    r.buffer_row_length,
                    r.buffer_image_height,
                    astc.bw,
                    astc.bh,
                    subresource(&r.image_subresource),
                    r.image_offset.x,
                    r.image_offset.y,
                    r.image_offset.z,
                    r.image_extent.width,
                    r.image_extent.height,
                    r.image_extent.depth
                )
            });
        }
    }
    let mut out = Vec::with_capacity(regions.len());
    let mut copies = Vec::with_capacity(regions.len());
    let mut size = 0u64;
    for r in regions {
        let row = if r.buffer_row_length != 0 {
            r.buffer_row_length
        } else {
            r.image_extent.width
        };
        let height = if r.buffer_image_height != 0 {
            r.buffer_image_height
        } else {
            r.image_extent.height
        };
        let row_blocks = row.div_ceil(astc.bw);
        let region = Region {
            src_offset: r.buffer_offset,
            row_blocks,
            slice_bytes: u64::from(row_blocks)
                * u64::from(height.div_ceil(astc.bh))
                * astc.block_bytes() as u64,
            w: r.image_extent.width,
            h: r.image_extent.height,
            layers: r.image_subresource.layer_count * r.image_extent.depth.max(1),
            dst_offset: size,
        };
        size += u64::from(region.w)
            * u64::from(region.h)
            * astc.texel_bytes() as u64
            * u64::from(region.layers);
        copies.push(vk::BufferImageCopy {
            buffer_offset: region.dst_offset,
            buffer_row_length: 0,
            buffer_image_height: 0,
            image_subresource: r.image_subresource,
            image_offset: r.image_offset,
            image_extent: r.image_extent,
        });
        out.push(region);
    }
    let Some((staging, staging_memory, staging_ptr)) = create_staging(device, f, size.max(4))
    else {
        log(
            0,
            &format!("ASTC: no staging buffer for an upload of {size} bytes"),
        );
        return;
    };
    // SAFETY: cb is in the recording state (the app is recording this copy into it).
    unsafe {
        (f.cmd_copy_buffer_to_image)(
            cb,
            staging,
            dst,
            layout,
            copies.len() as u32,
            copies.as_ptr(),
        )
    };
    let upload = Upload {
        src,
        astc,
        regions: out,
        staging,
        staging_memory,
        staging_ptr,
        hash: Default::default(),
    };
    if let Some(c) = state().cmd_buffers.get_mut(&cb) {
        c.uploads.push(upload);
    }
}

/// The command buffers a submit runs: `cbs` and the secondaries they execute.
fn with_secondaries(cbs: &[vk::CommandBuffer]) -> Vec<vk::CommandBuffer> {
    let s = state();
    let mut all = cbs.to_vec();
    let mut i = 0;
    while i < all.len() {
        if let Some(c) = s.cmd_buffers.get(&all[i]) {
            for sec in &c.secondaries {
                if !all.contains(sec) {
                    all.push(*sec);
                }
            }
        }
        i += 1;
    }
    all
}

/// Decodes the uploads of command buffers about to be submitted.
fn run_uploads(device: vk::Device, cbs: &[vk::CommandBuffer]) {
    let Some(f) = device_fns(device) else { return };
    for cb in &with_secondaries(cbs) {
        // Snapshot what the uploads read, then decode without the lock.
        let work: Vec<(Upload, vk::DeviceMemory, u64, usize, Option<u32>)> = {
            let s = state();
            let Some(c) = s.cmd_buffers.get(cb) else {
                continue;
            };
            c.uploads
                .iter()
                .filter_map(|u| {
                    let Some(&(memory, offset)) = s.buffers.get(&u.src) else {
                        log(1, &format!("ASTC: upload from unbound buffer {:?}", u.src));
                        return None;
                    };
                    let base = s.mappings.get(&memory).copied().unwrap_or(0);
                    Some((
                        u.clone(),
                        memory,
                        offset,
                        base,
                        s.memory_types.get(&memory).copied(),
                    ))
                })
                .collect()
        };
        for (u, memory, offset, mut base, memory_type) in work {
            let mut own = false;
            if base == 0 {
                // Not mapped by the app: map it here if the CPU can read it.
                let visible = memory_type.is_some_and(|t| {
                    f.memory_props.memory_types[t as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::HOST_VISIBLE)
                });
                let mut ptr: *mut c_void = std::ptr::null_mut();
                let mapped = visible && {
                    // SAFETY: memory is the app's host-visible allocation bound to the upload's
                    // source buffer, and the app does not have it mapped.
                    unsafe {
                        (f.map_memory)(
                            device,
                            memory,
                            0,
                            vk::WHOLE_SIZE,
                            vk::MemoryMapFlags::empty(),
                            &mut ptr,
                        )
                    }
                } == vk::Result::SUCCESS;
                if !mapped {
                    static LOGGED: OnceLock<()> = OnceLock::new();
                    LOGGED.get_or_init(|| {
                        log(
                            1,
                            "ASTC: upload from a buffer the CPU cannot read; texture left blank",
                        )
                    });
                    continue;
                }
                base = ptr as usize;
                own = true;
            }
            let texel = u.astc.texel_bytes();
            for r in &u.regions {
                for l in 0..r.layers {
                    let src = base as u64 + offset + r.src_offset + u64::from(l) * r.slice_bytes;
                    let dst = u.staging_ptr as u64
                        + r.dst_offset
                        + u64::from(l) * u64::from(r.w) * u64::from(r.h) * texel as u64;
                    let blocks_len = r.slice_bytes as usize;
                    let out_len = r.w as usize * r.h as usize * texel;
                    // SAFETY: src points at the app's blocks for this region and layer (the app
                    // keeps the buffer valid until the copy executes), dst into the staging
                    // buffer sized for the region.
                    let ok = unsafe {
                        decode(
                            u.astc,
                            std::slice::from_raw_parts(src as *const u8, blocks_len),
                            r.row_blocks,
                            r.w,
                            r.h,
                            std::slice::from_raw_parts_mut(dst as *mut u8, out_len),
                        )
                    };
                    if !ok {
                        log(
                            0,
                            &format!("ASTC: decode of a {}x{} texture failed", r.w, r.h),
                        );
                    }
                    // SAFETY: as for the decode above.
                    unsafe {
                        dump(
                            u.astc,
                            r,
                            std::slice::from_raw_parts(src as *const u8, blocks_len),
                            std::slice::from_raw_parts(dst as *const u8, out_len),
                        )
                    };
                }
            }
            if tracing() && !own {
                if let Some(h) = upload_hash(&u) {
                    u.hash.store(h, std::sync::atomic::Ordering::Relaxed);
                }
            }
            if own {
                // SAFETY: mapped above.
                unsafe { (f.unmap_memory)(device, memory) };
            }
        }
    }
}

// ---- trace ----

/// Whether `debug.nvvk.astc_trace` is 1 (read once per process): then every call that touches a
/// compressed image is logged (`ASTC trace: ...`), up to 300 or 3000 lines of each kind.
fn tracing() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| probe::prop_is_one(c"debug.nvvk.astc_trace"))
}

fn trace(msg: impl FnOnce() -> String) {
    if !tracing() {
        return;
    }
    // Each kind of line (its first word) has its own cap, so the many uploads at load do not
    // crowd out the rarer events later.
    static LINES: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    let msg = msg();
    let kind = msg.split(' ').next().unwrap_or("").to_string();
    let cap = if kind == "upload" || kind == "view" {
        300
    } else {
        3000
    };
    let mut lines = LINES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let n = lines.entry(kind).or_insert(0);
    *n += 1;
    if *n <= cap {
        log(2, &format!("ASTC trace: {msg}"));
    }
}

/// The format of an image for the trace: "ASTC_6x6_SRGB", or the app's VkFormat.
fn traced_format(image: vk::Image) -> String {
    let s = state();
    match s.traced.get(&image) {
        Some((format, flags)) => format!("{format:?}/{flags:?}"),
        None => "?".into(),
    }
}

/// The property flags of the memory bound to `buffer`, and whether the app has it mapped.
fn traced_buffer(buffer: vk::Buffer, f: &DeviceFns) -> String {
    let s = state();
    let Some(&(memory, offset)) = s.buffers.get(&buffer) else {
        return "unbound".into();
    };
    let flags = s
        .memory_types
        .get(&memory)
        .map(|&t| f.memory_props.memory_types[t as usize].property_flags);
    format!(
        "memory {memory:?}+{offset} {flags:?} mapped {}",
        s.mappings.contains_key(&memory)
    )
}

fn subresource(r: &vk::ImageSubresourceLayers) -> String {
    format!(
        "mip {} layers {}+{}",
        r.mip_level, r.base_array_layer, r.layer_count
    )
}

/// With `debug.nvvk.astc_dump` 1, writes the first 4000 decoded regions to
/// `/data/local/tmp/nvvk/<n>.bin` (the directory must exist and be writable by the app): a header
/// of 6 little-endian u32 (codec, block width, block height, width, height, row blocks), the
/// app's blocks, then the decoded texels. For comparing the decoder with a reference.
fn dump(c: Compressed, r: &Region, blocks: &[u8], decoded: &[u8]) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static ON: OnceLock<bool> = OnceLock::new();
    static N: AtomicU32 = AtomicU32::new(0);
    if !*ON.get_or_init(|| probe::prop_is_one(c"debug.nvvk.astc_dump")) {
        return;
    }
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n >= 4000 {
        return;
    }
    let codec = match c.codec {
        Codec::Astc => 0u32,
        Codec::Etc2Rgb => 1,
        Codec::Etc2RgbA1 => 2,
        Codec::Etc2Rgba => 3,
        Codec::EacR11 { .. } => 4,
        Codec::EacRg11 { .. } => 5,
    };
    let mut out = Vec::with_capacity(24 + blocks.len() + decoded.len());
    for v in [codec, c.bw, c.bh, r.w, r.h, r.row_blocks] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(blocks);
    out.extend_from_slice(decoded);
    let _ = std::fs::write(format!("/data/local/tmp/nvvk/{n}.bin"), out);
}

// ---- slow-call log ----

/// Calls `f` and logs it when it takes over 50 ms (a wait that times out, a long decode).
fn timed<R: std::fmt::Debug>(what: &str, f: impl FnOnce() -> R) -> R {
    let start = std::time::Instant::now();
    let r = f();
    let ms = start.elapsed().as_millis();
    if ms > 50 {
        log(1, &format!("slow {what}: {ms} ms -> {r:?}"));
    }
    r
}

// ---- instance entry points ----

unsafe extern "system" fn get_physical_device_features(
    physical: vk::PhysicalDevice,
    features: *mut vk::PhysicalDeviceFeatures,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        next_instance!(
            "vkGetPhysicalDeviceFeatures",
            vk::PFN_vkGetPhysicalDeviceFeatures
        )(physical, features);
        (*features).texture_compression_astc_ldr = vk::TRUE;
        (*features).texture_compression_etc2 = vk::TRUE;
    }
}

unsafe extern "system" fn get_physical_device_features2(
    physical: vk::PhysicalDevice,
    features: *mut vk::PhysicalDeviceFeatures2,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        next_instance!(
            "vkGetPhysicalDeviceFeatures2",
            vk::PFN_vkGetPhysicalDeviceFeatures2
        )(physical, features);
        (*features).features.texture_compression_astc_ldr = vk::TRUE;
        (*features).features.texture_compression_etc2 = vk::TRUE;
    }
}

unsafe extern "system" fn get_physical_device_format_properties(
    physical: vk::PhysicalDevice,
    format: vk::Format,
    props: *mut vk::FormatProperties,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        next_instance!(
            "vkGetPhysicalDeviceFormatProperties",
            vk::PFN_vkGetPhysicalDeviceFormatProperties
        )(physical, emulated(format), props);
        if compressed_of(format).is_some() {
            mask_format_properties(&mut *props);
        }
    }
}

unsafe extern "system" fn get_physical_device_format_properties2(
    physical: vk::PhysicalDevice,
    format: vk::Format,
    props: *mut vk::FormatProperties2,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        next_instance!(
            "vkGetPhysicalDeviceFormatProperties2",
            vk::PFN_vkGetPhysicalDeviceFormatProperties2
        )(physical, emulated(format), props);
        if compressed_of(format).is_none() {
            return;
        }
        mask_format_properties(&mut (*props).format_properties);
        let mut s = (*props).p_next as *mut vk::BaseOutStructure;
        while !s.is_null() {
            if (*s).s_type == vk::StructureType::FORMAT_PROPERTIES_3 {
                let p3 = &mut *(s as *mut vk::FormatProperties3);
                p3.linear_tiling_features = vk::FormatFeatureFlags2::empty();
                p3.optimal_tiling_features &=
                    vk::FormatFeatureFlags2::from_raw(u64::from(ASTC_FEATURES.as_raw()));
                p3.buffer_features = vk::FormatFeatureFlags2::empty();
            }
            s = (*s).p_next;
        }
    }
}

unsafe extern "system" fn get_physical_device_image_format_properties(
    physical: vk::PhysicalDevice,
    format: vk::Format,
    ty: vk::ImageType,
    tiling: vk::ImageTiling,
    usage: vk::ImageUsageFlags,
    flags: vk::ImageCreateFlags,
    props: *mut vk::ImageFormatProperties,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        if compressed_of(format).is_some()
            && (tiling != vk::ImageTiling::OPTIMAL || !ASTC_USAGE.contains(usage))
        {
            return vk::Result::ERROR_FORMAT_NOT_SUPPORTED;
        }
        next_instance!(
            "vkGetPhysicalDeviceImageFormatProperties",
            vk::PFN_vkGetPhysicalDeviceImageFormatProperties
        )(
            physical,
            emulated(format),
            ty,
            tiling,
            usage,
            flags & !vk::ImageCreateFlags::BLOCK_TEXEL_VIEW_COMPATIBLE,
            props,
        )
    }
}

unsafe extern "system" fn get_physical_device_image_format_properties2(
    physical: vk::PhysicalDevice,
    info: *const vk::PhysicalDeviceImageFormatInfo2,
    props: *mut vk::ImageFormatProperties2,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let next = next_instance!(
            "vkGetPhysicalDeviceImageFormatProperties2",
            vk::PFN_vkGetPhysicalDeviceImageFormatProperties2
        );
        if compressed_of((*info).format).is_none() {
            return next(physical, info, props);
        }
        if (*info).tiling != vk::ImageTiling::OPTIMAL || !ASTC_USAGE.contains((*info).usage) {
            return vk::Result::ERROR_FORMAT_NOT_SUPPORTED;
        }
        let mut nv = *info;
        nv.format = emulated(nv.format);
        nv.flags &= !vk::ImageCreateFlags::BLOCK_TEXEL_VIEW_COMPATIBLE;
        next(physical, &nv, props)
    }
}

// ---- device entry points ----

/// For the duration of a driver call, points every VkImageFormatListCreateInfo in a chain at a
/// copy of its formats with the ASTC ones replaced (the chain is the app's; it is put back as it
/// was when this is dropped).
struct FormatLists {
    saved: Vec<(
        *mut vk::ImageFormatListCreateInfo,
        *const vk::Format,
        Vec<vk::Format>,
    )>,
}

impl FormatLists {
    /// # Safety
    /// `next` is a valid pNext chain.
    unsafe fn new(next: *const c_void) -> FormatLists {
        // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
        // and pointers), which is what every operation below relies on.
        unsafe {
            let mut saved = Vec::new();
            let mut s = next as *const vk::BaseInStructure;
            while !s.is_null() {
                if (*s).s_type == vk::StructureType::IMAGE_FORMAT_LIST_CREATE_INFO {
                    let list = s as *mut vk::ImageFormatListCreateInfo;
                    let formats: Vec<vk::Format> = (0..(*list).view_format_count as usize)
                        .map(|i| emulated(*(*list).p_view_formats.add(i)))
                        .collect();
                    saved.push((list, (*list).p_view_formats, formats));
                }
                s = (*s).p_next;
            }
            for (list, _, formats) in &saved {
                (**list).p_view_formats = formats.as_ptr();
            }
            FormatLists { saved }
        }
    }

    /// # Safety
    /// `next` is a valid pNext chain.
    unsafe fn have_astc(next: *const c_void) -> bool {
        // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
        // and pointers), which is what every operation below relies on.
        unsafe {
            let mut s = next as *const vk::BaseInStructure;
            while !s.is_null() {
                if (*s).s_type == vk::StructureType::IMAGE_FORMAT_LIST_CREATE_INFO {
                    let list = &*(s as *const vk::ImageFormatListCreateInfo);
                    if (0..list.view_format_count as usize)
                        .any(|i| compressed_of(*list.p_view_formats.add(i)).is_some())
                    {
                        return true;
                    }
                }
                s = (*s).p_next;
            }
            false
        }
    }
}

impl Drop for FormatLists {
    fn drop(&mut self) {
        for (list, original, _) in &self.saved {
            // SAFETY: the app's structure, valid for the call this guard spans.
            unsafe { (**list).p_view_formats = *original };
        }
    }
}

unsafe extern "system" fn create_image(
    device: vk::Device,
    info: *const vk::ImageCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    image: *mut vk::Image,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let astc = compressed_of((*info).format);
        if astc.is_none() && !FormatLists::have_astc((*info).p_next) {
            let result = (f.create_image)(device, info, allocator, image);
            if tracing() && result == vk::Result::SUCCESS {
                let i = &*info;
                state().traced.insert(*image, (i.format, i.flags));
                if i.flags.contains(vk::ImageCreateFlags::MUTABLE_FORMAT) {
                    trace(|| {
                        format!(
                            "image {:?} {:?} {}x{}x{} mips {} layers {} {:?} usage {:?} (mutable)",
                            *image,
                            i.format,
                            i.extent.width,
                            i.extent.height,
                            i.extent.depth,
                            i.mip_levels,
                            i.array_layers,
                            i.flags,
                            i.usage
                        )
                    });
                }
            }
            return result;
        }
        let mut nv = *info;
        nv.format = emulated(nv.format);
        nv.flags &= !vk::ImageCreateFlags::BLOCK_TEXEL_VIEW_COMPATIBLE;
        let lists = FormatLists::new(nv.p_next);
        let result = (f.create_image)(device, &nv, allocator, image);
        drop(lists);
        if tracing() && result == vk::Result::SUCCESS {
            let i = &*info;
            state().traced.insert(*image, (i.format, i.flags));
            if astc.is_some() {
                state().upload_counts.insert(
                    *image,
                    (
                        0,
                        std::time::Instant::now(),
                        format!(
                            "{:?} {}x{} mips {} layers {}",
                            i.format, i.extent.width, i.extent.height, i.mip_levels, i.array_layers
                        ),
                    ),
                );
            }
            trace(|| {
                format!(
                    "image {:?} {:?} {}x{}x{} mips {} layers {} {:?} usage {:?} tiling {:?}{}",
                    *image,
                    i.format,
                    i.extent.width,
                    i.extent.height,
                    i.extent.depth,
                    i.mip_levels,
                    i.array_layers,
                    i.flags,
                    i.usage,
                    i.tiling,
                    if astc.is_none() {
                        " (ASTC in its format list)"
                    } else {
                        ""
                    }
                )
            });
        }
        if result == vk::Result::SUCCESS {
            if let Some(a) = astc {
                static LOGGED: OnceLock<()> = OnceLock::new();
                LOGGED.get_or_init(|| {
                    log(
                        2,
                        "ASTC textures are decoded on the CPU (no ASTC in the driver)",
                    )
                });
                state().images.insert(*image, a);
            }
        }
        result
    }
}

unsafe extern "system" fn destroy_image(
    device: vk::Device,
    image: vk::Image,
    allocator: *const vk::AllocationCallbacks,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        {
            let mut s = state();
            s.images.remove(&image);
            s.upload_counts.remove(&image);
            if s.traced.remove(&image).is_some() {
                for list in s.bindings.values_mut() {
                    list.retain(|b| b.2 != image);
                }
            }
        }
        if let Some(f) = device_fns(device) {
            (f.destroy_image)(device, image, allocator);
        }
    }
}

unsafe extern "system" fn create_image_view(
    device: vk::Device,
    info: *const vk::ImageViewCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    view: *mut vk::ImageView,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        if tracing() {
            let i = &*info;
            let image_astc = emulated_image(i.image).is_some();
            if image_astc || compressed_of(i.format).is_some() {
                trace(|| {
                    format!(
                        "view of image {:?} ({}) as {:?} {:?} mips {}+{} layers {}+{}",
                        i.image,
                        traced_format(i.image),
                        i.format,
                        i.view_type,
                        i.subresource_range.base_mip_level,
                        i.subresource_range.level_count,
                        i.subresource_range.base_array_layer,
                        i.subresource_range.layer_count
                    )
                });
            }
        }
        if compressed_of((*info).format).is_none() {
            return (f.create_image_view)(device, info, allocator, view);
        }
        let mut nv = *info;
        nv.format = emulated(nv.format);
        (f.create_image_view)(device, &nv, allocator, view)
    }
}

unsafe extern "system" fn get_device_image_memory_requirements(
    device: vk::Device,
    info: *const vk::DeviceImageMemoryRequirements,
    req: *mut vk::MemoryRequirements2,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(next) = device_fns(device).and_then(|f| f.get_device_image_memory_requirements)
        else {
            return;
        };
        let create = (*info).p_create_info;
        if create.is_null() || compressed_of((*create).format).is_none() {
            return next(device, info, req);
        }
        let mut image = *create;
        image.format = emulated(image.format);
        image.flags &= !vk::ImageCreateFlags::BLOCK_TEXEL_VIEW_COMPATIBLE;
        let lists = FormatLists::new(image.p_next);
        let mut nv = *info;
        nv.p_create_info = &image;
        next(device, &nv, req);
        drop(lists);
    }
}

unsafe extern "system" fn allocate_memory(
    device: vk::Device,
    info: *const vk::MemoryAllocateInfo,
    allocator: *const vk::AllocationCallbacks,
    memory: *mut vk::DeviceMemory,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = (f.allocate_memory)(device, info, allocator, memory);
        if result == vk::Result::SUCCESS {
            state()
                .memory_types
                .insert(*memory, (*info).memory_type_index);
        }
        result
    }
}

unsafe extern "system" fn free_memory(
    device: vk::Device,
    memory: vk::DeviceMemory,
    allocator: *const vk::AllocationCallbacks,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        {
            let mut s = state();
            s.mappings.remove(&memory);
            s.memory_types.remove(&memory);
            s.bindings.remove(&memory);
        }
        if let Some(f) = device_fns(device) {
            (f.free_memory)(device, memory, allocator);
        }
    }
}

unsafe extern "system" fn map_memory(
    device: vk::Device,
    memory: vk::DeviceMemory,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
    flags: vk::MemoryMapFlags,
    data: *mut *mut c_void,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = (f.map_memory)(device, memory, offset, size, flags, data);
        if result == vk::Result::SUCCESS {
            state()
                .mappings
                .insert(memory, (*data as usize).wrapping_sub(offset as usize));
        }
        result
    }
}

unsafe extern "system" fn unmap_memory(device: vk::Device, memory: vk::DeviceMemory) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        state().mappings.remove(&memory);
        if let Some(f) = device_fns(device) {
            (f.unmap_memory)(device, memory);
        }
    }
}

unsafe extern "system" fn bind_buffer_memory(
    device: vk::Device,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    offset: vk::DeviceSize,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = (f.bind_buffer_memory)(device, buffer, memory, offset);
        if result == vk::Result::SUCCESS {
            state().buffers.insert(buffer, (memory, offset));
        }
        result
    }
}

unsafe extern "system" fn bind_buffer_memory2(
    device: vk::Device,
    count: u32,
    infos: *const vk::BindBufferMemoryInfo,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(next) = device_fns(device).and_then(|f| f.bind_buffer_memory2) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = next(device, count, infos);
        if result == vk::Result::SUCCESS {
            let mut s = state();
            for i in 0..count as usize {
                let b = &*infos.add(i);
                s.buffers.insert(b.buffer, (b.memory, b.memory_offset));
            }
        }
        result
    }
}

unsafe extern "system" fn destroy_buffer(
    device: vk::Device,
    buffer: vk::Buffer,
    allocator: *const vk::AllocationCallbacks,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        state().buffers.remove(&buffer);
        if let Some(f) = device_fns(device) {
            (f.destroy_buffer)(device, buffer, allocator);
        }
    }
}

unsafe extern "system" fn allocate_command_buffers(
    device: vk::Device,
    info: *const vk::CommandBufferAllocateInfo,
    cbs: *mut vk::CommandBuffer,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let result = (f.allocate_command_buffers)(device, info, cbs);
        if result == vk::Result::SUCCESS {
            let mut s = state();
            for i in 0..(*info).command_buffer_count as usize {
                s.cmd_buffers.insert(
                    *cbs.add(i),
                    CommandBuffer {
                        device,
                        pool: (*info).command_pool,
                        uploads: Vec::new(),
                        secondaries: Vec::new(),
                    },
                );
            }
        }
        result
    }
}

unsafe extern "system" fn free_command_buffers(
    device: vk::Device,
    pool: vk::CommandPool,
    count: u32,
    cbs: *const vk::CommandBuffer,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        release_command_buffers(std::slice::from_raw_parts(cbs, count as usize), true);
        if let Some(f) = device_fns(device) {
            (f.free_command_buffers)(device, pool, count, cbs);
        }
    }
}

unsafe extern "system" fn begin_command_buffer(
    cb: vk::CommandBuffer,
    info: *const vk::CommandBufferBeginInfo,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = cmd_device(cb).and_then(device_fns) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        release_command_buffers(&[cb], false);
        (f.begin_command_buffer)(cb, info)
    }
}

unsafe extern "system" fn reset_command_buffer(
    cb: vk::CommandBuffer,
    flags: vk::CommandBufferResetFlags,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = cmd_device(cb).and_then(device_fns) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        release_command_buffers(&[cb], false);
        (f.reset_command_buffer)(cb, flags)
    }
}

unsafe extern "system" fn reset_command_pool(
    device: vk::Device,
    pool: vk::CommandPool,
    flags: vk::CommandPoolResetFlags,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        release_command_buffers(&pool_command_buffers(pool), false);
        (f.reset_command_pool)(device, pool, flags)
    }
}

unsafe extern "system" fn destroy_command_pool(
    device: vk::Device,
    pool: vk::CommandPool,
    allocator: *const vk::AllocationCallbacks,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        release_command_buffers(&pool_command_buffers(pool), true);
        if let Some(f) = device_fns(device) {
            (f.destroy_command_pool)(device, pool, allocator);
        }
    }
}

fn emulated_image(image: vk::Image) -> Option<Compressed> {
    state().images.get(&image).copied()
}

unsafe extern "system" fn cmd_copy_buffer_to_image(
    cb: vk::CommandBuffer,
    src: vk::Buffer,
    dst: vk::Image,
    layout: vk::ImageLayout,
    count: u32,
    regions: *const vk::BufferImageCopy,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(device) = cmd_device(cb) else { return };
        let Some(f) = device_fns(device) else { return };
        let Some(astc) = emulated_image(dst) else {
            return (f.cmd_copy_buffer_to_image)(cb, src, dst, layout, count, regions);
        };
        let regions: Vec<CopyRegion> = std::slice::from_raw_parts(regions, count as usize)
            .iter()
            .map(|r| CopyRegion {
                buffer_offset: r.buffer_offset,
                buffer_row_length: r.buffer_row_length,
                buffer_image_height: r.buffer_image_height,
                image_subresource: r.image_subresource,
                image_offset: r.image_offset,
                image_extent: r.image_extent,
            })
            .collect();
        record_upload(cb, device, &f, src, dst, layout, astc, &regions);
    }
}

unsafe extern "system" fn cmd_copy_buffer_to_image2(
    cb: vk::CommandBuffer,
    info: *const vk::CopyBufferToImageInfo2,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(device) = cmd_device(cb) else { return };
        let Some(f) = device_fns(device) else { return };
        let info = &*info;
        let Some(astc) = emulated_image(info.dst_image) else {
            if let Some(next) = f.cmd_copy_buffer_to_image2 {
                next(cb, info);
            }
            return;
        };
        let regions: Vec<CopyRegion> =
            std::slice::from_raw_parts(info.p_regions, info.region_count as usize)
                .iter()
                .map(|r| CopyRegion {
                    buffer_offset: r.buffer_offset,
                    buffer_row_length: r.buffer_row_length,
                    buffer_image_height: r.buffer_image_height,
                    image_subresource: r.image_subresource,
                    image_offset: r.image_offset,
                    image_extent: r.image_extent,
                })
                .collect();
        record_upload(
            cb,
            device,
            &f,
            info.src_buffer,
            info.dst_image,
            info.dst_image_layout,
            astc,
            &regions,
        );
    }
}

/// Logs a copy or blit between images when one of them is an ASTC image.
fn trace_image_copy(
    what: &str,
    cb: vk::CommandBuffer,
    src: vk::Image,
    dst: vk::Image,
    regions: String,
) {
    if tracing() && (emulated_image(src).is_some() || emulated_image(dst).is_some()) {
        trace(|| {
            format!(
                "{what} cb {cb:?} image {src:?} ({}) -> image {dst:?} ({}): {regions}",
                traced_format(src),
                traced_format(dst)
            )
        });
    }
}

fn image_copy_regions<'a>(
    regions: impl Iterator<
        Item = (
            &'a vk::ImageSubresourceLayers,
            vk::Offset3D,
            &'a vk::ImageSubresourceLayers,
            vk::Offset3D,
            vk::Extent3D,
        ),
    >,
) -> String {
    regions
        .map(|(s, so, d, doff, e)| {
            format!(
                "[{} at {},{},{} -> {} at {},{},{} extent {}x{}x{}]",
                subresource(s),
                so.x,
                so.y,
                so.z,
                subresource(d),
                doff.x,
                doff.y,
                doff.z,
                e.width,
                e.height,
                e.depth
            )
        })
        .collect()
}

unsafe extern "system" fn cmd_copy_image(
    cb: vk::CommandBuffer,
    src: vk::Image,
    src_layout: vk::ImageLayout,
    dst: vk::Image,
    dst_layout: vk::ImageLayout,
    count: u32,
    regions: *const vk::ImageCopy,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = cmd_device(cb).and_then(device_fns) else {
            return;
        };
        if tracing() {
            let r = std::slice::from_raw_parts(regions, count as usize);
            trace_image_copy(
                "vkCmdCopyImage",
                cb,
                src,
                dst,
                image_copy_regions(r.iter().map(|r| {
                    (
                        &r.src_subresource,
                        r.src_offset,
                        &r.dst_subresource,
                        r.dst_offset,
                        r.extent,
                    )
                })),
            );
        }
        (f.cmd_copy_image)(cb, src, src_layout, dst, dst_layout, count, regions)
    }
}

unsafe extern "system" fn cmd_copy_image2(cb: vk::CommandBuffer, info: *const vk::CopyImageInfo2) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(next) = cmd_device(cb)
            .and_then(device_fns)
            .and_then(|f| f.cmd_copy_image2)
        else {
            return;
        };
        if tracing() {
            let i = &*info;
            let r = std::slice::from_raw_parts(i.p_regions, i.region_count as usize);
            trace_image_copy(
                "vkCmdCopyImage2",
                cb,
                i.src_image,
                i.dst_image,
                image_copy_regions(r.iter().map(|r| {
                    (
                        &r.src_subresource,
                        r.src_offset,
                        &r.dst_subresource,
                        r.dst_offset,
                        r.extent,
                    )
                })),
            );
        }
        next(cb, info)
    }
}

fn blit_regions<'a>(
    regions: impl Iterator<
        Item = (
            &'a vk::ImageSubresourceLayers,
            [vk::Offset3D; 2],
            &'a vk::ImageSubresourceLayers,
            [vk::Offset3D; 2],
        ),
    >,
) -> String {
    regions
        .map(|(s, so, d, doff)| {
            format!(
                "[{} {},{}-{},{} -> {} {},{}-{},{}]",
                subresource(s),
                so[0].x,
                so[0].y,
                so[1].x,
                so[1].y,
                subresource(d),
                doff[0].x,
                doff[0].y,
                doff[1].x,
                doff[1].y
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn cmd_blit_image(
    cb: vk::CommandBuffer,
    src: vk::Image,
    src_layout: vk::ImageLayout,
    dst: vk::Image,
    dst_layout: vk::ImageLayout,
    count: u32,
    regions: *const vk::ImageBlit,
    filter: vk::Filter,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = cmd_device(cb).and_then(device_fns) else {
            return;
        };
        if tracing() {
            let r = std::slice::from_raw_parts(regions, count as usize);
            trace_image_copy(
                "vkCmdBlitImage",
                cb,
                src,
                dst,
                blit_regions(r.iter().map(|r| {
                    (
                        &r.src_subresource,
                        r.src_offsets,
                        &r.dst_subresource,
                        r.dst_offsets,
                    )
                })),
            );
        }
        (f.cmd_blit_image)(cb, src, src_layout, dst, dst_layout, count, regions, filter)
    }
}

unsafe extern "system" fn cmd_blit_image2(cb: vk::CommandBuffer, info: *const vk::BlitImageInfo2) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(next) = cmd_device(cb)
            .and_then(device_fns)
            .and_then(|f| f.cmd_blit_image2)
        else {
            return;
        };
        if tracing() {
            let i = &*info;
            let r = std::slice::from_raw_parts(i.p_regions, i.region_count as usize);
            trace_image_copy(
                "vkCmdBlitImage2",
                cb,
                i.src_image,
                i.dst_image,
                blit_regions(r.iter().map(|r| {
                    (
                        &r.src_subresource,
                        r.src_offsets,
                        &r.dst_subresource,
                        r.dst_offsets,
                    )
                })),
            );
        }
        next(cb, info)
    }
}

unsafe extern "system" fn cmd_copy_image_to_buffer(
    cb: vk::CommandBuffer,
    src: vk::Image,
    layout: vk::ImageLayout,
    dst: vk::Buffer,
    count: u32,
    regions: *const vk::BufferImageCopy,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = cmd_device(cb).and_then(device_fns) else {
            return;
        };
        if tracing() && emulated_image(src).is_some() {
            trace(|| {
                format!("vkCmdCopyImageToBuffer cb {cb:?} image {src:?} -> buffer {dst:?} ({count} regions)")
            });
        }
        (f.cmd_copy_image_to_buffer)(cb, src, layout, dst, count, regions)
    }
}

unsafe extern "system" fn cmd_copy_image_to_buffer2(
    cb: vk::CommandBuffer,
    info: *const vk::CopyImageToBufferInfo2,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(next) = cmd_device(cb)
            .and_then(device_fns)
            .and_then(|f| f.cmd_copy_image_to_buffer2)
        else {
            return;
        };
        if tracing() && emulated_image((*info).src_image).is_some() {
            trace(|| {
                format!(
                    "vkCmdCopyImageToBuffer2 cb {cb:?} image {:?}",
                    (*info).src_image
                )
            });
        }
        next(cb, info)
    }
}

unsafe extern "system" fn cmd_execute_commands(
    cb: vk::CommandBuffer,
    count: u32,
    secondaries: *const vk::CommandBuffer,
) {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = cmd_device(cb).and_then(device_fns) else {
            return;
        };
        let list = std::slice::from_raw_parts(secondaries, count as usize);
        {
            let mut s = state();
            let uploads: usize = list
                .iter()
                .filter_map(|c| s.cmd_buffers.get(c))
                .map(|c| c.uploads.len())
                .sum();
            if uploads > 0 {
                trace(|| {
                    format!("vkCmdExecuteCommands cb {cb:?}: {count} secondaries, {uploads} ASTC uploads")
                });
            }
            if let Some(c) = s.cmd_buffers.get_mut(&cb) {
                c.secondaries.extend_from_slice(list);
            }
        }
        (f.cmd_execute_commands)(cb, count, secondaries)
    }
}

unsafe extern "system" fn create_shader_module(
    device: vk::Device,
    info: *const vk::ShaderModuleCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    module: *mut vk::ShaderModule,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        if !invariant_position() {
            return (f.create_shader_module)(device, info, allocator, module);
        }
        let code = std::slice::from_raw_parts((*info).p_code, (*info).code_size / 4);
        let Some(patched) = spirv::invariant_position(code) else {
            return (f.create_shader_module)(device, info, allocator, module);
        };
        static LOGGED: OnceLock<()> = OnceLock::new();
        LOGGED.get_or_init(|| log(2, "vertex shader positions are made invariant"));
        let mut nv = *info;
        nv.code_size = patched.len() * 4;
        nv.p_code = patched.as_ptr();
        (f.create_shader_module)(device, &nv, allocator, module)
    }
}

/// With the trace, every 20 s: the compressed images older than 20 s that were never uploaded.
fn report_idle_images() {
    static LAST: OnceLock<Mutex<std::time::Instant>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| Mutex::new(std::time::Instant::now()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if last.elapsed().as_secs() < 20 {
        return;
    }
    *last = std::time::Instant::now();
    let idle: Vec<String> = state()
        .upload_counts
        .iter()
        .filter(|(_, c)| c.0 == 0 && c.1.elapsed().as_secs() >= 20)
        .map(|(i, c)| format!("{i:?} {}", c.2))
        .collect();
    let total = state().upload_counts.len();
    trace(|| {
        format!(
            "idle: {} of {total} compressed images never uploaded",
            idle.len()
        )
    });
    for line in idle.iter().take(40) {
        trace(|| format!("idle: {line}"));
    }
}

/// Whether vertex shader positions are made invariant (spirv.rs): `debug.nvvk.invariant_position`
/// is 1 (read once).
fn invariant_position() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| probe::prop_is_one(c"debug.nvvk.invariant_position"))
}

/// Logs an image bound where another image already is, when one of them is an ASTC image.
fn trace_image_binding(
    device: vk::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    offset: u64,
) {
    if !tracing() {
        return;
    }
    let Some(f) = device_fns(device) else { return };
    let mut req = vk::MemoryRequirements::default();
    // SAFETY: a live image of this device.
    unsafe { (f.get_image_memory_requirements)(device, image, &mut req) };
    let size = req.size;
    let others = {
        let mut s = state();
        let list = s.bindings.entry(memory).or_default();
        let others: Vec<(u64, u64, vk::Image)> = list
            .iter()
            .filter(|b| b.0 < offset + size && offset < b.0 + b.1)
            .copied()
            .collect();
        list.push((offset, size, image));
        others
    };
    let astc = |i: &vk::Image| emulated_image(*i).is_some();
    if !others.is_empty() && (astc(&image) || others.iter().any(|o| astc(&o.2))) {
        trace(|| {
            let list: Vec<String> = others
                .iter()
                .map(|o| format!("{:?} ({}) +{} size {}", o.2, traced_format(o.2), o.0, o.1))
                .collect();
            format!(
                "alias: image {image:?} ({}) at memory {memory:?}+{offset} size {size} overlaps live {}",
                traced_format(image),
                list.join(", ")
            )
        });
    }
}

unsafe extern "system" fn bind_image_memory(
    device: vk::Device,
    image: vk::Image,
    memory: vk::DeviceMemory,
    offset: vk::DeviceSize,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let r = (f.bind_image_memory)(device, image, memory, offset);
        if r == vk::Result::SUCCESS {
            trace_image_binding(device, image, memory, offset);
        }
        r
    }
}

unsafe extern "system" fn bind_image_memory2(
    device: vk::Device,
    count: u32,
    infos: *const vk::BindImageMemoryInfo,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let Some(next) = device_fns(device).and_then(|f| f.bind_image_memory2) else {
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        };
        let r = next(device, count, infos);
        if r == vk::Result::SUCCESS {
            for b in std::slice::from_raw_parts(infos, count as usize) {
                trace_image_binding(device, b.image, b.memory, b.memory_offset);
            }
        }
        r
    }
}

unsafe extern "system" fn queue_submit(
    queue: vk::Queue,
    count: u32,
    submits: *const vk::SubmitInfo,
    fence: vk::Fence,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let device = nvvk_queue_device(queue);
        let Some(f) = device_fns(device) else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        if tracing() {
            report_idle_images();
        }
        timed("ASTC decode at vkQueueSubmit", || {
            for s in std::slice::from_raw_parts(submits, count as usize) {
                if tracing() && s.command_buffer_count > 0 {
                    let cbs = std::slice::from_raw_parts(
                        s.p_command_buffers,
                        s.command_buffer_count as usize,
                    );
                    let uploads: usize = {
                        let st = state();
                        cbs.iter()
                            .filter_map(|c| st.cmd_buffers.get(c))
                            .map(|c| c.uploads.len())
                            .sum()
                    };
                    if uploads > 0 {
                        trace(|| {
                            format!(
                                "submit: {uploads} uploads in {} command buffers, {} wait semaphores, thread {:?}",
                                cbs.len(),
                                s.wait_semaphore_count,
                                std::thread::current().id()
                            )
                        });
                    }
                }
                if s.command_buffer_count > 0 {
                    run_uploads(
                        device,
                        std::slice::from_raw_parts(
                            s.p_command_buffers,
                            s.command_buffer_count as usize,
                        ),
                    );
                }
            }
        });
        let start = std::time::Instant::now();
        let r = timed("vkQueueSubmit", || {
            (f.queue_submit)(queue, count, submits, fence)
        });
        probe::record(probe::SUBMIT, start, r);
        r
    }
}

unsafe extern "system" fn queue_submit2(
    queue: vk::Queue,
    count: u32,
    submits: *const vk::SubmitInfo2,
    fence: vk::Fence,
) -> vk::Result {
    // SAFETY: the caller keeps the Vulkan valid-usage rules of this call (valid handles
    // and pointers), which is what every operation below relies on.
    unsafe {
        let device = nvvk_queue_device(queue);
        let Some(next) = device_fns(device).and_then(|f| f.queue_submit2) else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        for s in std::slice::from_raw_parts(submits, count as usize) {
            if s.command_buffer_info_count > 0 {
                let cbs: Vec<vk::CommandBuffer> = std::slice::from_raw_parts(
                    s.p_command_buffer_infos,
                    s.command_buffer_info_count as usize,
                )
                .iter()
                .map(|i| i.command_buffer)
                .collect();
                run_uploads(device, &cbs);
            }
        }
        next(queue, count, submits, fence)
    }
}

unsafe extern "system" fn wait_for_fences(
    device: vk::Device,
    count: u32,
    fences: *const vk::Fence,
    wait_all: vk::Bool32,
    timeout: u64,
) -> vk::Result {
    let Some(f) = device_fns(device) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    timed(
        &format!("vkWaitForFences({count} fences, timeout {timeout} ns)"),
        || {
            // SAFETY: the app's call, passed on unchanged.
            unsafe { (f.wait_for_fences)(device, count, fences, wait_all, timeout) }
        },
    )
}

unsafe extern "system" fn wait_semaphores(
    device: vk::Device,
    info: *const vk::SemaphoreWaitInfo,
    timeout: u64,
) -> vk::Result {
    let Some(next) = device_fns(device).and_then(|f| f.wait_semaphores) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    timed(&format!("vkWaitSemaphores(timeout {timeout} ns)"), || {
        // SAFETY: the app's call, passed on unchanged.
        unsafe { next(device, info, timeout) }
    })
}

unsafe extern "system" fn queue_wait_idle(queue: vk::Queue) -> vk::Result {
    // SAFETY: a queue the HAL has seen.
    let device = unsafe { nvvk_queue_device(queue) };
    let Some(f) = device_fns(device) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    // SAFETY: the app's call, passed on unchanged.
    timed("vkQueueWaitIdle", || unsafe { (f.queue_wait_idle)(queue) })
}

unsafe extern "system" fn device_wait_idle(device: vk::Device) -> vk::Result {
    let Some(f) = device_fns(device) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    // SAFETY: the app's call, passed on unchanged.
    timed("vkDeviceWaitIdle", || unsafe {
        (f.device_wait_idle)(device)
    })
}

// ---- lookup ----

/// This module's entry point of a Vulkan function, or null: the HAL's vkGet*ProcAddr ask here
/// first.
///
/// # Safety
/// `name` is null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn nvvk_astc_proc(name: *const c_char) -> *const c_void {
    if name.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the loader passes a NUL-terminated name.
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
        return std::ptr::null();
    };
    let p = probe::proc(name);
    if !p.is_null() {
        return p;
    }
    macro_rules! table {
        ($($n:literal => $f:expr),+ $(,)?) => {
            match name { $($n => $f as *const c_void,)+ _ => std::ptr::null() }
        };
    }
    table! {
        "vkGetPhysicalDeviceFeatures" => get_physical_device_features as vk::PFN_vkGetPhysicalDeviceFeatures,
        "vkGetPhysicalDeviceFeatures2" => get_physical_device_features2 as vk::PFN_vkGetPhysicalDeviceFeatures2,
        "vkGetPhysicalDeviceFeatures2KHR" => get_physical_device_features2 as vk::PFN_vkGetPhysicalDeviceFeatures2,
        "vkGetPhysicalDeviceFormatProperties" => get_physical_device_format_properties as vk::PFN_vkGetPhysicalDeviceFormatProperties,
        "vkGetPhysicalDeviceFormatProperties2" => get_physical_device_format_properties2 as vk::PFN_vkGetPhysicalDeviceFormatProperties2,
        "vkGetPhysicalDeviceFormatProperties2KHR" => get_physical_device_format_properties2 as vk::PFN_vkGetPhysicalDeviceFormatProperties2,
        "vkGetPhysicalDeviceImageFormatProperties" => get_physical_device_image_format_properties as vk::PFN_vkGetPhysicalDeviceImageFormatProperties,
        "vkGetPhysicalDeviceImageFormatProperties2" => get_physical_device_image_format_properties2 as vk::PFN_vkGetPhysicalDeviceImageFormatProperties2,
        "vkGetPhysicalDeviceImageFormatProperties2KHR" => get_physical_device_image_format_properties2 as vk::PFN_vkGetPhysicalDeviceImageFormatProperties2,
        "vkCreateImage" => create_image as vk::PFN_vkCreateImage,
        "vkDestroyImage" => destroy_image as vk::PFN_vkDestroyImage,
        "vkCreateImageView" => create_image_view as vk::PFN_vkCreateImageView,
        "vkGetDeviceImageMemoryRequirements" => get_device_image_memory_requirements as vk::PFN_vkGetDeviceImageMemoryRequirements,
        "vkGetDeviceImageMemoryRequirementsKHR" => get_device_image_memory_requirements as vk::PFN_vkGetDeviceImageMemoryRequirements,
        "vkAllocateMemory" => allocate_memory as vk::PFN_vkAllocateMemory,
        "vkFreeMemory" => free_memory as vk::PFN_vkFreeMemory,
        "vkMapMemory" => map_memory as vk::PFN_vkMapMemory,
        "vkUnmapMemory" => unmap_memory as vk::PFN_vkUnmapMemory,
        "vkBindBufferMemory" => bind_buffer_memory as vk::PFN_vkBindBufferMemory,
        "vkBindBufferMemory2" => bind_buffer_memory2 as vk::PFN_vkBindBufferMemory2,
        "vkBindBufferMemory2KHR" => bind_buffer_memory2 as vk::PFN_vkBindBufferMemory2,
        "vkDestroyBuffer" => destroy_buffer as vk::PFN_vkDestroyBuffer,
        "vkAllocateCommandBuffers" => allocate_command_buffers as vk::PFN_vkAllocateCommandBuffers,
        "vkFreeCommandBuffers" => free_command_buffers as vk::PFN_vkFreeCommandBuffers,
        "vkBeginCommandBuffer" => begin_command_buffer as vk::PFN_vkBeginCommandBuffer,
        "vkResetCommandBuffer" => reset_command_buffer as vk::PFN_vkResetCommandBuffer,
        "vkResetCommandPool" => reset_command_pool as vk::PFN_vkResetCommandPool,
        "vkDestroyCommandPool" => destroy_command_pool as vk::PFN_vkDestroyCommandPool,
        "vkCmdCopyBufferToImage" => cmd_copy_buffer_to_image as vk::PFN_vkCmdCopyBufferToImage,
        "vkCmdCopyBufferToImage2" => cmd_copy_buffer_to_image2 as vk::PFN_vkCmdCopyBufferToImage2,
        "vkCmdCopyBufferToImage2KHR" => cmd_copy_buffer_to_image2 as vk::PFN_vkCmdCopyBufferToImage2,
        "vkCmdCopyImage" => cmd_copy_image as vk::PFN_vkCmdCopyImage,
        "vkCmdCopyImage2" => cmd_copy_image2 as vk::PFN_vkCmdCopyImage2,
        "vkCmdCopyImage2KHR" => cmd_copy_image2 as vk::PFN_vkCmdCopyImage2,
        "vkCmdBlitImage" => cmd_blit_image as vk::PFN_vkCmdBlitImage,
        "vkCmdBlitImage2" => cmd_blit_image2 as vk::PFN_vkCmdBlitImage2,
        "vkCmdBlitImage2KHR" => cmd_blit_image2 as vk::PFN_vkCmdBlitImage2,
        "vkCmdCopyImageToBuffer" => cmd_copy_image_to_buffer as vk::PFN_vkCmdCopyImageToBuffer,
        "vkCmdCopyImageToBuffer2" => cmd_copy_image_to_buffer2 as vk::PFN_vkCmdCopyImageToBuffer2,
        "vkCmdCopyImageToBuffer2KHR" => cmd_copy_image_to_buffer2 as vk::PFN_vkCmdCopyImageToBuffer2,
        "vkCmdExecuteCommands" => cmd_execute_commands as vk::PFN_vkCmdExecuteCommands,
        "vkCreateShaderModule" => create_shader_module as vk::PFN_vkCreateShaderModule,
        "vkBindImageMemory" => bind_image_memory as vk::PFN_vkBindImageMemory,
        "vkBindImageMemory2" => bind_image_memory2 as vk::PFN_vkBindImageMemory2,
        "vkBindImageMemory2KHR" => bind_image_memory2 as vk::PFN_vkBindImageMemory2,
        "vkQueueSubmit" => queue_submit as vk::PFN_vkQueueSubmit,
        "vkQueueSubmit2" => queue_submit2 as vk::PFN_vkQueueSubmit2,
        "vkQueueSubmit2KHR" => queue_submit2 as vk::PFN_vkQueueSubmit2,
        "vkWaitForFences" => wait_for_fences as vk::PFN_vkWaitForFences,
        "vkWaitSemaphores" => wait_semaphores as vk::PFN_vkWaitSemaphores,
        "vkWaitSemaphoresKHR" => wait_semaphores as vk::PFN_vkWaitSemaphores,
        "vkQueueWaitIdle" => queue_wait_idle as vk::PFN_vkQueueWaitIdle,
        "vkDeviceWaitIdle" => device_wait_idle as vk::PFN_vkDeviceWaitIdle,
    }
}
