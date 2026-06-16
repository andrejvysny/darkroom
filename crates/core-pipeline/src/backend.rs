//! wgpu/Metal develop backend.
//!
//! `prepare()` uploads the cached linear buffer to a GPU texture and allocates the per-image
//! output/readback/uniform resources ONCE. `render()` then only rewrites the uniform and redraws,
//! so slider changes re-render in single-digit milliseconds. Output resolution == input resolution.

use crate::error::PipelineError;
use crate::params::DevelopParams;
use core_raw::LinearImage;
use wgpu::util::DeviceExt;

/// Long-lived GPU device + queue.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    pub fn new() -> Result<Self, PipelineError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .map_err(|_| PipelineError::NoAdapter)?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("darkroom-device"),
            ..Default::default()
        }))
        .map_err(|e| PipelineError::Device(e.to_string()))?;
        Ok(Self { device, queue })
    }
}

const ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

fn padded_bpr(width: u32) -> u32 {
    (width * 4).div_ceil(ALIGN) * ALIGN
}

/// Upload a 256x1 RGBA8 tone-curve LUT (`LUT_SIZE*4` bytes) to `tex`.
fn write_lut(ctx: &GpuContext, tex: &wgpu::Texture, lut: &[u8]) {
    let w = crate::curve::LUT_SIZE as u32;
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        lut,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: w,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

/// Per-image GPU resources, reused across many `render()` calls (one per slider change).
pub struct PreparedImage {
    pub width: u32,
    pub height: u32,
    bpr: u32,
    uniform: wgpu::Buffer,
    fx_uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    output: wgpu::Texture,
    readback: wgpu::Buffer,
    // Tone-curve LUT, rewritten per render() from the current params.
    lut: wgpu::Texture,
    // Kept alive for the bind group's texture view.
    _input: wgpu::Texture,
}

/// The reusable develop render pipeline (created once per `GpuContext`).
pub struct DevelopPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl DevelopPipeline {
    pub fn new(ctx: &GpuContext) -> Self {
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("develop-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("develop.wgsl").into()),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("develop-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Tone-curve LUT (256x1 RGBA8), read via textureLoad (no sampler needed).
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Secondary effects uniform (HSL bands; grows with crop/lens/detail later).
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("develop-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("develop-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("develop-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            bind_layout,
            sampler,
        }
    }

    /// Upload `img` (linear RGB) and allocate per-image GPU resources.
    /// Returns an error (rather than panicking) if a GPU allocation fails — e.g. a full-resolution
    /// export texture that exceeds device memory.
    pub fn prepare(
        &self,
        ctx: &GpuContext,
        img: &LinearImage,
    ) -> Result<PreparedImage, PipelineError> {
        let device = &ctx.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let (w, h) = (img.width, img.height);

        // RGB f32 -> RGBA f32 (alpha = 1).
        let mut rgba = vec![0f32; (w * h * 4) as usize];
        for (i, px) in img.data.chunks_exact(3).enumerate() {
            let o = i * 4;
            rgba[o] = px[0];
            rgba[o + 1] = px[1];
            rgba[o + 2] = px[2];
            rgba[o + 3] = 1.0;
        }

        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let input = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-input"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&rgba),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 16),
                rows_per_image: Some(h),
            },
            size,
        );

        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-output"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-uniform"),
            contents: bytemuck::bytes_of(&DevelopParams::default().to_uniform()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let fx_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-fx-uniform"),
            contents: bytemuck::bytes_of(&crate::params::FxUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // Tone-curve LUT: 256x1 RGBA8, seeded with identity (overwritten each render()).
        let lut_size = wgpu::Extent3d {
            width: crate::curve::LUT_SIZE as u32,
            height: 1,
            depth_or_array_layers: 1,
        };
        let lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-lut"),
            size: lut_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_lut(ctx, &lut, &crate::curve::identity_lut());

        let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
        let lut_view = lut.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop-bg"),
            layout: &self.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: fx_uniform.as_entire_binding(),
                },
            ],
        });

        let bpr = padded_bpr(w);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop-readback"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(PipelineError::Device(format!(
                "GPU allocation failed: {err}"
            )));
        }
        Ok(PreparedImage {
            width: w,
            height: h,
            bpr,
            uniform,
            fx_uniform,
            bind_group,
            output,
            readback,
            lut,
            _input: input,
        })
    }

    /// Render `prepared` with `params`; returns tightly-packed RGBA8 (`w*h*4`).
    pub fn render(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
    ) -> Result<Vec<u8>, PipelineError> {
        let device = &ctx.device;
        let (w, h) = (prepared.width, prepared.height);

        ctx.queue.write_buffer(
            &prepared.uniform,
            0,
            bytemuck::bytes_of(&params.to_uniform()),
        );
        ctx.queue
            .write_buffer(&prepared.fx_uniform, 0, bytemuck::bytes_of(&params.to_fx()));

        // Refresh the tone-curve LUT (identity is cheap; skips spline work when no curve set).
        let lut = if params.tone_curve.is_identity() {
            crate::curve::identity_lut()
        } else {
            crate::curve::build_lut(&params.tone_curve)
        };
        write_lut(ctx, &prepared.lut, &lut);

        let output_view = prepared
            .output
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("develop-enc"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("develop-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &prepared.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &prepared.output,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &prepared.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(prepared.bpr),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit(Some(enc.finish()));

        let (tx, rx) = std::sync::mpsc::channel();
        prepared
            .readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .map_err(|e| PipelineError::Map(e.to_string()))?
            .map_err(|e| PipelineError::Map(e.to_string()))?;

        let data = prepared.readback.slice(..).get_mapped_range();
        let mut out = vec![0u8; (w * h * 4) as usize];
        let row = (w * 4) as usize;
        for y in 0..h as usize {
            let src = y * prepared.bpr as usize;
            let dst = y * row;
            out[dst..dst + row].copy_from_slice(&data[src..src + row]);
        }
        drop(data);
        prepared.readback.unmap();
        Ok(out)
    }

    /// One-shot convenience: prepare + render (used for full-res export).
    pub fn render_once(
        &self,
        ctx: &GpuContext,
        img: &LinearImage,
        params: &DevelopParams,
    ) -> Result<Vec<u8>, PipelineError> {
        let prepared = self.prepare(ctx, img)?;
        self.render(ctx, &prepared, params)
    }
}
