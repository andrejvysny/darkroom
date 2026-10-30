use core_pipeline::{GpuContext, PipelineError};

/// Headless local runs may skip when no adapter exists. GPU validation jobs must opt in to a
/// hard requirement, so a missing/broken adapter cannot turn the entire pixel suite into a pass.
pub fn gpu() -> Option<GpuContext> {
    match GpuContext::new() {
        Ok(ctx) => Some(ctx),
        Err(PipelineError::NoAdapter)
            if std::env::var("DARKROOM_REQUIRE_GPU").as_deref() != Ok("1") =>
        {
            eprintln!("no GPU adapter — skipping (set DARKROOM_REQUIRE_GPU=1 to require one)");
            None
        }
        Err(error) => {
            panic!("GPU initialization failed: {error}; DARKROOM_REQUIRE_GPU=1 requires an adapter")
        }
    }
}
