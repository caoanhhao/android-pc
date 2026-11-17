// Copyright 2026 The Android PC Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! With `debug.nvvk.astc_trace` 1 and `debug.nvvk.trace_shader` set to a SPIR-V name (e.g.
//! `sWaterMapTextureArraySampler`): for the first draws with a pipeline whose shaders have that
//! name, logs every named image the shaders read and the image bound there (format, size,
//! layers, mips, usage). For finding which texture a misdrawn effect samples (Phase L4).
//!
//! The entry points here are only handed out while tracing (`proc`).

use crate::{fn_ptr, resolve, trace, tracing};
use ash::vk;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// A shader variable that reads an image: its name, descriptor set and binding.
#[derive(Clone)]
struct Var {
    name: String,
    set: u32,
    binding: u32,
}

#[derive(Default)]
struct State {
    /// Modules that have the traced name, with their image variables.
    modules: HashMap<vk::ShaderModule, Vec<Var>>,
    pipelines: HashMap<vk::Pipeline, Vec<Var>>,
    /// view -> (image, view format, view type)
    views: HashMap<vk::ImageView, (vk::Image, vk::Format, vk::ImageViewType)>,
    /// image -> description, and (format, width, height, layers, mips) for a readback
    images: HashMap<vk::Image, String>,
    infos: HashMap<vk::Image, (vk::Format, u32, u32, u32, u32)>,
    /// images a traced draw reads, by variable name, to read back once after a while
    dump: Vec<(String, vk::Image)>,
    dump_after: u32,
    dumped: bool,
    /// (set, binding) -> view
    sets: HashMap<(vk::DescriptorSet, u32), vk::ImageView>,
    /// template -> its image entries (binding, offset, stride, count)
    templates: HashMap<vk::DescriptorUpdateTemplate, Vec<(u32, usize, usize, u32)>>,
    /// command buffer -> (bound graphics pipeline, bound sets by index)
    cbs: HashMap<vk::CommandBuffer, (vk::Pipeline, HashMap<u32, vk::DescriptorSet>)>,
    logged: u32,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The traced SPIR-V name (`debug.nvvk.trace_shader`), read once.
fn wanted() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| crate::probe::prop_string(c"debug.nvvk.trace_shader"))
}

/// The image variables of a module if it has the traced name.
fn scan(code: &[u32]) -> Option<Vec<Var>> {
    let want = wanted();
    if want.is_empty() || code.len() < 5 {
        return None;
    }
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut sets: HashMap<u32, u32> = HashMap::new();
    let mut bindings: HashMap<u32, u32> = HashMap::new();
    let mut i = 5;
    while i < code.len() {
        let (len, op) = ((code[i] >> 16) as usize, code[i] & 0xffff);
        if len == 0 || i + len > code.len() {
            return None;
        }
        let w = &code[i..i + len];
        match op {
            5 if len >= 3 => {
                let bytes: Vec<u8> = w[2..].iter().flat_map(|x| x.to_le_bytes()).collect();
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                names.insert(w[1], String::from_utf8_lossy(&bytes[..end]).into_owned());
            }
            71 if len >= 4 && w[2] == 34 => {
                sets.insert(w[1], w[3]);
            }
            71 if len >= 4 && w[2] == 33 => {
                bindings.insert(w[1], w[3]);
            }
            _ => {}
        }
        i += len;
    }
    if !names.values().any(|n| n == want) {
        return None;
    }
    let mut vars: Vec<Var> = bindings
        .iter()
        .filter_map(|(id, &binding)| {
            Some(Var {
                name: names.get(id)?.clone(),
                set: *sets.get(id)?,
                binding,
            })
        })
        .collect();
    vars.sort_by_key(|v| (v.set, v.binding));
    Some(vars)
}

macro_rules! next {
    ($device:expr, $name:literal, $ty:ty) => {{
        static P: OnceLock<usize> = OnceLock::new();
        let p = *P.get_or_init(|| resolve($device, &[$name]));
        // The entry point of this name, whose type is $ty (used inside unsafe blocks).
        fn_ptr::<$ty>(p)
    }};
}

unsafe extern "system" fn create_shader_module(
    device: vk::Device,
    info: *const vk::ShaderModuleCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    module: *mut vk::ShaderModule,
) -> vk::Result {
    // SAFETY: the app's valid create info and output.
    unsafe {
        let code = std::slice::from_raw_parts((*info).p_code, (*info).code_size / 4);
        let vars = scan(code);
        static NAN: OnceLock<u8> = OnceLock::new();
        let mode = *NAN.get_or_init(|| {
            if crate::probe::prop_is_one(c"debug.nvvk.nan_debug") {
                1
            } else if crate::probe::prop_is_two(c"debug.nvvk.nan_debug") {
                2
            } else {
                0
            }
        });
        let patched = if vars.is_some() && mode != 0 {
            crate::spirv::nan_to_magenta(code, mode == 2)
        } else {
            None
        };
        let r = match &patched {
            Some(p) => {
                trace(|| "nan_debug: a traced fragment shader shows NaN as magenta".into());
                let mut nv = *info;
                nv.code_size = p.len() * 4;
                nv.p_code = p.as_ptr();
                crate::create_shader_module(device, &nv, allocator, module)
            }
            None => crate::create_shader_module(device, info, allocator, module),
        };
        if r == vk::Result::SUCCESS {
            if let Some(vars) = vars {
                state().modules.insert(*module, vars);
            }
        }
        r
    }
}

unsafe extern "system" fn create_graphics_pipelines(
    device: vk::Device,
    cache: vk::PipelineCache,
    count: u32,
    infos: *const vk::GraphicsPipelineCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    pipelines: *mut vk::Pipeline,
) -> vk::Result {
    // SAFETY: the app's valid create infos and outputs.
    unsafe {
        let r = crate::create_graphics_pipelines(device, cache, count, infos, allocator, pipelines);
        if r == vk::Result::SUCCESS {
            let mut s = state();
            for (k, i) in std::slice::from_raw_parts(infos, count as usize)
                .iter()
                .enumerate()
            {
                let mut vars = Vec::new();
                for st in std::slice::from_raw_parts(i.p_stages, i.stage_count as usize) {
                    if let Some(v) = s.modules.get(&st.module) {
                        vars.extend(v.iter().cloned());
                    }
                }
                if !vars.is_empty() {
                    s.pipelines.insert(*pipelines.add(k), vars);
                }
            }
        }
        r
    }
}

unsafe extern "system" fn create_image(
    device: vk::Device,
    info: *const vk::ImageCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    image: *mut vk::Image,
) -> vk::Result {
    // SAFETY: the app's valid create info and output.
    unsafe {
        let r = crate::create_image(device, info, allocator, image);
        if r == vk::Result::SUCCESS {
            let i = &*info;
            state().infos.insert(
                *image,
                (
                    i.format,
                    i.extent.width,
                    i.extent.height,
                    i.array_layers,
                    i.mip_levels,
                ),
            );
            state().images.insert(
                *image,
                format!(
                    "{:?} {}x{} layers {} mips {} usage {:?} flags {:?}",
                    i.format,
                    i.extent.width,
                    i.extent.height,
                    i.array_layers,
                    i.mip_levels,
                    i.usage,
                    i.flags
                ),
            );
        }
        r
    }
}

unsafe extern "system" fn create_image_view(
    device: vk::Device,
    info: *const vk::ImageViewCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    view: *mut vk::ImageView,
) -> vk::Result {
    // SAFETY: the app's valid create info and output.
    unsafe {
        let r = crate::create_image_view(device, info, allocator, view);
        if r == vk::Result::SUCCESS {
            let i = &*info;
            state()
                .views
                .insert(*view, (i.image, i.format, i.view_type));
        }
        r
    }
}

fn is_image(t: vk::DescriptorType) -> bool {
    matches!(
        t,
        vk::DescriptorType::COMBINED_IMAGE_SAMPLER
            | vk::DescriptorType::SAMPLED_IMAGE
            | vk::DescriptorType::STORAGE_IMAGE
            | vk::DescriptorType::INPUT_ATTACHMENT
    )
}

unsafe extern "system" fn update_descriptor_sets(
    device: vk::Device,
    write_count: u32,
    writes: *const vk::WriteDescriptorSet,
    copy_count: u32,
    copies: *const vk::CopyDescriptorSet,
) {
    // SAFETY: the app's valid writes and copies.
    unsafe {
        {
            let mut s = state();
            for w in std::slice::from_raw_parts(writes, write_count as usize) {
                if is_image(w.descriptor_type) && !w.p_image_info.is_null() {
                    for k in 0..w.descriptor_count {
                        let info = &*w.p_image_info.add(k as usize);
                        s.sets.insert(
                            (w.dst_set, w.dst_binding + w.dst_array_element + k),
                            info.image_view,
                        );
                    }
                }
            }
        }
        next!(
            device,
            "vkUpdateDescriptorSets",
            vk::PFN_vkUpdateDescriptorSets
        )(device, write_count, writes, copy_count, copies)
    }
}

unsafe extern "system" fn create_descriptor_update_template(
    device: vk::Device,
    info: *const vk::DescriptorUpdateTemplateCreateInfo,
    allocator: *const vk::AllocationCallbacks,
    template: *mut vk::DescriptorUpdateTemplate,
) -> vk::Result {
    // SAFETY: the app's valid create info and output.
    unsafe {
        let r = next!(
            device,
            "vkCreateDescriptorUpdateTemplate",
            vk::PFN_vkCreateDescriptorUpdateTemplate
        )(device, info, allocator, template);
        if r == vk::Result::SUCCESS {
            let i = &*info;
            let entries = std::slice::from_raw_parts(
                i.p_descriptor_update_entries,
                i.descriptor_update_entry_count as usize,
            )
            .iter()
            .filter(|e| is_image(e.descriptor_type))
            .map(|e| {
                (
                    e.dst_binding + e.dst_array_element,
                    e.offset,
                    e.stride,
                    e.descriptor_count,
                )
            })
            .collect();
            state().templates.insert(*template, entries);
        }
        r
    }
}

unsafe extern "system" fn update_descriptor_set_with_template(
    device: vk::Device,
    set: vk::DescriptorSet,
    template: vk::DescriptorUpdateTemplate,
    data: *const c_void,
) {
    // SAFETY: the app's valid template data.
    unsafe {
        {
            let mut s = state();
            if let Some(entries) = s.templates.get(&template).cloned() {
                for (binding, offset, stride, count) in entries {
                    for k in 0..count {
                        let p = (data as *const u8).add(offset + stride * k as usize)
                            as *const vk::DescriptorImageInfo;
                        s.sets.insert((set, binding + k), (*p).image_view);
                    }
                }
            }
        }
        next!(
            device,
            "vkUpdateDescriptorSetWithTemplate",
            vk::PFN_vkUpdateDescriptorSetWithTemplate
        )(device, set, template, data)
    }
}

unsafe extern "system" fn cmd_bind_pipeline(
    cb: vk::CommandBuffer,
    point: vk::PipelineBindPoint,
    pipeline: vk::Pipeline,
) {
    // SAFETY: the app's valid call.
    unsafe {
        let Some(device) = crate::cmd_device(cb) else {
            return;
        };
        if point == vk::PipelineBindPoint::GRAPHICS {
            state().cbs.entry(cb).or_default().0 = pipeline;
        }
        next!(device, "vkCmdBindPipeline", vk::PFN_vkCmdBindPipeline)(cb, point, pipeline)
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn cmd_bind_descriptor_sets(
    cb: vk::CommandBuffer,
    point: vk::PipelineBindPoint,
    layout: vk::PipelineLayout,
    first: u32,
    count: u32,
    sets: *const vk::DescriptorSet,
    dynamic_count: u32,
    dynamic: *const u32,
) {
    // SAFETY: the app's valid call.
    unsafe {
        let Some(device) = crate::cmd_device(cb) else {
            return;
        };
        if point == vk::PipelineBindPoint::GRAPHICS {
            let mut s = state();
            let e = s.cbs.entry(cb).or_default();
            for k in 0..count {
                e.1.insert(first + k, *sets.add(k as usize));
            }
        }
        next!(
            device,
            "vkCmdBindDescriptorSets",
            vk::PFN_vkCmdBindDescriptorSets
        )(
            cb,
            point,
            layout,
            first,
            count,
            sets,
            dynamic_count,
            dynamic,
        )
    }
}

/// Logs what a draw with a traced pipeline reads (the first 40 such draws).
fn log_draw(cb: vk::CommandBuffer) {
    let mut s = state();
    if s.logged >= 40 {
        return;
    }
    let Some((pipeline, sets)) = s.cbs.get(&cb).cloned() else {
        return;
    };
    let Some(vars) = s.pipelines.get(&pipeline).cloned() else {
        return;
    };
    s.logged += 1;
    let n = s.logged;
    for v in vars {
        let what = sets
            .get(&v.set)
            .and_then(|set| s.sets.get(&(*set, v.binding)))
            .map(|view| match s.views.get(view) {
                Some((image, format, ty)) => format!(
                    "view {format:?} {ty:?} of {}",
                    s.images.get(image).cloned().unwrap_or_else(|| "?".into())
                ),
                None => "unknown view".into(),
            })
            .unwrap_or_else(|| "nothing bound".into());
        trace(|| {
            format!(
                "draw {n}: {} (set {} binding {}): {what}",
                v.name, v.set, v.binding
            )
        });
        let image = sets
            .get(&v.set)
            .and_then(|set| s.sets.get(&(*set, v.binding)))
            .and_then(|view| s.views.get(view))
            .map(|v| v.0);
        if let Some(image) = image {
            if !s.dump.iter().any(|d| d.1 == image) {
                s.dump.push((v.name.clone(), image));
            }
        }
    }
}

unsafe extern "system" fn cmd_draw_indexed(
    cb: vk::CommandBuffer,
    index_count: u32,
    instance_count: u32,
    first_index: u32,
    vertex_offset: i32,
    first_instance: u32,
) {
    // SAFETY: the app's valid call.
    unsafe {
        let Some(device) = crate::cmd_device(cb) else {
            return;
        };
        log_draw(cb);
        next!(device, "vkCmdDrawIndexed", vk::PFN_vkCmdDrawIndexed)(
            cb,
            index_count,
            instance_count,
            first_index,
            vertex_offset,
            first_instance,
        )
    }
}

unsafe extern "system" fn cmd_draw(
    cb: vk::CommandBuffer,
    vertex_count: u32,
    instance_count: u32,
    first_vertex: u32,
    first_instance: u32,
) {
    // SAFETY: the app's valid call.
    unsafe {
        let Some(device) = crate::cmd_device(cb) else {
            return;
        };
        log_draw(cb);
        next!(device, "vkCmdDraw", vk::PFN_vkCmdDraw)(
            cb,
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        )
    }
}

/// Bytes per texel of the formats a readback handles.
fn texel_bytes(format: vk::Format) -> Option<u64> {
    Some(match format {
        vk::Format::R8G8B8A8_UNORM
        | vk::Format::R8G8B8A8_SRGB
        | vk::Format::B8G8R8A8_UNORM
        | vk::Format::R32_SFLOAT
        | vk::Format::B10G11R11_UFLOAT_PACK32
        | vk::Format::A2B10G10R10_UNORM_PACK32
        | vk::Format::R16G16_UNORM
        | vk::Format::R16G16_SNORM => 4,
        vk::Format::R16G16B16A16_SFLOAT => 8,
        vk::Format::R16_UNORM | vk::Format::R16_SNORM | vk::Format::R16_SFLOAT => 2,
        vk::Format::R8_UNORM => 1,
        _ => return None,
    })
}

/// After the app's submits: some 300 submits after the first traced draw, copies every mip and
/// layer of the images it read to `/data/local/tmp/nvvk/img_<name>_<format>_m<mip>_l<layer>.raw`
/// (width and height in the name), once.
pub(crate) fn after_submit(queue: vk::Queue, device: vk::Device) {
    if !tracing() || wanted().is_empty() {
        return;
    }
    let images: Vec<(String, vk::Image, (vk::Format, u32, u32, u32, u32))> = {
        let mut s = state();
        if s.dumped || s.dump.is_empty() {
            return;
        }
        s.dump_after += 1;
        if s.dump_after < 100 {
            return;
        }
        s.dumped = true;
        s.dump
            .iter()
            .filter_map(|(n, i)| s.infos.get(i).map(|info| (n.clone(), *i, *info)))
            .collect()
    };
    // SAFETY: the HAL's own objects on the app's device and queue; the queue is idle when the
    // copies run (vkQueueWaitIdle before and after).
    unsafe {
        let wait = next!(device, "vkQueueWaitIdle", vk::PFN_vkQueueWaitIdle);
        let _ = wait(queue);
        for (name, image, (format, w, h, layers, mips)) in images {
            let real = crate::emulated(format);
            let Some(tb) = texel_bytes(real) else {
                trace(|| format!("readback: {name} {format:?} skipped"));
                continue;
            };
            let mut regions = Vec::new();
            let mut size = 0u64;
            for m in 0..mips {
                let (mw, mh) = ((w >> m).max(1), (h >> m).max(1));
                for l in 0..layers {
                    regions.push((
                        m,
                        l,
                        mw,
                        mh,
                        vk::BufferImageCopy {
                            buffer_offset: size,
                            buffer_row_length: 0,
                            buffer_image_height: 0,
                            image_subresource: vk::ImageSubresourceLayers {
                                aspect_mask: vk::ImageAspectFlags::COLOR,
                                mip_level: m,
                                base_array_layer: l,
                                layer_count: 1,
                            },
                            image_offset: vk::Offset3D::default(),
                            image_extent: vk::Extent3D {
                                width: mw,
                                height: mh,
                                depth: 1,
                            },
                        },
                    ));
                    size += u64::from(mw) * u64::from(mh) * tb;
                }
            }
            let Some(f) = crate::device_fns(device) else {
                return;
            };
            let Some((buffer, memory, ptr)) = crate::create_staging_dst(device, &f, size) else {
                continue;
            };
            let pool_info = vk::CommandPoolCreateInfo {
                queue_family_index: 0,
                ..Default::default()
            };
            let mut pool = vk::CommandPool::null();
            let _ = next!(device, "vkCreateCommandPool", vk::PFN_vkCreateCommandPool)(
                device,
                &pool_info,
                std::ptr::null(),
                &mut pool,
            );
            let alloc = vk::CommandBufferAllocateInfo {
                command_pool: pool,
                level: vk::CommandBufferLevel::PRIMARY,
                command_buffer_count: 1,
                ..Default::default()
            };
            let mut cb = vk::CommandBuffer::null();
            let _ = (f.allocate_command_buffers)(device, &alloc, &mut cb);
            let begin = vk::CommandBufferBeginInfo::default();
            let _ = (f.begin_command_buffer)(cb, &begin);
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mips,
                base_array_layer: 0,
                layer_count: layers,
            };
            let barrier = |old, new| vk::ImageMemoryBarrier {
                src_access_mask: vk::AccessFlags::MEMORY_WRITE,
                dst_access_mask: vk::AccessFlags::MEMORY_READ,
                old_layout: old,
                new_layout: new,
                src_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                dst_queue_family_index: vk::QUEUE_FAMILY_IGNORED,
                image,
                subresource_range: range,
                ..Default::default()
            };
            let pb = next!(device, "vkCmdPipelineBarrier", vk::PFN_vkCmdPipelineBarrier);
            let b = barrier(
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            );
            pb(
                cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &b,
            );
            let copies: Vec<vk::BufferImageCopy> = regions.iter().map(|r| r.4).collect();
            next!(
                device,
                "vkCmdCopyImageToBuffer",
                vk::PFN_vkCmdCopyImageToBuffer
            )(
                cb,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                copies.len() as u32,
                copies.as_ptr(),
            );
            let b = barrier(
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            );
            pb(
                cb,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &b,
            );
            let _ = next!(device, "vkEndCommandBuffer", vk::PFN_vkEndCommandBuffer)(cb);
            let submit = vk::SubmitInfo {
                command_buffer_count: 1,
                p_command_buffers: &cb,
                ..Default::default()
            };
            let _ = (f.queue_submit)(queue, 1, &submit, vk::Fence::null());
            let _ = wait(queue);
            for (m, l, mw, mh, c) in &regions {
                let len = (u64::from(*mw) * u64::from(*mh) * tb) as usize;
                let data =
                    std::slice::from_raw_parts((ptr as u64 + c.buffer_offset) as *const u8, len);
                let _ = std::fs::write(
                    format!("/data/local/tmp/nvvk/img_{name}_{real:?}_{mw}x{mh}_m{m}_l{l}.raw"),
                    data,
                );
            }
            trace(|| format!("readback: {name} {format:?} {w}x{h} layers {layers} mips {mips}"));
            (f.free_command_buffers)(device, pool, 1, &cb);
            (f.destroy_command_pool)(device, pool, std::ptr::null());
            (f.free_memory)(device, memory, std::ptr::null());
            (f.destroy_buffer)(device, buffer, std::ptr::null());
        }
    }
}

/// This module's entry point of `name` while a shader is traced, or null.
pub(crate) fn proc(name: &str) -> *const c_void {
    if !tracing() || wanted().is_empty() {
        return std::ptr::null();
    }
    macro_rules! table {
        ($($n:literal => $f:expr),+ $(,)?) => {
            match name { $($n => $f as *const c_void,)+ _ => std::ptr::null() }
        };
    }
    table! {
        "vkCreateShaderModule" => create_shader_module as vk::PFN_vkCreateShaderModule,
        "vkCreateGraphicsPipelines" => create_graphics_pipelines as vk::PFN_vkCreateGraphicsPipelines,
        "vkCreateImage" => create_image as vk::PFN_vkCreateImage,
        "vkCreateImageView" => create_image_view as vk::PFN_vkCreateImageView,
        "vkUpdateDescriptorSets" => update_descriptor_sets as vk::PFN_vkUpdateDescriptorSets,
        "vkCreateDescriptorUpdateTemplate" => create_descriptor_update_template as vk::PFN_vkCreateDescriptorUpdateTemplate,
        "vkCreateDescriptorUpdateTemplateKHR" => create_descriptor_update_template as vk::PFN_vkCreateDescriptorUpdateTemplate,
        "vkUpdateDescriptorSetWithTemplate" => update_descriptor_set_with_template as vk::PFN_vkUpdateDescriptorSetWithTemplate,
        "vkUpdateDescriptorSetWithTemplateKHR" => update_descriptor_set_with_template as vk::PFN_vkUpdateDescriptorSetWithTemplate,
        "vkCmdBindPipeline" => cmd_bind_pipeline as vk::PFN_vkCmdBindPipeline,
        "vkCmdBindDescriptorSets" => cmd_bind_descriptor_sets as vk::PFN_vkCmdBindDescriptorSets,
        "vkCmdDraw" => cmd_draw as vk::PFN_vkCmdDraw,
        "vkCmdDrawIndexed" => cmd_draw_indexed as vk::PFN_vkCmdDrawIndexed,
    }
}
