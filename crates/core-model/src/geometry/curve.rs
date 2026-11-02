use super::{js_max, js_min};
use crate::CurvePoint;

fn clamp(v: f64) -> f64 {
    js_max(0.0, js_min(1.0, v))
}

/// Transient UI control point with JavaScript number precision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiCurvePoint {
    pub x: f64,
    pub y: f64,
}

/// Converting a persisted point preserves its existing f32 quantization.
impl From<CurvePoint> for UiCurvePoint {
    fn from(point: CurvePoint) -> Self {
        Self {
            x: point.x.into(),
            y: point.y.into(),
        }
    }
}

/// Evaluate the UI spline using f64 JavaScript arithmetic.
pub fn eval_ui_curve(points: &[UiCurvePoint], x: f64) -> f64 {
    let mut pts: Vec<(f64, f64)> = points
        .iter()
        .filter(|p| p.x.is_finite() && p.y.is_finite())
        .map(|p| (clamp(p.x), clamp(p.y)))
        .collect();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
    eval_clean(&pts, x)
}
fn eval_clean(pts: &[(f64, f64)], x: f64) -> f64 {
    let n = pts.len();
    if n < 2 {
        return clamp(x);
    }
    if x <= pts[0].0 {
        return pts[0].1;
    }
    if x >= pts[n - 1].0 {
        return pts[n - 1].1;
    }
    let m = tangents(pts);
    let mut k = 0;
    while k < n - 1 && x > pts[k + 1].0 {
        k += 1;
    }
    let h = pts[k + 1].0 - pts[k].0;
    let t = (x - pts[k].0) / h;
    let t2 = t * t;
    let t3 = t2 * t;
    clamp(
        (2.0 * t3 - 3.0 * t2 + 1.0) * pts[k].1
            + (t3 - 2.0 * t2 + t) * h * m[k]
            + (-2.0 * t3 + 3.0 * t2) * pts[k + 1].1
            + (t3 - t2) * h * m[k + 1],
    )
}

fn tangents(pts: &[(f64, f64)]) -> Vec<f64> {
    let n = pts.len();
    let d: Vec<f64> = pts
        .windows(2)
        .map(|p| (p[1].1 - p[0].1) / (p[1].0 - p[0].0))
        .collect();
    let mut m = vec![0.0; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 {
            0.0
        } else {
            (d[i - 1] + d[i]) * 0.5
        };
    }
    for i in 0..n - 1 {
        if d[i].abs() < 1e-9 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let a = m[i] / d[i];
            let b = m[i + 1] / d[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }
    m
}
