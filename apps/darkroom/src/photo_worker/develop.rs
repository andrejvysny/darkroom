//! The worker's jobs: decode + upload a source (`prepare_image`), render a frame (`develop`),
//! warm the cache ahead of the edit document (`warm`) and the histogram follow-up.

use std::sync::Arc;
use std::time::{Duration, Instant};

use core_model::identity::QualityTier;
use core_pipeline::backend::PreparedImage;
use core_pipeline::{DevelopPipeline, GpuContext, ViewParams};
use render_wgpu::Renderer;

use super::prefetch;
use super::prepared::{elapsed_ms, HistogramJob, PreparedCache, SourceKey, StageTimings};
use super::{unsupported_reason, Ctl, Job, PhotoRequest, PhotoResult, WarmRequest};

/// Longest a render waits for a pooled target (every target of its size leased, e.g. while the
/// presenter still holds the previous frame) before surfacing "busy".
const POOL_WAIT_LIMIT: Duration = Duration::from_secs(5);
/// Re-check interval while waiting: a missed wakeup costs at most this much.
const POOL_WAIT_SLICE: Duration = Duration::from_millis(100);

/// Why [`render_until_free`] gave up.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RenderStop {
    Abandoned,
    Busy,
    Failed(String),
}

/// One worker job on the real GPU (the production runner body).
pub(crate) fn run(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    renderer: &Renderer,
    job: Job,
    cache: &mut PreparedCache,
    ctl: &Ctl<'_>,
) -> Option<PhotoResult> {
    match job {
        Job::Render(request) => develop(ctx, pipeline, renderer, request, cache, ctl),
        Job::Warm(request) => {
            warm(ctx, pipeline, &request, cache, ctl);
            None
        }
        Job::Upload(decoded) => {
            upload(ctx, pipeline, *decoded, cache);
            None
        }
        Job::Histogram(job) => {
            let started = Instant::now();
            let result = histogram_result(ctx, pipeline, &job);
            cache.timings.histogram_ms = elapsed_ms(started);
            result
        }
    }
}

/// Retry `attempt` while the pool is exhausted, waiting for a released target (the pool's return
/// signal wakes [`Ctl::wait_pool`]) and re-checking supersession/stop before every attempt, so a
/// superseded or shutting-down request leaves promptly. Bounded by [`POOL_WAIT_LIMIT`].
pub(crate) fn render_until_free<T>(
    ctl: &Ctl<'_>,
    source: &SourceKey,
    mut attempt: impl FnMut() -> Result<T, render_wgpu::RenderError>,
) -> Result<T, RenderStop> {
    let deadline = Instant::now() + POOL_WAIT_LIMIT;
    loop {
        if ctl.output_superseded(source) {
            return Err(RenderStop::Abandoned);
        }
        let seen = ctl.pool_returns();
        match attempt() {
            Ok(value) => return Ok(value),
            Err(render_wgpu::RenderError::PoolExhausted { .. }) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(RenderStop::Busy);
                }
                ctl.wait_pool(seen, source, POOL_WAIT_SLICE.min(deadline - now));
            }
            Err(error) => return Err(RenderStop::Failed(format!("develop failed: {error}"))),
        }
    }
}

/// Decode + upload `path` (cached by `source`), checking supersession between stages. `Ok(None)`
/// = abandoned. Only a request for ANOTHER source abandons this work: a newer request for the
/// same photo (new params, new viewport) needs exactly this upload.
fn prepare_image(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    source: &SourceKey,
    path: &std::path::Path,
    cache: &mut PreparedCache,
    ctl: &Ctl<'_>,
) -> Result<Option<Arc<PreparedImage>>, String> {
    if let Some(prepared) = cache.get(source).cloned() {
        cache.timings.prepared_cache_hit = true;
        return Ok(Some(prepared));
    }
    // A neighbour the prefetch thread already decoded (the full-quality decode, same as below).
    let promoted = cache
        .handoff
        .as_ref()
        .and_then(|handoff| handoff.take_matching(source));
    let linear = if let Some(linear) = promoted {
        cache.timings.prefetch_hit = true;
        linear
    } else {
        let started = Instant::now();
        let raw = core_raw::source_from_path(path).map_err(|error| {
            crate::presence::missing_source_message(path, &error)
                .unwrap_or_else(|| format!("could not read {}: {error}", path.display()))
        })?;
        cache.timings.read_ms = elapsed_ms(started);
        if ctl.source_superseded(source) {
            return Ok(None);
        }
        let started = Instant::now();
        let linear = core_raw::develop_linear(&raw)
            .map_err(|error| format!("could not develop {}: {error}", path.display()))?;
        cache.timings.develop_ms = elapsed_ms(started);
        if ctl.source_superseded(source) {
            return Ok(None);
        }
        linear
    };
    cache.note_decode(std::mem::size_of_val(linear.data.as_slice()) as u64);
    // Evict BEFORE the expensive upload. Masks may be allocated lazily later, so reserve them.
    let (w, h) = (linear.width, linear.height);
    cache.make_room(
        core_pipeline::backend::estimate_prepare_bytes(w, h)
            + core_pipeline::backend::estimate_mask_bytes(w, h),
        None,
    );
    let started = Instant::now();
    let prepared = pipeline
        .prepare(ctx, &linear)
        .map_err(|error| format!("GPU prepare failed: {error}"))?;
    cache.timings.prepare_ms = elapsed_ms(started);
    let prepared = Arc::new(prepared);
    // Cached even if superseded right after: it is valid work for this source.
    cache.put(source.clone(), Arc::clone(&prepared));
    Ok(Some(prepared))
}

/// Upload a prefetched neighbour if memory admission allows; otherwise drop the decoded image.
/// Runs only when no render/warm was queued, and never touches `timings` (those describe the
/// photo on screen). Failures are silent: speculation must not surface errors.
fn upload(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    (source, linear): prefetch::Decoded,
    cache: &mut PreparedCache,
) {
    // Admitted by what an upload actually holds. The mask array is allocated lazily by the first
    // masked render; it is reserved when this photo is opened (see `develop`), not speculatively —
    // counting it here would keep every 32 MP neighbour out of the budget.
    let incoming = core_pipeline::backend::estimate_prepare_bytes(linear.width, linear.height);
    if cache.contains(&source) || !cache.admit_speculative(incoming) {
        return;
    }
    if let Ok(prepared) = pipeline.prepare(ctx, &linear) {
        cache.put_speculative(source, Arc::new(prepared));
    }
}

/// Decode + upload the selected photo while its edit document is still loading, so the first
/// render finds it cached. Failures are silent here: the render request reports them.
fn warm(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    request: &WarmRequest,
    cache: &mut PreparedCache,
    ctl: &Ctl<'_>,
) {
    let source = request.source_key();
    cache.timings = StageTimings::default();
    cache.timings.prefetch_hit = cache.pin(&source);
    if cache.contains(&source) {
        return;
    }
    let _ = prepare_image(ctx, pipeline, &source, &request.path, cache, ctl);
    if crate::open_journey::enabled() {
        trace_timings("warm", &cache.timings);
    }
}

/// Edge of the square render the histogram is counted over (legacy used 384; 256 is plenty for
/// 256 bins and keeps the extra readback tiny).
const HIST_RENDER_EDGE: u32 = 256;

/// Whole-crop histogram of a frame already delivered. Failure is silent: the histogram is a
/// readout, never a reason to fail the frame.
fn histogram_result(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    job: &HistogramJob,
) -> Option<PhotoResult> {
    let view = ViewParams {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
        out_w: HIST_RENDER_EDGE,
        out_h: HIST_RENDER_EDGE,
        active: true,
        overlay_layer: -1,
        overlay_color: [0.0, 0.0, 0.0],
        overlay_strength: 0.0,
        crop_preview: false,
    };
    match pipeline.render_view(ctx, &job.prepared, &job.params, &view) {
        Ok(rgba) => {
            let h = core_pipeline::histogram(&rgba);
            Some(PhotoResult::Histogram {
                key: job.key,
                r: h.r,
                g: h.g,
                b: h.b,
            })
        }
        Err(error) => {
            eprintln!("histogram render failed: {error}");
            None
        }
    }
}

/// Decode (or reuse the cached upload) + develop into a leased target. Worker thread only.
fn develop(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    renderer: &Renderer,
    request: PhotoRequest,
    cache: &mut PreparedCache,
    ctl: &Ctl<'_>,
) -> Option<PhotoResult> {
    let key = request.key;
    let source = request.source_key();
    cache.timings = StageTimings::default();
    cache.timings.prefetch_hit = cache.pin(&source);
    let failed = |message: String| Some(PhotoResult::Failed { key, message });
    if let Some(feature) = unsupported_reason(&request.params) {
        return failed(format!(
            "Darkroom can't render this photo yet: {feature} is not supported by this build — \
             nothing was changed."
        ));
    }
    if ctl.output_superseded(&source) {
        return Some(PhotoResult::Abandoned(key));
    }
    // Intrinsic to the file, derived here — never trust a persisted `display_referred`
    // (legacy `decode_develop` does exactly this).
    let mut params = (*request.params).clone();
    params.display_referred = core_raw::is_display(&request.path);

    let prepared = match prepare_image(ctx, pipeline, &source, &request.path, cache, ctl) {
        Ok(Some(prepared)) => prepared,
        Ok(None) => return Some(PhotoResult::Abandoned(key)),
        Err(message) => return failed(message),
    };
    if cache.timings.prefetch_hit {
        // A prefetched upload was admitted without its lazy mask reservation; now that it is the
        // open photo, make that room by evicting others (never this one: it is pinned).
        cache.make_room(
            core_pipeline::backend::estimate_mask_bytes(prepared.width, prepared.height),
            Some(&source),
        );
    }
    let started = Instant::now();
    let rendered = render_until_free(ctl, &source, || {
        renderer.render(key, &prepared, &params, &request.view)
    });
    cache.timings.render_ms = elapsed_ms(started);
    match rendered {
        Ok(lease) => {
            // The histogram (a second 256² render plus a blocking readback) is owed only for
            // settled frames and runs AFTER this frame is delivered (see `worker_loop`).
            if key.quality == QualityTier::Settled {
                cache.histogram_due = Some(Box::new(HistogramJob {
                    key,
                    prepared,
                    params,
                }));
            }
            Some(PhotoResult::Frame(lease))
        }
        Err(RenderStop::Abandoned) => Some(PhotoResult::Abandoned(key)),
        Err(RenderStop::Busy) => failed("GPU render targets are busy; try again".into()),
        Err(RenderStop::Failed(message)) => failed(message),
    }
}

/// One stderr line per finished job (`DARKROOM_TRACE_OPEN=1`).
pub(crate) fn trace_timings(outcome: &str, t: &StageTimings) {
    eprintln!(
        "open/worker {outcome} cache_hit={} prefetch_hit={} read_ms={} develop_ms={} prepare_ms={} render_ms={} \
         histogram_ms={}",
        t.prepared_cache_hit, t.prefetch_hit, t.read_ms, t.develop_ms, t.prepare_ms, t.render_ms, t.histogram_ms
    );
}

/// What a finished render job is, for the trace line.
pub(crate) fn outcome(result: &Option<PhotoResult>) -> String {
    match result {
        Some(PhotoResult::Frame(lease)) => format!("frame {:?}", lease.key().quality),
        Some(PhotoResult::Failed { .. }) => "failed".to_owned(),
        Some(PhotoResult::Abandoned(_)) => "abandoned".to_owned(),
        Some(PhotoResult::Histogram { .. }) => "histogram".to_owned(),
        _ => "other".to_owned(),
    }
}
