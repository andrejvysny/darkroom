//! Golden test for the Lightroom `.xmp` importer: a realistic preset must map the reliable keys,
//! flag the approximate ones, and DROP the structurally-incompatible ones (absolute WB, color grade)
//! — the honest-import contract the design review demanded.

use core_preset::Registry;
use std::path::Path;

fn keys(items: &[core_preset::ReportItem]) -> Vec<&str> {
    items.iter().map(|i| i.key.as_str()).collect()
}

#[test]
fn imports_lightroom_xmp_with_honest_report() {
    let bytes = include_bytes!("fixtures/sample.xmp");
    let reg = Registry::with_defaults();
    let parsed = reg
        .import(bytes, Some(Path::new("sample.xmp")))
        .expect("xmp should import");
    let p = &parsed.params;
    let r = &parsed.report;

    assert_eq!(r.source_format, "lightroom-xmp");
    assert_eq!(r.source_process_version.as_deref(), Some("11.0"));

    // ── CLEAN ──
    assert!((p["exposure"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    assert!((p["sharpen"].as_f64().unwrap() - 45.0).abs() < 1e-6);
    assert!((p["nrLuma"].as_f64().unwrap() - 20.0).abs() < 1e-6);
    assert!((p["nrColor"].as_f64().unwrap() - 25.0).abs() < 1e-6);
    // HSL is 1:1 by color index: Red=0 (hue +10, sat −20), Blue=5 (lum +15).
    let hsl = p["hsl"].as_array().unwrap();
    assert_eq!(hsl.len(), 8);
    assert!((hsl[0]["h"].as_f64().unwrap() - 10.0).abs() < 1e-6);
    assert!((hsl[0]["s"].as_f64().unwrap() + 20.0).abs() < 1e-6);
    assert!((hsl[5]["l"].as_f64().unwrap() - 15.0).abs() < 1e-6);
    // Tone curve points are the S-curve, normalized /255.
    let rgb = p["toneCurve"]["rgb"].as_array().unwrap();
    assert_eq!(rgb.len(), 5);
    assert!(rgb[0]["x"].as_f64().unwrap().abs() < 1e-6);
    assert!((rgb[2]["x"].as_f64().unwrap() - 128.0 / 255.0).abs() < 1e-4);
    assert!((rgb[2]["y"].as_f64().unwrap() - 140.0 / 255.0).abs() < 1e-4);

    // ── APPROX ──
    assert!((p["contrast"].as_f64().unwrap() - 15.0).abs() < 1e-6);
    // Saturation folds Vibrance at half weight: 12 + 0.5*20 = 22.
    assert!((p["saturation"].as_f64().unwrap() - 22.0).abs() < 1e-6);
    assert!((p["vignette"].as_f64().unwrap() + 15.0).abs() < 1e-6);
    // Tint −150..150 → −100..100 (20/1.5 ≈ 13.33).
    assert!((p["tint"].as_f64().unwrap() - 20.0 / 1.5).abs() < 1e-4);
    // Un-rotated crop maps to center/half-extent.
    assert!((p["crop"]["cx"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    assert!((p["crop"]["hw"].as_f64().unwrap() - 0.45).abs() < 1e-6);

    let mapped = keys(&r.mapped);
    let approx = keys(&r.approximated);
    let dropped = keys(&r.dropped);
    assert!(
        mapped.contains(&"exposure") && mapped.contains(&"toneCurve") && mapped.contains(&"hsl")
    );
    assert!(
        approx.contains(&"contrast") && approx.contains(&"saturation") && approx.contains(&"tint")
    );

    // ── DROPPED (the four review landmines) ──
    assert!(
        dropped.contains(&"Temperature"),
        "absolute WB must be dropped"
    );
    assert!(dropped.contains(&"Clarity2012"), "clarity has no target");
    assert!(
        dropped.contains(&"ColorGradeMidtoneHue"),
        "color grading must be dropped"
    );
    // Temperature must NOT have leaked into the params.
    assert!(
        !p.contains_key("temp"),
        "absolute WB temp must not be applied"
    );
    // ConvertToGrayscale=False is a no-op and must NOT be reported as dropped.
    assert!(!dropped.contains(&"ConvertToGrayscale"));
}

#[test]
fn imports_lrtemplate_via_same_mapping() {
    let bytes = include_bytes!("fixtures/sample.lrtemplate");
    let reg = Registry::with_defaults();
    let parsed = reg
        .import(bytes, Some(Path::new("sample.lrtemplate")))
        .expect("lrtemplate should import");
    let p = &parsed.params;
    let r = &parsed.report;

    assert_eq!(r.source_format, "lightroom-lrtemplate");
    assert!((p["exposure"].as_f64().unwrap() - 0.5).abs() < 1e-6);
    assert!((p["contrast"].as_f64().unwrap() - 15.0).abs() < 1e-6);
    // Vibrance folded: 12 + 0.5*20 = 22.
    assert!((p["saturation"].as_f64().unwrap() - 22.0).abs() < 1e-6);
    // HSL 1:1 by index; tone curve parsed from the Lua string array.
    assert!((p["hsl"][0]["h"].as_f64().unwrap() - 10.0).abs() < 1e-6);
    assert!((p["hsl"][5]["l"].as_f64().unwrap() - 15.0).abs() < 1e-6);
    assert_eq!(p["toneCurve"]["rgb"].as_array().unwrap().len(), 3);

    let dropped: Vec<&str> = r.dropped.iter().map(|i| i.key.as_str()).collect();
    assert!(dropped.contains(&"Temperature"));
    assert!(dropped.contains(&"Clarity2012"));
    assert!(!p.contains_key("temp"));
}

#[test]
fn unknown_bytes_are_rejected() {
    let reg = Registry::with_defaults();
    assert!(reg
        .import(b"not a preset", Some(Path::new("x.txt")))
        .is_err());
}
