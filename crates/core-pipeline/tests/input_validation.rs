mod common;

use core_pipeline::{
    AiCoverage, DevelopParams, DevelopPipeline, LinearImage, PipelineError, ViewParams,
};

#[test]
fn malformed_images_and_coverages_return_errors_before_gpu_upload() {
    let Some(ctx) = common::gpu() else { return };
    let pipeline = DevelopPipeline::new(&ctx);
    for image in [
        LinearImage {
            width: 0,
            height: 1,
            data: vec![],
        },
        LinearImage {
            width: 1,
            height: 1,
            data: vec![0.3; 2],
        },
        LinearImage {
            width: 1,
            height: 1,
            data: vec![0.3; 4],
        },
        LinearImage {
            width: 1,
            height: 1,
            data: vec![f32::NAN; 3],
        },
        LinearImage {
            width: 1,
            height: 1,
            data: vec![f32::INFINITY; 3],
        },
    ] {
        assert!(matches!(
            pipeline.prepare(&ctx, &image),
            Err(PipelineError::InvalidInput(_))
        ));
    }
    let image = LinearImage {
        width: 2,
        height: 2,
        data: vec![0.3; 12],
    };
    let prepared = pipeline.prepare(&ctx, &image).unwrap();
    for coverage in [
        AiCoverage {
            hash: "bad".to_owned(),
            width: 0,
            height: 1,
            alpha: vec![],
        },
        AiCoverage {
            hash: "bad".to_owned(),
            width: 2,
            height: 2,
            alpha: vec![255],
        },
    ] {
        assert!(matches!(
            pipeline.render_view_with_ai_coverages(
                &ctx,
                &prepared,
                &DevelopParams::default(),
                &ViewParams::full(2, 2),
                &[coverage]
            ),
            Err(PipelineError::InvalidInput(_))
        ));
    }
    // A rejected request must not poison the prepared image or GPU error scopes.
    assert_eq!(
        pipeline
            .render(&ctx, &prepared, &DevelopParams::default())
            .unwrap()
            .len(),
        16
    );
}
