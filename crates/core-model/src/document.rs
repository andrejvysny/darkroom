//! Read-only access to legacy edit JSON without discarding the original document.

use crate::{ComponentKind, DevelopParams, LocalAdjust};
use serde_json::Value;

/// Whether the document can be interpreted safely by this version of the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadStatus {
    Supported,
    UnsupportedVersion,
    UnsupportedFields(Vec<String>),
    Invalid,
}

/// Original JSON and its separately stored process version. No write or merge is implied.
/// `PartialEq` compares interpretation (status, preview, version, raw text), so two documents the
/// model reads identically are equal; it is required because workflow completions carry it.
#[derive(Debug, Clone, PartialEq)]
pub struct EditDocument {
    raw_json: String,
    process_version: i64,
    status: ReadStatus,
    preview: Option<DevelopParams>,
}

impl EditDocument {
    /// Versions 1 through 6 have additive schema changes and require no migration today.
    pub fn read(raw_json: impl Into<String>, process_version: i64) -> Self {
        let raw_json = raw_json.into();
        let (status, preview) = read_preview(&raw_json, process_version);
        Self {
            raw_json,
            process_version,
            status,
            preview,
        }
    }

    pub fn raw_json(&self) -> &str {
        &self.raw_json
    }
    pub fn process_version(&self) -> i64 {
        self.process_version
    }
    pub fn status(&self) -> &ReadStatus {
        &self.status
    }
    pub fn preview(&self) -> Option<&DevelopParams> {
        self.preview.as_ref()
    }
}

/// Serialize params exactly as the catalog stores them, refusing non-finite values (a NaN would
/// otherwise serialize as `null` and silently corrupt the stored edit).
pub fn params_to_json(params: &DevelopParams) -> Result<String, String> {
    if !validate_finite(params) {
        return Err("edit parameters contain a non-finite value".into());
    }
    serde_json::to_string(params).map_err(|e| e.to_string())
}

fn read_preview(raw: &str, version: i64) -> (ReadStatus, Option<DevelopParams>) {
    if !(1..=6).contains(&version) {
        return (ReadStatus::UnsupportedVersion, None);
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return (ReadStatus::Invalid, None);
    };
    if !value.is_object() {
        return (ReadStatus::Invalid, None);
    }
    let Ok(params) = serde_json::from_str::<DevelopParams>(raw) else {
        return (ReadStatus::Invalid, None);
    };
    if !validate_finite(&params) {
        return (ReadStatus::Invalid, None);
    }
    let Ok(known) = serde_json::to_value(&params) else {
        return (ReadStatus::Invalid, None);
    };
    let mut unknown = Vec::new();
    collect_unknown(&value, &known, "", &mut unknown);
    if unknown.is_empty() {
        (ReadStatus::Supported, Some(params))
    } else {
        (ReadStatus::UnsupportedFields(unknown), None)
    }
}

// Paths use JSON Pointer escaping so keys containing '/' or '~' remain unambiguous.
fn collect_unknown(input: &Value, known: &Value, path: &str, out: &mut Vec<String>) {
    match (input, known) {
        (Value::Object(input), Value::Object(known)) => {
            for (key, value) in input {
                let path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                if let Some(expected) = known.get(key) {
                    collect_unknown(value, expected, &path, out);
                } else {
                    out.push(path);
                }
            }
        }
        (Value::Array(input), Value::Array(known)) => {
            for (index, (value, expected)) in input.iter().zip(known).enumerate() {
                collect_unknown(value, expected, &format!("{path}/{index}"), out);
            }
        }
        _ => {}
    }
}

/// Check every floating-point edit value before serializing; no ranges are changed.
pub fn validate_finite(p: &DevelopParams) -> bool {
    scalar_finite(p)
        && [
            &p.cb_rgb.global,
            &p.cb_rgb.shadows,
            &p.cb_rgb.midtones,
            &p.cb_rgb.highlights,
            &p.channel_mix.red,
            &p.channel_mix.green,
            &p.channel_mix.blue,
            &p.border.color,
        ]
        .into_iter()
        .flatten()
        .all(|v| v.is_finite())
        && p.hsl
            .iter()
            .all(|b| [b.h, b.s, b.l].into_iter().all(f32::is_finite))
        && [
            &p.tone_curve.rgb,
            &p.tone_curve.r,
            &p.tone_curve.g,
            &p.tone_curve.b,
        ]
        .into_iter()
        .flatten()
        .all(|point| point.x.is_finite() && point.y.is_finite())
        && p.masks.iter().all(|mask| {
            mask.opacity.is_finite()
                && local_finite(&mask.adjust)
                && mask
                    .components
                    .iter()
                    .all(|component| kind_finite(&component.kind))
        })
}

fn scalar_finite(p: &DevelopParams) -> bool {
    let scalars = [
        p.exposure,
        p.temp,
        p.tint,
        p.contrast,
        p.saturation,
        p.highlights,
        p.shadows,
        p.blacks,
        p.whites,
        p.sharpen,
        p.nr_luma,
        p.nr_color,
        p.vignette,
        p.dist_k1,
        p.dist_k2,
        p.ca_red,
        p.ca_blue,
        p.clarity,
        p.texture,
        p.dehaze,
        p.tone_amount,
        p.crop.cx,
        p.crop.cy,
        p.crop.hw,
        p.crop.hh,
        p.crop.angle,
        p.cb_rgb.contrast,
        p.cb_rgb.saturation,
        p.border.size,
        p.border.aspect_w,
        p.border.aspect_h,
        p.denoise.amount,
    ];
    scalars.into_iter().all(f32::is_finite)
}

fn local_finite(a: &LocalAdjust) -> bool {
    [
        a.exposure,
        a.temp,
        a.tint,
        a.contrast,
        a.saturation,
        a.highlights,
        a.shadows,
        a.blacks,
        a.whites,
    ]
    .into_iter()
    .all(f32::is_finite)
}

fn kind_finite(kind: &ComponentKind) -> bool {
    match kind {
        ComponentKind::Linear { p0, p1 } => p0.iter().chain(p1).all(|v| v.is_finite()),
        ComponentKind::Radial {
            center,
            radius,
            angle,
            feather,
        } => center
            .iter()
            .chain(radius)
            .chain([angle, feather])
            .all(|v| v.is_finite()),
        ComponentKind::Brush { strokes } => strokes.iter().all(|s| {
            [s.size, s.hardness, s.flow, s.opacity]
                .into_iter()
                .all(f32::is_finite)
                && s.points.iter().flatten().all(|v| v.is_finite())
        }),
        ComponentKind::LuminanceRange { lo, hi, feather } => {
            [lo, hi, feather].into_iter().all(|v| v.is_finite())
        }
        ComponentKind::ColorRange {
            hue,
            sat,
            tol,
            feather,
        } => [hue, sat, tol, feather].into_iter().all(|v| v.is_finite()),
        ComponentKind::Ai { points, .. } => {
            points.iter().all(|p| p.x.is_finite() && p.y.is_finite())
        }
    }
}
