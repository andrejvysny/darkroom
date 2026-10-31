mod application;
mod gpu;
mod paint;
mod window;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => window::run(),
        [arg] if arg == "--gpu-probe" => probe(),
        [arg, path] if arg == "--catalog" => window::run_with_catalog(Some(path.into())),
        [arg, path] if arg == "--catalog-probe" => application::probe(path.into())
            .map(|value| println!("{value}"))
            .map_err(|e| e.to_string()),
        _ => Err(
            "usage: darkroom-native [--gpu-probe | --catalog PATH | --catalog-probe PATH]".into(),
        ),
    };
    if let Err(error) = result {
        if args.iter().any(|arg| arg == "--gpu-probe") {
            println!("{}", serde_json::json!({"status":"error", "error": error}));
        } else {
            eprintln!("{error}");
        }
        std::process::exit(1);
    }
}

fn probe() -> Result<(), String> {
    let gpu = gpu::Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle()),
        None,
    )?;
    let bytes = gpu.develop_probe()?;
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let texture = probe_target(&gpu.context.device, format);
    let mut renderer = egui_wgpu::Renderer::new(&gpu.context.device, format, Default::default());
    let egui = egui::Context::default();
    egui.set_missing_glyph_policy(egui::epaint::text::MissingGlyphPolicy::Panic);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(256.0, 128.0),
        )),
        ..Default::default()
    };
    let output = egui.run_ui(input, paint::ui);
    paint::paint(
        &gpu.context,
        &mut renderer,
        &texture.create_view(&Default::default()),
        output,
        &egui,
        [256, 128],
    );
    gpu.context
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::json!({"status":"ok", "adapter":gpu.context.info, "develop_bytes":bytes, "egui_rendered":true})
    );
    Ok(())
}

fn probe_target(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("probe-target"),
        size: wgpu::Extent3d {
            width: 256,
            height: 128,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}
