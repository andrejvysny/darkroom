//! Golden tests for the sparse merge engine + IR mapping. These are the regression guard for the
//! two landmines the design review flagged: (1) a partial preset must NOT zero untouched fields
//! (e.g. `toneAmount` defaults to 100, masks may be non-empty), and (2) amount-blend must be a
//! monotone interpolation between base and preset.

use core_preset::apply::{apply_sparse, subset};
use core_preset::ir::{HslIr, PresetIr, ToneCurveIr};
use core_preset::map::ir_to_sparse;
use core_preset::scope::{all_field_keys, fields_for_groups};
use serde_json::{json, Map, Value};

/// A representative full edit blob (a subset of DevelopParams fields is enough for these tests).
fn base() -> Value {
    json!({
        "exposure": 0.0,
        "temp": 0.0,
        "tint": 0.0,
        "contrast": 10.0,
        "highlights": -20.0,
        "saturation": 0.0,
        "toneAmount": 100.0,
        "hsl": [{"h": 0.0, "s": 0.0, "l": 0.0}, {"h": 0.0, "s": 0.0, "l": 0.0}],
        "masks": [{"name": "m", "enabled": true}],
    })
}

fn map_of(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

#[test]
fn sparse_apply_only_touches_present_keys() {
    let b = base();
    let overlay = map_of(json!({ "exposure": 1.5 }));
    let out = apply_sparse(&b, &overlay, 1.0);

    // The touched field changes...
    assert_eq!(out["exposure"], json!(1.5));
    // ...and EVERYTHING else is byte-identical — incl. the toneAmount=100 default trap + existing
    // contrast/highlights/masks. This is the C2 fix from the review.
    assert_eq!(out["toneAmount"], json!(100.0));
    assert_eq!(out["contrast"], json!(10.0));
    assert_eq!(out["highlights"], json!(-20.0));
    assert_eq!(out["masks"], b["masks"]);
}

#[test]
fn amount_blend_is_linear_between_base_and_preset() {
    let b = base(); // exposure 0.0
    let overlay = map_of(json!({ "exposure": 2.0 }));

    assert_eq!(apply_sparse(&b, &overlay, 0.0)["exposure"], json!(0.0));
    assert_eq!(apply_sparse(&b, &overlay, 0.5)["exposure"], json!(1.0));
    assert_eq!(apply_sparse(&b, &overlay, 1.0)["exposure"], json!(2.0));
}

#[test]
fn amount_blends_numeric_array_leaves_elementwise() {
    let b = base();
    let overlay = map_of(
        json!({ "hsl": [{"h": 100.0, "s": 0.0, "l": 0.0}, {"h": 0.0, "s": 40.0, "l": 0.0}] }),
    );
    let out = apply_sparse(&b, &overlay, 0.5);
    assert_eq!(out["hsl"][0]["h"], json!(50.0));
    assert_eq!(out["hsl"][1]["s"], json!(20.0));
}

#[test]
fn structural_or_bool_leaves_switch_at_threshold() {
    let b = base();
    // masks is a Vec — different shape than base; below 0.5 keeps base, at/above takes preset.
    let overlay = map_of(json!({ "masks": [] }));
    assert_eq!(apply_sparse(&b, &overlay, 0.4)["masks"], b["masks"]);
    assert_eq!(apply_sparse(&b, &overlay, 0.6)["masks"], json!([]));
}

#[test]
fn subset_extracts_only_requested_fields() {
    let b = base();
    let s = subset(
        &b,
        &["exposure".into(), "contrast".into(), "doesNotExist".into()],
    );
    assert_eq!(s.len(), 2);
    assert!(s.contains_key("exposure") && s.contains_key("contrast"));
}

#[test]
fn scope_groups_cover_field_keys() {
    let all = all_field_keys();
    assert!(all.contains(&"exposure") && all.contains(&"cbRgb") && all.contains(&"masks"));
    // No duplicate keys across groups.
    let mut sorted = all.clone();
    sorted.sort_unstable();
    let mut dedup = sorted.clone();
    dedup.dedup();
    assert_eq!(sorted, dedup, "scope groups must be disjoint");

    let light = fields_for_groups(&["light".into()]);
    assert!(light.contains(&"exposure".to_string()) && light.contains(&"blacks".to_string()));
    assert!(!light.contains(&"temp".to_string()));
}

#[test]
fn ir_mapping_classifies_tiers_and_emits_camelcase() {
    let ir = PresetIr {
        lr_process_version: Some("11.0".into()),
        exposure: Some(1.0),
        contrast: Some(25.0),
        saturation: Some(10.0),
        tone_curve: Some(ToneCurveIr {
            rgb: vec![(0.0, 0.0), (1.0, 1.0)],
            ..Default::default()
        }),
        hsl: Some([HslIr::default(); 8]),
        seen_dropped: vec![("Temperature".into(), "no anchor".into())],
        ..Default::default()
    };
    let (params, present, report) = ir_to_sparse(&ir, "lightroom-xmp");

    assert!(params.contains_key("exposure") && params.contains_key("toneCurve"));
    assert_eq!(present.len(), params.len());
    assert_eq!(params["toneCurve"]["rgb"][1]["y"], json!(1.0));

    let clean: Vec<&str> = report.mapped.iter().map(|i| i.key.as_str()).collect();
    let approx: Vec<&str> = report.approximated.iter().map(|i| i.key.as_str()).collect();
    let dropped: Vec<&str> = report.dropped.iter().map(|i| i.key.as_str()).collect();
    assert!(clean.contains(&"exposure") && clean.contains(&"toneCurve"));
    assert!(approx.contains(&"contrast") && approx.contains(&"saturation"));
    assert!(dropped.contains(&"Temperature"));
    assert_eq!(report.source_process_version.as_deref(), Some("11.0"));
}
