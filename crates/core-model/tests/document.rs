use core_model::document::{validate_finite, EditDocument, ReadStatus};
use core_model::{
    AiPoint, BrushStroke, ComponentKind, CurvePoint, DevelopParams, Mask, MaskComponent,
};
use serde_json::Value;

#[test]
fn fixtures_are_supported_without_rewriting_raw_json() {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../../../tests/migration/model/inputs.json")).unwrap();
    for (fixture, version) in fixtures.iter().zip([6, 5, 6, 6]) {
        let raw = serde_json::to_string_pretty(&fixture["params"]).unwrap();
        let document = EditDocument::read(&raw, version);
        assert_eq!(
            document.status(),
            &ReadStatus::Supported,
            "{}",
            fixture["name"]
        );
        assert!(document.preview().is_some());
        assert_eq!(document.raw_json(), raw);
        assert_eq!(document.process_version(), version);
    }
}

#[test]
fn unsupported_documents_keep_exact_original_without_preview() {
    for (raw, version, expected) in [
        (" {\"exposure\":1,\"future\":true} ", 6, ReadStatus::UnsupportedFields(vec!["/future".into()])),
        ("{\"crop\":{\"extra\":1}}", 6, ReadStatus::UnsupportedFields(vec!["/crop/extra".into()])),
        ("{\"displayReferred\":true}", 6, ReadStatus::UnsupportedFields(vec!["/displayReferred".into()])),
        ("{\"masks\":[{\"components\":[{\"kind\":{\"type\":\"linear\",\"p0\":[0,0],\"p1\":[1,1],\"extra\":3}}]}]}", 6, ReadStatus::UnsupportedFields(vec!["/masks/0/components/0/kind/extra".into()])),
        ("{\"masks\":[{\"components\":[{\"kind\":{\"type\":\"future\"}}]}]}", 6, ReadStatus::Invalid),
        ("{ future }", 6, ReadStatus::Invalid),
        ("[]", 6, ReadStatus::Invalid),
        ("{\"exposure\":1,\"exposure\":2}", 6, ReadStatus::Invalid),
        ("{\"future\":1}", 7, ReadStatus::UnsupportedVersion),
        ("{}", 0, ReadStatus::UnsupportedVersion),
    ] {
        let document = EditDocument::read(raw, version);
        assert_eq!(document.status(), &expected, "{raw}");
        assert!(document.preview().is_none());
        assert_eq!(document.raw_json(), raw);
    }
}

#[test]
fn finite_validation_covers_nested_values_without_clamping() {
    let mut p = DevelopParams {
        exposure: 10000.0,
        ..Default::default()
    };
    assert!(validate_finite(&p));
    p.tone_curve.rgb.push(CurvePoint {
        x: f32::NAN,
        y: 0.0,
    });
    assert!(!validate_finite(&p));
    p.tone_curve.rgb.clear();
    p.masks.push(Mask {
        components: vec![MaskComponent {
            kind: ComponentKind::Brush {
                strokes: vec![BrushStroke {
                    points: vec![[0.0, f32::INFINITY]],
                    ..Default::default()
                }],
            },
            ..Default::default()
        }],
        ..Default::default()
    });
    assert!(!validate_finite(&p));
    p.masks[0].components[0].kind = ComponentKind::Ai {
        model: String::new(),
        hash: String::new(),
        points: vec![AiPoint {
            x: 0.0,
            y: f32::NEG_INFINITY,
            positive: true,
        }],
    };
    assert!(!validate_finite(&p));
    p.masks.clear();
    p.channel_mix.blue[1] = f32::NAN;
    assert!(!validate_finite(&p));
}
