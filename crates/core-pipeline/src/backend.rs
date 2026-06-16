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
    // Per-mask scalar deltas, rewritten per render() from the current params.
    mask_buffer: wgpu::Buffer,
    // Pre-pass uniform (one mask's components), rewritten per mask per render().
    prepass_uniform: wgpu::Buffer,
    prepass_bind: wgpu::BindGroup,
    // Brush bake: per-mask scratch coverage (R16Float) + the size→uv-radius uniform/bind.
    brush_tex: wgpu::Texture,
    // Kept alive for the bake bind group.
    _bake_uniform: wgpu::Buffer,
    bake_bind: wgpu::BindGroup,
    // Mask alpha scratch: pre-pass writes here, then refine (or passthrough) writes the layer.
    // Two single-layer R16Float targets to ping-pong the separable bilateral.
    scratch_a: wgpu::Texture,
    scratch_b: wgpu::Texture,
    refine_uniform: wgpu::Buffer,
    // Kept alive for the bind group's texture views.
    _input: wgpu::Texture,
    // Mask alpha layers (R16Float, MASK_CAP layers). The pre-pass writes per-mask coverage here;
    // the develop pass samples it. Kept alive for the bind group's array view + per-layer targets.
    mask_tex: wgpu::Texture,
}

/// The reusable develop render pipeline (created once per `GpuContext`).
pub struct DevelopPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    // Filtering sampler for the mask alpha array (linear, clamp).
    mask_sampler: wgpu::Sampler,
    // Mask pre-pass (parametric coverage → per-mask alpha layer).
    prepass: crate::mask::MaskPrepass,
    // Brush bake (strokes → per-mask brush coverage texture).
    brush_bake: crate::mask::BrushBake,
    // Edge-aware mask refinement (cross-bilateral).
    refine: crate::mask::MaskRefine,
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
                // Mask alpha layers (R16Float D2 array, filterable).
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                // Filtering sampler for the mask alpha array.
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Per-mask scalar deltas (read-only storage buffer).
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
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

        // Filtering sampler for the mask alpha array (smooth coverage interpolation).
        let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("develop-mask-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let prepass = crate::mask::MaskPrepass::new(device);
        let brush_bake = crate::mask::BrushBake::new(device);
        let refine = crate::mask::MaskRefine::new(device);

        Self {
            pipeline,
            bind_layout,
            sampler,
            mask_sampler,
            prepass,
            brush_bake,
            refine,
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

        // Mask alpha layers: R16Float, MASK_CAP layers, same size as the image. Zero-initialised by
        // wgpu; the mask pre-pass (later phase) writes real coverage via RENDER_ATTACHMENT
        // (fragment-to-texture — R16Float is not storage-bindable on Metal).
        let mask_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-mask-alpha"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: crate::params::MASK_CAP as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Per-mask scalar deltas storage buffer (default = count 0 ⇒ masking is a no-op).
        let mask_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-mask-buffer"),
            contents: bytemuck::bytes_of(&crate::params::MaskBufferUniform::default()),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());

        // Pre-pass uniform (one mask's components at a time, rewritten per mask in render()).
        let prepass_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop-prepass-uniform"),
            size: std::mem::size_of::<crate::mask::PrepassUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Brush coverage scratch (R16Float, image-sized) — baked per brush mask, sampled by prepass.
        let brush_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-brush-scratch"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let brush_scratch_view = brush_tex.create_view(&wgpu::TextureViewDescriptor::default());

        // Mask-alpha scratch ping-pong (single-layer R16Float) for the pre-pass + refine.
        let r16_scratch = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        let scratch_a = r16_scratch("mask-scratch-a");
        let scratch_b = r16_scratch("mask-scratch-b");
        let refine_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mask-refine-uniform"),
            size: std::mem::size_of::<crate::mask::RefineUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bake_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-bake-uniform"),
            contents: bytemuck::bytes_of(&crate::mask::BakeUniform::for_size(w, h)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bake_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop-bake-bg"),
            layout: &self.brush_bake.bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: bake_uniform.as_entire_binding(),
            }],
        });

        let prepass_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop-prepass-bg"),
            layout: &self.prepass.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: prepass_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&brush_scratch_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
            ],
        });

        let lut_view = lut.create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = mask_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("develop-mask-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
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
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: mask_buffer.as_entire_binding(),
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
            mask_buffer,
            prepass_uniform,
            prepass_bind,
            brush_tex,
            _bake_uniform: bake_uniform,
            bake_bind,
            scratch_a,
            scratch_b,
            refine_uniform,
            _input: input,
            mask_tex,
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
        ctx.queue.write_buffer(
            &prepared.mask_buffer,
            0,
            bytemuck::bytes_of(&params.to_mask_buffer()),
        );

        // Refresh the tone-curve LUT (identity is cheap; skips spline work when no curve set).
        let lut = if params.tone_curve.is_identity() {
            crate::curve::identity_lut()
        } else {
            crate::curve::build_lut(&params.tone_curve)
        };
        write_lut(ctx, &prepared.lut, &lut);

        // Mask pre-pass: compute each enabled mask's composited alpha into its alpha layer. Same
        // order as `to_mask_buffer` (enabled masks, dense 0..count). One submit per mask so the
        // per-mask uniform write is ordered before its draw (the uniform buffer is reused).
        for (layer, mask) in params
            .masks
            .iter()
            .filter(|m| m.enabled)
            .take(crate::params::MASK_CAP)
            .enumerate()
        {
            // Bake this mask's brush strokes (if any) into the brush scratch before its pre-pass.
            if crate::mask::mask_has_brush(mask) {
                self.bake_brush(ctx, prepared, mask);
            }

            let pre = crate::mask::PrepassUniform::from_mask(mask);
            ctx.queue
                .write_buffer(&prepared.prepass_uniform, 0, bytemuck::bytes_of(&pre));
            let layer_view = prepared.mask_tex.create_view(&wgpu::TextureViewDescriptor {
                label: Some("mask-layer-view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer as u32,
                array_layer_count: Some(1),
                ..Default::default()
            });
            // Pre-pass writes the composited alpha into scratch_a.
            let scratch_a_view = prepared
                .scratch_a
                .create_view(&wgpu::TextureViewDescriptor::default());
            let mut penc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mask-prepass-enc"),
            });
            {
                let mut pass = penc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("mask-prepass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &scratch_a_view,
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
                pass.set_pipeline(&self.prepass.pipeline);
                pass.set_bind_group(0, &prepared.prepass_bind, &[]);
                pass.draw(0..3, 0..1);
            }
            ctx.queue.submit(Some(penc.finish()));

            // Refine into the alpha layer. Feathered: bilateral H (a→b) then V (b→layer).
            // Otherwise a single passthrough copy (a→layer).
            let l = w.max(h) as f32;
            let texel = [1.0 / w as f32, 1.0 / h as f32];
            let luma_sigma = 0.10f32;
            if crate::mask::mask_feathered(mask) {
                let sigma_px = (0.006 * l).max(1.0);
                let scratch_b_view = prepared
                    .scratch_b
                    .create_view(&wgpu::TextureViewDescriptor::default());
                self.refine_pass(
                    ctx,
                    prepared,
                    &scratch_a_view,
                    &scratch_b_view,
                    [1.0, 0.0],
                    sigma_px,
                    luma_sigma,
                    texel,
                );
                self.refine_pass(
                    ctx,
                    prepared,
                    &scratch_b_view,
                    &layer_view,
                    [0.0, 1.0],
                    sigma_px,
                    luma_sigma,
                    texel,
                );
            } else {
                // Passthrough copy (sigma 0 ⇒ shader returns the source unchanged).
                self.refine_pass(
                    ctx,
                    prepared,
                    &scratch_a_view,
                    &layer_view,
                    [0.0, 0.0],
                    0.0,
                    luma_sigma,
                    texel,
                );
            }
        }

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

    /// Bake a mask's brush strokes into the per-image brush scratch texture. Clears first, then
    /// draws each stroke's instanced dabs in order (paint = MAX blend, erase = multiply).
    fn bake_brush(&self, ctx: &GpuContext, prepared: &PreparedImage, mask: &crate::params::Mask) {
        let device = &ctx.device;
        let strokes = crate::mask::mask_brush_strokes(mask);
        let batches = crate::mask::flatten_strokes(&strokes);
        let view = prepared
            .brush_tex
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Clear pass (runs even with no strokes, so stale coverage never leaks in).
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("brush-clear-enc"),
        });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("brush-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
        }
        ctx.queue.submit(Some(enc.finish()));

        for batch in &batches {
            if batch.count == 0 {
                continue;
            }
            let inst = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("brush-instances"),
                contents: &batch.instances,
                usage: wgpu::BufferUsages::VERTEX,
            });
            let mut benc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("brush-bake-enc"),
            });
            {
                let mut pass = benc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("brush-bake"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(if batch.is_erase {
                    &self.brush_bake.erase
                } else {
                    &self.brush_bake.paint
                });
                pass.set_bind_group(0, &prepared.bake_bind, &[]);
                pass.set_vertex_buffer(0, inst.slice(..));
                pass.draw(0..6, 0..batch.count);
            }
            ctx.queue.submit(Some(benc.finish()));
        }
    }

    /// One edge-aware refine pass: read `src`, write `dst`. `dir`=(1,0)/(0,1) selects the separable
    /// axis; `sigma_px`=0 makes it a plain copy.
    #[allow(clippy::too_many_arguments)]
    fn refine_pass(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        dir: [f32; 2],
        sigma_px: f32,
        luma_sigma: f32,
        texel: [f32; 2],
    ) {
        let device = &ctx.device;
        ctx.queue.write_buffer(
            &prepared.refine_uniform,
            0,
            bytemuck::bytes_of(&crate::mask::RefineUniform {
                dir,
                sigma_px,
                luma_sigma,
                texel,
                _pad: [0.0, 0.0],
            }),
        );
        let input_view = prepared
            ._input
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask-refine-bg"),
            layout: &self.refine.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: prepared.refine_uniform.as_entire_binding(),
                },
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mask-refine-enc"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mask-refine"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
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
            pass.set_pipeline(&self.refine.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        ctx.queue.submit(Some(enc.finish()));
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
