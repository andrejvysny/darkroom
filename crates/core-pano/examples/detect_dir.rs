//! Detect panorama groups in a directory of images (PNG/JPEG) and print diagnostics.
//!
//! Loads every image in filename order, downscales each so its long side is at most `--edge` px
//! (default 512, mirroring the in-app 512 px thumb the detector really runs on), then runs
//! [`detect_groups`] and prints every verified edge (conf / overlap / shift / class) followed by the
//! detected groups. `--stitch` renders each group to `/tmp/detect_<n>.png`.
//!
//! ```text
//! cargo run -p core-pano --example detect_dir -- /path/to/images [--edge 512] [--all-pairs] [--stitch]
//! ```
//!
//! `--all-pairs` forces all-pairs verification regardless of set size (otherwise `detect_groups`
//! chooses all-pairs vs. sliding window from `DetectOptions`).

use std::path::PathBuf;

use core_pano::{detect_groups, stitch, DetectOptions, Frame, Phase, Projection, StitchOptions};

fn main() {
    // --- Argument parsing: <dir> [--edge N] [--all-pairs] [--stitch] ---
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut dir: Option<PathBuf> = None;
    let mut edge: u32 = 512;
    let mut all_pairs = false;
    let mut do_stitch = false;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--edge" => {
                edge = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|&e| e > 0)
                    .unwrap_or(512);
            }
            "--all-pairs" => all_pairs = true,
            "--stitch" => do_stitch = true,
            other => {
                if dir.is_none() {
                    dir = Some(PathBuf::from(other));
                }
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: detect_dir <directory> [--edge N] [--all-pairs] [--stitch]");
        std::process::exit(2);
    };

    // --- Collect image paths in filename order ---
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

    // --- Load + downscale so the long side <= edge ---
    let mut frames: Vec<Frame> = Vec::with_capacity(paths.len());
    let mut names: Vec<String> = Vec::with_capacity(paths.len());
    for p in &paths {
        match image::open(p) {
            Ok(img) => {
                let (w0, h0) = (img.width(), img.height());
                let long = w0.max(h0);
                let img = if long > edge {
                    let s = edge as f32 / long as f32;
                    let nw = ((w0 as f32 * s).round() as u32).max(1);
                    let nh = ((h0 as f32 * s).round() as u32).max(1);
                    img.resize(nw, nh, image::imageops::FilterType::Triangle)
                } else {
                    img
                };
                let rgb = img.to_rgb8();
                let (w, h) = (rgb.width() as usize, rgb.height() as usize);
                let data: Vec<f32> = rgb.into_raw().iter().map(|&b| b as f32 / 255.0).collect();
                let name = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();
                println!("loaded {name} ({w}x{h})");
                frames.push(Frame {
                    width: w,
                    height: h,
                    rgb: data,
                    focal_seed_px: None,
                });
                names.push(name);
            }
            Err(e) => eprintln!("skip {}: {e}", p.display()),
        }
    }

    if frames.len() < 2 {
        eprintln!("fewer than 2 images decoded");
        std::process::exit(1);
    }

    // --- Detect ---
    let opts = DetectOptions::default();
    let n = frames.len();
    let pairs_hint = if all_pairs {
        Some(
            (0..n)
                .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let report = detect_groups(&frames, pairs_hint, &opts, &|| false);

    println!("\n--- verified edges ---");
    for e in &report.edges {
        println!(
            "{} {} conf={:.3} overlap={:.3} shift={:.3} class={:?}",
            names[e.i], names[e.j], e.conf, e.overlap, e.shift, e.class
        );
    }

    println!("\n--- groups ({}) ---", report.groups.len());
    for (gi, g) in report.groups.iter().enumerate() {
        let files: Vec<&str> = g.members.iter().map(|&m| names[m].as_str()).collect();
        println!(
            "group {gi}: confidence={:.3} members={:?} [{}]",
            g.confidence,
            g.members,
            files.join(", ")
        );
    }

    // --- Optional stitch of each detected group ---
    if do_stitch {
        let opt = StitchOptions {
            projection: Projection::Auto,
            boundary_warp: 0.0,
            auto_crop: true,
            max_long_side: 4000,
            preview: false,
        };
        let progress = |_phase: Phase| {};
        for (gi, g) in report.groups.iter().enumerate() {
            // stitch() takes a contiguous slice; build a sub-Frame vector for the group (Frame is not
            // Clone, so copy fields explicitly).
            let sub: Vec<Frame> = g
                .members
                .iter()
                .map(|&m| Frame {
                    width: frames[m].width,
                    height: frames[m].height,
                    rgb: frames[m].rgb.clone(),
                    focal_seed_px: frames[m].focal_seed_px,
                })
                .collect();
            match stitch(&sub, &opt, &progress) {
                Ok(res) => {
                    let mut buf = vec![0u8; res.width * res.height * 3];
                    for i in 0..res.width * res.height {
                        for c in 0..3 {
                            let v = (res.rgb[i * 3 + c] * 4.0).clamp(0.0, 1.0);
                            buf[i * 3 + c] = (v.powf(1.0 / 2.2) * 255.0).round() as u8;
                        }
                    }
                    let out = format!("/tmp/detect_{gi}.png");
                    match image::save_buffer(
                        &out,
                        &buf,
                        res.width as u32,
                        res.height as u32,
                        image::ColorType::Rgb8,
                    ) {
                        Ok(()) => println!("group {gi}: wrote {out} ({}x{})", res.width, res.height),
                        Err(e) => eprintln!("group {gi}: failed to write {out}: {e}"),
                    }
                }
                Err(e) => eprintln!("group {gi}: stitch failed: {e}"),
            }
        }
    }
}
