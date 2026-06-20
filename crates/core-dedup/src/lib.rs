//! core-dedup — duplicate detection (byte-identical + same-capture) and safe resolution.
//!
//! Detection is a `GROUP BY` over precomputed hashes (no rescan). Resolution routes files to the
//! Trash (never hard-deletes) and removes their catalog rows; the keeper is never touched.

pub mod error;

pub use error::DedupError;

use core_db::rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

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
        "SELECT i.{col}, i.id, i.content_hash, i.path, i.original_filename, i.file_size, i.capture_date
         FROM images i
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
    let small = image::load_from_memory(bytes)
        .ok()?
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
    Some(hash)
}

/// Hamming distance between two dHashes (number of differing bits, 0..=64).
#[inline]
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Near-duplicate groups: images whose dHashes are within `threshold` bits of each other, linked
/// transitively (a burst forms one group via union-find). Reads the precomputed `phash` column —
/// rows with a NULL `phash` are ignored (the caller fills them lazily before scanning).
///
/// O(n²) pairwise over images that have a phash. Fine for an on-demand scan at ≤50k; swap in a
/// BK-tree if that ever gets too slow.
pub fn find_perceptual(conn: &Connection, threshold: u32) -> Result<Vec<DupGroup>, DedupError> {
    let mut stmt = conn.prepare(
        "SELECT id, content_hash, path, original_filename, file_size, capture_date, phash
         FROM images
         WHERE status='present' AND phash IS NOT NULL
         ORDER BY id",
    )?;
    let rows: Vec<(DupImage, u64)> = stmt
        .query_map([], |r| {
            let hash: Vec<u8> = r.get(1)?;
            Ok((
                DupImage {
                    id: r.get(0)?,
                    content_hash: hexs(&hash),
                    path: r.get(2)?,
                    filename: r.get(3)?,
                    file_size: r.get(4)?,
                    capture_date: r.get(5)?,
                },
                r.get::<_, i64>(6)? as u64,
            ))
        })?
        .collect::<core_db::rusqlite::Result<_>>()?;

    let n = rows.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if hamming(rows[i].1, rows[j].1) <= threshold {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }

    let mut by_root: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..n {
        let r = root(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    let mut out: Vec<DupGroup> = by_root
        .into_values()
        .filter(|idxs| idxs.len() > 1)
        .map(|idxs| DupGroup {
            key: format!("p{:016x}", rows[idxs[0]].1),
            category: "perceptual".to_string(),
            images: idxs.iter().map(|&i| rows[i].0.clone()).collect(),
        })
        .collect();
    // Stable order: by the smallest image id in each group.
    out.sort_by_key(|g| g.images.iter().map(|i| i.id).min().unwrap_or(0));
    Ok(out)
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
