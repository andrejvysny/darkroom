use core_model::controls::{control_spec, ControlScope, CONTROL_SPECS};
use std::collections::HashSet;

#[test]
fn specifications_are_unique_and_validate_finite_ui_ranges() {
    let mut ids = HashSet::new();
    for spec in CONTROL_SPECS {
        assert!(ids.insert(spec.id), "duplicate {}", spec.id);
        assert!(spec.accepts_ui_value(spec.min));
        assert!(spec.accepts_ui_value(spec.max));
        assert!(spec.accepts_ui_value(spec.default));
        assert!(!spec.accepts_ui_value(spec.min - 1.0));
        assert!(!spec.accepts_ui_value(spec.max + 1.0));
        assert!(!spec.accepts_ui_value(f64::NAN));
        assert!(!spec.accepts_ui_value(f64::INFINITY));
        assert!(!spec.accepts_ui_value(f64::NEG_INFINITY));
        assert_eq!(
            spec.step,
            if spec.id == "preset.amount" {
                Some(1.0)
            } else {
                None
            }
        );
    }
}

#[test]
fn mapping_display_and_reset_follow_widgets() {
    let diagonal = control_spec("channelMix.diagonal").unwrap();
    assert_eq!(diagonal.default, 100.0);
    assert_eq!(diagonal.reset_value(), 0.0);
    assert_eq!(diagonal.scale, 0.01);
    let exposure = control_spec("exposure").unwrap();
    assert_eq!(
        (exposure.min, exposure.max, exposure.decimals),
        (-5.0, 5.0, 2)
    );
    let color = control_spec("cbRgb.channel").unwrap();
    assert_eq!(100.0 * color.scale, 0.5);
    let mask = control_spec("mask.luminanceRange.lo").unwrap();
    assert!(mask.rounded_readback);
    assert_eq!(mask.scope, ControlScope::PersistedEdit);
    assert_eq!(
        control_spec("brush.size").unwrap().scope,
        ControlScope::TransientWidget
    );
    assert!(control_spec("unknown").is_none());
}

#[test]
fn concrete_fields_resolve_to_stable_paths_and_family_defaults() {
    use core_model::controls::{ColorZone, FieldControl, HslComponent, RgbChannel};
    let channels = [RgbChannel::Red, RgbChannel::Green, RgbChannel::Blue];
    let components = [
        HslComponent::Hue,
        HslComponent::Saturation,
        HslComponent::Luminance,
    ];
    let zones = [
        ColorZone::Global,
        ColorZone::Shadows,
        ColorZone::Midtones,
        ColorZone::Highlights,
    ];
    let mut ids = HashSet::new();
    for band in 0..8 {
        for component in components {
            let field = FieldControl::Hsl { band, component }.resolve().unwrap();
            assert!(ids.insert(field.field_id));
            assert_eq!(field.spec.default, 0.0);
        }
    }
    assert!(FieldControl::Hsl {
        band: 8,
        component: HslComponent::Hue
    }
    .resolve()
    .is_none());
    for zone in zones {
        for channel in channels {
            let field = FieldControl::ColorBalance { zone, channel }
                .resolve()
                .unwrap();
            assert!(ids.insert(field.field_id));
            assert_eq!(field.spec.default, 0.0);
        }
    }
    for output in channels {
        for source in channels {
            let field = FieldControl::ChannelMix { output, source }
                .resolve()
                .unwrap();
            assert!(ids.insert(field.field_id));
            assert_eq!(
                field.spec.default,
                if output == source { 100.0 } else { 0.0 }
            );
        }
    }
    assert_eq!(ids.len(), 45);
    assert!(ids.contains("hsl.0.h"));
    assert!(ids.contains("cbRgb.global.0"));
    assert!(ids.contains("channelMix.red.0"));
    assert!(control_spec("cbRgb.contrast").is_some());
    assert!(control_spec("cbRgb.saturation").is_some());
}

#[test]
fn conversion_preserves_out_of_range_saved_values_and_js_rounding() {
    let color = control_spec("cbRgb.channel").unwrap();
    assert_eq!(color.ui_to_stored(100.0), Some(0.5));
    assert_eq!(color.ui_to_stored(101.0), None);
    assert_eq!(color.ui_to_stored(f64::NAN), None);
    assert_eq!(color.stored_to_ui(0.75), Some(150.0));
    assert_eq!(color.stored_to_ui(f64::INFINITY), None);
    let mask = control_spec("mask.opacity").unwrap();
    assert_eq!(mask.stored_to_ui(0.125), Some(13.0));
    assert_eq!(mask.stored_to_ui(-0.125), Some(-12.0));
    assert_eq!(mask.ui_to_stored(12.5), Some(0.125));
    assert_eq!(
        mask.stored_to_ui(-0.005).unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
}
