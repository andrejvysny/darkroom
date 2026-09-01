//! The unified AI-scan job: one user action, one guard, one cancel flag.
//!
//! Sequencing the analysis pass and panorama detection from the frontend looked simpler, but it
//! could not be made correct:
//!
//! * `analysis_cancel` is a no-op unless `analysis_running` is already set, while model downloads
//!   happen *before* the pass starts — so pressing Stop during a 190 MB download did nothing and the
//!   scan began anyway once it finished.
//! * The two jobs have independent guards (`analysis_running`, `pano_detect_running`), so nothing
//!   prevented both CPU-bound passes running at once.
//! * A renderer-side "abort" flag cannot survive a reload, and `triggerAnalysis` swallows its
//!   errors, so the caller could not tell success from failure from never-started.
//!
//! Owning the whole operation here fixes all three: `scan_cancel` is checked before the download,
//! between phases, and before panorama, and it works no matter which phase is live.

use std::sync::atomic::{AtomicBool, Ordering};

use core_library::{ScanScope, StageId};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

/// Every stage that runs inside the unified analysis pass (i.e. all but panorama detection, which
/// is a separate job). Used as the default for the legacy `analysis_run` entry point.
pub const AI_STAGES: [StageId; 5] = [
    StageId::Objects,
    StageId::Animals,
    StageId::Faces,
    StageId::Captions,
    StageId::Embeddings,
];

/// What one scan run should do.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSelection {
    pub stages: Vec<StageId>,
    /// Container to narrow to; `None`/empty = whole library. Never applies to panorama detection,
    /// which has no scoped entry point.
    #[serde(default)]
    pub scope: Option<ScanScope>,
    /// Re-run stages that are already up to date.
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    /// True when the run stopped early because the user asked it to. Not an error — everything
    /// committed before the stop is kept.
    pub cancelled: bool,
    pub analyzed: usize,
    pub failed: usize,
    /// Freshly-suggested panorama groups, when that stage ran.
    pub panoramas_found: usize,
}

/// Resets `scan_running`/`scan_cancel` on drop so an early return or panic can't wedge the gate.
struct RunGuard<'a> {
    running: &'a AtomicBool,
    cancel: &'a AtomicBool,
}
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.cancel.store(false, Ordering::SeqCst);
    }
}

/// Request the running scan to stop, whichever phase it is in. Unlike the per-job cancels this is
/// meaningful even before any pass has started (i.e. during a model download).
pub fn cancel(st: &AppState) {
    if st.scan_running.load(Ordering::SeqCst) {
        st.scan_cancel.store(true, Ordering::SeqCst);
        // Forward to whichever phase is actually live so it stops between batches/clusters.
        st.analysis_cancel.store(true, Ordering::SeqCst);
        st.pano_detect_cancel.store(true, Ordering::SeqCst);
        st.analysis_dl_cancel.store(true, Ordering::SeqCst);
        st.faces_dl_cancel.store(true, Ordering::SeqCst);
    }
}

/// Whether a unified scan is in flight — lets the UI re-attach after a reload instead of showing an
/// idle button while a scan is still running in the background.
pub fn is_running(st: &AppState) -> bool {
    st.scan_running.load(Ordering::SeqCst)
}

/// Run the selected stages as ONE job: ensure models → analysis pass → panorama detection.
pub fn run<R: Runtime>(app: &AppHandle<R>, sel: &ScanSelection) -> Result<ScanResult, String> {
    let st = app.state::<AppState>();
    if st.scan_running.swap(true, Ordering::SeqCst) {
        return Err("a scan is already running".into());
    }
    st.scan_cancel.store(false, Ordering::SeqCst);
    let _guard = RunGuard {
        running: &st.scan_running,
        cancel: &st.scan_cancel,
    };

    if sel.stages.is_empty() {
        return Err("no scan stages selected".into());
    }
    // Reject an unavailable stage rather than dropping it: a silently-skipped stage is
    // indistinguishable from one that ran and found nothing, and the UI would report work that
    // never happened.
    for stage in &sel.stages {
        if !crate::analysis::stage_models_ready(&st, *stage) {
            return Err(format!("{} models are not installed", stage_label(*stage)));
        }
    }

    let cancelled = || st.scan_cancel.load(Ordering::SeqCst);
    let ai: Vec<StageId> = sel
        .stages
        .iter()
        .copied()
        .filter(|s| *s != StageId::Panoramas)
        .collect();
    let scope = sel.scope.clone().unwrap_or_default();
    let mut out = ScanResult::default();

    if !ai.is_empty() {
        if cancelled() {
            out.cancelled = true;
            return Ok(finish(app, out));
        }
        let stats = crate::analysis::run_pass(app, sel.force, &scope, &ai)?;
        out.analyzed = stats.analyzed;
        out.failed = stats.failed;
    }

    if sel.stages.contains(&StageId::Panoramas) {
        // A Stop during the analysis phase must not roll straight into panorama detection.
        if cancelled() {
            out.cancelled = true;
            return Ok(finish(app, out));
        }
        // `run_pass`'s RunGuard cleared `analysis_cancel` on the way out; clear the panorama flag
        // too so a cancel from the *previous* phase can't abort this one before it starts.
        st.pano_detect_cancel.store(false, Ordering::SeqCst);
        out.panoramas_found = crate::pano_detect::run(app, sel.force)?;
    }

    out.cancelled = cancelled();
    Ok(finish(app, out))
}

fn finish<R: Runtime>(app: &AppHandle<R>, out: ScanResult) -> ScanResult {
    let _ = app.emit("scan:done", &out);
    // Top up `image_features` for anything still missing it. Not on the cancel path: the user just
    // asked for the machine back, and the next scan or import picks the work up anyway.
    if !out.cancelled {
        crate::features::spawn_backfill(app);
        // Fresh embeddings can make previously unusable labels trainable, so this is the natural
        // place to (re)fit — but only when the labels themselves have actually moved on.
        crate::suggest::maybe_spawn_train(app);
    }
    out
}

fn stage_label(stage: StageId) -> &'static str {
    match stage {
        StageId::Objects => "Object detection",
        StageId::Animals => "Animal detection",
        StageId::Faces => "Face detection",
        StageId::Captions => "Caption",
        StageId::Embeddings => "Embedding",
        StageId::Panoramas => "Panorama",
    }
}
