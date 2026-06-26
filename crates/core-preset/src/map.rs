//! Turn a parsed [`PresetIr`] into the camelCase sparse params JSON + an [`ImportReport`]. Each
//! emitted field is classified into the report's CLEAN vs APPROX tier here (the tier is a property of
//! the *mapping*, e.g. our contrast is always a magnitude-only approximation of LR's PV2012), while
//! the parser-recorded `seen_dropped`/`seen_approx` notes are appended verbatim.

use serde_json::{json, Map, Value};

use crate::ir::{PresetIr, ToneCurveIr};
use crate::report::ImportReport;

const NOTE_TONE: &str = "magnitude only — our control is a display-space curve, not LR's PV2012 region-adaptive algorithm; retune expected";
const NOTE_CROP: &str = "imported with angle≈0 only; LR crop rect is in the rotated frame";
const NOTE_TINT: &str = "relative tint nudge only; absolute WB temperature was dropped (no anchor)";
const NOTE_VIGNETTE: &str = "amount only — no midpoint/feather/roundness target";
const NOTE_SAT: &str = "saturation (incl. any vibrance folded in)";

fn points_to_json(pts: &[(f32, f32)]) -> Value {
    Value::Array(
        pts.iter()
            .map(|(x, y)| json!({ "x": *x, "y": *y }))
            .collect(),
    )
}

fn tone_curve_to_json(tc: &ToneCurveIr) -> Value {
    json!({
        "rgb": points_to_json(&tc.rgb),
        "r": points_to_json(&tc.r),
        "g": points_to_json(&tc.g),
        "b": points_to_json(&tc.b),
    })
}

/// Emit `(sparse params map, present field keys, report)` from a parsed IR. `source_format` labels
/// the report (e.g. "lightroom-xmp").
pub fn ir_to_sparse(
    ir: &PresetIr,
    source_format: &str,
) -> (Map<String, Value>, Vec<String>, ImportReport) {
    let mut out = Map::new();
    let mut report = ImportReport {
        source_format: source_format.to_string(),
        source_process_version: ir.lr_process_version.clone(),
        ..Default::default()
    };

    // CLEAN: faithful transfers.
    if let Some(v) = ir.exposure {
        out.insert("exposure".into(), json!(v));
        report.mark_clean("exposure");
    }
    if let Some(v) = ir.sharpen {
        out.insert("sharpen".into(), json!(v));
        report.mark_clean("sharpen");
    }
    if let Some(v) = ir.nr_luma {
        out.insert("nrLuma".into(), json!(v));
        report.mark_clean("nrLuma");
    }
    if let Some(v) = ir.nr_color {
        out.insert("nrColor".into(), json!(v));
        report.mark_clean("nrColor");
    }
    if let Some(tc) = &ir.tone_curve {
        out.insert("toneCurve".into(), tone_curve_to_json(tc));
        report.mark_clean("toneCurve");
    }
    if let Some(bands) = &ir.hsl {
        let arr: Vec<Value> = bands
            .iter()
            .map(|b| json!({ "h": b.h, "s": b.s, "l": b.l }))
            .collect();
        out.insert("hsl".into(), Value::Array(arr));
        report.mark_clean("hsl");
    }

    // APPROX: mapped with a caveat.
    if let Some(v) = ir.contrast {
        out.insert("contrast".into(), json!(v));
        report.mark_approx("contrast", NOTE_TONE);
    }
    if let Some(v) = ir.highlights {
        out.insert("highlights".into(), json!(v));
        report.mark_approx("highlights", NOTE_TONE);
    }
    if let Some(v) = ir.shadows {
        out.insert("shadows".into(), json!(v));
        report.mark_approx("shadows", NOTE_TONE);
    }
    if let Some(v) = ir.whites {
        out.insert("whites".into(), json!(v));
        report.mark_approx("whites", NOTE_TONE);
    }
    if let Some(v) = ir.blacks {
        out.insert("blacks".into(), json!(v));
        report.mark_approx("blacks", NOTE_TONE);
    }
    if let Some(v) = ir.saturation {
        out.insert("saturation".into(), json!(v));
        report.mark_approx("saturation", NOTE_SAT);
    }
    if let Some(v) = ir.vignette {
        out.insert("vignette".into(), json!(v));
        report.mark_approx("vignette", NOTE_VIGNETTE);
    }
    if let Some(v) = ir.tint {
        out.insert("tint".into(), json!(v));
        report.mark_approx("tint", NOTE_TINT);
    }
    if let Some(c) = &ir.crop {
        out.insert(
            "crop".into(),
            json!({ "cx": c.cx, "cy": c.cy, "hw": c.hw, "hh": c.hh, "angle": c.angle }),
        );
        report.mark_approx("crop", NOTE_CROP);
    }

    // DROPPED / extra-approx: parser-collected, appended verbatim.
    for (key, note) in &ir.seen_approx {
        report.mark_approx(key, note);
    }
    for (key, note) in &ir.seen_dropped {
        report.mark_dropped(key, note);
    }

    let present: Vec<String> = out.keys().cloned().collect();
    (out, present, report)
}
