//! UI geometry semantics, separate from renderer and persisted model state.
mod curve;
pub use curve::{eval_ui_curve, UiCurvePoint};

use crate::ui_numeric::js_round as round;
use crate::{Border, ComponentKind, LocalAdjust, Mask, MaskComponent, MaskOp};

/// Frame settings retain JavaScript number precision before model persistence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameSettings {
    pub size: f64,
    pub aspect_w: f64,
    pub aspect_h: f64,
}

/// Converting persisted settings preserves their existing f32 quantization.
impl From<&Border> for FrameSettings {
    fn from(border: &Border) -> Self {
        Self {
            size: border.size.into(),
            aspect_w: border.aspect_w.into(),
            aspect_h: border.aspect_h.into(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Frame {
    pub ow: f64,
    pub oh: f64,
    pub off_x: f64,
    pub off_y: f64,
}

fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

pub fn compute_frame(w: f64, h: f64, border: &FrameSettings) -> Frame {
    let margin = round(js_max(0.0, border.size) / 100.0 * js_max(w, h));
    let base_w = w + 2.0 * margin;
    let base_h = h + 2.0 * margin;
    let (mut ow, mut oh) = (base_w, base_h);
    if border.aspect_w > 0.0 && border.aspect_h > 0.0 {
        let r = border.aspect_w / border.aspect_h;
        if base_w / base_h < r {
            ow = round(base_h * r);
        } else {
            oh = round(base_w / r);
        }
    }
    Frame {
        ow,
        oh,
        off_x: ((ow - w) / 2.0).floor(),
        off_y: ((oh - h) / 2.0).floor(),
    }
}

pub fn rgb3_to_css(c: [f64; 3]) -> String {
    fn q(v: f64) -> String {
        let n = round(js_min(1.0, js_max(0.0, v)) * 255.0);
        if n.is_nan() {
            "NaN".into()
        } else {
            format!("{n}")
        }
    }
    format!("rgb({}, {}, {})", q(c[0]), q(c[1]), q(c[2]))
}

pub fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Rect and pointer coordinates are transient view state, not persisted mask data.
pub fn client_to_norm(left: f64, top: f64, width: f64, height: f64, x: f64, y: f64) -> [f64; 2] {
    [
        clamp01((x - left) / js_max(width, 1.0)),
        clamp01((y - top) / js_max(height, 1.0)),
    ]
}

pub fn rgb_to_hsv(r: f64, g: f64, b: f64) -> [f64; 3] {
    let max = js_max(js_max(r, g), b);
    let min = js_min(js_min(r, g), b);
    let d = max - min;
    let mut h = 0.0;
    if d > 1e-6 {
        h = if max == r {
            ((g - b) / d) % 6.0
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h /= 6.0;
        if h < 0.0 {
            h += 1.0;
        }
    }
    [h, if max <= 1e-6 { 0.0 } else { d / max }, max]
}

fn new_mask(name: &str, kind: ComponentKind, feather: bool) -> Mask {
    Mask {
        name: name.into(),
        components: vec![MaskComponent {
            kind,
            op: MaskOp::Add,
            invert: false,
            feather,
        }],
        adjust: LocalAdjust::default(),
        opacity: 1.0,
        enabled: true,
    }
}
pub fn new_linear_mask() -> Mask {
    new_mask(
        "Linear",
        ComponentKind::Linear {
            p0: [0.5, 0.25],
            p1: [0.5, 0.6],
        },
        false,
    )
}
pub fn new_radial_mask() -> Mask {
    new_mask(
        "Radial",
        ComponentKind::Radial {
            center: [0.5, 0.5],
            radius: [0.3, 0.3],
            angle: 0.0,
            feather: 0.5,
        },
        false,
    )
}
pub fn new_brush_mask() -> Mask {
    new_mask("Brush", ComponentKind::Brush { strokes: vec![] }, false)
}
pub fn new_ai_mask() -> Mask {
    new_mask(
        "AI Select",
        ComponentKind::Ai {
            model: "mobile_sam".into(),
            points: vec![],
            hash: String::new(),
        },
        true,
    )
}
pub fn new_luminance_mask() -> Mask {
    new_mask(
        "Luminance",
        ComponentKind::LuminanceRange {
            lo: 0.4,
            hi: 1.0,
            feather: 0.08,
        },
        false,
    )
}
pub fn new_color_mask() -> Mask {
    new_mask(
        "Color",
        ComponentKind::ColorRange {
            hue: 0.5,
            sat: 0.5,
            tol: 0.08,
            feather: 0.06,
        },
        false,
    )
}
pub fn mask_kind_label(mask: &Mask) -> &'static str {
    match mask.components.first().map(|c| &c.kind) {
        Some(ComponentKind::Linear { .. }) => "Linear",
        Some(ComponentKind::Radial { .. }) => "Radial",
        Some(ComponentKind::Brush { .. }) => "Brush",
        Some(ComponentKind::LuminanceRange { .. }) => "Luminance",
        Some(ComponentKind::ColorRange { .. }) => "Color",
        Some(ComponentKind::Ai { .. }) => "AI",
        None => "Mask",
    }
}
