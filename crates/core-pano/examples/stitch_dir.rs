//! Register a directory of images (PNG/JPEG) and print diagnostics.
//!
//! P1 only performs registration (no compositing yet), so this example loads plain 8-bit images via
//! the `image` crate — camera-native f32 decoding isn't needed to exercise features → matching →
//! RANSAC → bundle adjustment. Run with:
//!
//! ```text
//! cargo run -p core-pano --example stitch_dir -- /path/to/images
//! ```

use std::path::PathBuf;
use std::time::Instant;

use nalgebra::Rotation3;

use core_pano::{compose, register, Frame, Phase, Projection, StitchOptions};

fn main() {
    let dir = match std::env::args().nth(1) {
        Some(d) => PathBuf::from(d),
        None => {
            eprintln!("usage: stitch_dir <directory-of-images>");
            std::process::exit(2);
        }
    };

    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()),
                    Some(ref e) if e == "png" || e == "jpg" || e == "jpeg"
                )
            })
            .collect(),
        Err(e) => {
            eprintln!("cannot read {}: {e}", dir.display());
            std::process::exit(1);
        }
    };
    paths.sort();

    if paths.len() < 2 {
        eprintln!("need at least 2 images, found {}", paths.len());
        std::process::exit(1);
    }

    let mut frames = Vec::with_capacity(paths.len());
    for p in &paths {
        match image::open(p) {
            Ok(img) => {
                let rgb = img.to_rgb8();
                let (w, h) = (rgb.width() as usize, rgb.height() as usize);
                // 8-bit -> linear-ish f32 in [0,1]; registration only needs contrast, not colorimetry.
                let data: Vec<f32> = rgb.into_raw().iter().map(|&b| b as f32 / 255.0).collect();
                frames.push(Frame {
                    width: w,
                    height: h,
                    rgb: data,
                    focal_seed_px: None,
                });
                println!("loaded {} ({w}x{h})", p.display());
            }
            Err(e) => {
                eprintln!("skip {}: {e}", p.display());
            }
        }
    }

    let progress = |phase: Phase| println!("== phase: {phase:?}");
    let reg = match register(&frames, &progress) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("registration failed: {e}");
            std::process::exit(1);
        }
    };

    println!("\n--- verified pairs ---");
    for pm in &reg.pairwise {
        println!("  ({}, {}): {} inliers", pm.i, pm.j, pm.n_inliers);
    }

    println!("\n--- cameras ---");
    println!(
        "used = {:?}, reference = {}",
        reg.used_indices, reg.reference_index
    );
    for (i, cam) in reg.cameras.iter().enumerate() {
        match cam {
            Some(c) => {
                let aa = Rotation3::from_matrix_unchecked(c.rotation).scaled_axis();
                let deg = aa.norm().to_degrees();
                let axis = if aa.norm() > 1e-9 { aa / aa.norm() } else { aa };
                println!(
                    "  frame {i}: focal = {:.1} px, rotation = {:.2}° about [{:.2}, {:.2}, {:.2}]",
                    c.focal_px, deg, axis.x, axis.y, axis.z
                );
            }
            None => println!("  frame {i}: (not in panorama)"),
        }
    }

    println!(
        "\nBA: rms = {:.6} (radians-ish), converged = {}",
        reg.ba_final_rms, reg.ba_converged
    );

    // --- Full compositing (P2), reusing the registration above via `compose`. ---
    let opt = StitchOptions {
        projection: Projection::Auto,
        boundary_warp: 0.0,
        auto_crop: true,
        max_long_side: 8000,
        preview: false,
    };
    let t0 = Instant::now();
    let res = match compose(&frames, &reg, &opt, &progress) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("compositing failed: {e}");
            std::process::exit(1);
        }
    };
    let dt = t0.elapsed();

    println!("\n--- stitch ---");
    println!(
        "output {}x{} px, projection = {:?}, capped = {}, {} frames used, composited in {:.2?}",
        res.width,
        res.height,
        res.projection_used,
        res.capped,
        res.used_indices.len(),
        dt
    );

    // Quick display encode: camera-native linear is dim, so exposure up ×4, clamp, then sRGB-ish
    // gamma (x^(1/2.2)). Purely for eyeballing the result — not a colour-managed export.
    let mut buf = vec![0u8; res.width * res.height * 3];
    for i in 0..res.width * res.height {
        for c in 0..3 {
            let v = (res.rgb[i * 3 + c] * 4.0).clamp(0.0, 1.0);
            buf[i * 3 + c] = (v.powf(1.0 / 2.2) * 255.0).round() as u8;
        }
    }
    let out = "/tmp/pano_out.png";
    match image::save_buffer(
        out,
        &buf,
        res.width as u32,
        res.height as u32,
        image::ColorType::Rgb8,
    ) {
        Ok(()) => println!("wrote {out}"),
        Err(e) => eprintln!("failed to write {out}: {e}"),
    }
}
