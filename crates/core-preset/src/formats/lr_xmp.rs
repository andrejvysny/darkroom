//! Lightroom / Adobe Camera Raw `.xmp` preset importer. Parses the `crs:` (Camera Raw Settings)
//! namespace into a [`PresetIr`], best-effort: faithful keys map directly, magnitude-approximate ones
//! are flagged by `map.rs`, and structurally-incompatible ones (absolute WB Kelvin, color grading /
//! split toning, clarity/texture/dehaze, grain, lens, B&W mix, transform) are recorded as dropped.
//!
//! Scalar settings live on the `rdf:Description` either as `crs:` attributes (the common form) or as
//! `crs:` child elements (e.g. `<crs:Exposure2012>0.50</crs:Exposure2012>` — Adobe writes both);
//! tone curves are child elements holding an `rdf:Seq` of "x, y" control points (0–255). We read by
//! the crs namespace URI so prefix variations don't matter, and scope the scan to the
//! crs-bearing `rdf:Description` so settings from unrelated nodes never leak in.

use std::collections::HashMap;
use std::path::Path;

use crate::error::PresetError;
use crate::ir::{CropIr, HslIr, PresetIr, ToneCurveIr};
use crate::registry::PresetImporter;

const CRS_NS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";

/// LR HSL color order — identical to Darkroom's 8 hue bands, so the mapping is 1:1 by index.
const LR_COLORS: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

/// crs keys with no faithful Darkroom target. Reported as dropped when present with a meaningful value.
const DROPPED_KEYS: &[(&str, &str)] = &[
    ("Clarity2012", "no clarity module"),
    ("Texture", "no texture module"),
    ("Dehaze", "no dehaze module"),
    ("GrainAmount", "no grain module"),
    (
        "ColorGradeMidtoneHue",
        "color grading uses incompatible gain/power channels — not mapped",
    ),
    (
        "SplitToningShadowHue",
        "split toning not mapped (incompatible grading model)",
    ),
    ("ConvertToGrayscale", "B&W channel mix not mapped"),
    ("LensProfileEnable", "lens corrections not supported"),
    ("PerspectiveUpright", "transform / upright not supported"),
];

pub struct LightroomXmp;

fn ext_eq(path: Option<&Path>, ext: &str) -> bool {
    path.and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

impl PresetImporter for LightroomXmp {
    fn format_name(&self) -> &'static str {
        "lightroom-xmp"
    }

    fn detect(&self, bytes: &[u8], path: Option<&Path>) -> bool {
        if ext_eq(path, "xmp") {
            return true;
        }
        if ext_eq(path, "lrtemplate") {
            return false; // Lua, handled by another importer
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
        head.contains("camera-raw-settings")
            && (head.contains("xmpmeta") || head.contains("rdf:RDF") || head.contains("<rdf"))
    }

    fn parse(&self, bytes: &[u8], _path: Option<&Path>) -> Result<PresetIr, PresetError> {
        let text = std::str::from_utf8(bytes).map_err(|e| PresetError::Malformed(e.to_string()))?;
        let doc =
            roxmltree::Document::parse(text).map_err(|e| PresetError::Malformed(e.to_string()))?;

        // crs settings live on an `rdf:Description` — as attributes and/or as child elements. Scope
        // the scan to the Description node(s) that actually carry crs content so crs keys from
        // unrelated parts of the document never merge in. Fall back to the whole tree only if no such
        // Description is found (lenient — `detect` already saw "camera-raw-settings").
        let mut attrs: HashMap<String, String> = HashMap::new();
        let mut tc = ToneCurveIr::default();
        let mut scopes: Vec<roxmltree::Node> = doc
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "Description" && node_has_crs(*n))
            .collect();
        if scopes.is_empty() {
            scopes.push(doc.root_element());
        }
        for scope in scopes {
            collect_crs(scope, &mut attrs, &mut tc);
        }

        if attrs.is_empty() && tc.rgb.is_empty() {
            return Err(PresetError::Malformed(
                "no camera-raw-settings found".into(),
            ));
        }

        Ok(build_ir(&attrs, tc))
    }
}

/// Parse one "x, y" tone-curve control point (0–255) into a normalized [0,1] pair. Shared with the
/// `.lrtemplate` importer (whose curves are Lua strings of the same form).
pub(crate) fn parse_xy(s: &str) -> Option<(f32, f32)> {
    let mut it = s.split(',');
    let x = it.next()?.trim().parse::<f32>().ok()?;
    let y = it.next()?.trim().parse::<f32>().ok()?;
    Some((x / 255.0, y / 255.0))
}

/// Parse a tone-curve element's `rdf:li` "x, y" points (0–255) into normalized [0,1] pairs.
fn curve_points(node: roxmltree::Node) -> Vec<(f32, f32)> {
    node.descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "li")
        .filter_map(|li| parse_xy(li.text()?))
        .collect()
}

/// True when a node carries any Camera-Raw-Settings content (a crs attribute or a crs descendant
/// element) — used to scope the scan to the `rdf:Description` that actually holds the preset.
fn node_has_crs(n: roxmltree::Node) -> bool {
    n.attributes().any(|a| a.namespace() == Some(CRS_NS))
        || n.descendants()
            .any(|d| d.is_element() && d.tag_name().namespace() == Some(CRS_NS))
}

/// Collect crs scalar settings — both attribute form and element form
/// (`<crs:Exposure2012>0.50</crs:Exposure2012>`) — plus the tone-curve child elements from one scope
/// node into `attrs`/`tc`. Attributes are scanned first and win on conflict (the canonical form).
fn collect_crs(scope: roxmltree::Node, attrs: &mut HashMap<String, String>, tc: &mut ToneCurveIr) {
    for a in scope.attributes() {
        if a.namespace() == Some(CRS_NS) {
            attrs
                .entry(a.name().to_string())
                .or_insert_with(|| a.value().to_string());
        }
    }
    for el in scope
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().namespace() == Some(CRS_NS))
    {
        match el.tag_name().name() {
            "ToneCurvePV2012" => tc.rgb = curve_points(el),
            "ToneCurvePV2012Red" => tc.r = curve_points(el),
            "ToneCurvePV2012Green" => tc.g = curve_points(el),
            "ToneCurvePV2012Blue" => tc.b = curve_points(el),
            // Scalar setting serialized as an element rather than an attribute.
            name => {
                if let Some(t) = el.text().map(str::trim) {
                    if !t.is_empty() {
                        attrs
                            .entry(name.to_string())
                            .or_insert_with(|| t.to_string());
                    }
                }
            }
        }
    }
}

fn is_identity_curve(pts: &[(f32, f32)]) -> bool {
    pts.iter().all(|(x, y)| (x - y).abs() < 1.5e-3)
}

/// Map a collected set of crs scalar attributes (local name → string value) + parsed tone curves into
/// the unit-neutral IR. Shared by the `.xmp` and `.lrtemplate` importers.
pub(crate) fn build_ir(attrs: &HashMap<String, String>, tc: ToneCurveIr) -> PresetIr {
    let getf = |k: &str| -> Option<f32> { attrs.get(k).and_then(|v| v.trim().parse::<f32>().ok()) };
    let gets = |k: &str| -> Option<&str> { attrs.get(k).map(|s| s.as_str()) };
    // True when a key is present with a value that actually does something (non-zero / non-False).
    let meaningful = |k: &str| -> bool {
        attrs
            .get(k)
            .map(|v| {
                let t = v.trim();
                if t.eq_ignore_ascii_case("false") {
                    return false;
                }
                match t.parse::<f32>() {
                    Ok(n) => n != 0.0,
                    Err(_) => !t.is_empty(),
                }
            })
            .unwrap_or(false)
    };

    let mut ir = PresetIr {
        lr_process_version: attrs.get("ProcessVersion").cloned(),
        ..Default::default()
    };

    // Basic tone — PV2012 keys preferred, a light legacy fallback otherwise.
    ir.exposure = getf("Exposure2012");
    ir.contrast = getf("Contrast2012");
    ir.highlights = getf("Highlights2012");
    ir.shadows = getf("Shadows2012");
    ir.whites = getf("Whites2012");
    ir.blacks = getf("Blacks2012");
    if ir.exposure.is_none() && ir.contrast.is_none() {
        // Legacy (pre-PV2012) basic tone.
        if let Some(v) = getf("Exposure") {
            ir.exposure = Some(v);
            ir.seen_approx
                .push(("Exposure".into(), "legacy ProcessVersion".into()));
        }
        ir.contrast = ir.contrast.or_else(|| getf("Contrast"));
        if let Some(v) = getf("Recovery") {
            ir.highlights = Some(-v);
            ir.seen_approx
                .push(("Recovery".into(), "mapped to −highlights".into()));
        }
        if let Some(v) = getf("FillLight") {
            ir.shadows = Some(v);
            ir.seen_approx
                .push(("FillLight".into(), "mapped to shadows".into()));
        }
    }

    // Saturation (+ Vibrance folded at half weight).
    let sat = getf("Saturation");
    let vib = getf("Vibrance");
    if sat.is_some() || vib.is_some() {
        let s = sat.unwrap_or(0.0) + 0.5 * vib.unwrap_or(0.0);
        ir.saturation = Some(s.clamp(-100.0, 100.0));
        if vib.is_some() {
            ir.seen_approx.push((
                "Vibrance".into(),
                "folded into saturation at half weight".into(),
            ));
        }
    }

    // Detail + vignette.
    ir.sharpen = getf("Sharpness");
    ir.nr_luma = getf("LuminanceSmoothing");
    ir.nr_color = getf("ColorNoiseReduction");
    ir.vignette = getf("PostCropVignetteAmount");

    // White balance: absolute Temperature has no anchor → drop; import only a relative Tint nudge.
    if attrs.contains_key("Temperature") {
        ir.seen_dropped.push((
            "Temperature".into(),
            "absolute WB Kelvin has no anchor in our as-shot-relative model".into(),
        ));
    }
    if let Some(t) = getf("Tint") {
        // LR Tint −150..+150 → our −100..100.
        ir.tint = Some((t / 1.5).clamp(-100.0, 100.0));
    }
    if let Some(w) = gets("WhiteBalance") {
        if !w.eq_ignore_ascii_case("custom") {
            ir.seen_dropped.push((
                "WhiteBalance".into(),
                format!("'{w}' needs per-image EXIF; apply manually"),
            ));
        }
    }

    // Crop — only when un-rotated (the LR rect is in the rotated frame).
    if gets("HasCrop") == Some("True") {
        let angle = getf("CropAngle").unwrap_or(0.0);
        if angle.abs() < 0.01 {
            let l = getf("CropLeft").unwrap_or(0.0);
            let r = getf("CropRight").unwrap_or(1.0);
            let t = getf("CropTop").unwrap_or(0.0);
            let b = getf("CropBottom").unwrap_or(1.0);
            ir.crop = Some(CropIr {
                cx: (l + r) / 2.0,
                cy: (t + b) / 2.0,
                hw: ((r - l) / 2.0).abs(),
                hh: ((b - t) / 2.0).abs(),
                angle: 0.0,
            });
        } else {
            ir.seen_dropped.push((
                "Crop".into(),
                "rotated crop (CropAngle ≠ 0) not supported".into(),
            ));
        }
    }

    // HSL — 1:1 by color index.
    let mut hsl = [HslIr::default(); 8];
    let mut any_hsl = false;
    for (i, c) in LR_COLORS.iter().enumerate() {
        let h = getf(&format!("HueAdjustment{c}")).unwrap_or(0.0);
        let s = getf(&format!("SaturationAdjustment{c}")).unwrap_or(0.0);
        let l = getf(&format!("LuminanceAdjustment{c}")).unwrap_or(0.0);
        if h != 0.0 || s != 0.0 || l != 0.0 {
            any_hsl = true;
        }
        hsl[i] = HslIr { h, s, l };
    }
    if any_hsl {
        ir.hsl = Some(hsl);
    }

    // Tone curve — keep only non-identity channels; skip the default linear curve.
    let keep = |v: Vec<(f32, f32)>| -> Vec<(f32, f32)> {
        if v.len() >= 2 && !is_identity_curve(&v) {
            v
        } else {
            Vec::new()
        }
    };
    let out_tc = ToneCurveIr {
        rgb: keep(tc.rgb),
        r: keep(tc.r),
        g: keep(tc.g),
        b: keep(tc.b),
    };
    if !out_tc.rgb.is_empty()
        || !out_tc.r.is_empty()
        || !out_tc.g.is_empty()
        || !out_tc.b.is_empty()
    {
        ir.tone_curve = Some(out_tc);
    }

    // Report incompatible settings that are actually set.
    for (k, why) in DROPPED_KEYS {
        if meaningful(k) {
            ir.seen_dropped.push(((*k).into(), (*why).into()));
        }
    }

    ir
}
