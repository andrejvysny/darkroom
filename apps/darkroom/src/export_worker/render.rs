//! The production export body: one photo through the develop pipeline to a new file.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{ExportItem, ExportJob, Outcome};
use crate::photo_worker::unsupported_reason;
use core_app::export::ExportFormat;
use core_model::document::{EditDocument, ReadStatus};
use core_model::identity::{ImageId, CURRENT_PROCESS_VERSION};
use core_model::DevelopParams;
use core_pipeline::{DevelopPipeline, GpuContext};
use core_runtime::catalog::CatalogAdapter;
use core_runtime::export_io::{ensure_not_source, write_new_export};

/// What the production body needs.
pub struct ExportDeps {
    pub catalog: Arc<CatalogAdapter>,
    pub ctx: GpuContext,
    pub pipeline: Arc<DevelopPipeline>,
}

/// Edit values for `image`: the open document's when it is that image (unsaved edits export as
/// shown), else the stored edit, else defaults when none was ever saved. `stored` is only called
/// for the latter.
pub(super) fn resolve_params(
    open_doc: Option<&(ImageId, Arc<DevelopParams>)>,
    image: ImageId,
    stored: impl FnOnce() -> Result<Option<DevelopParams>, String>,
) -> Result<DevelopParams, String> {
    if let Some((open, params)) = open_doc {
        if *open == image {
            return Ok((**params).clone());
        }
    }
    Ok(stored()?.unwrap_or_default())
}

fn stored_params(json: String, version: i64) -> Result<DevelopParams, String> {
    let document = EditDocument::read(json, version);
    match (document.status(), document.preview()) {
        (ReadStatus::Supported, Some(params)) => Ok(params.clone()),
        _ => Err("the stored edit cannot be interpreted by this build".into()),
    }
}

pub(super) fn export_one(
    deps: &ExportDeps,
    job: &ExportJob,
    item: &ExportItem,
    cancel: &AtomicBool,
) -> Result<Outcome, String> {
    let stopped = || cancel.load(Ordering::SeqCst);
    if stopped() {
        return Ok(Outcome::Cancelled);
    }
    let (generation, database) = deps
        .catalog
        .shared()
        .map_err(|error| format!("the catalog is not open: {error}"))?;
    let (path, stored, sources) = {
        let db = database.lock().unwrap_or_else(PoisonError::into_inner);
        let row = core_library::image_by_id(&db.conn, item.image.0)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "image not found".to_owned())?;
        let stored = core_library::edits::get_edit_document_row(&db.conn, item.image.0)
            .map_err(|error| format!("could not read the edit: {error}"))?;
        let sources =
            core_library::index::all_image_paths(&db.conn).map_err(|error| error.to_string())?;
        (row.path, stored, sources)
    };
    let dest = job.dir.join(&item.file_name);
    ensure_not_source(&dest, &sources)?;
    let mut params = resolve_params(job.open_doc.as_ref(), item.image, || {
        stored
            .map(|(json, version, _)| stored_params(json, version))
            .transpose()
    })?;
    if let Some(reason) = unsupported_reason(&params) {
        return Err(format!(
            "{reason} edits cannot be exported by this build yet"
        ));
    }
    params.display_referred = core_raw::is_display(Path::new(&path));
    if stopped() {
        return Ok(Outcome::Cancelled);
    }

    let source = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
    let linear = core_raw::develop_linear(&source).map_err(|e| e.to_string())?;
    // Full-res export needs a source-sized GPU texture; fail with a clear message instead of an
    // opaque pipeline error.
    let max_edge = deps.ctx.max_texture_dim;
    if linear.width > max_edge || linear.height > max_edge {
        return Err(format!(
            "image is {}×{} but this GPU supports at most {max_edge}px per side for export",
            linear.width, linear.height
        ));
    }
    if stopped() {
        return Ok(Outcome::Cancelled);
    }
    let (rgba, cw, ch) = deps
        .pipeline
        .render_cropped_with_ai_coverages(&deps.ctx, &linear, &params, &[])
        .map_err(|e| e.to_string())?;
    let (rgba, width, height) = if params.border.is_active() {
        core_pipeline::frame_rgba8(
            &rgba,
            cw,
            ch,
            params.border.color_rgba8(),
            params.border.size,
            params.border.aspect(),
        )
    } else {
        (rgba, cw, ch)
    };
    let quality = job.quality.clamp(1, 100);
    let bytes = match job.format {
        ExportFormat::Png => core_pipeline::rgba8_to_png(&rgba, width, height),
        ExportFormat::Jpeg => core_pipeline::rgba8_to_jpeg(&rgba, width, height, quality),
    }
    .map_err(|e| e.to_string())?;

    // Re-checked after the render: a long render is a window for the catalog to change.
    if !deps.catalog.is_current(generation) {
        return Err("the catalog was closed during the export".into());
    }
    let sources = {
        let db = database.lock().unwrap_or_else(PoisonError::into_inner);
        core_library::index::all_image_paths(&db.conn).map_err(|error| error.to_string())?
    };
    ensure_not_source(&dest, &sources)?;
    write_new_export(&dest, &bytes)?;
    log_export(&database, item.image, &params, job.format, quality);
    Ok(Outcome::Written)
}

/// Export is the strongest edit-quality endorsement (legacy `develop.export`); best-effort.
fn log_export(
    database: &core_runtime::catalog::SharedDb,
    image: ImageId,
    params: &DevelopParams,
    format: ExportFormat,
    quality: u8,
) {
    let format = match format {
        ExportFormat::Jpeg => "jpeg",
        ExportFormat::Png => "png",
    };
    let event = core_library::Event {
        ts_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        session_id: "native".into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        event_type: "develop.export".into(),
        image_id: Some(image.0),
        process_version: Some(CURRENT_PROCESS_VERSION.0),
        params_after: serde_json::to_string(params).ok(),
        context: Some(serde_json::json!({ "format": format, "quality": quality }).to_string()),
        ..Default::default()
    };
    let db = database.lock().unwrap_or_else(PoisonError::into_inner);
    if let Err(error) = core_library::append_event(&db.conn, &event) {
        eprintln!("export event not logged: {error}");
    }
}
