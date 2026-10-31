//! Explicit domain-to-GPU conversion; numerical implementations preserved from the legacy adapter.
use super::color::{grading_matrices, M3};
use super::tone::{BASE_HUE_PROTECT, BASE_U_MAX, BASE_U_MIN};
use super::*;

/// Import to pack GPU uniforms from GPU-free domain parameters.
pub trait GpuParamsExt {
    fn to_uniform(&self) -> ParamsUniform;
    fn to_extra(&self, texel: [f32; 2]) -> ExtraUniform;
    fn to_wb_uniform(&self) -> WbUniform;
    fn to_fx(&self) -> FxUniform;
    fn to_mask_buffer(&self) -> MaskBufferUniform;
    fn to_tone_op(&self) -> ToneOpUniform;
    fn base_curve_lut(&self) -> Vec<f32>;
    fn to_geom(&self, src_aspect: f32, out_aspect: f32) -> GeomUniform;
    fn lens_is_active(&self) -> bool;
    fn lens_coeffs(&self) -> [f32; 4];
    fn to_cbrgb(&self) -> CbRgbUniform;
    fn to_chanmix(&self) -> ChanMixUniform;
}
impl GpuParamsExt for DevelopParams {
    fn to_uniform(&self) -> ParamsUniform {
        ParamsUniform {
            // Global WB now rides the CAT matrix (`WbUniform`, binding 8); keep this neutral so the
            // shader's `apply_local_linear` is a no-op for the global wb_gain. Masks still use
            // `wb_gain_from` as a per-mask delta (see `to_mask_buffer`).
            wb_gain: [1.0, 1.0, 1.0],
            exposure: self.exposure,
            saturation: (self.saturation / 100.0).clamp(-1.0, 1.0),
            contrast: (self.contrast / 100.0).clamp(-1.0, 1.0),
            highlights: (self.highlights / 100.0).clamp(-1.0, 1.0),
            shadows: (self.shadows / 100.0).clamp(-1.0, 1.0),
            blacks: (self.blacks / 100.0).clamp(-1.0, 1.0),
            whites: (self.whites / 100.0).clamp(-1.0, 1.0),
            _pad0: 0.0,
            _pad1: 0.0,
        }
    }

    /// Pack the Detail (sharpen/NR) + Lens (vignette) scalars for the GPU. `texel` = (1/w, 1/h) of
    /// the image being rendered (for neighborhood sampling), supplied by the backend.
    fn to_extra(&self, texel: [f32; 2]) -> ExtraUniform {
        ExtraUniform {
            detail: [
                (self.sharpen / 100.0).max(0.0), // 0..1.5
                (self.nr_luma / 100.0).clamp(0.0, 1.0),
                (self.nr_color / 100.0).clamp(0.0, 1.0),
                (self.vignette / 100.0).clamp(-1.0, 1.0),
            ],
            // texel.z carries the scene-linear baseline gain (ACR-brightness calibration); .w spare.
            texel: [texel[0], texel[1], BASELINE_GAIN, 0.0],
            // Presence (clarity/texture/dehaze). local.w = active flag ⇒ the shader skips the whole
            // multi-scale sample at defaults (byte-identical render).
            local: [
                (self.clarity / 100.0).clamp(-1.0, 1.0),
                (self.texture / 100.0).clamp(-1.0, 1.0),
                (self.dehaze / 100.0).clamp(-1.0, 1.0),
                if self.local_is_active() { 1.0 } else { 0.0 },
            ],
        }
    }

    /// Pack the global white-balance CAT matrix for the GPU (std140 mat3 = 3 × `vec4` columns).
    fn to_wb_uniform(&self) -> WbUniform {
        let m = wb_matrix(self.temp, self.tint); // row-major
                                                 // Column j (for `mat3x3 * v` in WGSL) is (m[0][j], m[1][j], m[2][j]).
        WbUniform {
            cols: [
                [m[0][0], m[1][0], m[2][0], 0.0],
                [m[0][1], m[1][1], m[2][1], 0.0],
                [m[0][2], m[1][2], m[2][2], 0.0],
            ],
        }
    }

    /// Pack the per-hue HSL bands into the secondary GPU uniform (each band normalized to [-1,1]
    /// as `vec4(hue, sat, lum, _pad)`). Separate from `ParamsUniform` so the guarded std140
    /// `wb_gain` layout is never touched.
    fn to_fx(&self) -> FxUniform {
        let mut hsl = [[0.0f32; 4]; HSL_BANDS];
        for (i, b) in self.hsl.iter().enumerate() {
            hsl[i] = [
                (b.h / 100.0).clamp(-1.0, 1.0),
                (b.s / 100.0).clamp(-1.0, 1.0),
                (b.l / 100.0).clamp(-1.0, 1.0),
                0.0,
            ];
        }
        FxUniform { hsl }
    }

    /// Pack the enabled masks' scalar deltas into the develop storage buffer. Geometry/components do
    /// NOT go here — they drive the mask pre-pass which writes the alpha texture-array. Enabled masks
    /// are packed densely (layers 0..count) in stacking order; the pre-pass must use the same order.
    fn to_mask_buffer(&self) -> MaskBufferUniform {
        let mut masks = [bytemuck::Zeroable::zeroed(); MASK_CAP];
        let mut count = 0usize;
        for m in self.masks.iter().filter(|m| m.enabled).take(MASK_CAP) {
            let a = &m.adjust;
            masks[count] = MaskParamsUniform {
                wb_gain: wb_gain_from(a.temp, a.tint),
                exposure: a.exposure,
                contrast: (a.contrast / 100.0).clamp(-1.0, 1.0),
                saturation: (a.saturation / 100.0).clamp(-1.0, 1.0),
                highlights: (a.highlights / 100.0).clamp(-1.0, 1.0),
                shadows: (a.shadows / 100.0).clamp(-1.0, 1.0),
                blacks: (a.blacks / 100.0).clamp(-1.0, 1.0),
                whites: (a.whites / 100.0).clamp(-1.0, 1.0),
                opacity: m.opacity.clamp(0.0, 1.0),
                enabled: 1.0,
            };
            count += 1;
        }
        MaskBufferUniform {
            count: count as u32,
            _pad: [0; 3],
            masks,
        }
    }

    /// Pack the base tone operator's GPU uniform: the LUT log-exposure domain + hue-protect strength.
    /// `tone_amount` itself is baked into the LUT (CPU-side via `base_curve_lut`), not passed here.
    fn to_tone_op(&self) -> ToneOpUniform {
        ToneOpUniform {
            // params.w: display-referred bypass flag (1.0 ⇒ shader skips the scene-referred base
            // tone operator, passing the already-developed image through). 0.0 for RAW (unchanged).
            params: [
                BASE_U_MIN,
                BASE_U_MAX,
                BASE_HUE_PROTECT,
                if self.display_referred { 1.0 } else { 0.0 },
            ],
        }
    }

    /// Build this image's base-curve LUT (`tone_amount` normalized to [0,1]).
    fn base_curve_lut(&self) -> Vec<f32> {
        build_base_curve_lut((self.tone_amount / 100.0).clamp(0.0, 1.0))
    }

    /// Pack the crop + straighten geometry for the GPU. `src_aspect` = source W/H; `out_aspect` =
    /// render-target W/H (== src_aspect for preview; == crop pixel aspect for true-dims export, so
    /// the cropped content fills the target with no letterbox).
    fn to_geom(&self, src_aspect: f32, out_aspect: f32) -> GeomUniform {
        let c = &self.crop;
        let theta = c.angle.to_radians();
        GeomUniform {
            crop: [c.cx, c.cy, c.hw, c.hh],
            rot: [
                theta.cos(),
                theta.sin(),
                geom_autozoom(c, src_aspect),
                if c.is_identity() { 0.0 } else { 1.0 },
            ],
            // aspect.x = DISPLAYED-frame aspect (rot90 swaps W↔H); aspect.w = quarter-turn count.
            // aspect.z = lens-correction active flag (independent of crop/straighten in rot.w). At
            // defaults it is 0.0 so `lens_sample` in the shader falls through to a plain bilinear
            // fetch, keeping the render byte-identical to a pre-lens build.
            aspect: [
                c.eff_aspect(src_aspect),
                out_aspect,
                if self.lens_is_active() { 1.0 } else { 0.0 },
                c.rot_q() as f32,
            ],
            lens: self.lens_coeffs(),
        }
    }

    /// True when any lens distortion / chromatic-aberration control is engaged.
    fn lens_is_active(&self) -> bool {
        self.dist_k1 != 0.0 || self.dist_k2 != 0.0 || self.ca_red != 0.0 || self.ca_blue != 0.0
    }

    /// Physical lens coefficients for the GPU: `(k1, k2, ca_red, ca_blue)`. UI sliders are -100..100;
    /// the `LENS_*` scales convert them to sane radial magnitudes (the single tuning knob for the
    /// look). `k1/k2` scale the radial displacement `r2`/`r2²`; `ca_*` are per-channel radial scales.
    fn lens_coeffs(&self) -> [f32; 4] {
        [
            (self.dist_k1 / 100.0).clamp(-1.0, 1.0) * LENS_K1_SCALE,
            (self.dist_k2 / 100.0).clamp(-1.0, 1.0) * LENS_K2_SCALE,
            (self.ca_red / 100.0).clamp(-1.0, 1.0) * LENS_CA_SCALE,
            (self.ca_blue / 100.0).clamp(-1.0, 1.0) * LENS_CA_SCALE,
        ]
    }

    /// Pack the Color-balance-RGB grading for the GPU (`@binding(14)`). Ships the verified ProPhoto⇄
    /// grading-RGB matrices (so the shader has no magic constants) + the user controls. `params.z` is
    /// the active flag so the shader skips the whole stage at defaults (keeping the render
    /// byte-identical to a pre-binding-14 build).
    fn to_cbrgb(&self) -> CbRgbUniform {
        let c = &self.cb_rgb;
        let (fwd, inv) = grading_matrices(); // row-major f64
                                             // Pack row-major M3 → std140 mat3 columns (matching `mat3x3 * v` in WGSL), as `to_wb_uniform`.
        let cols = |m: &M3| {
            [
                [m[0][0] as f32, m[1][0] as f32, m[2][0] as f32, 0.0],
                [m[0][1] as f32, m[1][1] as f32, m[2][1] as f32, 0.0],
                [m[0][2] as f32, m[1][2] as f32, m[2][2] as f32, 0.0],
            ]
        };
        let v3 = |a: [f32; 3]| [a[0], a[1], a[2], 0.0];
        CbRgbUniform {
            fwd: cols(&fwd),
            inv: cols(&inv),
            global: v3(c.global),
            shadows: v3(c.shadows),
            midtones: v3(c.midtones),
            highlights: v3(c.highlights),
            params: [
                c.contrast.clamp(-1.0, 1.0),
                c.saturation.clamp(-1.0, 1.0),
                if c.is_identity() { 0.0 } else { 1.0 },
                0.0,
            ],
        }
    }

    /// Pack the channel mixer for the GPU (`@binding(15)`). Rows go straight in (`.w` = 0.0); the
    /// shader does a per-output `dot(row.xyz, color)`, so no std140 column transpose is needed.
    /// `params.x` is the active flag so the shader skips the mix at the identity default (keeping the
    /// render byte-identical to a pre-binding-15 build).
    fn to_chanmix(&self) -> ChanMixUniform {
        let c = &self.channel_mix;
        let row = |a: [f32; 3]| [a[0], a[1], a[2], 0.0];
        ChanMixUniform {
            r: row(c.red),
            g: row(c.green),
            b: row(c.blue),
            params: [if c.is_identity() { 0.0 } else { 1.0 }, 0.0, 0.0, 0.0],
        }
    }
}

/// GPU packing for the transient viewport descriptor.
pub trait GpuViewParamsExt {
    fn to_uniform(&self) -> ViewUniform;
}
impl GpuViewParamsExt for ViewParams {
    fn to_uniform(&self) -> ViewUniform {
        ViewUniform {
            rect: [self.origin[0], self.origin[1], self.size[0], self.size[1]],
            flags: [
                if self.active { 1.0 } else { 0.0 },
                self.overlay_layer as f32,
                self.overlay_strength,
                if self.crop_preview { 1.0 } else { 0.0 },
            ],
            color: [
                self.overlay_color[0],
                self.overlay_color[1],
                self.overlay_color[2],
                0.0,
            ],
        }
    }
}
