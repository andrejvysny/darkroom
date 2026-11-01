//! Shared egui presentation of developed frames.
//!
//! The window path and `--photo-probe` register the presenter's frame with egui through the
//! SAME [`register_presented_frame`], and the probe additionally paints one egui frame showing
//! it via [`paint_probe_frame`]. Headless read-back alone cannot catch texture-binding bugs:
//! the real-window crash (egui's `egui_user_image_0` bind group rejected because the pooled
//! target lacked `TEXTURE_BINDING`) is only observable when something actually samples the
//! frame, so the probe must run the sampling path too.

use crate::{gpu::Gpu, paint};
use render_wgpu::FrameLease;

/// Register a finished lease's texture with the host's egui renderer as a native texture
/// (linear filtering). Returns the egui texture id plus the frame's pixel size, ready for
/// `ui.image((id, size_in_points))`.
pub fn register_presented_frame(
    device: &wgpu::Device,
    renderer: &mut egui_wgpu::Renderer,
    lease: &FrameLease,
) -> (egui::TextureId, [u32; 2]) {
    let view = lease
        .texture()
        .create_view(&wgpu::TextureViewDescriptor::default());
    let texture_id = renderer.register_native_texture(device, &view, wgpu::FilterMode::Linear);
    let (width, height) = lease.key().viewport_px;
    (texture_id, [width, height])
}

/// Paint one egui frame showing the developed lease through the exact registration and paint
/// path the window uses, inside a Validation error scope: any wgpu validation error is
/// returned as `Err` (the probe maps it to `ProbeError::Render`, exit 6) instead of aborting.
pub fn paint_probe_frame(gpu: &Gpu, lease: &FrameLease) -> Result<(), String> {
    let (width, height) = lease.key().viewport_px;
    let size = egui::vec2(width as f32, height as f32);
    let scope = gpu
        .context
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = egui_wgpu::Renderer::new(&gpu.context.device, format, Default::default());
    let (texture_id, _) = register_presented_frame(&gpu.context.device, &mut renderer, lease);
    let target = paint::probe_target(&gpu.context.device, format, [width, height]);
    let egui = egui::Context::default();
    egui.set_missing_glyph_policy(egui::epaint::text::MissingGlyphPolicy::Panic);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        ..Default::default()
    };
    let output = egui.run_ui(input, |root| {
        egui::CentralPanel::default().show(root, |ui| {
            ui.image((texture_id, size));
        });
    });
    paint::paint(
        &gpu.context,
        &mut renderer,
        &target.create_view(&Default::default()),
        output,
        &egui,
        [width, height],
    );
    gpu.context
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| e.to_string())?;
    if let Some(error) = pollster::block_on(scope.pop()) {
        return Err(format!(
            "egui presentation of the developed frame failed: {error}"
        ));
    }
    Ok(())
}
