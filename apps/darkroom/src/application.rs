pub use crate::native_app::NativeApplication;
use crate::photo_worker;
use core_app::state::DocumentStatus;
use core_app::workflow::Command;
use core_app::AppError;
use core_model::identity::{
    CacheVersion, DeviceEpoch, DisplayRevision, FrameKey, ImageId, RenderIdentity, RendererVersion,
    SourceIdentity, SourceRevision,
};
use render_wgpu::QualityTier;
use std::{path::PathBuf, sync::Arc, time::Instant};
use ui_egui::{cropped_frame_aspect, fit_output, viewport_revision};

/// Why a `--catalog-probe` run failed. Each variant maps to a distinct process exit code
/// (see the table in `main.rs`); `kind` is the machine-readable failure category printed
/// in the error JSON on stdout.
#[derive(Debug)]
pub enum ProbeError {
    /// The catalog never became ready: open failed (corrupt file, schema written by a newer
    /// build, unwritable path) or readiness was never reported.
    Open { kind: &'static str, error: String },
    /// The selection effect failed, or the editor did not end up holding exactly the row
    /// the library query reported (id / filename / content hash must match).
    Selection { error: String },
    /// `RequestShutdown` did not run the runtime to completion (`join_shutdown` never happened).
    Shutdown { error: String },
    /// The catalog opened, but a subsequent read (library query) failed.
    Query { error: String },
    /// The photo worker could not produce a frame (decode, GPU, or timeout), or the produced
    /// frame did not match the offscreen reference render byte-for-byte.
    Render { error: String },
    /// The selected document is not renderable by this slice: not `Ready`, or an edit the SDR
    /// host refuses (AI denoise / AI masks).
    Document { error: String },
}
impl ProbeError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Open { kind, .. } => kind,
            Self::Selection { .. } => "selection",
            Self::Shutdown { .. } => "shutdown",
            Self::Query { .. } => "query",
            Self::Render { .. } => "render",
            Self::Document { .. } => "document",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Self::Open { error, .. }
            | Self::Selection { error, .. }
            | Self::Shutdown { error, .. }
            | Self::Query { error, .. }
            | Self::Render { error, .. }
            | Self::Document { error, .. } => error,
        }
    }
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Open { .. } => 2,
            Self::Selection { .. } => 3,
            Self::Shutdown { .. } => 4,
            Self::Query { .. } => 5,
            Self::Render { .. } => 6,
            Self::Document { .. } => 7,
        }
    }
}

/// Machine-readable category of an open-time `AppError` (subset of the `AppError` enum;
/// everything not listed is generic catalog IO).
fn app_error_kind(error: &AppError) -> &'static str {
    match error {
        AppError::SchemaTooNew { .. } => "schema_too_new",
        AppError::Corrupt(_) => "corrupt",
        AppError::NotOpen => "not_open",
        _ => "catalog",
    }
}

fn open_error(error: AppError) -> ProbeError {
    ProbeError::Open {
        kind: app_error_kind(&error),
        error: error.to_string(),
    }
}

/// Headless catalog health check. Unlike a naive snapshot dump, every stage must positively
/// prove itself: readiness after `wait_idle`, the selected row matching the queried row, and
/// a completed runtime shutdown — any unproven stage is an `Err`, never a success JSON.
pub fn probe(path: PathBuf, select_first: bool) -> Result<serde_json::Value, ProbeError> {
    let mut app = NativeApplication::new(path, false, Arc::new(|| {})).map_err(open_error)?;
    app.wait_idle().map_err(open_error)?;
    let initial = app.snapshot();
    if !initial.catalog_ready {
        return Err(match initial.error.as_ref() {
            Some(error) => open_error(error.clone()),
            None => ProbeError::Open {
                kind: "not_open",
                error: "catalog did not become ready".into(),
            },
        });
    }
    // Open succeeded but a follow-up read (library query / folders) failed: distinct from
    // an open failure so callers can tell "damaged catalog" from "unreadable contents".
    if let Some(error) = initial.error.as_ref() {
        return Err(ProbeError::Query {
            error: error.to_string(),
        });
    }

    let mut selected = serde_json::Value::Null;
    if let Some(first) = initial.library.rows.first().cloned() {
        if select_first {
            let selection_error = |error: AppError| ProbeError::Selection {
                error: error.to_string(),
            };
            app.dispatch(Command::SelectImage(Some(ImageId(first.id))))
                .map_err(selection_error)?;
            app.wait_idle().map_err(selection_error)?;
            let after = app.snapshot();
            if let Some(error) = after.error.as_ref() {
                return Err(ProbeError::Selection {
                    error: error.to_string(),
                });
            }
            match after.editor.image.as_ref() {
                None => {
                    return Err(ProbeError::Selection {
                        error: format!("editor holds no image after selecting id {}", first.id),
                    })
                }
                Some(image) => {
                    if image.id != first.id
                        || image.filename != first.filename
                        || image.content_hash != first.content_hash
                    {
                        return Err(ProbeError::Selection {
                            error: format!(
                                "selected image {:?} does not match library row {} ({}, {})",
                                (image.id, &image.filename, &image.content_hash),
                                first.id,
                                first.filename,
                                first.content_hash
                            ),
                        });
                    }
                    selected = serde_json::to_value(image).map_err(|e| ProbeError::Selection {
                        error: format!("selected image is not serializable: {e}"),
                    })?;
                }
            }
        }
    }

    let snapshot = app.snapshot();
    let result = serde_json::json!({
        "status": "ok",
        "catalog_epoch": snapshot.catalog_epoch.0,
        "count": snapshot.library.count,
        "empty": snapshot.library.count == 0 && snapshot.library.rows.is_empty(),
        "rows": snapshot.library.rows.as_ref(),
        "selected": selected,
    });

    let shutdown_error = |error: AppError| ProbeError::Shutdown {
        error: error.to_string(),
    };
    app.shutdown().map_err(shutdown_error)?;
    app.wait_idle().map_err(shutdown_error)?;
    // `pump` drains completed work and calls the runtime's `join_shutdown()` on the terminal
    // state, the same way the window host's `about_to_wait` exit path does.
    app.pump().map_err(shutdown_error)?;
    if !app.is_shutdown_complete() {
        return Err(ProbeError::Shutdown {
            error: "runtime shutdown did not complete".into(),
        });
    }
    Ok(result)
}

/// Fixed panel (physical px) the probe fits the frame into — deliberately SQUARE so an aspect
/// defect cannot hide: a 3:2 frame must come back 3:2-fitted, never panel-shaped. Fixed so runs
/// are comparable; clamped by the host code paths themselves (targets far below any device
/// limit).
const PHOTO_PROBE_PANEL: (u32, u32) = (1024, 1024);
const PHOTO_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Headless proof that native real-photo presentation is byte-exact: open the catalog, select an
/// image, require the document is `Ready`, then run the SAME `photo_worker` GPU path on a
/// surfaceless device and compare the presented frame, byte-for-byte (tolerance 0), against the
/// offscreen `DevelopPipeline::render_view` for the same prepared image, params and view.
pub fn photo_probe(
    path: PathBuf,
    image_id: Option<ImageId>,
) -> Result<serde_json::Value, ProbeError> {
    let mut app = NativeApplication::new(path, true, Arc::new(|| {})).map_err(open_error)?;
    app.wait_idle().map_err(open_error)?;
    let initial = app.snapshot();
    if !initial.catalog_ready {
        return Err(match initial.error.as_ref() {
            Some(error) => open_error(error.clone()),
            None => ProbeError::Open {
                kind: "not_open",
                error: "catalog did not become ready".into(),
            },
        });
    }
    if let Some(error) = initial.error.as_ref() {
        return Err(ProbeError::Query {
            error: error.to_string(),
        });
    }

    let selection_error = |error: AppError| ProbeError::Selection {
        error: error.to_string(),
    };
    let target = match image_id {
        Some(id) => Some(id),
        None => initial.library.rows.first().map(|row| ImageId(row.id)),
    };
    let Some(id) = target else {
        return Err(ProbeError::Document {
            error: "catalog has no images to render".into(),
        });
    };
    app.dispatch(Command::SelectImage(Some(id)))
        .map_err(selection_error)?;
    app.wait_idle().map_err(selection_error)?;
    let snapshot = app.snapshot();
    if let Some(error) = snapshot.error.as_ref() {
        return Err(ProbeError::Selection {
            error: error.to_string(),
        });
    }
    let image = snapshot
        .editor
        .image
        .as_ref()
        .filter(|row| ImageId(row.id) == id)
        .ok_or_else(|| ProbeError::Selection {
            error: format!("editor does not hold image {}", id.0),
        })?;
    if snapshot.editor.document.status != DocumentStatus::Ready {
        return Err(ProbeError::Document {
            error: format!(
                "document is {:?}, not Ready",
                snapshot.editor.document.status
            ),
        });
    }
    let params = snapshot
        .editor
        .document
        .params
        .clone()
        .ok_or_else(|| ProbeError::Document {
            error: "Ready document carries no params".into(),
        })?;
    if let Some(feature) = photo_worker::unsupported_reason(&params) {
        return Err(ProbeError::Document {
            error: format!(
                "edit uses unsupported feature: {feature} — Darkroom can't render this yet"
            ),
        });
    }
    let revision = snapshot
        .editor
        .document
        .revision
        .ok_or_else(|| ProbeError::Document {
            error: "Ready document carries no revision".into(),
        })?;
    let process_version =
        snapshot
            .editor
            .document
            .process_version
            .ok_or_else(|| ProbeError::Document {
                error: "Ready document carries no process version".into(),
            })?;

    // Surfaceless GPU — same construction the `--gpu-probe` uses.
    let gpu = crate::gpu::Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle()),
        None,
    )
    .map_err(|error| ProbeError::Render {
        error: format!("GPU unavailable: {error}"),
    })?;
    let device_epoch = DeviceEpoch(1);
    let source_path = PathBuf::from(&image.path);

    // Decode the reference once up front. Its true frame dims are the aspect ground truth —
    // what the pipeline outputs for this edit is the CROPPED frame of THIS decode — while the
    // wanted size mirrors the window's planning path (catalog display dims + crop, fitted into
    // the fixed square panel). The aspect check below compares the two derivations, so the
    // reported `aspect` is never a tautology of a key the probe built itself: catalog-dims
    // drift from the real decode fails the probe.
    let mut reference_params = (*params).clone();
    reference_params.display_referred = core_raw::is_display(&source_path);
    let source = core_raw::source_from_path(&source_path).map_err(|error| ProbeError::Render {
        error: format!("reference decode failed: {error}"),
    })?;
    let linear = core_raw::develop_linear(&source).map_err(|error| ProbeError::Render {
        error: format!("reference develop failed: {error}"),
    })?;
    let expected_aspect = cropped_frame_aspect(
        &params.crop,
        Some(i64::from(linear.width)),
        Some(i64::from(linear.height)),
        1.0,
    );
    let planned_aspect = cropped_frame_aspect(&params.crop, image.width, image.height, 1.0);
    let (width, height) = fit_output(PHOTO_PROBE_PANEL, planned_aspect);
    let key = FrameKey {
        identity: RenderIdentity {
            source: SourceIdentity {
                catalog_epoch: snapshot.catalog_epoch,
                image_id: id,
                source_revision: SourceRevision(0),
            },
            editor_session: snapshot.editor.editor_session,
            selection_generation: snapshot.editor.selection_generation,
            edit_revision: revision,
            process_version,
            renderer_version: RendererVersion(1),
            cache_version: CacheVersion(1),
            viewport_revision: viewport_revision(width, height),
            display_revision: DisplayRevision(0),
            device_epoch,
        },
        viewport_px: (width, height),
        scale_milli: 1000,
        quality: QualityTier::Settled,
    };
    let view = photo_worker::fit_view(width, height);

    let mut worker = photo_worker::PhotoWorker::start(
        &gpu.context,
        Arc::clone(&gpu.pipeline),
        device_epoch,
        Arc::new(|| {}),
    )
    .map_err(|error| ProbeError::Render { error })?;
    worker.request(photo_worker::PhotoRequest {
        key,
        path: source_path.clone(),
        content_hash: image.content_hash.clone(),
        params: Arc::clone(&params),
        view,
    });

    // A diagnostic renderer over the same device: `read_back` is explicit and never used by the
    // presentation path itself.
    let reader = render_wgpu::Renderer::new(
        core_pipeline::GpuContext {
            device: gpu.context.device.clone(),
            queue: gpu.context.queue.clone(),
            info: gpu.context.info.clone(),
            max_texture_dim: gpu.context.max_texture_dim,
        },
        Arc::clone(&gpu.pipeline),
        device_epoch,
        render_wgpu::PoolLimits::default(),
    );

    let deadline = Instant::now() + PHOTO_PROBE_TIMEOUT;
    let mut outcome: Option<Result<render_wgpu::FrameLease, ProbeError>> = None;
    while outcome.is_none() {
        for result in worker.drain_results() {
            outcome = Some(match result {
                photo_worker::PhotoResult::Frame(lease) => Ok(lease),
                photo_worker::PhotoResult::Failed { message, .. } => {
                    Err(ProbeError::Render { error: message })
                }
                photo_worker::PhotoResult::WorkerDied(error) => Err(ProbeError::Render { error }),
                photo_worker::PhotoResult::Abandoned(_)
                | photo_worker::PhotoResult::Histogram { .. } => continue,
            });
            if outcome.is_some() {
                break;
            }
        }
        if outcome.is_none() {
            if Instant::now() >= deadline {
                outcome = Some(Err(ProbeError::Render {
                    error: format!(
                        "photo worker produced no frame within {}s",
                        PHOTO_PROBE_TIMEOUT.as_secs()
                    ),
                }));
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    let _ = worker.shutdown();
    let lease = outcome.expect("loop ends with an outcome")?;

    // Aspect proof: the presented frame must carry the cropped frame's aspect, not the panel's
    // (the squashed-presentation regression this probe exists to catch). Tolerance is the
    // rounding of the short side to integer pixels: |aspect - expected| <= 1/min(w, h).
    let (frame_w, frame_h) = lease.key().viewport_px;
    let aspect = f64::from(frame_w) / f64::from(frame_h);
    let tolerance = 1.0 / f64::from(frame_w.min(frame_h).max(1));
    if (aspect - expected_aspect).abs() > tolerance {
        return Err(ProbeError::Render {
            error: format!(
                "presented frame is not aspect-correct: {frame_w}×{frame_h} (aspect \
                 {aspect:.6}) does not match the cropped frame aspect {expected_aspect:.6} \
                 within {tolerance:.6}"
            ),
        });
    }

    let native = reader
        .read_back(&lease)
        .map_err(|error| ProbeError::Render {
            error: format!("readback failed: {error}"),
        })?;

    // Offscreen reference: prepare + `render_view` on the same decode with identical inputs.
    let reference = {
        let prepared = gpu
            .pipeline
            .prepare(&gpu.context, &linear)
            .map_err(|error| ProbeError::Render {
                error: format!("reference prepare failed: {error}"),
            })?;
        gpu.pipeline
            .render_view(&gpu.context, &prepared, &reference_params, &view)
            .map_err(|error| ProbeError::Render {
                error: format!("reference render failed: {error}"),
            })?
    };

    let identical = native == reference;
    let mean_rgb = if native.is_empty() {
        [0.0; 3]
    } else {
        let mut sums = [0u64; 3];
        for pixel in native.chunks_exact(4) {
            for channel in 0..3 {
                sums[channel] += u64::from(pixel[channel]);
            }
        }
        let count = u64::try_from(native.len() / 4).unwrap_or(1).max(1);
        std::array::from_fn(|channel| sums[channel] as f64 / count as f64)
    };

    let resources = worker.resource_report();
    let shutdown_error = |error: AppError| ProbeError::Shutdown {
        error: error.to_string(),
    };
    // Present the produced frame through the window's exact egui path — same registration
    // helper, one egui frame painted into an offscreen target inside a Validation error scope
    // (see `present.rs`). This is what the read-back-only probe could not see: the real-window
    // crash where egui's `egui_user_image_0` bind group was rejected because the pooled target
    // lacked TEXTURE_BINDING. Any validation error is a render failure (exit 6).
    let composited_diff = crate::present::paint_probe_frame(&gpu, &lease)
        .map_err(|error| ProbeError::Render { error })?;
    app.shutdown().map_err(shutdown_error)?;
    app.wait_idle().map_err(shutdown_error)?;
    app.pump().map_err(shutdown_error)?;
    if !app.is_shutdown_complete() {
        return Err(ProbeError::Shutdown {
            error: "runtime shutdown did not complete".into(),
        });
    }

    Ok(serde_json::json!({
        "status": if identical { "ok" } else { "mismatch" },
        "image_id": image.id,
        // Read from the delivered lease, not the requested key: the aspect fields describe
        // the frame that actually arrived.
        "width": frame_w,
        "height": frame_h,
        "aspect": aspect,
        "expected_aspect": expected_aspect,
        "composited_mean_abs_diff": composited_diff,
        "mean_rgb": mean_rgb,
        "identical": identical,
        "egui_presented": true,
        // Nominal allocation arithmetic, not measured memory.
        "resources": {
            "prepared_gpu_bytes": resources.prepared_gpu_bytes,
            "prepared_count": resources.prepared_count,
            "pool_bytes": resources.pool_bytes,
            "pool_limit_bytes": resources.pool_limit_bytes,
            "last_decode_cpu_bytes": resources.last_decode_cpu_bytes,
            "peak_decode_cpu_bytes": resources.peak_decode_cpu_bytes,
            "over_budget": resources.over_budget,
        },
    }))
}
