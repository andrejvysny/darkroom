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
/// (linear filtering, or nearest-neighbour when `nearest`: a native-resolution window magnified
/// past 100 % must show real pixels, and egui picks the sampler at registration). Returns the
/// egui texture id plus the frame's pixel size, ready for `ui.image((id, size_in_points))`.
pub fn register_presented_frame(
    device: &wgpu::Device,
    renderer: &mut egui_wgpu::Renderer,
    lease: &FrameLease,
    nearest: bool,
) -> (egui::TextureId, [u32; 2]) {
    let view = lease
        .texture()
        .create_view(&wgpu::TextureViewDescriptor::default());
    let filter = if nearest {
        wgpu::FilterMode::Nearest
    } else {
        wgpu::FilterMode::Linear
    };
    let texture_id = renderer.register_native_texture(device, &view, filter);
    let (width, height) = lease.key().viewport_px;
    (texture_id, [width, height])
}

/// Paint one egui frame showing the developed lease through the exact registration and paint
/// path the window uses, inside a Validation error scope: any wgpu validation error is
/// returned as `Err` (the probe maps it to `ProbeError::Render`, exit 6) instead of aborting.
///
/// The composited target is then read back and compared with the developed lease (read back the
/// same way): the probe panel has no margin and the target equals the frame size, so the photo
/// fills the target 1:1 and any binding/format/sampling bug shows as a pixel difference. Returns
/// the mean absolute per-channel difference (0..255 scale).
pub fn paint_probe_frame(gpu: &Gpu, lease: &FrameLease) -> Result<f64, String> {
    let (width, height) = lease.key().viewport_px;
    let size = egui::vec2(width as f32, height as f32);
    let scope = gpu
        .context
        .device
        .push_error_scope(wgpu::ErrorFilter::Validation);
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = egui_wgpu::Renderer::new(&gpu.context.device, format, Default::default());
    let (texture_id, _) =
        register_presented_frame(&gpu.context.device, &mut renderer, lease, false);
    let target = paint::probe_target(&gpu.context.device, format, [width, height]);
    let egui = egui::Context::default();
    egui.set_missing_glyph_policy(egui::epaint::text::MissingGlyphPolicy::Panic);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        ..Default::default()
    };
    let output = egui.run_ui(input, |root| {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(root, |ui| {
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
    composited_diff(gpu, &target, lease)
}

/// Max tolerated mean absolute per-channel difference between the composited egui target and
/// the developed frame (8-bit scale): 1:1 bilinear sampling should be exact; 2 allows rounding.
const COMPOSITE_TOLERANCE: f64 = 2.0;

fn composited_diff(gpu: &Gpu, target: &wgpu::Texture, lease: &FrameLease) -> Result<f64, String> {
    let read = |texture: &wgpu::Texture| {
        gpu.pipeline
            .read_back_texture(&gpu.context, texture)
            .map_err(|e| format!("probe readback failed: {e}"))
    };
    let composited = read(target)?;
    let developed = read(lease.texture())?;
    if composited.len() != developed.len() || composited.is_empty() {
        return Err(format!(
            "composited target ({} bytes) and developed frame ({} bytes) differ in size",
            composited.len(),
            developed.len()
        ));
    }
    let first = &composited[..3];
    if composited.chunks_exact(4).all(|px| &px[..3] == first) {
        return Err(
            "composited photo region is uniform background: the frame was not drawn".into(),
        );
    }
    let mut sum = 0u64;
    for (a, b) in composited.chunks_exact(4).zip(developed.chunks_exact(4)) {
        sum += (0..3).map(|c| u64::from(a[c].abs_diff(b[c]))).sum::<u64>();
    }
    let diff = sum as f64 / (composited.len() / 4 * 3) as f64;
    if diff > COMPOSITE_TOLERANCE {
        return Err(format!(
            "composited egui output differs from the developed frame: mean abs diff \
             {diff:.3}/255 per channel exceeds {COMPOSITE_TOLERANCE}"
        ));
    }
    Ok(diff)
}
