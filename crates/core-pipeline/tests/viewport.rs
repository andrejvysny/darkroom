//! Viewport (`render_view`) + mask-overlay golden vectors. Validate the geometry split (crop-local
//! view → source) and the packed-layer red overlay. Each is GPU-gated (skips without a Metal adapter).
//!
//! The zoom/crop+straighten cases use a self-consistent reference: a zoomed sub-window must equal the
//! matching sub-region of the SAME develop rendered over the whole (crop-local) frame — both go
//! through identical develop math, so they differ only by the view rect they select.

use core_pipeline::{
    ComponentKind, Crop, DevelopParams, DevelopPipeline, GpuContext, LocalAdjust, Mask,
    MaskComponent, MaskOp, ViewParams,
};
use core_raw::LinearImage;

fn gpu() -> Option<GpuContext> {
    match GpuContext::new() {
        Ok(c) => Some(c),
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            None
        }
    }
}

/// Smooth gradient with all channels varying so the develop output is non-flat and comparable.
fn ramp(w: u32, h: u32) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = x as f32 / (w - 1).max(1) as f32;
            let g = y as f32 / (h - 1).max(1) as f32;
            let b = 0.25 + 0.5 * ((x + y) as f32 / (w + h) as f32);
            data.extend_from_slice(&[r * 0.9 + 0.05, g * 0.9 + 0.05, b]);
        }
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
}

fn px(buf: &[u8], w: u32, x: u32, y: u32, ch: usize) -> i32 {
    buf[((y * w + x) * 4) as usize + ch] as i32
}

/// View covering the whole crop-local frame into an `out_w × out_h` target (overlay off).
fn view_full(out_w: u32, out_h: u32) -> ViewParams {
    ViewParams {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
        out_w,
        out_h,
        active: true,
        overlay_layer: -1,
        overlay_color: [0.85, 0.10, 0.10],
        overlay_strength: 0.5,
    }
}

/// Max per-channel absolute difference between two RGBA8 buffers (must be equal length).
fn max_diff(a: &[u8], b: &[u8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (*x as i32 - *y as i32).abs())
        .max()
        .unwrap_or(0)
}

// 1. Identity: an active full-window view at source size is byte-identical to the legacy `render()`.
#[test]
fn identity_view_matches_legacy_render() {
    let Some(ctx) = gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let img = ramp(64, 48);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let p = DevelopParams {
        exposure: 0.4,
        contrast: 20.0,
        ..Default::default()
    };
    let legacy = pipe.render(&ctx, &prep, &p).unwrap();
    let active = pipe
        .render_view(&ctx, &prep, &p, &view_full(64, 48))
        .unwrap();
    assert_eq!(legacy.len(), active.len());
    assert_eq!(
        max_diff(&legacy, &active),
        0,
        "active full view must be byte-identical to legacy render"
    );
}

// 2. Zoom (no crop): the center-half view at 320×240 (1:1 with the source there) must match the
//    center 320×240 region of the full 640×480 render within rounding.
#[test]
fn zoom_window_matches_full_region() {
    let Some(ctx) = gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let img = ramp(640, 480);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let p = DevelopParams::default();
    let full = pipe.render(&ctx, &prep, &p).unwrap();

    let mut v = view_full(320, 240);
    v.origin = [0.25, 0.25];
    v.size = [0.5, 0.5];
    let zoom = pipe.render_view(&ctx, &prep, &p, &v).unwrap();

    // zoom px (x,y) samples the same source uv as full px (x+160, y+120).
    let mut worst = 0;
    for y in (0..240).step_by(37) {
        for x in (0..320).step_by(41) {
            for ch in 0..3 {
                let z = px(&zoom, 320, x, y, ch);
                let f = px(&full, 640, x + 160, y + 120, ch);
                worst = worst.max((z - f).abs());
            }
        }
    }
    assert!(worst <= 2, "zoom vs full-region diff {worst} > 2 LSB");
}

// 3. Fit/letterbox: a square crop shown in a 300×200 (1.5:1) frame letterboxes to ~50px side bars.
#[test]
fn fit_letterbox_side_bars() {
    let Some(ctx) = gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let img = ramp(400, 300); // src aspect 4:3
    let prep = pipe.prepare(&ctx, &img).unwrap();
    // Square crop: cropAspect = (hw/hh)*srcAspect = (.375/.5)*(4/3) = 1.0.
    let p = DevelopParams {
        crop: Crop {
            cx: 0.5,
            cy: 0.5,
            hw: 0.375,
            hh: 0.5,
            angle: 0.0,
        },
        ..Default::default()
    };
    // Fit the square crop into 300×200: size.x = displayAspect/cropAspect = 1.5, centered (origin -0.25).
    let mut v = view_full(300, 200);
    v.origin = [-0.25, 0.0];
    v.size = [1.5, 1.0];
    let out = pipe.render_view(&ctx, &prep, &p, &v).unwrap();

    let col_is_black = |c: u32| (0..200).all(|y| (0..3).all(|ch| px(&out, 300, c, y, ch) == 0));
    let col_has_content = |c: u32| (0..200).any(|y| (0..3).any(|ch| px(&out, 300, c, y, ch) > 8));
    assert!(
        col_is_black(20),
        "left bar (col 20) must be letterbox black"
    );
    assert!(
        col_is_black(280),
        "right bar (col 280) must be letterbox black"
    );
    assert!(col_has_content(150), "center (col 150) must show the crop");
}

// 4. Crop + straighten + zoom: the zoomed sub-window must equal the matching sub-region of the full
//    crop-local render (same crop+rotation params), proving view composition is correct.
#[test]
fn crop_straighten_zoom_window_consistent() {
    let Some(ctx) = gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let img = ramp(640, 480);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let p = DevelopParams {
        crop: Crop {
            cx: 0.52,
            cy: 0.48,
            hw: 0.32,
            hh: 0.28,
            angle: 12.0,
        },
        ..Default::default()
    };
    // Whole crop into 640×420; centered half into 320×210 (exact halving → aligned pixel centers).
    let full = pipe
        .render_view(&ctx, &prep, &p, &view_full(640, 420))
        .unwrap();
    let mut v = view_full(320, 210);
    v.origin = [0.25, 0.25];
    v.size = [0.5, 0.5];
    let zoom = pipe.render_view(&ctx, &prep, &p, &v).unwrap();

    let mut worst = 0;
    for y in (0..210).step_by(29) {
        for x in (0..320).step_by(31) {
            for ch in 0..3 {
                let z = px(&zoom, 320, x, y, ch);
                let f = px(&full, 640, x + 160, y + 105, ch);
                worst = worst.max((z - f).abs());
            }
        }
    }
    assert!(
        worst <= 3,
        "crop+straighten zoom sub-window diff {worst} > 3 LSB"
    );
}

// 5. Mask/overlay edge: a DISABLED mask precedes the selected (enabled) mask, so the selected mask
//    packs to GPU layer 0. Tinting layer 0 must red-tint the selected mask's coverage (not the
//    disabled mask's), and leave uncovered pixels untouched. Odd output dims exercise row padding.
#[test]
fn overlay_tints_packed_selected_layer() {
    let Some(ctx) = gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let img = ramp(256, 256);
    let prep = pipe.prepare(&ctx, &img).unwrap();

    let disabled_full = Mask {
        name: "disabled".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::Radial {
                center: [0.5, 0.5],
                radius: [10.0, 10.0], // covers everything — but it is DISABLED, so unpacked
                angle: 0.0,
                feather: 0.0,
            },
            op: MaskOp::Add,
            invert: false,
            feather: false,
        }],
        adjust: LocalAdjust::default(),
        opacity: 1.0,
        enabled: false,
    };
    let selected_left = Mask {
        name: "selected".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::Radial {
                center: [0.25, 0.5],
                radius: [0.18, 0.5],
                angle: 0.0,
                feather: 0.0,
            },
            op: MaskOp::Add,
            invert: false,
            feather: false,
        }],
        adjust: LocalAdjust::default(),
        opacity: 1.0,
        enabled: true,
    };
    let p = DevelopParams {
        masks: vec![disabled_full, selected_left],
        ..Default::default()
    };

    // Selected mask is the only enabled one → packed layer 0.
    let mut on = view_full(256, 256);
    on.overlay_layer = 0;
    let off = view_full(256, 256); // overlay off
    let tinted = pipe.render_view(&ctx, &prep, &p, &on).unwrap();
    let plain = pipe.render_view(&ctx, &prep, &p, &off).unwrap();

    // Inside the selected (left) mask: red channel pushed up, green/blue pulled toward red tint.
    let cx_in = 64; // x≈0.25*256
    let cy = 128;
    let r_in_t = px(&tinted, 256, cx_in, cy, 0);
    let r_in_p = px(&plain, 256, cx_in, cy, 0);
    let g_in_t = px(&tinted, 256, cx_in, cy, 1);
    let g_in_p = px(&plain, 256, cx_in, cy, 1);
    assert!(
        r_in_t > r_in_p + 10 || g_in_t + 10 < g_in_p,
        "covered pixel must show the red overlay (r {r_in_p}->{r_in_t}, g {g_in_p}->{g_in_t})"
    );

    // Far right (outside the selected mask): unchanged. If the DISABLED full mask had been tinted,
    // this would change — proving packed-layer resolution.
    let cx_out = 224; // x≈0.875
    for ch in 0..3 {
        let t = px(&tinted, 256, cx_out, cy, ch);
        let pl = px(&plain, 256, cx_out, cy, ch);
        assert!(
            (t - pl).abs() <= 2,
            "uncovered pixel ch{ch} must be untouched ({pl}->{t})"
        );
    }

    // Odd output dims: padded row pitch + exact returned length.
    let odd = pipe
        .render_view(&ctx, &prep, &p, &view_full(641, 359))
        .unwrap();
    assert_eq!(odd.len(), (641 * 359 * 4) as usize, "odd-dim length");
}
