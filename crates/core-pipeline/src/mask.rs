//! Mask pre-pass: compute each enabled mask's composited alpha into one layer of the develop
//! mask-alpha texture array. Parametric components (linear/radial) are evaluated procedurally in
//! `mask_prepass.wgsl`; brush/range/ai coverage is filled by later phases.

use crate::params::{BrushStroke, ComponentKind, Mask};

/// Max components evaluated per mask in one pre-pass draw (matches `array<Comp,8>` in the shader).
pub const MAX_PREPASS_COMPONENTS: usize = 8;

/// One component descriptor for the pre-pass uniform. std140-clean: 48 bytes (3 × 16-byte rows).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PrepassComponent {
    /// 0 linear, 1 radial, 2 brush, 3 lumaRange, 4 colorRange, 5 ai.
    pub kind: u32,
    /// 0 add, 1 subtract, 2 intersect.
    pub op: u32,
    pub invert: u32,
    pub _pad: u32,
    /// linear: (p0.xy, p1.xy) · radial: (center.xy, radius.xy).
    pub a: [f32; 4],
    /// radial: (angle, feather, _, _).
    pub b: [f32; 4],
}

/// Pre-pass uniform for one mask: a component count plus a fixed-size component array.
/// 16 + 8×48 = 400 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PrepassUniform {
    pub count: u32,
    pub _pad: [u32; 3],
    pub comps: [PrepassComponent; MAX_PREPASS_COMPONENTS],
}

impl PrepassUniform {
    /// Build the pre-pass uniform for one mask from its parametric components. Non-parametric kinds
    /// (brush/range/ai) are still emitted with their op/invert so they compose correctly once their
    /// coverage lands; in this phase they evaluate to zero coverage in the shader.
    pub fn from_mask(mask: &Mask) -> Self {
        let mut comps: [PrepassComponent; MAX_PREPASS_COMPONENTS] =
            [bytemuck::Zeroable::zeroed(); MAX_PREPASS_COMPONENTS];
        let mut count = 0usize;
        for c in mask.components.iter().take(MAX_PREPASS_COMPONENTS) {
            let (kind, a, b) = match &c.kind {
                ComponentKind::Linear { p0, p1 } => (0u32, [p0[0], p0[1], p1[0], p1[1]], [0.0; 4]),
                ComponentKind::Radial {
                    center,
                    radius,
                    angle,
                    feather,
                } => (
                    1u32,
                    [center[0], center[1], radius[0], radius[1]],
                    [*angle, *feather, 0.0, 0.0],
                ),
                ComponentKind::Brush { .. } => (2u32, [0.0; 4], [0.0; 4]),
                ComponentKind::LuminanceRange { lo, hi, feather } => {
                    (3u32, [*lo, *hi, *feather, 0.0], [0.0; 4])
                }
                ComponentKind::ColorRange {
                    hue,
                    sat,
                    tol,
                    feather,
                } => (4u32, [*hue, *sat, *tol, *feather], [0.0; 4]),
                ComponentKind::Ai { .. } => (5u32, [0.0; 4], [0.0; 4]),
            };
            comps[count] = PrepassComponent {
                kind,
                op: c.op as u32,
                invert: c.invert as u32,
                _pad: 0,
                a,
                b,
            };
            count += 1;
        }
        Self {
            count: count as u32,
            _pad: [0; 3],
            comps,
        }
    }
}

/// Reusable mask pre-pass pipeline (one per `GpuContext`). Renders a fullscreen triangle that writes
/// one mask's composited alpha to an R16Float layer.
pub struct MaskPrepass {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_layout: wgpu::BindGroupLayout,
}

impl MaskPrepass {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mask-prepass-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mask_prepass.wgsl").into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask-prepass-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Per-mask baked brush coverage (R16Float, filterable).
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Scene-linear input image (Rgba32Float), read via textureLoad for range masks.
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
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask-prepass-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mask-prepass-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_prepass"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R16Float,
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
        Self {
            pipeline,
            bind_layout,
        }
    }
}

/// Brush-bake uniform: converts a longest-edge-fraction brush size into per-axis uv radii so dabs
/// are circular in image-pixel space. 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BakeUniform {
    pub aspect: [f32; 2], // (L/w, L/h), L = max(w,h)
    pub _pad: [f32; 2],
}

impl BakeUniform {
    pub fn for_size(w: u32, h: u32) -> Self {
        let l = w.max(h) as f32;
        Self {
            aspect: [l / w as f32, l / h as f32],
            _pad: [0.0, 0.0],
        }
    }
}

/// One stroke's instanced dabs, ready to upload as a vertex buffer (stride 32: two vec4 per dab).
pub struct StrokeBatch {
    pub is_erase: bool,
    pub instances: Vec<u8>,
    pub count: u32,
}

/// Flatten brush strokes into dab instances. Each dab: a=(cx,cy,size,hardness), b=(strength,_,_,_).
/// Strokes are stamped along their polyline at spacing proportional to size. Returns one batch per
/// stroke (preserving paint/erase order).
pub fn flatten_strokes(strokes: &[BrushStroke]) -> Vec<StrokeBatch> {
    let mut batches = Vec::new();
    for s in strokes {
        if s.points.is_empty() {
            continue;
        }
        let size = s.size.max(0.001);
        let strength = s.opacity.clamp(0.0, 1.0);
        let hardness = s.hardness.clamp(0.0, 1.0);
        let spacing = (size * 0.25).max(0.0015);
        let mut dabs: Vec<f32> = Vec::new();
        let stamp = |x: f32, y: f32, out: &mut Vec<f32>| {
            out.extend_from_slice(&[x, y, size, hardness, strength, 0.0, 0.0, 0.0]);
        };
        if s.points.len() == 1 {
            stamp(s.points[0][0], s.points[0][1], &mut dabs);
        } else {
            for w in s.points.windows(2) {
                let (a, b) = (w[0], w[1]);
                let dx = b[0] - a[0];
                let dy = b[1] - a[1];
                let dist = (dx * dx + dy * dy).sqrt();
                let steps = (dist / spacing).ceil().max(1.0) as usize;
                for i in 0..steps {
                    let t = i as f32 / steps as f32;
                    stamp(a[0] + dx * t, a[1] + dy * t, &mut dabs);
                }
            }
            // Final point.
            let last = s.points[s.points.len() - 1];
            stamp(last[0], last[1], &mut dabs);
        }
        let count = (dabs.len() / 8) as u32;
        batches.push(StrokeBatch {
            is_erase: s.is_erase,
            instances: bytemuck::cast_slice(&dabs).to_vec(),
            count,
        });
    }
    batches
}

/// Brush bake pipelines (paint = MAX blend, erase = multiply blend) sharing `brush_bake.wgsl`.
pub struct BrushBake {
    pub paint: wgpu::RenderPipeline,
    pub erase: wgpu::RenderPipeline,
    pub bind_layout: wgpu::BindGroupLayout,
}

impl BrushBake {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("brush-bake-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("brush_bake.wgsl").into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("brush-bake-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("brush-bake-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: 32,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 16,
                    shader_location: 1,
                },
            ],
        };
        let make = |blend: wgpu::BlendState, label: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    buffers: std::slice::from_ref(&instance_layout),
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::R16Float,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        // Paint: out = max(src, dst).
        let paint = make(
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Max,
                },
                alpha: wgpu::BlendComponent::REPLACE,
            },
            "brush-paint-pipeline",
        );
        // Erase: out = dst * (1 - src).
        let erase = make(
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::OneMinusSrc,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent::REPLACE,
            },
            "brush-erase-pipeline",
        );
        Self {
            paint,
            erase,
            bind_layout,
        }
    }
}

/// Uniform for one refine pass (horizontal or vertical). 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RefineUniform {
    pub dir: [f32; 2],
    pub sigma_px: f32,
    pub luma_sigma: f32,
    pub texel: [f32; 2],
    pub _pad: [f32; 2],
}

/// Edge-aware mask refinement pipeline (separable cross-bilateral, `mask_refine.wgsl`).
pub struct MaskRefine {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_layout: wgpu::BindGroupLayout,
}

impl MaskRefine {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mask-refine-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mask_refine.wgsl").into()),
        });
        let tex = |binding: u32, filterable: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask-refine-bgl"),
            entries: &[
                tex(0, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                tex(2, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
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
            label: Some("mask-refine-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mask-refine-pipeline"),
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
                    format: wgpu::TextureFormat::R16Float,
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
        Self {
            pipeline,
            bind_layout,
        }
    }
}

/// True when a mask requests edge-aware refinement (any component flagged `feather`).
pub fn mask_feathered(mask: &Mask) -> bool {
    mask.components.iter().any(|c| c.feather)
}

/// True when a mask has at least one brush component (⇒ needs a brush bake before its pre-pass).
pub fn mask_has_brush(mask: &Mask) -> bool {
    mask.components
        .iter()
        .any(|c| matches!(c.kind, ComponentKind::Brush { .. }))
}

/// Collect all brush strokes across a mask's brush components (in order).
pub fn mask_brush_strokes(mask: &Mask) -> Vec<BrushStroke> {
    let mut out = Vec::new();
    for c in &mask.components {
        if let ComponentKind::Brush { strokes } = &c.kind {
            out.extend(strokes.iter().cloned());
        }
    }
    out
}

/// Stable hash of everything that affects a mask's **coverage** (the pre-pass output), and nothing
/// else: the parametric components (kind/op/invert/geometry, via `PrepassUniform`), the edge-aware
/// refine trigger, and the baked brush strokes. Deliberately EXCLUDES scalar adjustments, opacity,
/// the enabled flag, and the name — those are applied per-frame in the develop pass and never
/// require re-baking a mask layer. Used to skip the full-res pre-pass when geometry is unchanged
/// (pan / zoom / global + local scalar edits / overlay toggles).
pub fn mask_geometry_hash(mask: &Mask) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // Parametric components — the exact bytes the pre-pass consumes (all _pad fields are zeroed).
    bytemuck::bytes_of(&PrepassUniform::from_mask(mask)).hash(&mut h);
    // Per-component feather drives the (separable bilateral) refine pass.
    mask_feathered(mask).hash(&mut h);
    // Brush strokes are rasterized into coverage; hash their geometry + per-stroke settings.
    for s in &mask_brush_strokes(mask) {
        for p in &s.points {
            p[0].to_bits().hash(&mut h);
            p[1].to_bits().hash(&mut h);
        }
        s.size.to_bits().hash(&mut h);
        s.hardness.to_bits().hash(&mut h);
        s.flow.to_bits().hash(&mut h);
        s.opacity.to_bits().hash(&mut h);
        s.is_erase.hash(&mut h);
    }
    h.finish()
}
