//! The import report surfaced to the user after converting an external preset: which settings mapped
//! cleanly, which were approximated (and why), and which were dropped (and why). Honest feedback is
//! the whole point — Lightroom's look cannot be reproduced exactly on our scene-linear ProPhoto+ACR
//! engine, so we tell the user what survived the conversion.

use serde::Serialize;

/// One reported setting: a key (LR `crs:` name or our field) plus an optional explanatory note.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReportItem {
    pub key: String,
    pub note: String,
}

impl ReportItem {
    pub fn new(key: &str, note: &str) -> Self {
        Self {
            key: key.to_string(),
            note: note.to_string(),
        }
    }
}

/// Outcome of importing one preset file. Serializes to the frontend's import-report modal.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub source_format: String,
    pub source_process_version: Option<String>,
    /// Settings transferred faithfully.
    pub mapped: Vec<ReportItem>,
    /// Settings transferred with a caveat (magnitude-only, color-space mismatch, …).
    pub approximated: Vec<ReportItem>,
    /// Settings present in the source but not applied (no target, or structurally incompatible).
    pub dropped: Vec<ReportItem>,
}

impl ImportReport {
    pub fn mark_clean(&mut self, key: &str) {
        self.mapped.push(ReportItem::new(key, ""));
    }
    pub fn mark_approx(&mut self, key: &str, note: &str) {
        self.approximated.push(ReportItem::new(key, note));
    }
    pub fn mark_dropped(&mut self, key: &str, note: &str) {
        self.dropped.push(ReportItem::new(key, note));
    }
}
