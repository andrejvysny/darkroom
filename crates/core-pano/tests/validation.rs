use core_pano::{
    compose, estimate_alignment_rgb, register, register_streaming, AlignModel, CameraPose, Frame,
    FrameSource, LoadedFrame, PanoError, Projection, Registration, StitchOptions,
};
use nalgebra::Matrix3;

fn frame() -> Frame {
    Frame {
        width: 32,
        height: 24,
        rgb: vec![0.2; 32 * 24 * 3],
        focal_seed_px: None,
    }
}

fn registration() -> Registration {
    Registration {
        cameras: (0..2)
            .map(|_| {
                Some(CameraPose {
                    focal_px: 40.0,
                    rotation: Matrix3::identity(),
                    ppx: 16.0,
                    ppy: 12.0,
                })
            })
            .collect(),
        used_indices: vec![0, 1],
        reference_index: 0,
        pairwise: vec![],
        ba_final_rms: 0.0,
        ba_converged: true,
    }
}

fn options() -> StitchOptions {
    StitchOptions {
        projection: Projection::Perspective,
        boundary_warp: 0.0,
        auto_crop: false,
        max_long_side: 100,
        preview: false,
    }
}

#[test]
fn registration_rejects_malformed_buffers_before_feature_extraction() {
    let mut truncated = frame();
    truncated.rgb.pop();
    let empty = Frame {
        width: 0,
        height: 0,
        rgb: vec![],
        focal_seed_px: None,
    };
    let overflow = Frame {
        width: usize::MAX,
        height: 2,
        rgb: vec![],
        focal_seed_px: None,
    };
    let mut nonfinite = frame();
    nonfinite.rgb[5] = f32::NAN;
    for invalid in [truncated, empty, overflow, nonfinite] {
        assert!(matches!(
            register(&[invalid, frame()], &|_| {}),
            Err(PanoError::InvalidInput(_))
        ));
    }
}

#[test]
fn alignment_rejects_invalid_buffers_without_panicking() {
    assert!(estimate_alignment_rgb(&[], 10, 10, &[], 10, 10, AlignModel::Affine).is_none());
    assert!(estimate_alignment_rgb(&[], 0, 0, &[], 0, 0, AlignModel::Homography).is_none());
}

#[test]
fn compositor_rejects_invalid_registration_indices_and_poses() {
    let frames = vec![frame(), frame()];
    let mut cases = Vec::new();
    let mut reg = registration();
    reg.used_indices[1] = 9;
    cases.push(reg);
    let mut reg = registration();
    reg.used_indices[1] = 0;
    cases.push(reg);
    let mut reg = registration();
    reg.reference_index = 9;
    cases.push(reg);
    let mut reg = registration();
    reg.cameras[1] = None;
    cases.push(reg);
    let mut reg = registration();
    reg.cameras[1].as_mut().unwrap().focal_px = f64::INFINITY;
    cases.push(reg);
    let mut reg = registration();
    reg.cameras[1].as_mut().unwrap().rotation *= 2.0;
    cases.push(reg);
    let mut reg = registration();
    reg.cameras[1].as_mut().unwrap().rotation.fill(f64::MAX);
    cases.push(reg);
    for reg in cases {
        assert!(matches!(
            compose(&frames, &reg, &options(), &|_| {}),
            Err(PanoError::InvalidInput(_))
        ));
    }
}

#[test]
fn compositor_rejects_empty_canvas_instead_of_returning_a_black_panorama() {
    let mut opt = options();
    opt.max_long_side = 1; // The sole output sample is on the unweighted source boundary.
    assert!(matches!(
        compose(&[frame(), frame()], &registration(), &opt, &|_| {}),
        Err(PanoError::InvalidInput(_))
    ));
    opt.max_long_side = 0;
    assert!(matches!(
        compose(&[frame(), frame()], &registration(), &opt, &|_| {}),
        Err(PanoError::InvalidInput(_))
    ));
}

struct MalformedSource(usize, usize);
impl FrameSource for MalformedSource {
    fn len(&self) -> usize {
        2
    }
    fn load(&self, _: usize, _: Option<u32>) -> Result<LoadedFrame, PanoError> {
        Ok(LoadedFrame {
            rgb: vec![0.2; 32 * 24 * 3],
            width: 32,
            height: 24,
            full_width: self.0,
            full_height: self.1,
            focal_seed_px: None,
        })
    }
}

#[test]
fn streaming_registration_rejects_invalid_full_dimensions() {
    for source in [MalformedSource(0, 0), MalformedSource(64, 24)] {
        assert!(matches!(
            register_streaming(&source, &|_| {}, &|| false),
            Err(PanoError::InvalidInput(_))
        ));
    }
}

#[test]
fn resident_source_rejects_invalid_buffer_before_downscaling() {
    let mut bad = frame();
    bad.rgb.clear();
    let frames = [bad];
    assert!(matches!(
        FrameSource::load(&frames.as_slice(), 0, Some(10)),
        Err(PanoError::InvalidInput(_))
    ));
}
