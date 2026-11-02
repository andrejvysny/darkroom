use super::*;
// --- Crop + straighten geometry remap ------------------------------------------------------------
// The develop shader maps each output fragment's crop-local UV `u` ∈ [0,1] back to a source UV, so
// crop/straighten is one inverse-mapped sampling stage (Codex review). Rotation happens in
// PIXEL/aspect-correct space (rotating normalized UV would shear a non-square image). `src_aspect` =
// W/H of the source. These helpers mirror the WGSL exactly so the math is unit-testable on the CPU.

/// Auto-zoom factor (≥1) that keeps the rotated crop rectangle fully inside the source [0,1]², so a
/// straighten never samples past the image edge. Closed form from the Codex review.
pub fn geom_autozoom(c: &Crop, src_aspect: f32) -> f32 {
    let eff = c.eff_aspect(src_aspect);
    let theta = c.angle.to_radians();
    let (ct, st) = (theta.cos().abs(), theta.sin().abs());
    // Max normalized displacement of the rotated footprint (hh·H/W = hh/aspect; hw·W/H = hw·aspect).
    let ax = ct * c.hw + st * c.hh / eff;
    let ay = st * c.hw * eff + ct * c.hh;
    let mx = c.cx.min(1.0 - c.cx).max(1e-6);
    let my = c.cy.min(1.0 - c.cy).max(1e-6);
    1.0_f32.max(ax / mx).max(ay / my)
}

/// Permute a DISPLAYED-frame UV back to the un-rotated source UV for `q` clockwise quarter-turns.
/// q0: (x,y); q1(90° CW): (y, 1−x); q2(180°): (1−x, 1−y); q3(270° CW): (1−y, x).
pub fn rot90_uv(q: i32, e: [f32; 2]) -> [f32; 2] {
    match q.rem_euclid(4) {
        1 => [e[1], 1.0 - e[0]],
        2 => [1.0 - e[0], 1.0 - e[1]],
        3 => [1.0 - e[1], e[0]],
        _ => e,
    }
}

/// Map a crop-local output UV `u` ∈ [0,1]² to a source UV: inverse straighten (about the crop
/// center, pixel-space) + auto-zoom in the DISPLAYED frame, then the 90° permutation back to the
/// source. Mirrors `geom_resolve` / `crop_to_source` in develop.wgsl. `z` = `geom_autozoom`.
pub fn geom_src_uv(c: &Crop, src_aspect: f32, z: f32, u: [f32; 2]) -> [f32; 2] {
    let eff = c.eff_aspect(src_aspect);
    let theta = c.angle.to_radians();
    let (ct, st) = (theta.cos(), theta.sin());
    let d = [(2.0 * u[0] - 1.0) * c.hw, (2.0 * u[1] - 1.0) * c.hh];
    let dpx = [d[0] * eff, d[1]];
    // Inverse rotation R(-θ) in pixel space.
    let r = [ct * dpx[0] + st * dpx[1], -st * dpx[0] + ct * dpx[1]];
    let disp = [r[0] / eff / z, r[1] / z];
    // Displayed-frame UV, then permute back to the source for the 90° rotation.
    rot90_uv(c.rot_q(), [c.cx + disp[0], c.cy + disp[1]])
}

/// Apply the radial lens-distortion remap to a source UV about the image center (0.5, 0.5), in
/// aspect-correct space. `lens = (k1, k2, _, _)` (see `DevelopParams::lens_coeffs`). Mirrors
/// `lens_remap` in develop.wgsl exactly so it is unit-testable on the CPU. Center maps to itself;
/// displacement grows radially. `src_aspect` = source W/H.
pub fn geom_lens_uv(lens: [f32; 4], src_aspect: f32, uv: [f32; 2]) -> [f32; 2] {
    let d = [(uv[0] - 0.5) * src_aspect, uv[1] - 0.5];
    let r2 = d[0] * d[0] + d[1] * d[1];
    let s = 1.0 + lens[0] * r2 + lens[1] * r2 * r2;
    [0.5 + d[0] * s / src_aspect, 0.5 + d[1] * s]
}
#[cfg(test)]
mod geom_tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2], eps: f32) -> bool {
        (a[0] - b[0]).abs() < eps && (a[1] - b[1]).abs() < eps
    }

    #[test]
    fn lens_active_flag_and_geom_packing() {
        let d = DevelopParams::default();
        assert!(!d.lens_is_active());
        assert_eq!(d.to_geom(1.5, 1.5).aspect[2], 0.0);
        assert_eq!(d.lens_coeffs(), [0.0; 4]);

        let warped = DevelopParams {
            dist_k1: 100.0,
            ..Default::default()
        };
        assert!(warped.lens_is_active());
        assert_eq!(warped.to_geom(1.5, 1.5).aspect[2], 1.0);
        assert_eq!(warped.lens_coeffs()[0], LENS_K1_SCALE);
    }

    #[test]
    fn lens_remap_center_fixed_and_radially_monotone() {
        let lens = [LENS_K1_SCALE, 0.0, 0.0, 0.0]; // pincushion-ish k1 > 0
        let sa = 1.5;
        // Center maps to itself.
        assert!(close(geom_lens_uv(lens, sa, [0.5, 0.5]), [0.5, 0.5], 1e-6));
        // Displacement grows with radius (k1 > 0 pushes points outward).
        let near = geom_lens_uv(lens, sa, [0.6, 0.5]);
        let far = geom_lens_uv(lens, sa, [0.8, 0.5]);
        let dnear = near[0] - 0.5;
        let dfar = far[0] - 0.5;
        assert!(dnear > 0.1 && dfar > 0.3);
        assert!(
            dfar / 0.3 > dnear / 0.1,
            "outer point displaced proportionally more"
        );
    }

    #[test]
    fn identity_crop_maps_to_self() {
        let c = Crop::default();
        assert!(c.is_identity());
        for u in [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0], [0.25, 0.8]] {
            let src = geom_src_uv(&c, 1.5, geom_autozoom(&c, 1.5), u);
            assert!(
                close(src, u, 1e-6),
                "identity must map {u:?} -> self, got {src:?}"
            );
        }
    }

    #[test]
    fn crop_corners_map_to_rect() {
        let c = Crop {
            cx: 0.6,
            cy: 0.4,
            hw: 0.25,
            hh: 0.2,
            angle: 0.0,
            rot90: 0,
        };
        let a = 2.0;
        let z = 1.0;
        assert!(close(geom_src_uv(&c, a, z, [0.0, 0.0]), [0.35, 0.2], 1e-6));
        assert!(close(geom_src_uv(&c, a, z, [1.0, 1.0]), [0.85, 0.6], 1e-6));
        assert!(close(geom_src_uv(&c, a, z, [0.5, 0.5]), [0.6, 0.4], 1e-6));
    }

    #[test]
    fn rotation_is_pixel_space_on_nonsquare() {
        let c = Crop {
            angle: 90.0,
            ..Default::default()
        };
        let src = geom_src_uv(&c, 2.0, 1.0, [0.6, 0.5]);
        assert!(
            close(src, [0.5, 0.3], 1e-5),
            "90° non-square map wrong: {src:?}"
        );
    }

    #[test]
    fn export_rect_is_full_for_identity_and_tight_for_aspect() {
        assert_eq!(Crop::default().export_rect(3000, 2000), (0, 0, 3000, 2000));
        let c = Crop {
            cx: 0.5,
            cy: 0.5,
            hw: 0.5,
            hh: 0.5 / ((16.0 / 9.0) / 1.5),
            angle: 0.0,
            rot90: 0,
        };
        let (_x, y, w, h) = c.export_rect(3000, 2000);
        assert_eq!(w, 3000, "16:9 of 3:2 keeps full width");
        assert!(
            (w as f32 / h as f32 - 16.0 / 9.0).abs() < 0.01,
            "rect must be 16:9"
        );
        assert_eq!(y, (2000 - h) / 2, "crop must be vertically centered");
    }

    #[test]
    fn autozoom_keeps_corners_in_bounds() {
        let c = Crop {
            angle: 45.0,
            ..Default::default()
        };
        let a = 1.0;
        let z = geom_autozoom(&c, a);
        assert!(
            (z - 2.0_f32.sqrt()).abs() < 1e-3,
            "45° square autozoom ≈ √2, got {z}"
        );
        let mut touches = false;
        for u in [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]] {
            let s = geom_src_uv(&c, a, z, u);
            assert!(
                s[0] >= -1e-4 && s[0] <= 1.0 + 1e-4 && s[1] >= -1e-4 && s[1] <= 1.0 + 1e-4,
                "corner {u:?} -> {s:?} out of source bounds"
            );
            if s[0].min(1.0 - s[0]).min(s[1]).min(1.0 - s[1]) < 1e-3 {
                touches = true;
            }
        }
        assert!(
            touches,
            "at z_min at least one corner must touch the source edge"
        );
    }

    #[test]
    fn rot90_permutation_maps_corners() {
        // Displayed-frame corners permute to the un-rotated source per the CW quarter-turn table.
        assert!(close(rot90_uv(1, [1.0, 0.0]), [0.0, 0.0], 1e-6));
        assert!(close(rot90_uv(1, [0.0, 0.0]), [0.0, 1.0], 1e-6));
        assert!(close(rot90_uv(2, [0.2, 0.3]), [0.8, 0.7], 1e-6));
        assert!(close(rot90_uv(3, [1.0, 0.0]), [1.0, 1.0], 1e-6));
        assert!(close(
            rot90_uv(-1, [1.0, 0.0]),
            rot90_uv(3, [1.0, 0.0]),
            1e-6
        ));
    }

    #[test]
    fn rot90_identity_crop_rotates_whole_frame() {
        // 90° CW on a full-frame identity crop: the displayed frame IS the source rotated, so a
        // display point maps straight through the permutation (eff-aspect cancels at full extent).
        let c = Crop {
            rot90: 1,
            ..Default::default()
        };
        let a = 2.0; // landscape source; displayed frame is portrait
        assert!(!c.is_identity(), "a quarter-turn is not identity");
        assert!(close(geom_src_uv(&c, a, 1.0, [0.5, 0.5]), [0.5, 0.5], 1e-6));
        assert!(close(geom_src_uv(&c, a, 1.0, [1.0, 0.0]), [0.0, 0.0], 1e-6));
        assert!(close(geom_src_uv(&c, a, 1.0, [0.0, 0.0]), [0.0, 1.0], 1e-6));
    }

    #[test]
    fn display_referred_toggles_tone_op_bypass_flag() {
        // RAW default: scene-referred operator active (w == 0) — keeps every golden byte-identical.
        let raw = DevelopParams::default();
        assert_eq!(raw.to_tone_op().params[3], 0.0);
        // Display-referred (JPEG/PNG): shader bypasses the base tone operator (w == 1).
        let disp = DevelopParams {
            display_referred: true,
            ..Default::default()
        };
        assert_eq!(disp.to_tone_op().params[3], 1.0);
        // The first three slots (log-exposure domain + hue protect) are unchanged either way.
        assert_eq!(
            &raw.to_tone_op().params[0..3],
            &disp.to_tone_op().params[0..3]
        );
    }
}
