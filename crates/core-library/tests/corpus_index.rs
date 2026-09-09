//! Indexing a MIXED-maker folder: does the catalog describe what the decoder actually produced?
//!
//! `core-raw`'s own corpus test proves the develop is stable per file. This one proves the layer
//! above it stays consistent with that develop across makers in ONE folder: every supported
//! extension is enumerated, `process_file` labels the format the way `image_kind` does, the
//! width/height it stores are the dimensions `develop_linear` returns, and a `scan_root` over the
//! whole mixture lands every decodable file in the catalog while counting — not crashing on — the
//! ones this build cannot decode.
//!
//! With the RAW corpus fetched (`scripts/fetch_raw_corpus.sh`, see `tests/corpus/manifest.toml`)
//! it runs over real Canon/Nikon/Sony/Leica/Google/Apple/Pentax files — every manifest sample that
//! is on disk, whichever tiers were fetched (`DARKROOM_CORPUS_TIER` does not apply here: the
//! enumeration assertion has to describe the directory as it actually is). Without it, it authors a
//! synthetic mixed folder with `core_raw::synth` — two Bayer DNGs with different white balance,
//! size and orientation plus one monochrome DNG — so the assertions still have teeth on a machine
//! (or a CI job) that has no corpus.

use core_db::Db;
use core_library::{
    add_root, enumerate_raws, image_kind, process_file, scan_root, ThumbCache, SUPPORTED_EXT,
    THUMB_SIZE,
};
use core_raw::synth::{write_synthetic_bayer_dng, write_synthetic_mono_dng, SynthSpec};
use core_raw::{develop_linear, source_from_path};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Only the fields this test needs; `tests/corpus/manifest.toml` carries more.
#[derive(Deserialize)]
struct Manifest {
    #[serde(default, rename = "file")]
    files: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    path: String,
    support: String,
    #[serde(default)]
    xfail: Vec<String>,
}

/// One indexable file and what the catalog is expected to say about it.
struct Case {
    id: String,
    path: PathBuf,
    /// `develop_linear` must succeed, and its dimensions must be the ones the catalog stores.
    developable: bool,
    /// EXIF carries model + DateTimeOriginal, so `capture_fingerprint` is high-confidence.
    expect_fingerprint: bool,
    /// Assertion names known-red for this file (shared vocabulary with `core-raw`'s corpus test).
    xfail: Vec<String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpus_dir() -> PathBuf {
    match std::env::var_os("DARKROOM_RAW_CORPUS") {
        Some(v) => PathBuf::from(v),
        None => repo_root().join("target/raw-corpus"),
    }
}

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "darkroom-corpus-index-{}-{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("scratch dir");
    p
}

/// Path equality that survives a case-insensitive filesystem.
///
/// raw.pixls.us uses both `Sony/` and `SONY/` as maker directories. On macOS (and Windows) those
/// fold into ONE directory on disk, so `enumerate_raws` reports whichever casing was created first
/// while the manifest still names the upstream one. The bytes are the same file either way.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
}

fn has_supported_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| SUPPORTED_EXT.iter().any(|e| s.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// The real corpus, when it has been fetched. Only entries actually on disk are returned, so a
/// Tier-1-only checkout is a smaller run rather than a failure.
fn corpus_cases() -> Option<(PathBuf, Vec<Case>)> {
    let dir = corpus_dir();
    if !dir.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(repo_root().join("tests/corpus/manifest.toml")).ok()?;
    let manifest: Manifest = toml::from_str(&text).expect("parse tests/corpus/manifest.toml");

    // Selected by what is ON DISK, not by `DARKROOM_CORPUS_TIER`: this test asserts that the whole
    // corpus directory enumerates to exactly its manifest entries, so it has to describe the
    // directory as it actually is — whichever tiers happen to be fetched.
    let cases: Vec<Case> = manifest
        .files
        .iter()
        .filter(|e| has_supported_ext(Path::new(&e.path)))
        .map(|e| Case {
            id: e.id.clone(),
            path: dir.join(&e.path),
            developable: e.support == "ok",
            expect_fingerprint: true,
            xfail: e.xfail.clone(),
        })
        .filter(|c| c.path.exists())
        .collect();

    if cases.is_empty() {
        return None;
    }
    Some((dir, cases))
}

/// Corpus-free stand-in: a folder holding two different Bayer DNGs and one monochrome DNG.
///
/// These carry no `DateTimeOriginal`, so their capture fingerprint is deliberately low-confidence
/// (`None`) — asserted rather than skipped, because "no date ⇒ no fingerprint" is the rule the
/// catalog's grouping depends on.
fn synthetic_cases() -> (PathBuf, Vec<Case>) {
    let dir = scratch("synth");
    let specs: [(&str, SynthSpec, bool); 3] = [
        ("bayer_landscape.dng", SynthSpec::default(), false),
        (
            "bayer_portrait.dng",
            SynthSpec {
                width: 96,
                height: 64,
                orientation: 6,
                wb: [1.4, 1.0, 2.6, f32::NAN],
                ..Default::default()
            },
            false,
        ),
        ("mono.dng", SynthSpec::default(), true),
    ];

    let mut cases = Vec::new();
    for (name, spec, mono) in specs {
        let path = dir.join(name);
        if mono {
            write_synthetic_mono_dng(&path, &spec).expect("write synthetic mono DNG");
        } else {
            write_synthetic_bayer_dng(&path, &spec).expect("write synthetic Bayer DNG");
        }
        cases.push(Case {
            id: name.to_string(),
            path,
            developable: true,
            expect_fingerprint: false,
            xfail: Vec::new(),
        });
    }
    (dir, cases)
}

#[test]
fn mixed_maker_folder_indexes_consistently() {
    let corpus = corpus_cases();
    let synthetic = corpus.is_none();
    let (dir, cases) = corpus.unwrap_or_else(synthetic_cases);
    if synthetic {
        assert!(
            std::env::var("DARKROOM_REQUIRE_CORPUS").as_deref() != Ok("1"),
            "DARKROOM_REQUIRE_CORPUS=1 but no RAW corpus at {} — run scripts/fetch_raw_corpus.sh",
            corpus_dir().display()
        );
        eprintln!("no RAW corpus — running over a synthetic mixed folder instead");
    }

    let thumbs = ThumbCache::new(scratch("thumbs")).expect("thumb cache");
    let mut failures: Vec<String> = Vec::new();

    // 1. Enumeration: every case file, and nothing else (the corpus directory also holds the
    //    upstream `filelist.sha256`, which must not be mistaken for an image).
    let listed = enumerate_raws(&dir, true);
    assert_eq!(
        listed.len(),
        cases.len(),
        "enumerate_raws found {} files in {}, expected {} — the corpus directory must hold exactly \
         the manifest's samples ({:?})",
        listed.len(),
        dir.display(),
        cases.len(),
        listed
    );
    for case in &cases {
        assert!(
            listed.iter().any(|p| same_file(p, &case.path)),
            "enumerate_raws missed {}",
            case.path.display()
        );
    }

    // 2. Per file: the catalog row must describe the same image the decoder produces.
    for case in &cases {
        let p = match process_file(&case.path, &thumbs, THUMB_SIZE) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{}/process_file: {e}", case.id));
                continue;
            }
        };
        if p.format != image_kind(&case.path) {
            failures.push(format!(
                "{}/format: catalog says {:?}, image_kind says {:?}",
                case.id,
                p.format,
                image_kind(&case.path)
            ));
        }
        if p.capture_fingerprint.is_some() != case.expect_fingerprint {
            failures.push(format!(
                "{}/fingerprint: is_some() = {}, expected {}",
                case.id,
                p.capture_fingerprint.is_some(),
                case.expect_fingerprint
            ));
        }
        let src = source_from_path(&case.path).expect("open");
        if !case.developable {
            // The other half of the split below: a file the catalog happily indexes from its
            // embedded preview must still refuse to DEVELOP, or the manifest's `support` is stale.
            if let Ok(d) = develop_linear(&src) {
                failures.push(format!(
                    "{}/unsupported: manifest says undecodable, but it developed {}x{}",
                    case.id, d.width, d.height
                ));
            }
            continue;
        }

        let dev = match develop_linear(&src) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{}/develop: {e}", case.id));
                continue;
            }
        };
        // Shares its name with the `core-raw` corpus test's assertion: same claim, same xfail key.
        let agrees = (p.width, p.height) == (i64::from(dev.width), i64::from(dev.height));
        let expected_red = case.xfail.iter().any(|x| x == "catalog_dims");
        match (agrees, expected_red) {
            (true, false) | (false, true) => {}
            (false, false) => failures.push(format!(
                "{}/catalog_dims: catalog stores {}x{}, develop_linear returns {}x{}",
                case.id, p.width, p.height, dev.width, dev.height
            )),
            (true, true) => failures.push(format!(
                "{}/catalog_dims: listed in xfail but PASSED — remove it from the manifest",
                case.id
            )),
        }
    }

    // 3. Whole-folder scan into a fresh in-memory catalog: every decodable file lands, every
    //    undecodable one is counted, and the mixture does not take the scan down.
    let mut db = Db::open_in_memory().expect("in-memory catalog");
    let folder_id = add_root(&db.conn, &dir).expect("add root");
    let stats = scan_root(
        &mut db.conn,
        &thumbs,
        folder_id,
        &dir,
        THUMB_SIZE,
        |_, _| {},
    )
    .expect("scan_root over a mixed-maker folder");

    assert_eq!(stats.scanned, cases.len(), "scanned count");
    assert_eq!(
        stats.added + stats.failed + stats.skipped,
        cases.len(),
        "every scanned file must be added, skipped or failed ({stats:?})"
    );
    // Indexing is decode-free: hash + EXIF + embedded preview. So even the files this build cannot
    // DEVELOP (Nikon High Efficiency NEFs) index cleanly and appear in the library — they only fail
    // when Develop asks for pixels, which is asserted per file above. A non-zero `failed` here
    // therefore means a file broke metadata or thumbnail extraction, which no sample should.
    assert_eq!(
        stats.failed, 0,
        "no sample should fail indexing — indexing never develops ({stats:?})"
    );
    assert_eq!(
        stats.added,
        cases.len(),
        "every sample, decodable or not, must land in the catalog ({stats:?})"
    );
    let rows: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
        .expect("count rows");
    assert_eq!(
        rows, stats.added as i64,
        "catalog rows must match the added count ({stats:?})"
    );

    assert!(
        failures.is_empty(),
        "{} catalog/decoder divergence(s):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );

    let _ = std::fs::remove_dir_all(thumbs.root());
    if synthetic {
        let _ = std::fs::remove_dir_all(&dir);
    }
}
