use core_hdr::{
    ev100, reference_index, relative_scale, warp_into_reference, DeghostParams, ExposureInfo,
    MergeAccumulator,
};
use core_raw::LinearImage;

fn image(values: &[[f32; 3]]) -> LinearImage {
    LinearImage {
        width: values.len() as u32,
        height: 1,
        data: values.iter().flatten().copied().collect(),
    }
}

fn exposure(t: f64) -> ExposureInfo {
    ExposureInfo {
        exposure_time_s: t,
        f_number: 8.0,
        iso: 100.0,
    }
}

#[test]
fn clipped_reference_keeps_its_value_outside_shortest_frame_coverage() {
    let reference = image(&[[1.0; 3], [1.0; 3]]);
    let shortest = image(&[[0.0; 3], [0.75; 3]]);
    let mut merge =
        MergeAccumulator::with_reference(&reference, false, DeghostParams::default()).unwrap();
    merge
        .add_frame_masked(&shortest, 4.0, true, &[0, 1])
        .unwrap();
    let result = merge.finish().unwrap();
    assert_eq!(&result.data[..3], &[1.0; 3]);
    assert_eq!(&result.data[3..], &[3.0; 3]);
}

#[test]
fn exposure_metadata_must_be_finite_and_positive() {
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
        let mut frames = [exposure(0.01); 3];
        frames[0].exposure_time_s = invalid;
        frames[1].f_number = invalid;
        frames[2].iso = invalid;
        for frame in frames {
            assert!(ev100(&frame).is_err(), "accepted {frame:?}");
            assert!(reference_index(&[exposure(0.01), frame]).is_err());
        }
    }
}

#[test]
fn finite_extreme_metadata_does_not_overflow_intermediate_ev_math() {
    let large = ExposureInfo {
        exposure_time_s: 1e200,
        f_number: 1e200,
        iso: 1e200,
    };
    // N² / (t * ISO) is exactly one in real arithmetic, so EV100 is log2(100).
    assert!((ev100(&large).unwrap() - 100.0f64.log2()).abs() < 1e-10);
    assert_eq!(relative_scale(&large, &large).unwrap(), 1.0);
    assert!(relative_scale(&exposure(1e-300), &exposure(1e300)).is_err());
    assert!(relative_scale(&exposure(1e300), &exposure(1e-300)).is_err());
}

#[test]
fn rejected_frame_does_not_poison_accumulated_pixels() {
    let good = image(&[[0.3; 3]]);
    let mut merge = MergeAccumulator::new(1, 1);
    merge.add_frame(&good, 1.0, true).unwrap();
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0] {
        assert!(merge.add_frame(&good, invalid, false).is_err());
    }
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(merge
            .add_frame(&image(&[[invalid, 0.3, 0.3]]), 1.0, false)
            .is_err());
    }
    assert!(merge
        .add_frame(&image(&[[f32::MAX; 3]]), 2.0, true)
        .is_err());
    merge.add_frame(&good, 1.0, false).unwrap();
    assert_eq!(merge.finish().unwrap().data, good.data);
}

#[test]
fn invalid_masked_pixels_are_ignored_without_hiding_invalid_valid_pixels() {
    let good = image(&[[0.3; 3], [0.3; 3]]);
    let masked = image(&[[f32::NAN; 3], [0.3; 3]]);
    let mut merge = MergeAccumulator::new(2, 1);
    merge.add_frame(&good, 1.0, true).unwrap();
    assert!(merge
        .add_frame_masked(&masked, 1.0, false, &[1, 0])
        .is_err());
    merge
        .add_frame_masked(&masked, 1.0, false, &[0, 1])
        .unwrap();
    assert_eq!(merge.finish().unwrap().data, good.data);
}

#[test]
fn accumulator_overflow_is_reported_instead_of_exporting_infinity() {
    let bright = image(&[[0.5; 3]]);
    let mut merge = MergeAccumulator::new(1, 1);
    for i in 0..3 {
        merge.add_frame(&bright, f32::MAX, i == 0).unwrap();
    }
    assert!(merge.finish().is_err());
}

#[test]
fn malformed_reference_is_rejected_before_allocating_accumulator() {
    for reference in [
        image(&[]),
        LinearImage {
            width: u32::MAX,
            height: u32::MAX,
            data: vec![],
        },
        image(&[[f32::NAN; 3]]),
    ] {
        assert!(
            MergeAccumulator::with_reference(&reference, false, DeghostParams::default()).is_err()
        );
    }
}

#[test]
fn invalid_deghost_parameters_are_rejected() {
    let reference = image(&[[0.3; 3]]);
    for params in [
        DeghostParams { sigma: 0.0, k: 0.0 },
        DeghostParams {
            sigma: -1.0,
            k: 0.25,
        },
        DeghostParams {
            sigma: f32::NAN,
            k: 0.25,
        },
        DeghostParams {
            sigma: 0.05,
            k: -1.0,
        },
        DeghostParams {
            sigma: 0.05,
            k: f32::INFINITY,
        },
    ] {
        assert!(MergeAccumulator::with_reference(&reference, false, params).is_err());
    }
}

#[test]
fn invalid_warps_do_not_report_valid_samples() {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let moving = image(&[[0.3; 3]]);
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut transform = identity;
        transform[0][2] = invalid;
        let (rgb, valid) = warp_into_reference(&moving, 1, 1, &transform);
        assert_eq!(valid, [0]);
        assert_eq!(rgb, [0.0; 3]);
    }
    let malformed = LinearImage {
        width: 1,
        height: 1,
        data: vec![],
    };
    let empty = image(&[]);
    for moving in [&malformed, &empty] {
        let (rgb, valid) = warp_into_reference(moving, 1, 1, &identity);
        assert_eq!(valid, [0]);
        assert_eq!(rgb, [0.0; 3]);
    }
    let (rgb, valid) = warp_into_reference(&moving, 0, 1, &identity);
    assert!(rgb.is_empty() && valid.is_empty());
}
