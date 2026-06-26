//! core-preset — format-agnostic develop-preset import + the sparse merge engine.
//!
//! Pure CPU + `serde_json` only (**no wgpu / GPU**). Presets are a *sparse* subset of the camelCase
//! `DevelopParams` JSON: only the top-level fields a preset touches are present. Applying one
//! overlays exactly those keys onto the current edit (see [`apply::apply_sparse`]), so untouched
//! modules are never disturbed — this is what makes partial presets (and Lightroom's sparse presets)
//! behave correctly.
//!
//! External formats (Lightroom `.xmp` / `.lrtemplate`, …) parse into a unit-neutral [`ir::PresetIr`]
//! via the [`registry::PresetImporter`] trait, then [`map::ir_to_sparse`] emits the sparse JSON plus
//! an [`report::ImportReport`] (mapped / approximated / dropped) for honest user feedback.
//!
//! The typed `DevelopParams` round-trip (validation) happens in the `src-tauri` command layer, which
//! already links `core-pipeline`; keeping it out here means a preset/XMP parser never pulls in the
//! GPU stack.

pub mod apply;
pub mod error;
pub mod formats;
pub mod ir;
pub mod map;
pub mod migrate;
pub mod registry;
pub mod report;
pub mod scope;

pub use apply::{apply_sparse, subset};
pub use error::PresetError;
pub use ir::PresetIr;
pub use map::ir_to_sparse;
pub use migrate::{migrate_full, migrate_sparse};
pub use registry::{ParsedPreset, PresetImporter, Registry, MAX_IMPORT_BYTES};
pub use report::{ImportReport, ReportItem};
pub use scope::{all_field_keys, fields_for_groups, ScopeGroup, GROUPS};
