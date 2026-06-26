//! core-dedup — duplicate detection (byte-identical + same-capture) and safe resolution.
//!
//! Detection is a `GROUP BY` over precomputed hashes (no rescan). Resolution routes files to the
//! Trash (never hard-deletes) and removes their catalog rows; the keeper is never touched.

pub mod error;

pub use error::DedupError;

use core_db::rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub const SIMILARITY_FEATURE_VERSION: i64 = 1;

/// A trash context that deletes silently and without involving Finder. On macOS the `trash` crate's
/// default `DeleteMethod::Finder` shells out to `osascript`/Finder per call — playing the Trash
/// sound, spawning a subprocess, and pulling Finder forward (a white WKWebView repaint). Resolving
/// a duplicate group of N files would otherwise fire that N times. `NsFileManager` trashes silently
/// and directly; files remain recoverable from the Trash (sans one-click "Put Back").
fn make_trash_ctx() -> trash::TrashContext {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupImage {
    pub id: i64,
    pub content_hash: String,
    pub path: String,
    pub filename: String,
    pub file_size: i64,
    pub capture_date: Option<i64>,
    pub stars: i64,
    pub iso: Option<i64>,
    pub shutter: Option<String>,
    pub aperture: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    /// Hex of the shared hash/fingerprint.
    pub key: String,
    /// "byte" or "capture".
    pub category: String,
    pub images: Vec<DupImage>,
}

fn hexs(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Query images whose `col` value is shared by >1 present image, grouped. `col` is an unqualified
/// column on `images` (`content_hash` or `capture_fingerprint`).
fn grouped_by(conn: &Connection, col: &str, category: &str) -> Result<Vec<DupGroup>, DedupError> {
    let sql = format!(
        "SELECT i.{col}, i.id, i.content_hash, i.path, i.original_filename, i.file_size, i.capture_date,
                COALESCE(rf.stars, 0), i.iso, i.shutter, i.aperture
         FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         WHERE i.status='present' AND i.{col} IS NOT NULL AND i.{col} IN (
             SELECT {col} FROM images WHERE status='present' AND {col} IS NOT NULL
             GROUP BY {col} HAVING COUNT(*) > 1
         )
         ORDER BY i.{col}, i.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let key: Vec<u8> = r.get(0)?;
        let hash: Vec<u8> = r.get(2)?;
        Ok((
            hexs(&key),
            DupImage {
                id: r.get(1)?,
                content_hash: hexs(&hash),
                path: r.get(3)?,
                filename: r.get(4)?,
                file_size: r.get(5)?,
                capture_date: r.get(6)?,
                stars: r.get(7)?,
                iso: r.get(8)?,
                shutter: r.get(9)?,
                aperture: r.get(10)?,
            },
        ))
    })?;

    let mut groups: Vec<DupGroup> = Vec::new();
    for row in rows {
        let (key, img) = row?;
        match groups.last_mut() {
            Some(g) if g.key == key => g.images.push(img),
            _ => groups.push(DupGroup {
                key,
                category: category.to_string(),
                images: vec![img],
            }),
        }
    }
    Ok(groups)
}

/// 64-bit difference hash (dHash) of a JPEG thumbnail: convert to grayscale, resize to 9×8, and emit
/// one bit per horizontally-adjacent pixel pair (left < right). Visually similar images differ in
/// only a few bits. Returns `None` if the bytes can't be decoded.
pub fn dhash_from_jpeg(bytes: &[u8]) -> Option<u64> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(dhash_from_image(&img))
}

fn dhash_from_image(img: &image::DynamicImage) -> u64 {
    let small = img
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash: u64 = 0;
    let mut bit = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            if small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0] {
                hash |= 1 << bit;
            }
            bit += 1;
        }
    }
    hash
}

/// Hamming distance between two dHashes (number of differing bits, 0..=64).
#[inline]
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[derive(Debug, Clone)]
pub struct SimilarityFeatures {
    pub dhash64: u64,
    pub phash64: u64,
    pub tiny_luma32: Vec<u8>,
    pub color_grid4x4: Vec<u8>,
}

pub fn similarity_features_from_jpeg(bytes: &[u8]) -> Option<SimilarityFeatures> {
    let img = image::load_from_memory(bytes).ok()?;
    let luma = img
        .resize_exact(32, 32, image::imageops::FilterType::Triangle)
        .to_luma8()
        .into_raw();
    Some(SimilarityFeatures {
        dhash64: dhash_from_image(&img),
        phash64: phash_from_luma32(&luma),
        tiny_luma32: luma,
        color_grid4x4: color_grid4x4(&img),
    })
}

fn phash_from_luma32(luma: &[u8]) -> u64 {
    let mut coeffs: Vec<f32> = luma.iter().map(|&v| v as f32).collect();
    let mut planner = rustdct::DctPlanner::<f32>::new();
    let dct = planner.plan_dct2(32);
    for row in coeffs.chunks_mut(32) {
        dct.process_dct2(row);
    }
    for x in 0..32 {
        let mut col: Vec<f32> = (0..32).map(|y| coeffs[y * 32 + x]).collect();
        dct.process_dct2(&mut col);
        for y in 0..32 {
            coeffs[y * 32 + x] = col[y];
        }
    }
    phash_from_coeffs(&coeffs)
}

fn phash_from_coeffs(coeffs: &[f32]) -> u64 {
    let mut low = Vec::with_capacity(63);
    for y in 0..8 {
        for x in 0..8 {
            if x != 0 || y != 0 {
                low.push(coeffs[y * 32 + x]);
            }
        }
    }
    let median = median_f32(low.clone());
    low.into_iter().enumerate().fold(
        0u64,
        |acc, (i, v)| {
            if v > median {
                acc | (1u64 << i)
            } else {
                acc
            }
        },
    )
}

fn median_f32(mut values: Vec<f32>) -> f32 {
    values.sort_by(|a, b| a.total_cmp(b));
    values[values.len() / 2]
}

fn color_grid4x4(img: &image::DynamicImage) -> Vec<u8> {
    let rgb = img
        .resize_exact(64, 64, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let mut out = Vec::with_capacity(48);
    for gy in 0..4u32 {
        for gx in 0..4u32 {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for y in gy * 16..(gy + 1) * 16 {
                for x in gx * 16..(gx + 1) * 16 {
                    let p = rgb.get_pixel(x, y);
                    r += p[0] as u32;
                    g += p[1] as u32;
                    b += p[2] as u32;
                }
            }
            out.extend([(r / 256) as u8, (g / 256) as u8, (b / 256) as u8]);
        }
    }
    out
}

/// BK-tree over u64 dHashes (Hamming metric) for sub-quadratic near-duplicate queries. Identical
/// hashes collapse into one node carrying multiple payloads, so a large all-identical cluster (e.g.
/// many shots of a flat wall) stays cheap instead of degenerating to O(n²). `payload` is the caller's
/// row index, used to union matches.
#[derive(Default)]
struct BkTree {
    nodes: Vec<BkNode>,
}

struct BkNode {
    hash: u64,
    payloads: Vec<usize>,
    /// distance-to-this-node → child node index.
    children: std::collections::HashMap<u32, usize>,
}

impl BkTree {
    fn insert(&mut self, hash: u64, payload: usize) {
        if self.nodes.is_empty() {
            self.nodes.push(BkNode {
                hash,
                payloads: vec![payload],
                children: std::collections::HashMap::new(),
            });
            return;
        }
        let mut cur = 0usize;
        loop {
            let d = hamming(self.nodes[cur].hash, hash);
            if d == 0 {
                self.nodes[cur].payloads.push(payload); // identical dHash → same node
                return;
            }
            match self.nodes[cur].children.get(&d).copied() {
                Some(next) => cur = next,
                None => {
                    let new_idx = self.nodes.len();
                    self.nodes.push(BkNode {
                        hash,
                        payloads: vec![payload],
                        children: std::collections::HashMap::new(),
                    });
                    self.nodes[cur].children.insert(d, new_idx);
                    return;
                }
            }
        }
    }

    /// Append the payloads of every node within `max_dist` of `target` to `out`. Prunes children by
    /// the triangle inequality: only branches with key in `[d - max_dist, d + max_dist]` can contain
    /// a match.
    fn query(&self, target: u64, max_dist: u32, out: &mut Vec<usize>) {
        if self.nodes.is_empty() {
            return;
        }
        let mut stack = vec![0usize];
        while let Some(cur) = stack.pop() {
            let node = &self.nodes[cur];
            let d = hamming(node.hash, target);
            if d <= max_dist {
                out.extend_from_slice(&node.payloads);
            }
            let lo = d.saturating_sub(max_dist);
            let hi = d + max_dist;
            for (&k, &child) in &node.children {
                if k >= lo && k <= hi {
                    stack.push(child);
                }
            }
        }
    }
}

#[derive(Clone)]
struct CandidateRow {
    image: DupImage,
    camera_model: Option<String>,
    body_serial: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    features: SimilarityFeatures,
}

struct PairEval {
    accepted: bool,
    score: u32,
}

/// Same-scene / near-duplicate groups. Similarity is intentionally capture-time gated: visually
/// related photos from different sessions are not cleanup candidates.
pub fn find_perceptual(conn: &Connection, threshold: u32) -> Result<Vec<DupGroup>, DedupError> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.content_hash, i.path, i.original_filename, i.file_size, i.capture_date,
                sf.dhash64, sf.phash64, sf.tiny_luma32, sf.color_grid4x4,
                COALESCE(rf.stars, 0), i.iso, i.shutter, i.aperture,
                i.camera_model, i.body_serial, i.width, i.height
         FROM images i
         JOIN image_similarity_features sf ON sf.image_id = i.id
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         WHERE i.status='present' AND i.capture_date IS NOT NULL AND sf.feature_version=?1
         ORDER BY i.capture_date, i.id",
    )?;
    let rows: Vec<CandidateRow> = stmt
        .query_map(params![SIMILARITY_FEATURE_VERSION], |r| {
            let hash: Vec<u8> = r.get(1)?;
            Ok(CandidateRow {
                image: DupImage {
                    id: r.get(0)?,
                    content_hash: hexs(&hash),
                    path: r.get(2)?,
                    filename: r.get(3)?,
                    file_size: r.get(4)?,
                    capture_date: r.get(5)?,
                    stars: r.get(10)?,
                    iso: r.get(11)?,
                    shutter: r.get(12)?,
                    aperture: r.get(13)?,
                },
                camera_model: r.get(14)?,
                body_serial: r.get(15)?,
                width: r.get(16)?,
                height: r.get(17)?,
                features: SimilarityFeatures {
                    dhash64: r.get::<_, i64>(6)? as u64,
                    phash64: r.get::<_, i64>(7)? as u64,
                    tiny_luma32: r.get(8)?,
                    color_grid4x4: r.get(9)?,
                },
            })
        })?
        .collect::<core_db::rusqlite::Result<_>>()?;
    let rows: Vec<CandidateRow> = rows.into_iter().filter(valid_candidate).collect();
    let edges = find_similarity_edges(&rows, threshold);
    let mut out = medoid_groups(&rows, &edges);
    out.sort_by_key(|g| g.images.iter().map(|i| i.id).min().unwrap_or(0));
    Ok(out)
}

fn valid_candidate(row: &CandidateRow) -> bool {
    row.features.tiny_luma32.len() == 1024 && row.features.color_grid4x4.len() == 48
}

fn find_similarity_edges(rows: &[CandidateRow], threshold: u32) -> Vec<(usize, usize, u32)> {
    let mut seen = std::collections::HashSet::new();
    let mut edges = Vec::new();
    add_temporal_edges(rows, threshold, &mut seen, &mut edges);
    add_hash_edges(rows, threshold, &mut seen, &mut edges);
    edges
}

fn add_temporal_edges(
    rows: &[CandidateRow],
    threshold: u32,
    seen: &mut std::collections::HashSet<(usize, usize)>,
    edges: &mut Vec<(usize, usize, u32)>,
) {
    let max_window = time_window_secs(threshold);
    for i in 0..rows.len() {
        for (checked, j) in (0..i).rev().enumerate() {
            if capture_delta(&rows[i], &rows[j]) > max_window || checked >= 256 {
                break;
            }
            maybe_add_edge(rows, threshold, i, j, seen, edges);
        }
    }
}

fn add_hash_edges(
    rows: &[CandidateRow],
    threshold: u32,
    seen: &mut std::collections::HashSet<(usize, usize)>,
    edges: &mut Vec<(usize, usize, u32)>,
) {
    let (d_limit, p_limit) = candidate_limits(threshold);
    let (mut d_tree, mut p_tree) = (BkTree::default(), BkTree::default());
    let mut matches = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        matches.clear();
        d_tree.query(row.features.dhash64, d_limit, &mut matches);
        p_tree.query(row.features.phash64, p_limit, &mut matches);
        matches.sort_unstable();
        matches.dedup();
        for &j in &matches {
            maybe_add_edge(rows, threshold, i, j, seen, edges);
        }
        d_tree.insert(row.features.dhash64, i);
        p_tree.insert(row.features.phash64, i);
    }
}

fn maybe_add_edge(
    rows: &[CandidateRow],
    threshold: u32,
    i: usize,
    j: usize,
    seen: &mut std::collections::HashSet<(usize, usize)>,
    edges: &mut Vec<(usize, usize, u32)>,
) {
    let key = if i < j { (i, j) } else { (j, i) };
    if seen.insert(key) {
        if let Some(eval) = verify_pair(&rows[i], &rows[j], threshold) {
            if eval.accepted {
                edges.push((key.0, key.1, eval.score));
            }
        }
    }
}

fn verify_pair(a: &CandidateRow, b: &CandidateRow, threshold: u32) -> Option<PairEval> {
    let window = time_window_secs(threshold);
    if capture_delta(a, b) > window || camera_conflict(a, b) {
        return None;
    }
    let dh = hamming(a.features.dhash64, b.features.dhash64);
    let ph = hamming(a.features.phash64, b.features.phash64);
    if aspect_conflict(a, b) && !(dh <= 4 && ph <= 8) {
        return None;
    }
    let color = color_distance(&a.features.color_grid4x4, &b.features.color_grid4x4);
    let luma = ncc_u8(&a.features.tiny_luma32, &b.features.tiny_luma32);
    let edge = edge_ncc(&a.features.tiny_luma32, &b.features.tiny_luma32);
    let accepted = accepted_pair(window, dh, ph, color, luma, edge);
    Some(PairEval {
        accepted,
        score: pair_score(dh, ph, color, luma, edge),
    })
}

fn accepted_pair(window: i64, dh: u32, ph: u32, color: f32, luma: f32, edge: f32) -> bool {
    let strong_hash = (dh <= 4 && ph <= 10) || ph <= 8;
    let medium_hash = dh <= 8 || ph <= 14;
    let loose_hash = dh <= 14 || ph <= 22;
    let structure = luma >= 0.72 || edge >= 0.40;
    let strong_structure = luma >= 0.86 || edge >= 0.55;
    match window {
        0..=3 => {
            (color <= 0.18 && (loose_hash || structure))
                || (color <= 0.24 && strong_hash && (structure || color <= 0.06))
        }
        4..=30 => {
            (color <= 0.18 && medium_hash && structure)
                || (color <= 0.10 && strong_hash && (structure || color <= 0.04))
        }
        _ => color <= 0.10 && medium_hash && strong_structure,
    }
}

fn pair_score(dh: u32, ph: u32, color: f32, luma: f32, edge: f32) -> u32 {
    let raw = 1000.0 - dh as f32 * 8.0 - ph as f32 * 5.0 - color * 400.0
        + luma.max(0.0) * 180.0
        + edge.max(0.0) * 120.0;
    raw.clamp(0.0, 2000.0) as u32
}

fn medoid_groups(rows: &[CandidateRow], edges: &[(usize, usize, u32)]) -> Vec<DupGroup> {
    let mut adj = vec![Vec::<(usize, u32)>::new(); rows.len()];
    for &(a, b, score) in edges {
        adj[a].push((b, score));
        adj[b].push((a, score));
    }
    let mut assigned = vec![false; rows.len()];
    let mut out = Vec::new();
    while let Some(center) = next_center(&adj, &assigned) {
        let mut members = center_members(center, &adj, &assigned);
        assigned[center] = true;
        for &idx in &members {
            assigned[idx] = true;
        }
        members.insert(0, center);
        if members.len() > 1 {
            out.push(group_from_members(rows, &members));
        }
    }
    out
}

fn next_center(adj: &[Vec<(usize, u32)>], assigned: &[bool]) -> Option<usize> {
    (0..adj.len()).filter(|&i| !assigned[i]).max_by_key(|&i| {
        adj[i]
            .iter()
            .filter(|(j, _)| !assigned[*j])
            .map(|(_, s)| *s as u64)
            .sum::<u64>()
    })
}

fn center_members(center: usize, adj: &[Vec<(usize, u32)>], assigned: &[bool]) -> Vec<usize> {
    let mut links: Vec<(usize, u32)> = adj[center]
        .iter()
        .copied()
        .filter(|(idx, _)| !assigned[*idx])
        .collect();
    links.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    let mut members = Vec::new();
    for (idx, _) in links {
        if members.iter().all(|&member| connected(adj, idx, member)) {
            members.push(idx);
        }
    }
    members
}

fn connected(adj: &[Vec<(usize, u32)>], a: usize, b: usize) -> bool {
    adj[a].iter().any(|(idx, _)| *idx == b)
}

fn group_from_members(rows: &[CandidateRow], members: &[usize]) -> DupGroup {
    DupGroup {
        key: format!("p{:016x}", rows[members[0]].features.phash64),
        category: "perceptual".to_string(),
        images: members.iter().map(|&i| rows[i].image.clone()).collect(),
    }
}

fn time_window_secs(threshold: u32) -> i64 {
    match threshold {
        0..=4 => 3,
        5..=10 => 30,
        _ => 300,
    }
}

fn candidate_limits(threshold: u32) -> (u32, u32) {
    if threshold > 10 {
        (10, 16)
    } else {
        (threshold.clamp(4, 12), (threshold + 8).clamp(10, 18))
    }
}

fn capture_delta(a: &CandidateRow, b: &CandidateRow) -> i64 {
    (a.image.capture_date.unwrap_or(0) - b.image.capture_date.unwrap_or(0)).abs()
}

fn camera_conflict(a: &CandidateRow, b: &CandidateRow) -> bool {
    match (&a.body_serial, &b.body_serial) {
        (Some(x), Some(y)) if !x.is_empty() && !y.is_empty() && x != y => return true,
        _ => {}
    }
    matches!((&a.camera_model, &b.camera_model), (Some(x), Some(y)) if !x.is_empty() && !y.is_empty() && x != y)
}

fn aspect_conflict(a: &CandidateRow, b: &CandidateRow) -> bool {
    let (Some(aw), Some(ah), Some(bw), Some(bh)) = (a.width, a.height, b.width, b.height) else {
        return false;
    };
    if aw <= 0 || ah <= 0 || bw <= 0 || bh <= 0 {
        return false;
    }
    ((aw as f32 / ah as f32).ln() - (bw as f32 / bh as f32).ln()).abs() > 0.35
}

fn color_distance(a: &[u8], b: &[u8]) -> f32 {
    a.chunks_exact(3)
        .zip(b.chunks_exact(3))
        .map(|(x, y)| chroma_distance(x, y))
        .sum::<f32>()
        / 16.0
}

fn chroma_distance(a: &[u8], b: &[u8]) -> f32 {
    let sa = (a[0] as f32 + a[1] as f32 + a[2] as f32).max(1.0);
    let sb = (b[0] as f32 + b[1] as f32 + b[2] as f32).max(1.0);
    ((a[0] as f32 / sa - b[0] as f32 / sb).abs()
        + (a[1] as f32 / sa - b[1] as f32 / sb).abs()
        + (a[2] as f32 / sa - b[2] as f32 / sb).abs())
        / 3.0
}

fn ncc_u8(a: &[u8], b: &[u8]) -> f32 {
    let af: Vec<f32> = a.iter().map(|&v| v as f32).collect();
    let bf: Vec<f32> = b.iter().map(|&v| v as f32).collect();
    ncc_f32(&af, &bf)
}

fn edge_ncc(a: &[u8], b: &[u8]) -> f32 {
    ncc_f32(&edge_magnitudes(a), &edge_magnitudes(b))
}

fn edge_magnitudes(luma: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(31 * 31);
    for y in 0..31usize {
        for x in 0..31usize {
            let dx = luma[y * 32 + x + 1] as f32 - luma[y * 32 + x] as f32;
            let dy = luma[(y + 1) * 32 + x] as f32 - luma[y * 32 + x] as f32;
            out.push((dx * dx + dy * dy).sqrt());
        }
    }
    out
}

fn ncc_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let ma = a.iter().sum::<f32>() / a.len() as f32;
    let mb = b.iter().sum::<f32>() / b.len() as f32;
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for (&x, &y) in a.iter().zip(b.iter()) {
        cov += (x - ma) * (y - mb);
        va += (x - ma).powi(2);
        vb += (y - mb).powi(2);
    }
    if va <= 1e-6 || vb <= 1e-6 {
        return 0.0;
    }
    cov / (va.sqrt() * vb.sqrt())
}

#[cfg(test)]
mod bk_tests {
    use super::*;

    #[test]
    fn bktree_query_matches_brute_force() {
        // Deterministic pseudo-random 64-bit hashes (no rng dependency).
        let hashes: Vec<u64> = (0..600u64)
            .map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (i << 7))
            .collect();
        let mut tree = BkTree::default();
        for (idx, &h) in hashes.iter().enumerate() {
            tree.insert(h, idx);
        }
        for &q in &hashes {
            for &thr in &[0u32, 3, 8, 20] {
                let mut got = Vec::new();
                tree.query(q, thr, &mut got);
                got.sort_unstable();
                let mut want: Vec<usize> = hashes
                    .iter()
                    .enumerate()
                    .filter(|(_, &h)| hamming(h, q) <= thr)
                    .map(|(i, _)| i)
                    .collect();
                want.sort_unstable();
                assert_eq!(
                    got, want,
                    "BK-tree query diverged from brute force (thr={thr})"
                );
            }
        }
    }

    #[test]
    fn bktree_collapses_identical_hashes() {
        let mut tree = BkTree::default();
        for idx in 0..5 {
            tree.insert(42u64, idx);
        }
        let mut got = Vec::new();
        tree.query(42, 0, &mut got);
        got.sort_unstable();
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
    }
}

/// Byte-identical duplicates (same whole-file `content_hash`).
pub fn find_byte_identical(conn: &Connection) -> Result<Vec<DupGroup>, DedupError> {
    grouped_by(conn, "content_hash", "byte")
}

/// Same-capture duplicates (same `capture_fingerprint`; bytes may differ).
pub fn find_same_capture(conn: &Connection) -> Result<Vec<DupGroup>, DedupError> {
    grouped_by(conn, "capture_fingerprint", "capture")
}

/// Outcome of a resolve. `trashed_hashes` are the hex content-hashes of the removed images, so the
/// caller can GC orphaned thumbnails for any hash no longer referenced by a present row (a byte-
/// identical keeper still shares its hash, so the caller must re-check presence before deleting).
#[derive(Debug, Default, Clone)]
pub struct ResolveResult {
    pub trashed: usize,
    pub trashed_hashes: Vec<String>,
}

/// Trash the given images (never the keeper) and remove their catalog rows.
///
/// Consistency: each file is sent to the Trash *first*; a row is removed only once its file is gone
/// (or was already missing). A file that fails to trash keeps its row, so the catalog never points
/// at a still-present file it thinks it deleted. The row removals then run in a single transaction.
pub fn resolve(
    conn: &Connection,
    keep_id: i64,
    trash_ids: &[i64],
) -> Result<ResolveResult, DedupError> {
    // Guard: the keeper must be a present catalog row. A stale/already-resolved or foreign keeper
    // would otherwise let us trash every id in `trash_ids` with nothing guaranteed to survive — the
    // worst case being the last copy of a byte-identical group. Validate first; trash nothing on miss.
    let keeper_present: Option<i64> = conn
        .query_row(
            "SELECT id FROM images WHERE id=?1 AND status='present'",
            params![keep_id],
            |r| r.get(0),
        )
        .optional()?;
    if keeper_present.is_none() {
        return Err(DedupError::InvalidKeeper(format!(
            "id {keep_id} is not a present image"
        )));
    }

    // Snapshot (id, path, hash) for each victim; tolerate rows already gone.
    let mut victims: Vec<(i64, String, Vec<u8>)> = Vec::new();
    for &id in trash_ids {
        if id == keep_id {
            continue;
        }
        let row = conn
            .query_row(
                "SELECT path, content_hash FROM images WHERE id=?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?;
        if let Some((path, hash)) = row {
            victims.push((id, path, hash));
        }
    }

    // Trash files; collect only those whose file is gone (so the row can be safely removed).
    // Per-file (not batched) so a single failure only skips that one row — a batch `delete_all`
    // stops at the first error and would hide which files actually made it to the Trash.
    let trash_ctx = make_trash_ctx();
    let mut to_delete: Vec<i64> = Vec::new();
    let mut hashes: Vec<String> = Vec::new();
    for (id, path, hash) in &victims {
        let p = std::path::Path::new(path);
        if p.exists() && trash_ctx.delete(p).is_err() {
            continue; // leave the row intact; skip this one
        }
        to_delete.push(*id);
        hashes.push(hexs(hash));
    }

    // Atomic row removal.
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare("DELETE FROM images WHERE id=?1")?;
        for id in &to_delete {
            stmt.execute(params![id])?;
        }
    }
    tx.commit()?;

    Ok(ResolveResult {
        trashed: to_delete.len(),
        trashed_hashes: hashes,
    })
}

/// Keeper for a group: prefer the largest file (most complete), tiebreak by lowest id (stable /
/// oldest). Returns `(keep_id, trash_ids)`.
fn pick_keeper(group: &DupGroup) -> (i64, Vec<i64>) {
    let keep = group
        .images
        .iter()
        .max_by(|a, b| a.file_size.cmp(&b.file_size).then(b.id.cmp(&a.id)))
        .map(|i| i.id)
        .unwrap_or(0);
    let trash = group
        .images
        .iter()
        .map(|i| i.id)
        .filter(|&id| id != keep)
        .collect();
    (keep, trash)
}

/// Auto-resolve every byte-identical group: keep one copy, trash the bit-for-bit duplicates. Only
/// applied to byte-identical groups — same-capture / perceptual matches are intentional variants and
/// are never auto-trashed. Returns the aggregate outcome.
pub fn auto_resolve_byte_identical(conn: &Connection) -> Result<ResolveResult, DedupError> {
    let groups = find_byte_identical(conn)?;
    let mut out = ResolveResult::default();
    for g in &groups {
        let (keep, trash) = pick_keeper(g);
        let r = resolve(conn, keep, &trash)?;
        out.trashed += r.trashed;
        out.trashed_hashes.extend(r.trashed_hashes);
    }
    Ok(out)
}
