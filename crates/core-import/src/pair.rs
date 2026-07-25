//! RAW + JPEG/HEIF pair detection over a source listing.
//!
//! In "RAW+JPEG" (or "RAW+HEIF") mode a camera writes two files per shot, differing only in
//! extension: `855A1234.CR3` + `855A1234.JPG`. This module recognises those groups from paths alone
//! — no file reads, no EXIF — so the Import dialog can offer the choice before anything is copied.
//!
//! Deliberately path-based, not fingerprint-based: the same-stem convention is what cameras
//! guarantee, it costs nothing to evaluate on a full card, and it cannot mis-pair two different
//! shots the way a coincidental EXIF-second collision could.

use core_raw::{classify, ImageKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How an import treats a RAW and the JPEG/HEIF sitting beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pairing {
    /// Every file becomes its own catalog entry (the pre-pairing behaviour).
    #[default]
    Standalone,
    /// The companion is linked to its RAW (`image_pairs`), so the pair shows as one grid cell.
    Pair,
}

/// One detected RAW + companion(s) group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairGroup {
    /// Stable identity of the group: `‹parent dir›/‹lowercased stem›`. Also the frontend's grouping
    /// key, so selecting one member can select the whole pair.
    pub key: String,
    /// The RAW that anchors the pair.
    pub primary: PathBuf,
    /// The camera-written companions (JPEG/HEIF), sorted by path.
    pub secondaries: Vec<PathBuf>,
}

/// Is this a file a camera writes *beside* a RAW? PNG and EXR are excluded on purpose: they are
/// products of this app (exports, merged HDR), never a capture companion.
fn is_companion(path: &Path) -> bool {
    matches!(classify(path), ImageKind::Jpeg | ImageKind::Heif)
}

fn is_raw(path: &Path) -> bool {
    classify(path) == ImageKind::Raw
}

/// Group key: parent directory + case-insensitive filename stem. Files in different folders never
/// pair, even with identical names — two cards can both hold `IMG_0001`.
fn group_key(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?.to_lowercase();
    let dir = path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Some(format!("{dir}/{stem}"))
}

/// Find the RAW + JPEG/HEIF groups in `paths`.
///
/// A group pairs only when it holds at least one RAW **and** at least one companion; the RAW is the
/// primary. A stem carrying several RAWs (e.g. `A.CR3` + `A.DNG`) anchors on the first by path order
/// and leaves the rest standalone — they are separate captures' worth of data, not companions.
/// Returns groups ordered by key, each with sorted secondaries, so the result is deterministic.
pub fn detect_pairs(paths: &[PathBuf]) -> Vec<PairGroup> {
    let mut by_key: HashMap<String, (Vec<PathBuf>, Vec<PathBuf>)> = HashMap::new();
    for path in paths {
        let Some(key) = group_key(path) else { continue };
        let entry = by_key.entry(key).or_default();
        if is_raw(path) {
            entry.0.push(path.clone());
        } else if is_companion(path) {
            entry.1.push(path.clone());
        }
    }

    let mut groups: Vec<PairGroup> = by_key
        .into_iter()
        .filter_map(|(key, (mut raws, mut companions))| {
            if raws.is_empty() || companions.is_empty() {
                return None;
            }
            raws.sort();
            companions.sort();
            Some(PairGroup {
                key,
                primary: raws.remove(0),
                secondaries: companions,
            })
        })
        .collect();
    groups.sort_by(|a, b| a.key.cmp(&b.key));
    groups
}

/// Per-path pair role lookup (`"primary"` / `"secondary"`) built from [`detect_pairs`], for
/// annotating the import listing.
pub fn pair_roles(groups: &[PairGroup]) -> HashMap<PathBuf, (String, &'static str)> {
    let mut map = HashMap::new();
    for g in groups {
        map.insert(g.primary.clone(), (g.key.clone(), "primary"));
        for s in &g.secondaries {
            map.insert(s.clone(), (g.key.clone(), "secondary"));
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn pairs_raw_with_jpeg_and_heif_case_insensitively() {
        let groups = detect_pairs(&[
            p("/card/855A0001.CR3"),
            p("/card/855a0001.JPG"),
            p("/card/855A0002.cr3"),
            p("/card/855A0002.HIF"),
            p("/card/855A0002.jpg"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].primary, p("/card/855A0001.CR3"));
        assert_eq!(groups[0].secondaries, vec![p("/card/855a0001.JPG")]);
        // A shot can carry both companions; they sort by path.
        assert_eq!(
            groups[1].secondaries,
            vec![p("/card/855A0002.HIF"), p("/card/855A0002.jpg")]
        );
    }

    #[test]
    fn lone_files_and_app_products_never_pair() {
        let groups = detect_pairs(&[
            p("/card/A.CR3"), // RAW with no companion
            p("/card/B.JPG"), // JPEG with no RAW
            p("/card/C.CR3"),
            p("/card/C.PNG"), // export, not a camera companion
            p("/card/D.CR3"),
            p("/card/D.exr"), // merged HDR, not a camera companion
            p("/card/E.JPG"),
            p("/card/E.HIF"), // no RAW to anchor them
        ]);
        assert!(groups.is_empty(), "got {groups:?}");
    }

    #[test]
    fn same_stem_in_different_folders_does_not_pair() {
        let groups = detect_pairs(&[p("/card/a/IMG_0001.CR3"), p("/card/b/IMG_0001.JPG")]);
        assert!(groups.is_empty());
    }

    #[test]
    fn extra_raws_on_one_stem_stay_standalone() {
        let groups = detect_pairs(&[p("/card/A.CR3"), p("/card/A.DNG"), p("/card/A.JPG")]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].primary, p("/card/A.CR3"));
        assert_eq!(groups[0].secondaries, vec![p("/card/A.JPG")]);
        let roles = pair_roles(&groups);
        assert!(!roles.contains_key(&p("/card/A.DNG")));
    }
}
