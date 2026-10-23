//! Baked AI pixels belong to a render request and participate in mask-cache validity.
mod common;

use core_pipeline::{
    AiCoverage, ComponentKind, DevelopParams, DevelopPipeline, LinearImage, LocalAdjust, Mask,
    MaskComponent, ViewParams,
};

fn params() -> DevelopParams {
    DevelopParams {
        masks: vec![Mask {
            name: "subject".into(),
            components: vec![MaskComponent {
                kind: ComponentKind::Ai {
                    model: "test".into(),
                    points: vec![],
                    hash: "subject".into(),
                },
                ..Default::default()
            }],
            adjust: LocalAdjust {
                exposure: 1.0,
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn coverage_arrival_replacement_and_removal_rebake_cached_layers() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let image = LinearImage {
        width: 32,
        height: 24,
        data: vec![0.1; 32 * 24 * 3],
    };
    let prepared = pipe.prepare(&ctx, &image).unwrap();
    let params = params();
    let view = ViewParams::full(image.width, image.height);
    let full = AiCoverage {
        hash: "subject".into(),
        width: 8,
        height: 8,
        alpha: vec![255; 64],
    };
    let empty = AiCoverage {
        alpha: vec![0; 64],
        ..full.clone()
    };
    let render = |coverages: &[AiCoverage]| {
        pipe.render_view_with_ai_coverages(&ctx, &prepared, &params, &view, coverages)
            .unwrap()
    };
    let absent = render(&[]);
    let covered = render(std::slice::from_ref(&full));
    assert_ne!(
        covered, absent,
        "coverage arriving after a cache miss must affect pixels"
    );
    assert_eq!(
        render(std::slice::from_ref(&empty)),
        absent,
        "same key with new alpha must rebake"
    );
    assert_eq!(
        render(std::slice::from_ref(&full)),
        covered,
        "restoring alpha must rebake"
    );
    assert_eq!(
        render(&[]),
        absent,
        "removing coverage must clear its previous contribution"
    );

    // A request's explicit snapshot overrides unrelated legacy stored coverage.
    *prepared.ai_coverages.lock().unwrap() = vec![full.clone()];
    assert_eq!(render(&[]), absent);
    *prepared.ai_coverages.lock().unwrap() = vec![empty];
    assert_eq!(render(std::slice::from_ref(&full)), covered);

    // A newly prepared export/canonical render agrees with a warm interactive render.
    let fresh = pipe.prepare(&ctx, &image).unwrap();
    assert_eq!(
        pipe.render_view_with_ai_coverages(&ctx, &fresh, &params, &view, &[full])
            .unwrap(),
        covered,
    );
}
