use core_model::{geometry::*, Mask};
use serde_json::Value;

fn number(v: &Value) -> f64 {
    v.as_f64().unwrap()
}
fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 2e-7, "{actual} != {expected}");
}
fn compare(actual: &Value, expected: &Value) {
    match (actual, expected) {
        (Value::Number(_), Value::Number(_)) => near(number(actual), number(expected)),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b) {
                compare(x, y);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(a.len(), b.len());
            for (k, v) in a {
                compare(v, &b[k]);
            }
        }
        _ => assert_eq!(actual, expected),
    }
}

#[test]
fn matches_typescript_geometry_fixtures() {
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/geometry.json")).unwrap();
    for item in fixtures["frames"].as_array().unwrap() {
        let b = &item["border"];
        let border = FrameSettings {
            size: number(&b["size"]),
            aspect_w: number(&b["aspectW"]),
            aspect_h: number(&b["aspectH"]),
        };
        let result = compute_frame(number(&item["w"]), number(&item["h"]), &border);
        for (a, k) in [
            (result.ow, "ow"),
            (result.oh, "oh"),
            (result.off_x, "offX"),
            (result.off_y, "offY"),
        ] {
            near(a, number(&item["result"][k]));
        }
    }
    for item in fixtures["rects"].as_array().unwrap() {
        let r = &item["rect"];
        let actual = client_to_norm(
            number(&r["left"]),
            number(&r["top"]),
            number(&r["width"]),
            number(&r["height"]),
            number(&item["x"]),
            number(&item["y"]),
        );
        for (a, e) in actual.iter().zip(item["result"].as_array().unwrap()) {
            near(*a, number(e));
        }
    }
    for item in fixtures["colors"].as_array().unwrap() {
        let rgb = item["rgb"].as_array().unwrap();
        let (r, g, b) = (number(&rgb[0]), number(&rgb[1]), number(&rgb[2]));
        assert_eq!(rgb3_to_css([r, g, b]), item["css"].as_str().unwrap());
        for (a, e) in rgb_to_hsv(r, g, b)
            .iter()
            .zip(item["hsv"].as_array().unwrap())
        {
            near(*a, number(e));
        }
    }
    let masks = [
        new_linear_mask(),
        new_radial_mask(),
        new_brush_mask(),
        new_ai_mask(),
        new_luminance_mask(),
        new_color_mask(),
    ];
    for (actual, item) in masks.iter().zip(fixtures["masks"].as_array().unwrap()) {
        compare(&serde_json::to_value(actual).unwrap(), &item["value"]);
        assert_eq!(mask_kind_label(actual), item["label"].as_str().unwrap());
    }
    assert_eq!(mask_kind_label(&Mask::default()), "Mask");
    for item in fixtures["curves"].as_array().unwrap() {
        let points: Vec<UiCurvePoint> = item["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| UiCurvePoint {
                x: number(&p["x"]),
                y: number(&p["y"]),
            })
            .collect();
        for sample in item["samples"].as_array().unwrap() {
            near(
                eval_ui_curve(&points, number(&sample["x"])),
                number(&sample["y"]),
            );
        }
    }
}

#[test]
fn nonfinite_matches_javascript() {
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/geometry.json")).unwrap();
    let special = &fixtures["special"];
    assert!(clamp01(f64::NAN).is_nan());
    assert_eq!(special["clampNaN"], "NaN");
    let hsv = rgb_to_hsv(f64::NAN, 0.0, 1.0);
    assert_eq!(hsv[0], 0.0);
    assert!(hsv[1].is_nan() && hsv[2].is_nan());
    assert_eq!(special["hsvNaN"], serde_json::json!(["0", "NaN", "NaN"]));
    assert_eq!(
        rgb3_to_css([f64::NAN, f64::INFINITY, f64::NEG_INFINITY]),
        special["cssNaN"].as_str().unwrap()
    );
    assert!(eval_ui_curve(
        &[
            UiCurvePoint {
                x: f64::NAN,
                y: 1.0
            },
            UiCurvePoint { x: 0.0, y: 0.0 },
            UiCurvePoint { x: 1.0, y: 1.0 }
        ],
        f64::NAN
    )
    .is_nan());
    assert_eq!(special["curveNaN"], "NaN");
}
