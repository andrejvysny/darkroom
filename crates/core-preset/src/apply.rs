//! The sparse merge engine: overlay a preset's touched fields onto a base edit, with an optional
//! amount/intensity blend. Operates on `serde_json::Value` so it never needs the `DevelopParams`
//! type (and thus never pulls in `core-pipeline`/wgpu). The `src-tauri` layer chooses the `base`
//! (current edit for a normal apply, or `DevelopParams::default()` for a "replace all" apply) and
//! deserializes the merged value back into a typed `DevelopParams`.

use serde_json::{Map, Value};

/// Overlay every key of `overlay` onto a copy of `base` (a JSON object), blending numeric leaves of
/// each touched field from the base value toward the overlay value by `amount` (0 = keep base,
/// 1 = full preset). Non-numeric / structurally-mismatched leaves switch at the 0.5 threshold.
pub fn apply_sparse(base: &Value, overlay: &Map<String, Value>, amount: f32) -> Value {
    let mut out = base.as_object().cloned().unwrap_or_default();
    let t = amount.clamp(0.0, 1.0);
    for (key, target) in overlay {
        let blended = match out.get(key) {
            Some(current) => lerp_value(current, target, t),
            None => target.clone(),
        };
        out.insert(key.clone(), blended);
    }
    Value::Object(out)
}

/// Extract the sparse subset of a full params object holding exactly `field_keys` (used when saving a
/// preset from the current edit: keep only the checked modules' fields).
pub fn subset(full: &Value, field_keys: &[String]) -> Map<String, Value> {
    let mut out = Map::new();
    if let Some(obj) = full.as_object() {
        for key in field_keys {
            if let Some(v) = obj.get(key) {
                out.insert(key.clone(), v.clone());
            }
        }
    }
    out
}

/// Linearly interpolate two JSON values at `t` ∈ [0,1]. Recurses through arrays (same length) and
/// objects (per matching key); numbers blend; everything else (bool/string/null/shape mismatch)
/// switches at `t >= 0.5`. `t` is assumed already clamped.
fn lerp_value(a: &Value, b: &Value, t: f32) -> Value {
    if t >= 1.0 {
        return b.clone();
    }
    if t <= 0.0 {
        return a.clone();
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (x.as_f64().unwrap_or(0.0), y.as_f64().unwrap_or(0.0));
            let t = t as f64;
            Value::from(x * (1.0 - t) + y * t)
        }
        (Value::Array(xs), Value::Array(ys)) if xs.len() == ys.len() => Value::Array(
            xs.iter()
                .zip(ys)
                .map(|(x, y)| lerp_value(x, y, t))
                .collect(),
        ),
        (Value::Object(xo), Value::Object(yo)) => {
            let mut o = xo.clone();
            for (k, yv) in yo {
                let nv = match xo.get(k) {
                    Some(xv) => lerp_value(xv, yv, t),
                    None => yv.clone(),
                };
                o.insert(k.clone(), nv);
            }
            Value::Object(o)
        }
        _ => {
            if t >= 0.5 {
                b.clone()
            } else {
                a.clone()
            }
        }
    }
}
