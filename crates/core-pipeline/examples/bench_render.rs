//! Develop-pipeline benchmark: how long an edit takes, and how the viewport (display-res) render
//! compares to the legacy full-resolution render + JPEG/PNG encode. Run (always `--release`):
//!
//!   cargo run --release -p core-pipeline --example bench_render [FILE.CR3]
//!
//! For each config it times, separately: full-res `render()` + readback, viewport
//! `render_view()` + readback at a ~2560px display target (full-res source, small output — the
//! RapidRAW win), and JPEG/PNG encode of the full-res frame. GPU-gated (skips without an adapter).
//!
//! Methodology note: warmup absorbs first-use shader/pipeline compilation; medians (+p95 for the hot
//! render) over N reuse one prepared image per resolution. This is a directional A/B, not a
//! statistical microbenchmark — for CI rigor switch to `criterion` (intentionally omitted, no dep).
//! NB: masks are recomputed every render today (no cache yet); the mask rows show that cost, which
//! the planned mask-layer cache (A5) removes for pan/zoom/scalar edits.

use core_pipeline::{
    rgba8_to_jpeg, rgba8_to_png, ComponentKind, DevelopParams, DevelopPipeline, GpuContext,
    LocalAdjust, Mask, MaskComponent, MaskOp, ViewParams,
};
use core_raw::{develop_linear, source_from_path, LinearImage};
use std::path::PathBuf;
use std::time::Instant;

const WARMUP: usize = 2;
const ITERS: usize = 9;
const DISPLAY_EDGE: u32 = 2560;
const JPEG_Q: u8 = 92;

fn first_cr3() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026/2026-06-06")
        .canonicalize()
        .ok()?;
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v.into_iter().next()
}

/// (median ms, p95 ms) over ITERS timed runs after WARMUP discarded runs.
fn time_n(mut f: impl FnMut()) -> (f64, f64) {
    for _ in 0..WARMUP {
        f();
    }
    let mut ms: Vec<f64> = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        f();
        ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = ms[((ITERS as f64 - 1.0) * 0.95).round() as usize];
    (ms[ITERS / 2], p95)
}

fn radial(adjust: LocalAdjust, cx: f32) -> Mask {
    Mask {
        name: "r".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::Radial {
                center: [cx, 0.5],
                radius: [0.3, 0.4],
                angle: 0.0,
                feather: 0.3,
            },
            op: MaskOp::Add,
            invert: false,
            feather: false,
        }],
        adjust,
        opacity: 1.0,
        enabled: true,
    }
}

fn n_masks(n: usize) -> Vec<Mask> {
    (0..n)
        .map(|i| {
            radial(
                LocalAdjust {
                    exposure: 0.5,
                    ..Default::default()
                },
                0.2 + 0.6 * (i as f32 / n.max(1) as f32),
            )
        })
        .collect()
}

fn configs() -> Vec<(&'static str, DevelopParams)> {
    let global = DevelopParams {
        exposure: 0.8,
        contrast: 30.0,
        saturation: 25.0,
        temp: 18.0,
        shadows: 30.0,
        ..Default::default()
    };
    vec![
        ("no-edit", DevelopParams::default()),
        ("global", global),
        (
            "1 mask",
            DevelopParams {
                masks: n_masks(1),
                ..Default::default()
            },
        ),
        (
            "3 masks",
            DevelopParams {
                masks: n_masks(3),
                ..Default::default()
            },
        ),
        (
            "8 masks",
            DevelopParams {
                masks: n_masks(8),
                ..Default::default()
            },
        ),
    ]
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(first_cr3)
        .expect("no CR3 file (pass one as an argument)");
    println!("file: {}", path.display());

    let src = source_from_path(&path)?;
    let full: LinearImage = develop_linear(&src)?;
    let (fw, fh) = (full.width, full.height);
    let scale = DISPLAY_EDGE as f32 / fw.max(fh) as f32;
    let (dw, dh) = (
        ((fw as f32 * scale).round() as u32).max(1),
        ((fh as f32 * scale).round() as u32).max(1),
    );
    println!("full-res {fw}x{fh}  |  display viewport {dw}x{dh}  (DISPLAY_EDGE={DISPLAY_EDGE})\n");

    let Ok(ctx) = GpuContext::new() else {
        eprintln!("no GPU adapter — skipping");
        return Ok(());
    };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe.prepare(&ctx, &full)?;

    let full_mb = (fw * fh * 4) as f64 / 1.0e6;
    let disp_mb = (dw * dh * 4) as f64 / 1.0e6;
    // 16 mask layers (R16Float) + source f32 are the bulk of VRAM; readback is per-render.
    let vram_mb = (fw * fh) as f64 * (16.0 * 2.0 + 4.0 * 4.0) / 1.0e6;
    println!(
        "readback/frame: full {full_mb:.1} MB  |  viewport {disp_mb:.1} MB   (est mask+src VRAM ~{vram_mb:.0} MB)\n"
    );

    println!(
        "{:<9} | {:>13} | {:>13} | {:>11} | {:>11}",
        "config", "full r+rb", "viewport r+rb", "jpeg f/vp", "png f/vp"
    );
    println!("{}", "-".repeat(70));

    for (label, params) in configs() {
        let (ff, fp) = time_n(|| {
            pipe.render(&ctx, &prep, &params).unwrap();
        });
        let view = ViewParams {
            origin: [0.0, 0.0],
            size: [1.0, 1.0],
            out_w: dw,
            out_h: dh,
            active: true,
            overlay_layer: -1,
            overlay_color: [0.85, 0.10, 0.10],
            overlay_strength: 0.5,
        };
        let (vf, vp) = time_n(|| {
            pipe.render_view(&ctx, &prep, &params, &view).unwrap();
        });
        // Encode timings at BOTH resolutions, so the JPEG/PNG cost of the viewport frame (what the
        // canvas-fallback path would actually encode/transport) is separated from the full-res cost
        // (Codex: the full-res encode conflates "smaller resolution" with "no JPEG").
        let rgba_full = pipe.render(&ctx, &prep, &params)?;
        let rgba_vp = pipe.render_view(&ctx, &prep, &params, &view)?;
        let (jf, _) = time_n(|| {
            rgba8_to_jpeg(&rgba_full, fw, fh, JPEG_Q).unwrap();
        });
        let (jv, _) = time_n(|| {
            rgba8_to_jpeg(&rgba_vp, dw, dh, JPEG_Q).unwrap();
        });
        let (pf, _) = time_n(|| {
            rgba8_to_png(&rgba_full, fw, fh).unwrap();
        });
        let (pv, _) = time_n(|| {
            rgba8_to_png(&rgba_vp, dw, dh).unwrap();
        });
        println!(
            "{label:<9} | {ff:>5.1}/{fp:<6.1} | {vf:>5.1}/{vp:<6.1} | {jf:>5.0}/{jv:<5.0} | {pf:>5.0}/{pv:<5.0}"
        );
    }

    // Cache-cold: jitter every mask's geometry each render so the full-res pre-pass runs every time
    // (the worst case — dragging a mask handle). The rows above are cache-WARM (the mask cache skips
    // the pre-pass), which is what pan / zoom / global + local scalar edits hit.
    let view = ViewParams {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
        out_w: dw,
        out_h: dh,
        active: true,
        overlay_layer: -1,
        overlay_color: [0.85, 0.10, 0.10],
        overlay_strength: 0.5,
    };
    println!("\ncache-cold viewport r+rb (mask geometry changes every render):");
    for n in [1usize, 3, 8] {
        let counter = std::cell::Cell::new(0u32);
        let (ms, _) = time_n(|| {
            let i = counter.get();
            counter.set(i.wrapping_add(1));
            let jitter = 0.0005 * (i % 64) as f32;
            let masks: Vec<Mask> = (0..n)
                .map(|k| {
                    radial(
                        LocalAdjust {
                            exposure: 0.5,
                            ..Default::default()
                        },
                        0.2 + 0.6 * (k as f32 / n as f32) + jitter,
                    )
                })
                .collect();
            let p = DevelopParams {
                masks,
                ..Default::default()
            };
            pipe.render_view(&ctx, &prep, &p, &view).unwrap();
        });
        println!("  {n} masks: {ms:.1} ms  (warm was ~no-edit speed)");
    }

    println!(
        "\nlegend: r+rb = GPU render + CPU readback ms (p50/p95). jpeg/png f/vp = encode ms at"
    );
    println!(
        "full {fw}x{fh} vs viewport {dw}x{dh}. The canvas fallback encodes/transports only the"
    );
    println!("viewport frame (no full-res JPEG); the native-surface path drops even the readback.");
    Ok(())
}
