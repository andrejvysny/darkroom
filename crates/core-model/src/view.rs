/// CPU-side viewport descriptor consumed by `DevelopPipeline::render_view`. Renders only the
/// crop-local window `[origin, origin + size]` into an `out_w × out_h` target. `overlay_layer` is
/// the **packed enabled-mask layer** (not the frontend mask index; -1 = no overlay) whose coverage
/// is red-tinted over the result.
#[derive(Clone, Copy, Debug)]
pub struct ViewParams {
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub out_w: u32,
    pub out_h: u32,
    pub active: bool,
    pub overlay_layer: i32,
    pub overlay_color: [f32; 3],
    pub overlay_strength: f32,
    /// Crop-tool preview: force auto-zoom off (show the whole straightened/rotated frame) and dim
    /// the out-of-image corners, so the crop overlay maps 1:1 to the displayed frame.
    pub crop_preview: bool,
}

impl ViewParams {
    /// Identity view: whole crop, output == source size, no overlay → byte-identical to the legacy
    /// (pre-viewport) render. The 3-arg `render()` wrapper uses this.
    pub fn full(w: u32, h: u32) -> Self {
        Self {
            origin: [0.0, 0.0],
            size: [1.0, 1.0],
            out_w: w.max(1),
            out_h: h.max(1),
            active: false,
            overlay_layer: -1,
            overlay_color: [0.85, 0.10, 0.10],
            overlay_strength: 0.5,
            crop_preview: false,
        }
    }
}
