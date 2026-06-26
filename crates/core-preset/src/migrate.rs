//! Process-version migration shim for stored presets/snapshots.
//!
//! Every preset row and develop snapshot carries the `process_version` it was written under. Growth
//! has so far been purely *additive* — new `DevelopParams` fields land with `#[serde(default)]`, so an
//! old JSON deserializes fine and these functions are no-ops. The seam exists for the day a field
//! changes *meaning* (units, range, sign, packing): a version-keyed transform applied HERE — before
//! the typed `DevelopParams` round-trip in `src-tauri` — keeps old presets/snapshots correct instead
//! of silently misinterpreting them. Pure JSON; no GPU/`DevelopParams` dependency (the wgpu-free
//! invariant of this crate).
//!
//! When you bump `core_pipeline::PROCESS_VERSION` with a *breaking* field change, add the up-migration
//! step(s) to [`migrate_sparse`], keyed by the `from_pv` they upgrade FROM.

use serde_json::{Map, Value};

/// Migrate a **sparse** preset params object (only the touched top-level fields) written under
/// `from_pv` up to the current schema, in place. No-op today (all growth has been additive).
///
/// When a future `PROCESS_VERSION` bump changes a field's *meaning*, add a guarded step here, e.g.:
/// ```ignore
/// if from_pv < 5 { /* rename / rescale the affected key in `params` */ }
/// ```
/// Steps must be ordered and a current-version object must pass through untouched (every
/// `from_pv < N` guard is false for an up-to-date row).
pub fn migrate_sparse(params: &mut Map<String, Value>, from_pv: i64) {
    // No breaking field migrations yet — every version so far is a superset of the previous.
    let _ = (params, from_pv);
}

/// Migrate a **full** snapshot params object written under `from_pv` up to current, in place.
/// Delegates to [`migrate_sparse`] over the object's top-level fields. No-op today.
pub fn migrate_full(params: &mut Value, from_pv: i64) {
    if let Some(obj) = params.as_object_mut() {
        migrate_sparse(obj, from_pv);
    }
}
