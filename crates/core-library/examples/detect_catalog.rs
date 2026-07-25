//! Whole-library panorama detection over a real catalog, without the GUI — the headless mirror of
//! `src-tauri/src/pano_detect.rs::run` (same clustering, same cached-512px-thumb frames, same
//! `core_pano::detect_groups` call), so detection can be validated/tuned against a real library.
//!
//!   cargo run --release -p core-library --example detect_catalog -- <catalog.db> <thumbs-dir>
//!
//! Prints every metadata cluster it verifies and every group it finds, plus a summary. Read-only:
//! nothing is written back to the catalog.

use core_db::Db;
use core_library::pano_detect::{cluster_candidates, detect_candidates, Candidate, ClusterParams};
use core_library::{ThumbCache, THUMB_SIZE};
use core_pano::{detect_groups, DetectOptions, Frame};
use std::path::PathBuf;
use std::time::Instant;

fn load_frame(thumbs: &ThumbCache, c: &Candidate) -> Option<Frame> {
    let bytes = thumbs.read(&c.content_hash_hex, THUMB_SIZE).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgb = img.to_rgb8();
    let (width, height) = (rgb.width() as usize, rgb.height() as usize);
    let data: Vec<f32> = rgb
        .into_raw()
        .into_iter()
        .map(|b| b as f32 / 255.0)
        .collect();
    Some(Frame {
        width,
        height,
        rgb: data,
        focal_seed_px: None,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let db_path = PathBuf::from(
        args.next()
            .expect("usage: detect_catalog <catalog.db> <thumbs>"),
    );
    let thumbs_dir = PathBuf::from(
        args.next()
            .expect("usage: detect_catalog <catalog.db> <thumbs>"),
    );

    let db = Db::open(&db_path)?;
    let thumbs = ThumbCache::new(thumbs_dir)?;

    // id → filename, just for readable output (Candidate carries only the id + hash).
    let names: std::collections::HashMap<i64, String> = {
        let mut st = db.conn.prepare("SELECT id, path FROM images")?;
        let rows = st.query_map([], |r| {
            let p: String = r.get(1)?;
            Ok((
                r.get::<_, i64>(0)?,
                PathBuf::from(&p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(p),
            ))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    let cands = detect_candidates(&db.conn)?;
    let clusters = cluster_candidates(&cands, &ClusterParams::default());
    println!(
        "{} candidates → {} metadata clusters",
        cands.len(),
        clusters.len()
    );

    let opts = DetectOptions::default();
    let never_cancel = || false;
    let t0 = Instant::now();
    let (mut groups_total, mut pano_edges, mut burst_edges, mut weak_edges) = (0usize, 0, 0, 0);

    for idxs in &clusters {
        let cluster: Vec<&Candidate> = idxs.iter().map(|&i| &cands[i]).collect();
        let mut frames = Vec::with_capacity(cluster.len());
        let mut kept: Vec<&Candidate> = Vec::with_capacity(cluster.len());
        for c in &cluster {
            if let Some(f) = load_frame(&thumbs, c) {
                frames.push(f);
                kept.push(c);
            }
        }
        if frames.len() < 2 {
            continue;
        }

        let t = Instant::now();
        let report = detect_groups(&frames, None, &opts, &never_cancel);
        for e in &report.edges {
            match e.class {
                core_pano::EdgeClass::Pano => pano_edges += 1,
                core_pano::EdgeClass::Burst => burst_edges += 1,
                core_pano::EdgeClass::Weak => weak_edges += 1,
            }
        }
        if report.groups.is_empty() {
            continue;
        }
        for g in &report.groups {
            groups_total += 1;
            let member_names: Vec<String> = g
                .members
                .iter()
                .map(|&m| {
                    names
                        .get(&kept[m].id)
                        .cloned()
                        .unwrap_or_else(|| kept[m].id.to_string())
                })
                .collect();

            // Capture rate over the group's own span: a deliberate pano sweep is reframed by hand
            // (well under ~2 fps) while a continuous-drive burst runs many frames per second. Only
            // second-resolution timestamps are available, so the span is a lower bound — hence
            // frames/(span+1) rather than frames/span.
            let mut times: Vec<i64> = g.members.iter().map(|&m| kept[m].capture_date).collect();
            times.sort_unstable();
            let span = times.last().copied().unwrap_or(0) - times.first().copied().unwrap_or(0);
            let fps = g.members.len() as f64 / (span as f64 + 1.0);

            // Mean geometry of the edges that actually tie this group together.
            let ids: std::collections::HashSet<usize> = g.members.iter().copied().collect();
            let inner: Vec<&core_pano::VerifiedEdge> = report
                .edges
                .iter()
                .filter(|e| ids.contains(&e.i) && ids.contains(&e.j))
                .collect();
            let n = inner.len().max(1) as f64;
            let ov = inner.iter().map(|e| e.overlap).sum::<f64>() / n;
            let sh = inner.iter().map(|e| e.shift).sum::<f64>() / n;

            println!(
                "  group: {:2} frames conf={:.2} span={:3}s fps={:.2} overlap={:.2} shift={:.2} [{}]",
                g.members.len(),
                g.confidence,
                span,
                fps,
                ov,
                sh,
                member_names.join(", "),
            );
            let _ = t;
        }
    }

    println!(
        "\n{groups_total} groups in {:?}; edges: {pano_edges} pano / {burst_edges} burst / {weak_edges} weak",
        t0.elapsed()
    );
    Ok(())
}
