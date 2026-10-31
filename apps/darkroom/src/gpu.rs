use core_pipeline::{DevelopParams, DevelopPipeline, GpuContext, LinearImage};

pub struct Gpu {
    pub _instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub context: GpuContext,
    pub pipeline: DevelopPipeline,
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
        let context = GpuContext::from_device_queue(device, queue, adapter.get_info());
        let pipeline = DevelopPipeline::new(&context);
        Ok(Self {
            _instance: instance,
            adapter,
            context,
            pipeline,
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
