use serde::{Deserialize, Serialize};

/// Crop + straighten geometry. Crop rect is center (`cx`,`cy`) + half-extents (`hw`,`hh`) in
/// normalized image coords; `angle` is the straighten correction in degrees (CCW+). Default is the
/// full frame, no rotation (identity). Applied as a UV-remap in the develop shader (see `to_geom`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Crop {
    pub cx: f32,
    pub cy: f32,
    pub hw: f32,
    pub hh: f32,
    pub angle: f32,
    /// Whole-image 90° rotation, in clockwise quarter-turns (0..3). Applied as the OUTERMOST
    /// transform: the crop rect (`cx,cy,hw,hh`) and straighten `angle` are defined in the ROTATED
    /// (displayed) frame; the shader permutes the resolved UV back to the un-rotated source.
    #[serde(default)]
    pub rot90: i32,
}

impl Default for Crop {
    fn default() -> Self {
        Self {
            cx: 0.5,
            cy: 0.5,
            hw: 0.5,
            hh: 0.5,
            angle: 0.0,
            rot90: 0,
        }
    }
}

impl Crop {
    /// True when the crop is the full frame with no rotation (shader takes the identity fast-path).
    pub fn is_identity(&self) -> bool {
        self.cx == 0.5
            && self.cy == 0.5
            && self.hw >= 0.5
            && self.hh >= 0.5
            && self.angle == 0.0
            && self.rot_q() == 0
    }

    /// Normalized quarter-turn count 0..3 (handles negative / out-of-range `rot90`).
    pub fn rot_q(&self) -> i32 {
        self.rot90.rem_euclid(4)
    }

    /// The DISPLAYED-frame aspect (W/H) given the source aspect: odd quarter-turns swap W↔H.
    pub fn eff_aspect(&self, src_aspect: f32) -> f32 {
        if self.rot_q() % 2 == 1 {
            1.0 / src_aspect
        } else {
            src_aspect
        }
    }

    /// Native dimensions of the selected crop, in the displayed (quarter-turned) frame. This is
    /// the output size for export, not the letterboxed content size inside a source-sized preview.
    pub fn output_size(&self, source_w: u32, source_h: u32) -> (u32, u32) {
        let (w, h) = if self.rot_q() % 2 == 1 {
            (source_h, source_w)
        } else {
            (source_w, source_h)
        };
        let edge = |pixels: u32, half_extent: f32| {
            (pixels as f64 * f64::from(half_extent) * 2.0)
                .round()
                .clamp(1.0, f64::from(pixels.max(1))) as u32
        };
        (edge(w, self.hw), edge(h, self.hh))
    }

    /// Source long-edge budget needed for a cropped preview. Scaling the uncropped image straight
    /// to `output_edge` would discard detail twice for a tight crop. Keep enough source pixels for
    /// the requested crop, up to its native resolution and the GPU's source texture limit.
    pub fn preview_source_edge(
        &self,
        source_w: u32,
        source_h: u32,
        output_edge: u32,
        device_cap: u32,
    ) -> u32 {
        let source_edge = source_w.max(source_h).max(1);
        let (crop_w, crop_h) = self.output_size(source_w, source_h);
        let crop_edge = crop_w.max(crop_h).max(1);
        let output_edge = output_edge.max(1).min(crop_edge);
        let needed =
            (u64::from(source_edge) * u64::from(output_edge)).div_ceil(u64::from(crop_edge)) as u32;
        needed.min(source_edge).min(device_cap.max(1))
    }

    /// The cropped content's pixel rect `(x, y, w, h)` within an input-sized (`w`×`h`) render. The
    /// legacy develop shader letterbox-fits the crop into that target. This describes the preview
    /// content rect, not native export dimensions; use [`Self::output_size`] for export and render
    /// a crop-local viewport directly. Mirrors `geom_resolve` for `out_aspect == src_aspect`.
    pub fn export_rect(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        if self.is_identity() {
            return (0, 0, w, h);
        }
        let src_aspect = w as f32 / h as f32;
        // Crop pixel-aspect is measured in the DISPLAYED (rot90) frame, then letterboxed into the
        // source-sized render (out_aspect == src_aspect), so a 90° rotation swaps the content rect.
        let ac = (self.hw / self.hh) * self.eff_aspect(src_aspect);
        let (cwf, chf) = if src_aspect > ac {
            (ac / src_aspect, 1.0)
        } else {
            (1.0, src_aspect / ac)
        };
        let cw = (cwf * w as f32).round().clamp(1.0, w as f32) as u32;
        let ch = (chf * h as f32).round().clamp(1.0, h as f32) as u32;
        ((w - cw) / 2, (h - ch) / 2, cw, ch)
    }
}

#[cfg(test)]
mod crop_output_tests {
    use super::Crop;

    #[test]
    fn native_crop_dimensions_preserve_area_and_quarter_turns() {
        let mut crop = Crop::default();
        assert_eq!(crop.output_size(640, 480), (640, 480));
        crop.hw = 0.25;
        crop.hh = 0.25;
        assert_eq!(crop.output_size(640, 480), (320, 240));
        crop.rot90 = 1;
        assert_eq!(crop.output_size(640, 480), (240, 320));
        crop = Crop {
            rot90: -1,
            ..Default::default()
        };
        assert_eq!(crop.output_size(640, 480), (480, 640));
        crop.hw = 0.00001;
        assert_eq!(crop.output_size(640, 480), (1, 640));
    }

    #[test]
    fn preview_budget_preserves_tight_crops_without_upscaling() {
        let crop = Crop {
            hw: 0.05,
            hh: 0.05,
            ..Default::default()
        };
        assert_eq!(crop.output_size(6000, 4000), (600, 400));
        assert_eq!(crop.preview_source_edge(6000, 4000, 1536, 8192), 6000);
        assert_eq!(crop.preview_source_edge(6000, 4000, 300, 8192), 3000);
        assert_eq!(crop.preview_source_edge(6000, 4000, 1536, 4096), 4096);
        assert_eq!(
            Crop::default().preview_source_edge(600, 400, 1536, 8192),
            600
        );
        assert_eq!(
            Crop::default().preview_source_edge(6000, 4000, 1536, 8192),
            1536
        );
    }

    #[test]
    fn preview_budget_uses_rotated_crop_axes() {
        let crop = Crop {
            rot90: 1,
            hw: 0.5,
            hh: 0.25,
            ..Default::default()
        };
        assert_eq!(crop.output_size(6000, 4000), (4000, 3000));
        assert_eq!(crop.preview_source_edge(6000, 4000, 1000, 8192), 1500);
        assert_eq!(crop.output_size(1500, 1000), (1000, 750));
        assert_eq!(crop.preview_source_edge(6000, 4000, 5000, 2048), 2048);
    }
}
