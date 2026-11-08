//! Input-texture upload + mip-chain construction.
//!
//! Level 0 is converted RGB→RGBA in parallel (bit-identical to the former serial loop). Levels
//! 1.. are built either by a compute shader (`mips.wgsl`, one dispatch per level, one submit) or by
//! the CPU box filter `downsample_rgba_f32` (fallback, `DARKROOM_CPU_MIPS` set, or no compute
//! pipeline). The GPU path replicates the CPU arithmetic except for f64 accumulation (WGSL has none).

use crate::backend::{downsample_rgba_f32, mip_count_for, GpuContext};
use crate::error::PipelineError;
use core_raw::LinearImage;
use rayon::prelude::*;
use wgpu::util::DeviceExt;

/// Compute pipeline that downsamples mip level L-1 into level L.
pub(crate) struct MipGenerator {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Dims {
    src: [u32; 2],
    dst: [u32; 2],
}

impl MipGenerator {
    /// `None` when the device rejects the pipeline (caller falls back to the CPU chain).
    pub(crate) fn new(device: &wgpu::Device) -> Option<Self> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mips-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mips.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mips-bgl"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
                entry(
                    2,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mips-pl"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mips-pipeline"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(err) = pollster::block_on(scope.pop()) {
            tracing::warn!("GPU mip generator unavailable, using CPU mips: {err}");
            return None;
        }
        Some(Self { pipeline, layout })
    }

    /// Record one compute pass per level 1..`mips` into `encoder`.
    fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        tex: &wgpu::Texture,
        (w, h): (u32, u32),
        mips: u32,
    ) {
        let view = |level| {
            tex.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let (mut sw, mut sh) = (w, h);
        for level in 1..mips {
            let (dw, dh) = ((sw / 2).max(1), (sh / 2).max(1));
            let dims = Dims {
                src: [sw, sh],
                dst: [dw, dh],
            };
            let ubo = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mips-dims"),
                contents: bytemuck::bytes_of(&dims),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let (src, dst) = (view(level - 1), view(level));
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mips-bg"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&dst),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: ubo.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mips-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(dw.div_ceil(8), dh.div_ceil(8), 1);
            drop(pass);
            (sw, sh) = (dw, dh);
        }
    }
}

/// RGB f32 -> RGBA f32 (alpha = 1), parallel over pixels. `rgba_len` is the validated `w*h*4`.
pub(crate) fn rgb_to_rgba(img: &LinearImage, rgba_len: usize) -> Result<Vec<f32>, PipelineError> {
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(rgba_len)
        .map_err(|e| PipelineError::Allocation(format!("CPU upload: {e}")))?;
    rgba.resize(rgba_len, 0.0f32);
    rgba.par_chunks_exact_mut(4)
        .zip(img.data.par_chunks_exact(3))
        .for_each(|(o, px)| {
            o[0] = px[0];
            o[1] = px[1];
            o[2] = px[2];
            o[3] = 1.0;
        });
    Ok(rgba)
}

fn write_level(ctx: &GpuContext, tex: &wgpu::Texture, level: u32, data: &[f32], w: u32, h: u32) {
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(data),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 16),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

/// Create the full-chain `Rgba32Float` input texture, upload level 0 from `rgba` and fill the
/// remaining levels with `gpu` (compute) or the CPU box filter when `gpu` is `None`.
pub(crate) fn upload_chain(
    ctx: &GpuContext,
    gpu: Option<&MipGenerator>,
    rgba: Vec<f32>,
    (w, h): (u32, u32),
    extra_usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    let mips = mip_count_for(w, h);
    let mut usage =
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | extra_usage;
    if gpu.is_some() {
        usage |= wgpu::TextureUsages::STORAGE_BINDING;
    }
    let tex = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("develop-input"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: mips,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage,
        view_formats: &[],
    });
    write_level(ctx, &tex, 0, &rgba, w, h);
    if let Some(gen) = gpu {
        drop(rgba);
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mips-encoder"),
            });
        gen.encode(&ctx.device, &mut enc, &tex, (w, h), mips);
        ctx.queue.submit([enc.finish()]);
        return tex;
    }
    let mut level_data = rgba;
    let (mut lw, mut lh) = (w, h);
    for level in 1..mips {
        level_data = downsample_rgba_f32(&level_data, lw, lh);
        lw = (lw / 2).max(1);
        lh = (lh / 2).max(1);
        write_level(ctx, &tex, level, &level_data, lw, lh);
    }
    tex
}

/// Test hook: build the chain via the GPU or CPU path and read back every level (tight RGBA f32).
pub(crate) fn read_levels(
    ctx: &GpuContext,
    gen: Option<&MipGenerator>,
    img: &LinearImage,
    rgba_len: usize,
) -> Result<Vec<Vec<f32>>, PipelineError> {
    let (w, h) = (img.width, img.height);
    let rgba = rgb_to_rgba(img, rgba_len)?;
    let tex = upload_chain(ctx, gen, rgba, (w, h), wgpu::TextureUsages::COPY_SRC);
    let mips = tex.mip_level_count();
    let mut enc = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mips-readback"),
        });
    let mut bufs = Vec::new();
    for level in 0..mips {
        let (lw, lh) = ((w >> level).max(1), (h >> level).max(1));
        let bpr = (lw * 16).div_ceil(256) * 256;
        let buf = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mips-readback-buf"),
            size: u64::from(bpr) * u64::from(lh),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(lh),
                },
            },
            wgpu::Extent3d {
                width: lw,
                height: lh,
                depth_or_array_layers: 1,
            },
        );
        bufs.push((buf, lw, lh, bpr));
    }
    ctx.queue.submit([enc.finish()]);
    for (buf, ..) in &bufs {
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    }
    let _ = ctx.device.poll(wgpu::PollType::wait_indefinitely());
    let mut out = Vec::new();
    for (buf, lw, lh, bpr) in bufs {
        let data = buf
            .slice(..)
            .get_mapped_range()
            .map_err(|e| PipelineError::Device(format!("mip readback map: {e}")))?;
        let mut level = Vec::with_capacity(lw as usize * lh as usize * 4);
        for row in 0..lh as usize {
            let start = row * bpr as usize;
            level.extend_from_slice(bytemuck::cast_slice(&data[start..start + lw as usize * 16]));
        }
        drop(data);
        buf.unmap();
        out.push(level);
    }
    Ok(out)
}
