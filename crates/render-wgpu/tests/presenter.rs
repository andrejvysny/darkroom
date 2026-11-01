//! Stale-safe presentation: the presenter publishes only the frame the UI still wants.
//!
//! Leases come from a real (tiny) `Renderer::render` on a real GPU; the identity checks under
//! test are pure, but leases are opaque pool-typed objects, so they are built by rendering.

mod support;

use core_pipeline::{DevelopPipeline, GpuContext};
use render_wgpu::{Offer, Presenter, Renderer, StaleReason};
use std::sync::Arc;
use support::{gpu, params_a, ramp, view, KeySpec};

use core_model::identity::DeviceEpoch;

/// One shared GPU per test; leases are cheap 64×48 renders. `renderer` is the live device
/// (epoch 1); [`Harness::renderer_at`] builds the host's *next* renderer after a device reset —
/// a `Renderer` refuses keys from any other epoch, so tests about retired epochs need one per
/// epoch, exactly as the host does.
struct Harness {
    ctx: GpuContext,
    pipeline: Arc<DevelopPipeline>,
    renderer: Renderer,
    prepared: core_pipeline::backend::PreparedImage,
}

fn harness() -> Option<Harness> {
    let ctx = gpu()?;
    let pipeline = Arc::new(DevelopPipeline::new(&ctx));
    let prepared = pipeline.prepare(&ctx, &ramp(64, 48)).unwrap();
    let renderer = Renderer::new(
        GpuContext {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
            info: ctx.info.clone(),
            max_texture_dim: ctx.max_texture_dim,
        },
        Arc::clone(&pipeline),
        DeviceEpoch(1),
        render_wgpu::PoolLimits {
            per_size: 4,
            total_bytes: 64 * 1024 * 1024,
        },
    );
    Some(Harness {
        ctx,
        pipeline,
        renderer,
        prepared,
    })
}

impl Harness {
    fn renderer_at(&self, epoch: DeviceEpoch) -> Renderer {
        Renderer::new(
            GpuContext {
                device: self.ctx.device.clone(),
                queue: self.ctx.queue.clone(),
                info: self.ctx.info.clone(),
                max_texture_dim: self.ctx.max_texture_dim,
            },
            Arc::clone(&self.pipeline),
            epoch,
            render_wgpu::PoolLimits {
                per_size: 4,
                total_bytes: 64 * 1024 * 1024,
            },
        )
    }

    fn lease(&self, spec: &KeySpec) -> render_wgpu::FrameLease {
        self.lease_on(&self.renderer, spec)
    }

    fn lease_on(&self, renderer: &Renderer, spec: &KeySpec) -> render_wgpu::FrameLease {
        let key = spec.key();
        renderer
            .render(
                key,
                &self.prepared,
                &params_a(),
                &view(key.viewport_px.0, key.viewport_px.1),
            )
            .unwrap()
    }
}

// A→B→A with reordered completion: only the newest wanted frame may present.
#[test]
fn reordered_completion_presents_only_newest_wanted() {
    let Some(h) = harness() else { return };
    let a1 = KeySpec {
        image_id: 0,
        editor_session: 1,
        ..Default::default()
    };
    let b = KeySpec {
        image_id: 1,
        editor_session: 1,
        ..Default::default()
    };
    let a2 = KeySpec {
        image_id: 0,
        editor_session: 2,
        ..Default::default()
    };
    let mut p = Presenter::new();
    p.want(a1.key());
    p.want(b.key());
    p.want(a2.key()); // newest request wins

    let l_a1 = h.lease(&a1);
    let h_a1 = l_a1.handle();
    let l_a2 = h.lease(&a2);
    let h_a2 = l_a2.handle();
    let l_b = h.lease(&b);

    // Offers arrive out of order: A1 (superseded session), A2 (wanted), B (other image).
    assert_eq!(
        p.offer(l_a1, DeviceEpoch(1)),
        Offer::Stale(StaleReason::OtherImageOrSession)
    );
    assert_eq!(p.offer(l_a2, DeviceEpoch(1)), Offer::Presented);
    assert_eq!(
        p.offer(l_b, DeviceEpoch(1)),
        Offer::Stale(StaleReason::OtherImageOrSession)
    );
    assert_eq!(p.current().map(|l| l.handle()), Some(h_a2));
    assert_eq!(p.current().map(|l| *l.key()), Some(a2.key()));
    assert_ne!(h_a1, h_a2, "each frame gets a fresh opaque handle");
    assert_eq!(h.renderer.device_epoch(), DeviceEpoch(1));
}

// An older edit revision never replaces a newer presented frame.
#[test]
fn older_revision_never_replaces_newer_presented() {
    let Some(h) = harness() else { return };
    let wanted = KeySpec {
        edit_revision: 5,
        quality: render_wgpu::QualityTier::Settled,
        ..Default::default()
    };
    let older = KeySpec {
        edit_revision: 4,
        ..Default::default()
    };
    let mut p = Presenter::new();
    p.want(wanted.key());
    let good = h.lease(&wanted);
    let good_handle = good.handle();
    assert_eq!(p.offer(good, DeviceEpoch(1)), Offer::Presented);

    let late = h.lease(&older);
    assert_eq!(
        p.offer(late, DeviceEpoch(1)),
        Offer::Stale(StaleReason::OlderRevision)
    );
    assert_eq!(p.current().map(|l| l.handle()), Some(good_handle));
    assert_eq!(
        p.current().map(|l| l.key().identity.edit_revision.0),
        Some(5)
    );
}

// Interactive frames may present while a newer edit is pending (drag progress); the Settled
// frame for the wanted revision replaces them; late older frames are rejected.
#[test]
fn interactive_progress_then_settled_replaces() {
    let Some(h) = harness() else { return };
    let mut p = Presenter::new();
    p.want(
        KeySpec {
            edit_revision: 5,
            quality: render_wgpu::QualityTier::Interactive,
            ..Default::default()
        }
        .key(),
    );

    let drag = KeySpec {
        edit_revision: 3,
        ..Default::default()
    };
    assert_eq!(p.offer(h.lease(&drag), DeviceEpoch(1)), Offer::Presented);

    let settled = KeySpec {
        edit_revision: 5,
        quality: render_wgpu::QualityTier::Settled,
        ..Default::default()
    };
    let settled_handle = {
        let l = h.lease(&settled);
        let hh = l.handle();
        assert_eq!(p.offer(l, DeviceEpoch(1)), Offer::Presented);
        hh
    };
    assert_eq!(p.current().map(|l| l.handle()), Some(settled_handle));

    let late = KeySpec {
        edit_revision: 4,
        ..Default::default()
    };
    assert_eq!(
        p.offer(h.lease(&late), DeviceEpoch(1)),
        Offer::Stale(StaleReason::OlderRevision)
    );
    assert_eq!(p.current().map(|l| l.handle()), Some(settled_handle));
}

// Resizing (pixels, scale, or viewport revision) makes frames of the old geometry stale.
#[test]
fn resize_stales_old_viewport_frames() {
    let Some(h) = harness() else { return };
    let mut p = Presenter::new();

    p.want(KeySpec::default().key());
    assert_eq!(
        p.offer(h.lease(&KeySpec::default()), DeviceEpoch(1)),
        Offer::Presented
    );

    // UI resizes: same image/session, bigger pixels.
    let resized = KeySpec {
        viewport_px: (80, 60),
        ..Default::default()
    };
    p.want(resized.key());
    assert_eq!(p.offer(h.lease(&resized), DeviceEpoch(1)), Offer::Presented);

    // A late completion of the old-size frame is rejected; current keeps the new size.
    assert_eq!(
        p.offer(h.lease(&KeySpec::default()), DeviceEpoch(1)),
        Offer::Stale(StaleReason::ViewportChanged)
    );
    assert_eq!(p.current().map(|l| l.key().viewport_px), Some((80, 60)));

    // Scale and viewport_revision changes stale frames identically.
    let scaled = KeySpec {
        viewport_px: (80, 60),
        scale_milli: 2000,
        ..Default::default()
    };
    p.want(scaled.key());
    assert_eq!(
        p.offer(h.lease(&resized), DeviceEpoch(1)),
        Offer::Stale(StaleReason::ViewportChanged)
    );
    let moved = KeySpec {
        viewport_px: (80, 60),
        scale_milli: 2000,
        viewport_revision: 3,
        ..Default::default()
    };
    p.want(moved.key());
    assert_eq!(
        p.offer(h.lease(&scaled), DeviceEpoch(1)),
        Offer::Stale(StaleReason::ViewportChanged)
    );
    assert_eq!(p.offer(h.lease(&moved), DeviceEpoch(1)), Offer::Presented);
}

// Device epoch bump retires every frame rendered on the old device; clear() drops the current.
#[test]
fn device_retirement_rejects_and_clear_drops() {
    let Some(h) = harness() else { return };
    let mut p = Presenter::new();
    p.want(KeySpec::default().key());
    assert_eq!(
        p.offer(h.lease(&KeySpec::default()), DeviceEpoch(1)),
        Offer::Presented
    );
    assert!(p.current().is_some());

    // Host retired the device (epoch 2 is current now): the frame the old renderer was still
    // producing never presents.
    let old_frame = h.lease(&KeySpec::default());
    assert_eq!(
        p.offer(old_frame, DeviceEpoch(2)),
        Offer::Stale(StaleReason::DeviceRetired)
    );
    // The new device's own frame (its renderer, its epoch stamp) is likewise refused while the
    // UI still asks for the retired epoch's frame: the two are not interchangeable.
    let new_device = h.renderer_at(DeviceEpoch(2));
    let new_device_key = KeySpec {
        device_epoch: 2,
        ..Default::default()
    };
    assert_eq!(
        p.offer(h.lease_on(&new_device, &new_device_key), DeviceEpoch(2)),
        Offer::Stale(StaleReason::DeviceRetired)
    );

    p.clear();
    assert!(p.current().is_none());
    // After clear, once the UI wants the new epoch's frame, it presents again.
    p.want(new_device_key.key());
    assert_eq!(
        p.offer(h.lease_on(&new_device, &new_device_key), DeviceEpoch(2)),
        Offer::Presented
    );
}

// With no wanted frame yet, any current-device frame may present (first paint).
#[test]
fn no_wanted_yet_presents_any_current_frame() {
    let Some(h) = harness() else { return };
    let mut p = Presenter::new();
    assert_eq!(
        p.offer(h.lease(&KeySpec::default()), DeviceEpoch(1)),
        Offer::Presented
    );
}

// A Settled frame for a revision other than the wanted one is NotWanted.
#[test]
fn settled_for_other_revision_is_not_wanted() {
    let Some(h) = harness() else { return };
    let mut p = Presenter::new();
    p.want(
        KeySpec {
            edit_revision: 7,
            quality: render_wgpu::QualityTier::Interactive,
            ..Default::default()
        }
        .key(),
    );
    let early_settled = KeySpec {
        edit_revision: 6,
        quality: render_wgpu::QualityTier::Settled,
        ..Default::default()
    };
    assert_eq!(
        p.offer(h.lease(&early_settled), DeviceEpoch(1)),
        Offer::Stale(StaleReason::NotWanted)
    );
    assert!(p.current().is_none());
}

// Rejected leases return their textures: repeated rejected renders of the same size reuse a
// single pooled texture instead of allocating one per offer.
#[test]
fn rejected_leases_return_textures_to_pool() {
    let Some(h) = harness() else { return };
    let size = (64u32, 48u32);
    let mut p = Presenter::new();
    p.want(
        KeySpec {
            image_id: 9,
            ..Default::default()
        }
        .key(),
    );
    let other = KeySpec::default();
    for _ in 0..4 {
        assert!(matches!(
            p.offer(h.lease(&other), DeviceEpoch(1)),
            Offer::Stale(_)
        ));
    }
    let pool = h
        .renderer
        .pool()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(pool.live_count(size), 0, "no lease may stay checked out");
    assert_eq!(
        pool.free_count(size),
        1,
        "four rejected renders must have reused exactly one returned texture"
    );
}

// Switching photos starts a new editor session whose revisions restart at 1; they must never
// be compared with the previous subject's high revision (bug: every B frame was rejected as
// OlderRevision forever after presenting A rev 10).
#[test]
fn new_subject_with_lower_revision_presents() {
    let Some(h) = harness() else { return };
    let a = KeySpec {
        image_id: 0,
        editor_session: 1,
        edit_revision: 10,
        ..Default::default()
    };
    let mut p = Presenter::new();
    p.want(a.key());
    let a_handle = {
        let l = h.lease(&a);
        let hh = l.handle();
        assert_eq!(p.offer(l, DeviceEpoch(1)), Offer::Presented);
        hh
    };
    assert_eq!(p.current().map(|l| l.handle()), Some(a_handle));

    // User selects B: different image, new editor session, its revisions start at 1.
    let b = KeySpec {
        image_id: 1,
        editor_session: 2,
        edit_revision: 1,
        ..Default::default()
    };
    p.want(b.key());
    assert!(
        p.current().is_none(),
        "A's pixels must not linger under B's selection"
    );

    let l_b = h.lease(&b);
    let b_handle = l_b.handle();
    assert_eq!(p.offer(l_b, DeviceEpoch(1)), Offer::Presented);
    assert_eq!(p.current().map(|l| l.handle()), Some(b_handle));
    assert_eq!(
        p.current().map(|l| l.key().identity.edit_revision.0),
        Some(1)
    );
}

// A late completion of the *old* subject after the switch is rejected against the wanted key
// and must not resurrect the old photo's pixels.
#[test]
fn late_old_subject_frame_after_switch_is_stale() {
    let Some(h) = harness() else { return };
    let a = KeySpec {
        image_id: 0,
        editor_session: 1,
        edit_revision: 10,
        ..Default::default()
    };
    let mut p = Presenter::new();
    p.want(a.key());
    assert_eq!(p.offer(h.lease(&a), DeviceEpoch(1)), Offer::Presented);

    let b = KeySpec {
        image_id: 1,
        editor_session: 2,
        edit_revision: 1,
        ..Default::default()
    };
    p.want(b.key());
    assert!(p.current().is_none());

    // A's in-flight rev 11 render completes after the switch to B.
    let a_late = KeySpec {
        image_id: 0,
        editor_session: 1,
        edit_revision: 11,
        ..Default::default()
    };
    assert_eq!(
        p.offer(h.lease(&a_late), DeviceEpoch(1)),
        Offer::Stale(StaleReason::OtherImageOrSession)
    );
    assert!(
        p.current().is_none(),
        "the rejected old-subject frame must not become current"
    );

    // B's own frame still presents afterwards.
    let l_b = h.lease(&b);
    let b_handle = l_b.handle();
    assert_eq!(p.offer(l_b, DeviceEpoch(1)), Offer::Presented);
    assert_eq!(p.current().map(|l| l.handle()), Some(b_handle));
}

// A viewport change for the same subject keeps the current frame (same photo, letterboxed by
// the host) until the render for the new geometry lands; only the new geometry presents.
#[test]
fn same_subject_viewport_change_keeps_current_until_new_size() {
    let Some(h) = harness() else { return };
    let mut p = Presenter::new();
    p.want(KeySpec::default().key());
    let old_handle = {
        let l = h.lease(&KeySpec::default());
        let hh = l.handle();
        assert_eq!(p.offer(l, DeviceEpoch(1)), Offer::Presented);
        hh
    };

    let resized = KeySpec {
        viewport_px: (80, 60),
        ..Default::default()
    };
    p.want(resized.key());
    assert_eq!(
        p.current().map(|l| l.handle()),
        Some(old_handle),
        "same-subject resize keeps the old-size frame on screen until the new size arrives"
    );

    // A completion of the old size is stale against the wanted geometry...
    assert_eq!(
        p.offer(h.lease(&KeySpec::default()), DeviceEpoch(1)),
        Offer::Stale(StaleReason::ViewportChanged)
    );
    assert_eq!(p.current().map(|l| l.handle()), Some(old_handle));

    // ...and the new size replaces it.
    let l_new = h.lease(&resized);
    let new_handle = l_new.handle();
    assert_eq!(p.offer(l_new, DeviceEpoch(1)), Offer::Presented);
    assert_eq!(p.current().map(|l| l.handle()), Some(new_handle));
    assert_eq!(p.current().map(|l| l.key().viewport_px), Some((80, 60)));
}

// The settling race: the `Settled` frame of a revision is final, but the drag render of *that same
// revision* can still be in flight and finish later. Publishing it dropped the settled lease, so
// its texture went back to the pool and got overwritten — the drag approximation stayed on screen
// with nothing left to recover the settled pixels until the next edit.
#[test]
fn drag_straggler_never_replaces_the_settled_frame_of_its_revision() {
    let Some(h) = harness() else { return };
    let mut p = Presenter::new();
    let settled = KeySpec {
        edit_revision: 5,
        quality: render_wgpu::QualityTier::Settled,
        ..Default::default()
    };
    p.want(settled.key());
    let settled_handle = {
        let l = h.lease(&settled);
        let hh = l.handle();
        assert_eq!(p.offer(l, DeviceEpoch(1)), Offer::Presented);
        hh
    };

    // The worker's rev-5 drag frame completes late (same subject, same geometry, same revision).
    let straggler = KeySpec {
        edit_revision: 5,
        quality: render_wgpu::QualityTier::Interactive,
        ..Default::default()
    };
    assert_eq!(
        p.offer(h.lease(&straggler), DeviceEpoch(1)),
        Offer::Stale(StaleReason::OlderRevision),
        "a drag frame of an already-settled revision is a straggler, not progress"
    );
    assert_eq!(
        p.current().map(|l| l.handle()),
        Some(settled_handle),
        "the settled pixels must stay on screen"
    );

    // A drag of a *newer* revision is a genuinely later edit and still presents...
    let next_drag = KeySpec {
        edit_revision: 6,
        quality: render_wgpu::QualityTier::Interactive,
        ..Default::default()
    };
    p.want(next_drag.key());
    let drag_handle = {
        let l = h.lease(&next_drag);
        let hh = l.handle();
        assert_eq!(p.offer(l, DeviceEpoch(1)), Offer::Presented);
        hh
    };
    assert_eq!(p.current().map(|l| l.handle()), Some(drag_handle));

    // ...and rev 6's settled frame replaces it, after which rev 6's straggler is refused again.
    let settled6 = KeySpec {
        edit_revision: 6,
        quality: render_wgpu::QualityTier::Settled,
        ..Default::default()
    };
    p.want(settled6.key());
    let settled6_handle = {
        let l = h.lease(&settled6);
        let hh = l.handle();
        assert_eq!(p.offer(l, DeviceEpoch(1)), Offer::Presented);
        hh
    };
    assert_eq!(p.current().map(|l| l.handle()), Some(settled6_handle));
    assert_eq!(
        p.offer(h.lease(&next_drag), DeviceEpoch(1)),
        Offer::Stale(StaleReason::OlderRevision)
    );
    assert_eq!(p.current().map(|l| l.handle()), Some(settled6_handle));
}

// `RenderIdentity`'s contract is "two frames are interchangeable only if equal", yet
// `renderer_version` (shader generation) and `cache_version` were compared nowhere: after an
// in-flight generation bump, a stale-shader or stale-cache frame passed every gate and presented.
#[test]
fn renderer_or_cache_generation_change_stales_frames() {
    let Some(h) = harness() else { return };
    let generations = [
        (
            "renderer_version",
            KeySpec {
                renderer_version: 2,
                ..Default::default()
            },
        ),
        (
            "cache_version",
            KeySpec {
                cache_version: 2,
                ..Default::default()
            },
        ),
    ];
    for (name, other_generation) in generations {
        let mut p = Presenter::new();
        p.want(KeySpec::default().key());
        let handle = {
            let l = h.lease(&KeySpec::default());
            let hh = l.handle();
            assert_eq!(p.offer(l, DeviceEpoch(1)), Offer::Presented);
            hh
        };
        assert_eq!(
            p.offer(h.lease(&other_generation), DeviceEpoch(1)),
            Offer::Stale(StaleReason::OtherImageOrSession),
            "a frame from another {name} generation is not the wanted subject"
        );
        assert_eq!(
            p.current().map(|l| l.handle()),
            Some(handle),
            "the rejected {name} frame must not become current"
        );
    }

    // The UI asking for the new generation drops the old frame immediately: those pixels came
    // from the previous shader/cache generation.
    let mut p = Presenter::new();
    p.want(KeySpec::default().key());
    assert_eq!(
        p.offer(h.lease(&KeySpec::default()), DeviceEpoch(1)),
        Offer::Presented
    );
    let bumped = KeySpec {
        renderer_version: 2,
        cache_version: 2,
        ..Default::default()
    };
    p.want(bumped.key());
    assert!(
        p.current().is_none(),
        "a generation bump retires the frame rendered before it"
    );
    assert_eq!(p.offer(h.lease(&bumped), DeviceEpoch(1)), Offer::Presented);
}
