//! Preset and snapshot settings: the portable envelope, import parsing and the merge helpers that
//! turn a stored preset / copied edit / snapshot into full `DevelopParams`. Pure (no catalog
//! access) so the legacy host and the native catalog adapter share one implementation.
use core_app::AppError;
use core_library::PresetFull;
use core_model::DevelopParams;
use serde_json::{Map, Value};

/// Portable preset file envelope (export/import) and the format of the bundled built-ins. `params`
/// is the sparse DevelopParams object (only the touched top-level fields). `schema_version` is
/// REQUIRED for a file to be treated as our native preset on import (its presence disambiguates
/// our envelope from an arbitrary `.json` that merely happens to have `params`/`fieldKeys`).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetEnvelope {
    #[serde(default)]
    pub schema_version: Option<i64>,
    pub name: String,
    #[serde(default)]
    pub group_name: Option<String>,
    #[serde(default)]
    pub field_keys: Vec<String>,
    #[serde(default)]
    pub process_version: Option<i64>,
    pub params: Value,
}

/// `(name, group, field_keys, sparse_params_json, report)` of an imported preset file.
pub type ParsedImport = (
    String,
    String,
    Vec<String>,
    String,
    core_preset::ImportReport,
);

pub fn sparse_map(params_json: &str) -> Result<Map<String, Value>, String> {
    let v: Value = serde_json::from_str(params_json).map_err(|e| e.to_string())?;
    v.as_object()
        .cloned()
        .ok_or_else(|| "preset params must be a JSON object".to_string())
}

/// Validate that a sparse params object overlays cleanly onto defaults and deserializes into a typed
/// `DevelopParams` — so a buggy importer / hand-edited preset fails at WRITE time (save/import) with a
/// clear error, instead of silently storing junk that only blows up later at apply.
pub fn validate_sparse_params(sparse: &Map<String, Value>) -> Result<(), String> {
    let base = serde_json::to_value(DevelopParams::default()).map_err(|e| e.to_string())?;
    let merged = core_preset::apply_sparse(&base, sparse, 1.0);
    serde_json::from_value::<DevelopParams>(merged)
        .map(|_| ())
        .map_err(|e| format!("preset params are not valid develop settings: {e}"))
}

/// Parse an import file into `(name, group, field_keys, sparse_params_json, report)`. Tries our
/// native envelope first, then the external-format registry (Lightroom etc.).
pub fn parse_import(bytes: &[u8], src_path: &str) -> Result<ParsedImport, String> {
    if let Ok(env) = serde_json::from_slice::<PresetEnvelope>(bytes) {
        // Require an explicit schemaVersion so an arbitrary `.json` that merely has `params`/`fieldKeys`
        // isn't mistaken for our native envelope (it falls through to the external-format registry).
        if env.schema_version.is_some() && env.params.is_object() && !env.field_keys.is_empty() {
            let mut report = core_preset::ImportReport {
                source_format: "native-json".into(),
                ..Default::default()
            };
            for k in &env.field_keys {
                report.mark_clean(k);
            }
            let group = env.group_name.unwrap_or_else(|| "Imported".into());
            let sparse_json = env.params.to_string();
            return Ok((env.name, group, env.field_keys, sparse_json, report));
        }
    }
    let registry = core_preset::Registry::with_defaults();
    let parsed = registry
        .import(bytes, Some(std::path::Path::new(src_path)))
        .map_err(|e| e.to_string())?;
    let name = std::path::Path::new(src_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported preset")
        .to_string();
    let sparse_json =
        serde_json::to_string(&Value::Object(parsed.params)).map_err(|e| e.to_string())?;
    Ok((
        name,
        "Imported".into(),
        parsed.field_keys,
        sparse_json,
        parsed.report,
    ))
}

/// A stored preset's sparse overlay, migrated from the process version it was written under.
pub fn preset_overlay(preset: &PresetFull) -> Result<Map<String, Value>, String> {
    let mut overlay = sparse_map(&preset.params)?;
    core_preset::migrate_sparse(&mut overlay, preset.process_version);
    Ok(overlay)
}

/// The fields of `params` named by `field_keys` as an overlay; `None` copies every field.
pub fn copied_overlay(
    params: &DevelopParams,
    field_keys: Option<&[String]>,
) -> Result<Map<String, Value>, String> {
    let full = serde_json::to_value(params).map_err(|e| e.to_string())?;
    Ok(match field_keys {
        Some(keys) => core_preset::subset(&full, keys),
        None => {
            let all: Vec<String> = core_preset::all_field_keys()
                .into_iter()
                .map(str::to_owned)
                .collect();
            core_preset::subset(&full, &all)
        }
    })
}

/// A snapshot's full params, migrated from the process version it was saved under and validated
/// through the current schema.
pub fn snapshot_params(json: &str, process_version: i64) -> Result<DevelopParams, String> {
    let mut value: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    core_preset::migrate_full(&mut value, process_version);
    serde_json::from_value::<DevelopParams>(value).map_err(|e| e.to_string())
}

/// `overlay` blended onto `base` by `amount` (0..=1); `replace_all` starts from the defaults so the
/// overlay becomes the complete look.
pub fn resolve(
    base: &DevelopParams,
    overlay: &Map<String, Value>,
    amount: f32,
    replace_all: bool,
) -> Result<DevelopParams, AppError> {
    let base = if replace_all {
        serde_json::to_value(DevelopParams::default())
    } else {
        serde_json::to_value(base)
    }
    .map_err(|e| AppError::InvalidCommand(e.to_string()))?;
    let merged = core_preset::apply_sparse(&base, overlay, amount);
    serde_json::from_value(merged).map_err(|e| AppError::InvalidCommand(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(params: &str, process_version: i64) -> PresetFull {
        PresetFull {
            id: 1,
            name: "p".into(),
            group_name: "g".into(),
            builtin: false,
            is_favorite: false,
            field_keys: vec!["exposure".into()],
            params: params.into(),
            process_version,
        }
    }

    fn exposed(ev: f32) -> DevelopParams {
        DevelopParams {
            exposure: ev,
            ..Default::default()
        }
    }

    #[test]
    fn half_amount_blends_a_numeric_field() {
        let overlay = preset_overlay(&preset(r#"{"exposure": 2.0}"#, 6)).unwrap();
        let out = resolve(&exposed(0.0), &overlay, 0.5, false).unwrap();
        assert!((out.exposure - 1.0).abs() < 1e-5, "got {}", out.exposure);
        let full = resolve(&exposed(0.0), &overlay, 1.0, false).unwrap();
        assert!((full.exposure - 2.0).abs() < 1e-5);
    }

    #[test]
    fn untouched_fields_survive_a_sparse_overlay() {
        let base = DevelopParams {
            exposure: 1.0,
            contrast: 30.0,
            ..Default::default()
        };
        let overlay = preset_overlay(&preset(r#"{"exposure": 2.0}"#, 6)).unwrap();
        let out = resolve(&base, &overlay, 1.0, false).unwrap();
        assert_eq!(out.contrast, 30.0);
    }

    #[test]
    fn replace_all_starts_from_defaults() {
        let base = DevelopParams {
            exposure: 1.0,
            contrast: 30.0,
            ..Default::default()
        };
        let overlay = preset_overlay(&preset(r#"{"exposure": 2.0}"#, 6)).unwrap();
        let out = resolve(&base, &overlay, 1.0, true).unwrap();
        assert_eq!(out.contrast, DevelopParams::default().contrast);
        assert!((out.exposure - 2.0).abs() < 1e-5);
    }

    #[test]
    fn copied_none_copies_every_field_and_some_only_the_named() {
        let source = DevelopParams {
            exposure: 1.5,
            contrast: 20.0,
            ..Default::default()
        };
        let base = DevelopParams::default();
        let all = copied_overlay(&source, None).unwrap();
        let out = resolve(&base, &all, 1.0, false).unwrap();
        assert_eq!(out.exposure, 1.5);
        assert_eq!(out.contrast, 20.0);

        let keys = vec!["exposure".to_string()];
        let some = copied_overlay(&source, Some(&keys)).unwrap();
        let out = resolve(&base, &some, 1.0, false).unwrap();
        assert_eq!(out.exposure, 1.5);
        assert_eq!(out.contrast, base.contrast);
    }

    #[test]
    fn invalid_preset_json_is_an_error() {
        assert!(preset_overlay(&preset("not json", 6)).is_err());
        assert_eq!(
            preset_overlay(&preset("[1]", 6)).unwrap_err(),
            "preset params must be a JSON object"
        );
        let bad: Map<String, Value> = serde_json::from_str(r#"{"exposure": "loud"}"#).unwrap();
        assert!(resolve(&DevelopParams::default(), &bad, 1.0, false).is_err());
        assert!(validate_sparse_params(&bad).is_err());
    }

    #[test]
    fn snapshot_params_round_trip_through_the_migration_path() {
        let json = serde_json::to_string(&exposed(0.75)).unwrap();
        let out = snapshot_params(&json, 1).unwrap();
        assert_eq!(out, exposed(0.75));
        assert!(snapshot_params("{", 6).is_err());
    }
}
