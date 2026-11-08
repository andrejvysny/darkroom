use core_pipeline::{DevelopParams, DevelopPipeline, GpuContext, LinearImage};
use std::sync::{Arc, Mutex};

/// First uncaptured wgpu error on a [`Gpu`]'s device, recorded instead of the default handler's
/// mid-frame abort: the window loop checks it each frame (`take`) and routes it through the
/// fatal surface-failure path. First error wins; later ones are dropped.
#[derive(Clone, Default)]
pub struct GpuErrors(Arc<Mutex<Option<String>>>);

impl GpuErrors {
    /// The handler to install with `wgpu::Device::on_uncaptured_error`.
    pub fn hook(&self) -> Arc<dyn wgpu::UncapturedErrorHandler> {
        let slot = Arc::clone(&self.0);
        Arc::new(move |error: wgpu::Error| {
            let message = error.to_string();
            let mut slot = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(message);
            }
        })
    }

    /// The recorded error, if any; taking it clears the slot so each error is surfaced once.
    pub fn take(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

pub struct Gpu {
    pub _instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub context: GpuContext,
    /// Shared with `render_wgpu::Renderer` (which takes it by `Arc`) — one device, one set of
    /// compiled pipeline objects, no per-worker duplication.
    pub pipeline: Arc<DevelopPipeline>,
    /// Uncaptured device errors (validation/OOM/internal) land here; see [`GpuErrors`].
    pub errors: GpuErrors,
}

impl Gpu {
    pub fn new(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, String> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: surface,
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("darkroom-native"),
            required_limits: adapter.limits(),
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let errors = GpuErrors::default();
        device.on_uncaptured_error(errors.hook());
        let context = GpuContext::from_device_queue(device, queue, adapter.get_info());
        let pipeline = Arc::new(DevelopPipeline::new(&context));
        Ok(Self {
            _instance: instance,
            adapter,
            context,
            pipeline,
            errors,
        })
    }

    pub fn develop_probe(&self) -> Result<usize, String> {
        let image = LinearImage {
            width: 2,
            height: 2,
            data: vec![0.18; 12],
        };
        let prepared = self
            .pipeline
            .prepare(&self.context, &image)
            .map_err(|e| e.to_string())?;
        let pixels = self
            .pipeline
            .render(&self.context, &prepared, &DevelopParams::default())
            .map_err(|e| e.to_string())?;
        if pixels.len() != 16 || pixels.chunks_exact(4).any(|p| p[3] != 255) {
            return Err("synthetic develop output invalid".into());
        }
        Ok(pixels.len())
    }
}

#[cfg(test)]
mod tests {
    use super::GpuErrors;

    fn validation_error(description: &str) -> wgpu::Error {
        wgpu::Error::Validation {
            description: description.to_string(),
            source: Box::new(std::io::Error::other("test")),
        }
    }

    #[test]
    fn recorder_stores_the_uncaptured_error_message() {
        let errors = GpuErrors::default();
        assert!(errors.take().is_none(), "fresh recorder is empty");
        (errors.hook())(validation_error("bind group rejected"));
        assert_eq!(
            errors.take().as_deref(),
            Some("bind group rejected"),
            "the hook must record the error's message"
        );
        assert!(errors.take().is_none(), "take must clear the slot");

        (errors.hook())(validation_error("first"));
        (errors.hook())(validation_error("second"));
        assert_eq!(
            errors.take().as_deref(),
            Some("first"),
            "the first error wins; later ones must not overwrite it"
        );
    }
}
