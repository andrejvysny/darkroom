//! Renderer + TexturePool: leased target reuse, bounded memory, and pixel-exact output.
//!
//! Synthetic images only; GPU-gated via `support::gpu()`.

mod support;

use core_pipeline::DevelopPipeline;
use core_pipeline::GpuContext;
use render_wgpu::{PoolLimits, RenderError, Renderer};
use std::sync::Arc;
use support::{gpu, params_a, params_b, ramp, view, KeySpec};

use core_model::identity::DeviceEpoch;

const SIZE: (u32, u32) = (64, 48);

fn bytes_of(size: (u32, u32)) -> u64 {
    u64::from(size.0) * u64::from(size.1) * 4
}

/// The pool's accounting (live/free counts, retained bytes).
fn pool(r: &Renderer) -> std::sync::MutexGuard<'_, render_wgpu::TexturePool> {
    r.pool()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `Ctx::render` without the `unwrap`, for tests that assert on the typed error.
fn try_render(
    c: &Ctx,
    r: &Renderer,
    size: (u32, u32),
    view: core_pipeline::ViewParams,
) -> Result<render_wgpu::FrameLease, RenderError> {
    let spec = KeySpec {
        viewport_px: size,
        ..Default::default()
    };
    r.render(spec.key(), &c.prepared, &params_a(), &view)
}

struct Ctx {
    ctx: GpuContext,
    pipeline: Arc<DevelopPipeline>,
    prepared: core_pipeline::backend::PreparedImage,
}

/// Real GPU + a renderer sharing the same device (a second `GpuContext` handle over the
/// host's `Device`/`Queue`, like `Renderer::new` receives in the app).
fn ctx() -> Option<Ctx> {
    let ctx = gpu()?;
    let pipeline = Arc::new(DevelopPipeline::new(&ctx));
    let prepared = pipeline.prepare(&ctx, &ramp(64, 48)).unwrap();
    Some(Ctx {
        ctx,
        pipeline,
        prepared,
    })
}

impl Ctx {
    fn renderer(&self, per_size: usize) -> Renderer {
        self.renderer_limits(PoolLimits {
            per_size,
            total_bytes: 64 * 1024 * 1024,
        })
    }

    fn renderer_limits(&self, limits: PoolLimits) -> Renderer {
        Renderer::new(
            GpuContext {
                device: self.ctx.device.clone(),
                queue: self.ctx.queue.clone(),
                info: self.ctx.info.clone(),
                max_texture_dim: self.ctx.max_texture_dim,
            },
            Arc::clone(&self.pipeline),
            DeviceEpoch(1),
            limits,
        )
    }

    fn render(
        &self,
        r: &Renderer,
        params: &core_pipeline::DevelopParams,
        size: (u32, u32),
    ) -> render_wgpu::FrameLease {
        let spec = KeySpec {
            viewport_px: size,
            ..Default::default()
        };
        r.render(spec.key(), &self.prepared, params, &view(size.0, size.1))
            .unwrap()
    }

    fn live(&self, r: &Renderer, size: (u32, u32)) -> usize {
        r.pool()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_count(size)
    }
}

// Lease reuse: dropping a lease returns its exact texture to the free list; live count per
// size is bounded by `per_size`; a 4th concurrent render at per_size=3 is PoolExhausted.
#[test]
fn lease_reuse_and_per_size_bound() {
    let Some(c) = ctx() else { return };
    let r = c.renderer(3);

    let first = c.render(&r, &params_a(), SIZE);
    let first_ptr = first.texture_ptr();
    drop(first);
    let second = c.render(&r, &params_a(), SIZE);
    assert_eq!(
        second.texture_ptr(),
        first_ptr,
        "a freed texture must be handed out again, not re-created"
    );
    drop(second);

    // Three concurrent leases exhaust per_size; the fourth is a typed PoolExhausted.
    let a = c.render(&r, &params_a(), SIZE);
    let a_ptr = a.texture_ptr();
    let _b = c.render(&r, &params_a(), SIZE);
    let _d = c.render(&r, &params_a(), SIZE);
    assert_eq!(c.live(&r, SIZE), 3);
    let spec = KeySpec {
        viewport_px: SIZE,
        ..Default::default()
    };
    let exhausted = r.render(spec.key(), &c.prepared, &params_a(), &view(SIZE.0, SIZE.1));
    assert!(
        matches!(exhausted, Err(RenderError::PoolExhausted { size }) if size == SIZE),
        "4th concurrent render at per_size=3 must be PoolExhausted, got {exhausted:?}"
    );

    // Dropping one lease frees a slot and the recycled texture is the dropped one.
    drop(a);
    let e = c.render(&r, &params_a(), SIZE);
    assert_eq!(
        e.texture_ptr(),
        a_ptr,
        "freed slot must reuse the dropped texture"
    );
    assert_eq!(c.live(&r, SIZE), 3);

    // A different size is a different pool bucket with its own texture.
    let other = c.render(&r, &params_a(), (64, 49));
    assert_ne!(other.texture_ptr(), e.texture_ptr());
    assert_eq!(c.live(&r, (64, 49)), 1);

    // Byte accounting covers every texture the pool created (leased + free).
    let total = r
        .pool()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .total_bytes();
    assert_eq!(total, 3 * bytes_of(SIZE) + bytes_of((64, 49)));
}

// The leased GPU path must produce EXACTLY the bytes `DevelopPipeline::render_view` produces
// (SDR offscreen comparison, tolerance 0 — the develop shader is deterministic and undithered;
// same contract as core-pipeline's texture_target goldens).
#[test]
fn read_back_matches_render_view_exactly() {
    let Some(c) = ctx() else { return };
    let r = c.renderer(3);
    let params = params_a();
    let v = view(SIZE.0, SIZE.1);

    let expected = c
        .pipeline
        .render_view(&c.ctx, &c.prepared, &params, &v)
        .unwrap();
    let lease = c.render(&r, &params, SIZE);
    let got = r.read_back(&lease).unwrap();

    assert_eq!(got.len(), (SIZE.0 * SIZE.1 * 4) as usize);
    assert_eq!(got.len(), expected.len());
    assert_eq!(
        got, expected,
        "leased render must be byte-identical to render_view"
    );
}

// Back-to-back renders with different params into two simultaneously alive leases each keep
// their own pixels (no wait between submits; wgpu snapshots uniforms at submit).
#[test]
fn concurrent_leases_keep_their_own_pixels() {
    let Some(c) = ctx() else { return };
    let r = c.renderer(3);
    let va = view(SIZE.0, SIZE.1);

    let lease_a = c.render(&r, &params_a(), SIZE);
    let lease_b = c.render(&r, &params_b(), SIZE);
    assert_ne!(lease_a.handle(), lease_b.handle());
    assert_ne!(
        lease_a.texture_ptr(),
        lease_b.texture_ptr(),
        "concurrent leases must never share a texture"
    );

    let expect_a = c
        .pipeline
        .render_view(&c.ctx, &c.prepared, &params_a(), &va)
        .unwrap();
    let expect_b = c
        .pipeline
        .render_view(&c.ctx, &c.prepared, &params_b(), &va)
        .unwrap();
    let got_a = r.read_back(&lease_a).unwrap();
    let got_b = r.read_back(&lease_b).unwrap();
    assert_eq!(got_a, expect_a, "lease A must hold params_a pixels");
    assert_eq!(got_b, expect_b, "lease B must hold params_b pixels");
    assert_ne!(got_a, got_b, "params_a and params_b must visibly differ");
}

// The host registers the presented frame with egui, which samples it in a fragment shader.
// That is only legal if the pooled target carries TEXTURE_BINDING — the real-window crash
// (`egui_user_image_0` bind group rejected: view lacks TEXTURE_BINDING` happened with targets
// created RENDER_ATTACHMENT | COPY_SRC only, which the read-back tests could never see).
#[test]
fn leased_frames_are_sampleable() {
    let Some(c) = ctx() else { return };
    let r = c.renderer(1);
    let lease = c.render(&r, &params_a(), SIZE);
    assert!(
        lease.texture().usage().contains(render_wgpu::FRAME_USAGE)
            && lease.texture().usage().contains(
                wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
            ),
        "presented frames must be sampleable, got {:?}",
        lease.texture().usage()
    );

    // Minimal egui-style sampled-texture binding: float filterable (Rgba8Unorm is filterable),
    // D2, fragment-visible. Creating the bind group over the lease's view inside a Validation
    // scope must capture nothing.
    let layout = c
        .ctx
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sample-frame"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
    let scope = c.ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let view = lease
        .texture()
        .create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = c.ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sample-frame"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });
    drop(bind_group);
    c.ctx
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll after bind-group creation");
    let error = pollster::block_on(scope.pop());
    assert!(
        error.is_none(),
        "bind group sampling a leased frame must validate, got {error:?}"
    );
}

// Every window-resize / ptP step asks for a new exact-device-pixel size, so the pool sees a long
// series of one-shot geometries. Retaining one texture per size it ever created made the byte cap
// monotonically fill and then refuse *every* un-cached size forever (the worker's retry loop never
// clears that); idle textures must be evicted instead, so a pool with no live leases can always
// serve a new size.
#[test]
fn dropped_leases_never_exhaust_the_byte_cap() {
    let Some(c) = ctx() else { return };
    let cap = 64 * 1024;
    let r = c.renderer_limits(PoolLimits {
        per_size: 3,
        total_bytes: cap,
    });
    for step in 0..40u32 {
        let size = (64, 48 + step);
        let lease = c.render(&r, &params_a(), size);
        assert_eq!(c.live(&r, size), 1, "the lease is checked out");
        let retained = pool(&r).total_bytes();
        assert!(
            retained <= cap,
            "retained bytes must respect the cap mid-series ({retained} at size {size:?})"
        );
        drop(lease);
        assert_eq!(c.live(&r, size), 0);
    }
    {
        let p = pool(&r);
        assert!(
            p.total_bytes() <= cap,
            "40 one-shot geometries must not retain more than the cap"
        );
        assert_eq!(
            p.free_count((64, 87)),
            1,
            "the most recently used size keeps its texture"
        );
        assert_eq!(
            p.free_count((64, 48)),
            0,
            "the least recently used idle texture was evicted, not kept forever"
        );
    }
    // A size the pool already evicted is served again by creating a fresh target.
    let again = c.render(&r, &params_a(), (64, 48));
    assert_eq!(c.live(&r, (64, 48)), 1);
    drop(again);
}

// Eviction frees *idle* textures only: textures held by live leases still bound memory, so the
// cap is a real bound and not just a limit on caching.
#[test]
fn idle_textures_evict_but_live_leases_still_bound_memory() {
    let Some(c) = ctx() else { return };
    let unit = bytes_of(SIZE);
    let r = c.renderer_limits(PoolLimits {
        per_size: 4,
        total_bytes: 2 * unit,
    });
    let same_bytes_a = SIZE; // 64×48
    let same_bytes_b = (48, 64); // different bucket, identical bytes
    let same_bytes_c = (32, 96); // third bucket, identical bytes

    let a = c.render(&r, &params_a(), same_bytes_a);
    drop(a); // parked on the free list, still counted against the cap
    let _b = c.render(&r, &params_a(), same_bytes_a); // reuses a's texture
    let _c = c.render(&r, &params_a(), same_bytes_b);
    assert_eq!(
        pool(&r).total_bytes(),
        2 * unit,
        "two leased textures fill the cap exactly"
    );

    // Nothing idle to evict while two leases are alive: still bounded, the retry contract.
    let refused = try_render(&c, &r, same_bytes_c, view(same_bytes_c.0, same_bytes_c.1));
    assert!(
        matches!(refused, Err(RenderError::PoolExhausted { size }) if size == same_bytes_c),
        "a third size over the cap must be refused, got {refused:?}"
    );

    // Drop a lease: its texture becomes idle, so the new size evicts it instead of failing.
    drop(_b);
    let _d = c.render(&r, &params_a(), same_bytes_c);
    let p = pool(&r);
    assert_eq!(p.live_count(same_bytes_c), 1);
    assert_eq!(
        p.free_count(same_bytes_a),
        0,
        "the idle target was evicted to pay for the new one"
    );
    assert_eq!(
        p.total_bytes(),
        2 * unit,
        "evicted bytes were given back, then spent on the new texture"
    );
}

// The pool counts a texture as live the moment `acquire` hands it out, so any early exit between
// then and the lease owning it must give the slot back. Leaking it there is permanent: enough
// failed renders and that size can never lease a target again.
#[test]
fn failed_render_returns_the_leased_texture() {
    let Some(c) = ctx() else { return };
    let r = c.renderer(3);
    // Target geometry comes from `key.viewport_px` (64×48); the view renders 64×49, so
    // `validate_render_target` rejects it.
    let bad_view = view(SIZE.0, SIZE.1 + 1);
    for attempt in 0..4 {
        let failed = try_render(&c, &r, SIZE, bad_view);
        assert!(
            matches!(failed, Err(RenderError::Pipeline(_))),
            "attempt {attempt}: a mismatched target/view must be a typed pipeline error, got {failed:?}"
        );
        let p = pool(&r);
        assert_eq!(
            p.live_count(SIZE),
            0,
            "attempt {attempt}: a failed render must leave live accounting untouched"
        );
        assert_eq!(
            p.free_count(SIZE),
            1,
            "attempt {attempt}: the leased texture must be back on the free list"
        );
        assert_eq!(
            p.total_bytes(),
            bytes_of(SIZE),
            "it stays cached, not re-created"
        );
    }
    // The recycled target serves the next, valid render without allocating.
    let first_ptr = {
        let probe = c.render(&r, &params_a(), SIZE);
        probe.texture_ptr()
    };
    let ok = c.render(&r, &params_a(), SIZE);
    assert_eq!(ok.texture_ptr(), first_ptr, "reused, not re-created");
    assert_eq!(c.live(&r, SIZE), 1);
}

// `Renderer` is the device it was built for; a worker that kept a retired device's renderer must
// not be able to produce frames at all — refused with a typed error *before* a target is leased.
#[test]
fn render_refuses_a_key_from_another_device_epoch() {
    let Some(c) = ctx() else { return };
    let r = c.renderer(3); // renderer holds DeviceEpoch(1)
    let retired = KeySpec {
        viewport_px: SIZE,
        device_epoch: 2,
        ..Default::default()
    };
    let refused = r.render(
        retired.key(),
        &c.prepared,
        &params_a(),
        &view(SIZE.0, SIZE.1),
    );
    assert!(
        matches!(
            refused,
            Err(RenderError::DeviceEpochMismatch { key, renderer })
                if key == DeviceEpoch(2) && renderer == DeviceEpoch(1)
        ),
        "a key from another device epoch must be refused with a typed error, got {refused:?}"
    );
    {
        let p = pool(&r);
        assert_eq!(p.total_bytes(), 0, "refused before acquiring anything");
        assert_eq!(p.live_count(SIZE), 0);
        assert_eq!(p.free_count(SIZE), 0);
    }

    // The same geometry on the renderer's own epoch renders normally.
    let ok = c.render(&r, &params_a(), SIZE);
    assert_eq!(c.live(&r, SIZE), 1);
    drop(ok);
}

// A target bigger than the whole byte cap can never be served: it is refused with a distinct
// error instead of `PoolExhausted`, so callers do not wait for a release that cannot help.
#[test]
fn a_target_larger_than_the_cap_fails_fast() {
    let Some(c) = ctx() else { return };
    let r = c.renderer_limits(PoolLimits {
        per_size: 3,
        total_bytes: bytes_of(SIZE) - 1,
    });
    let refused = try_render(&c, &r, SIZE, view(SIZE.0, SIZE.1));
    assert!(
        matches!(refused, Err(RenderError::TargetTooLarge { size }) if size == SIZE),
        "got {refused:?}"
    );
}

// Every lease drop fires the return signal, so an exhausted caller can wait instead of polling.
#[test]
fn dropping_a_lease_fires_the_return_signal() {
    let Some(c) = ctx() else { return };
    let r = c.renderer(1);
    let returns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&returns);
    r.set_return_signal(Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    let lease = c.render(&r, &params_a(), SIZE);
    assert_eq!(returns.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(lease);
    assert_eq!(returns.load(std::sync::atomic::Ordering::SeqCst), 1);
}
