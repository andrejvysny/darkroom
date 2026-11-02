//! Export workflow contract between the UI and the host's export worker.
//!
//! Export is long-running GPU + file IO, so it never runs on the catalog lane: the host owns an
//! [`ExportView`] (what the dialog shows) and executes the [`ExportRequest`]s the UI emits. The
//! naming rules here are the legacy `src/lib/export.ts` rules, kept byte-compatible so a batch
//! exported by either host lands under the same file names.

use core_model::identity::ImageId;
pub use core_model::settings::{ExportFormat, ExportPreset};
use std::collections::HashSet;
use std::path::PathBuf;

/// JPEG quality shown first (legacy `ExportModal` default).
pub const DEFAULT_QUALITY: u8 = 92;
/// Template shown when nothing was persisted.
pub const DEFAULT_TEMPLATE: &str = "{original}";
/// Built-in presets, never persisted and never deletable (legacy `BUILTIN_PRESETS`).
pub const BUILTIN_PRESETS: [(&str, &str); 3] = [
    ("Original name", "{original}"),
    ("Date + name", "{date}_{original}"),
    ("Name + sequence", "{original}_{seq}"),
];

/// One photo queued for export. `date` is the local `YYYY-MM-DD` the host formatted from the
/// capture date (else the import time); this crate has no clock or time zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportTarget {
    pub image: ImageId,
    pub filename: String,
    pub date: String,
}

/// File extension a format is written with.
pub fn extension(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Jpeg => "jpg",
        ExportFormat::Png => "png",
    }
}

/// Resolve `template` for one photo: `{original}` is the file name without its extension,
/// `{date}` the host-formatted day, `{seq}` the 1-based index padded to 3 digits. Characters
/// illegal in a file name on macOS/Windows become `_`; an empty result falls back to the base name
/// and then to `image`.
pub fn apply_template(template: &str, original_filename: &str, date: &str, seq: usize) -> String {
    let base = strip_extension(original_filename);
    let out = template
        .replace("{original}", base)
        .replace("{date}", date)
        .replace("{seq}", &format!("{seq:03}"));
    let out: String = out
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let out = out.trim();
    if !out.is_empty() {
        out.to_owned()
    } else if !base.is_empty() {
        base.to_owned()
    } else {
        "image".to_owned()
    }
}

/// Legacy `/\.[^.]+$/`: drop the last dot and what follows, but only when something follows.
fn strip_extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot + 1 < name.len() => &name[..dot],
        _ => name,
    }
}

/// Output file names (with extension) for `targets`, in order. Collisions are de-duplicated
/// case-insensitively (APFS default) with a `-N` suffix on the base name.
pub fn plan_names(targets: &[ExportTarget], template: &str, format: ExportFormat) -> Vec<String> {
    let ext = extension(format);
    let mut used: HashSet<String> = HashSet::new();
    targets
        .iter()
        .enumerate()
        .map(|(index, target)| {
            let name = apply_template(template, &target.filename, &target.date, index + 1);
            let mut unique = name.clone();
            let mut n = 1;
            while used.contains(&unique.to_lowercase()) {
                unique = format!("{name}-{n}");
                n += 1;
            }
            used.insert(unique.to_lowercase());
            format!("{unique}.{ext}")
        })
        .collect()
}

/// Where the export run stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ExportPhase {
    #[default]
    Idle,
    Running {
        done: usize,
        failed: usize,
        total: usize,
    },
    /// Cancel was requested; the current photo finishes, the rest are skipped.
    Cancelling {
        done: usize,
        failed: usize,
        total: usize,
    },
    Finished {
        done: usize,
        failed: usize,
        first_error: Option<String>,
        dir: PathBuf,
    },
}

/// Immutable export state published by the host each frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportView {
    /// The dialog is open.
    pub open: bool,
    pub targets: Vec<ExportTarget>,
    pub dir: Option<PathBuf>,
    pub template: String,
    pub format: ExportFormat,
    /// JPEG quality 1..=100 (ignored for PNG).
    pub quality: u8,
    /// User presets (the built-ins are [`BUILTIN_PRESETS`]).
    pub presets: Vec<ExportPreset>,
    pub phase: ExportPhase,
    /// Why the last Start / preset action was refused.
    pub error: Option<String>,
}

impl Default for ExportView {
    fn default() -> Self {
        Self {
            open: false,
            targets: Vec::new(),
            dir: None,
            template: DEFAULT_TEMPLATE.to_owned(),
            format: ExportFormat::Jpeg,
            quality: DEFAULT_QUALITY,
            presets: Vec::new(),
            phase: ExportPhase::Idle,
            error: None,
        }
    }
}

impl ExportView {
    /// A run is in flight (the dialog cannot be closed or reconfigured).
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            ExportPhase::Running { .. } | ExportPhase::Cancelling { .. }
        )
    }

    /// File names the targets resolve to under the current template and format.
    pub fn planned_names(&self) -> Vec<String> {
        plan_names(&self.targets, &self.template, self.format)
    }
}

/// UI -> host requests. The host executes them; none touches the catalog lane directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportRequest {
    /// Open the dialog for these photos (the host resolves them against the snapshot).
    Open(Vec<ImageId>),
    /// Close the dialog (refused while a run is in flight).
    Close,
    /// Show the platform folder picker for the destination.
    PickDir,
    SetTemplate(String),
    SetFormat(ExportFormat),
    SetQuality(u8),
    /// Save the current template as a user preset under this name (replaces a same-named one).
    SavePreset(String),
    DeletePreset(String),
    Start,
    /// Stop after the photo being rendered; the rest are skipped.
    Cancel,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> ExportTarget {
        ExportTarget {
            image: ImageId(1),
            filename: name.to_owned(),
            date: "2026-06-06".to_owned(),
        }
    }

    #[test]
    fn tokens_resolve_and_extension_is_stripped() {
        assert_eq!(
            apply_template("{date}_{original}_{seq}", "_55A3947.CR3", "2026-06-06", 7),
            "2026-06-06__55A3947_007"
        );
        assert_eq!(apply_template("{original}", "a.b.cr3", "d", 1), "a.b");
        assert_eq!(apply_template("{original}", "noext", "d", 1), "noext");
    }

    #[test]
    fn seq_pads_to_three_and_grows_past_it() {
        assert_eq!(apply_template("{seq}", "a.jpg", "d", 5), "005");
        assert_eq!(apply_template("{seq}", "a.jpg", "d", 1234), "1234");
    }

    #[test]
    fn illegal_characters_become_underscores() {
        assert_eq!(
            apply_template("a/b\\c:d*e?f\"g<h>i|j", "x.jpg", "d", 1),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(apply_template("{original}", "a:b.jpg", "d", 1), "a_b");
    }

    #[test]
    fn empty_template_falls_back_to_base_then_image() {
        assert_eq!(apply_template("", "shot.cr3", "d", 1), "shot");
        assert_eq!(apply_template("   ", "shot.cr3", "d", 1), "shot");
        assert_eq!(apply_template("", ".hidden", "d", 1), "image");
    }

    #[test]
    fn dedupe_is_case_insensitive_with_dash_n() {
        let targets = [target("A.CR3"), target("a.cr3"), target("a.jpg")];
        assert_eq!(
            plan_names(&targets, "{original}", ExportFormat::Jpeg),
            ["A.jpg", "a-1.jpg", "a-2.jpg"]
        );
    }

    #[test]
    fn extension_follows_format() {
        let targets = [target("a.cr3")];
        assert_eq!(plan_names(&targets, "x", ExportFormat::Png), ["x.png"]);
        assert_eq!(plan_names(&targets, "x", ExportFormat::Jpeg), ["x.jpg"]);
    }
}
