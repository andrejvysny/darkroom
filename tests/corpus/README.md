# Multi-maker RAW test corpus

One committed Canon R7 CR3 proves the decode pipeline runs. It cannot prove the pipeline behaves
the same way on a Nikon High Efficiency NEF, a Canon sRAW, a Leica monochrome DNG or a phone's
ProRAW — and those are exactly the files that break a RAW developer. This directory is the
answer: a **downloadable** 19-file, 7-maker corpus with recorded per-file statistics.

| file            | what it is                                                                  |
| --------------- | --------------------------------------------------------------------------- |
| `manifest.toml` | identity + acquisition only (path, sha256, size, tier, support, xfail)       |
| `expected.toml` | machine-recorded goldens per file (dims, CFA, levels, patch means, chroma)   |

Nothing is committed except those two files. The RAW files land in `target/raw-corpus/`
(git-ignored, ~680 MB for both tiers).

## Provenance and licence

Every sample comes from the **[raw.pixls.us](https://raw.pixls.us/) CC0 pool**, whose submission
terms place each file under CC0 (public domain dedication). That is the only pool this corpus draws
from — do not add a sample from anywhere else, and do not commit RAW bytes to this repository.

`manifest.toml` records the upstream dataset stamp (`upstream_timestamp`, from
`https://raw.pixls.us/data/timestamp.txt`) that the hashes were taken against.

## Fetching

```bash
scripts/fetch_raw_corpus.sh              # Tier 1 (~367 MB) — what CI fetches
scripts/fetch_raw_corpus.sh --tier 2     # + the extended set (~678 MB total)
scripts/fetch_raw_corpus.sh --verify-only  # re-hash what is already on disk, download nothing
```

The script downloads the live `filelist.sha256` first and **refuses** any manifest path that is not
in it (and reports a manifest whose sha256 no longer matches upstream as `STALE`). Each file is
written to `<name>.part`, verified, then moved into place; a file whose hash already matches is
skipped, so a warm run is a no-op. `DARKROOM_RAW_CORPUS` overrides the destination.

**Retry rule:** `curl -fL --retry 3 --retry-delay 2 --connect-timeout 20`. A download that still
fails is reported and the script exits non-zero — it never leaves a half file behind and never
silently substitutes a partial one. Re-running is always safe: it resumes with whatever is missing.

## Running the tests

```bash
DARKROOM_REQUIRE_CORPUS=1 cargo test -p core-raw -p core-library --test corpus --test corpus_index
```

| variable                 | default            | meaning                                        |
| ------------------------ | ------------------ | ---------------------------------------------- |
| `DARKROOM_RAW_CORPUS`    | `target/raw-corpus`| where the corpus lives                          |
| `DARKROOM_REQUIRE_CORPUS`| unset              | `1` turns "no corpus" from a skip into a failure |
| `DARKROOM_CORPUS_TIER`   | `1`                | `2` also checks the extended set                 |
| `DARKROOM_CORPUS_QUICK`  | unset              | `1` skips the Tier-B (multi-develop) checks      |

Without a corpus both tests skip cleanly (`corpus_index` falls back to a synthetic mixed folder
authored by `core_raw::synth`, so it still has teeth).

## Recording goldens

```bash
scripts/fetch_raw_corpus.sh --tier 2
cargo run --release -p core-raw --example corpus_probe -- --record > tests/corpus/expected.toml
cargo run --release -p core-raw --example corpus_probe -- --diff     # golden vs actual table
```

`corpus_probe.rs` is both the recorder and — via `#[path]` — the implementation `tests/corpus.rs`
runs, so the two can never measure different things. Tests never write into the repository, which
is why recording is an example and not a `--bless` flag on the test.

The recorder measures whatever is **on disk**, both tiers by default (`--tier 1` restricts it).
**Fetch Tier 2 before re-recording:** the `> expected.toml` redirection truncates the file before the
recorder starts, so an entry that is not on disk loses its golden. The recorder prints a `WARNING`
naming every such entry, and the corpus test then fails that file with "no golden in expected.toml"
as soon as anyone runs at `DARKROOM_CORPUS_TIER=2`.

**A colour-pipeline change is supposed to move these numbers.** Re-record, and read the diff: it
names exactly which makers moved and by how much. A change that moves one maker and not the others
is the interesting case.

## What is checked

Tier A (every file, one develop each):

| assertion          | claim                                                                     |
| ------------------ | ------------------------------------------------------------------------- |
| `meta_identity`    | EXIF make/model contain the manifest's strings (case-insensitive)          |
| `wb_finite`        | as-shot WB is finite, positive, in `[0.05, 20]`, green normalised to 1     |
| `develop_dims`     | develop dimensions match the golden                                        |
| `catalog_dims`     | the thumbnailer's *display* dims (what the catalog stores) match them      |
| `native_dims`      | the thumbnailer's *native* dims are those dims or their transpose          |
| `orientation`      | EXIF orientation matches, and 5–8 means the develop is transposed          |
| `cfa`              | CFA pattern string matches (`""` for monochrome / sRAW / linear DNG)       |
| `levels`           | per-CFA-position black and white levels match                              |
| `patch_mean`       | centre 64×64 mean, linear ProPhoto — abs 2e-3 or rel 1 %                   |
| `highlight_chroma` | mean chroma of the brightest 0.05 % — abs 0.02                             |
| `thumbnail_sane`   | a JPEG (SOI) over 1 kB                                                     |
| `fingerprint_some` | the capture fingerprint is high-confidence                                 |

Tier B (`deep = true` only, three more develops each; skipped by `DARKROOM_CORPUS_QUICK=1`):

| assertion         | claim                                                                       |
| ----------------- | --------------------------------------------------------------------------- |
| `wb_identity`     | `develop_linear_wb(src, None)` is byte-identical to `develop_linear(src)`    |
| `preview_agrees`  | the half-res preview's centre patch is within 0.02 of the full develop's     |
| `clipped_neutral` | a mosaic forced to its saturation level develops neutral everywhere          |

`support = "unsupported"` files assert the opposite: `develop_linear` must return a typed
`RawError::Unsupported` and must not panic. `support = "panics"` files must be contained by
`core_raw::panic` (an escaped panic or `RawError::DecoderPanic`, never a clean develop).

## XFAIL policy

`xfail` lists assertion names that are **known-red today**, with the reason in a comment above the
entry. Two rules:

1. An assertion in `xfail` that starts **passing** fails the test ("remove it from xfail"). A fix
   must delete its exemption, or the next regression hides under a stale one.
2. `xfail` is for a divergence you have understood and decided not to fix yet — never for one you
   have not looked at. Write the reason down.

Current entries:

- **`canon-5d2-sraw1`** — `catalog_dims`, `native_dims`, `orientation`. Canon sRAW is the one format
  where the embedded preview and the mosaic describe different images: the preview is the full
  sensor readout (5616×3744) while the sRAW planes develop to 3861×2574. The catalog therefore
  stores dimensions the develop does not produce. Real inconsistency, surfaced by this corpus.

## Adding a sample

1. Find it in `https://raw.pixls.us/data/filelist.sha256` (the line is `<sha256> *<Make>/<Model>/<File>`).
2. Copy the path **exactly** — upstream casing is inconsistent (`Sony/ILCE-7M3` next to
   `SONY/ILCE-6400A`) and the manifest must not normalise it. Note that on macOS and Windows those
   two fold into one directory on disk; that is harmless, and `corpus_index.rs` compares
   case-insensitively because of it.
3. Add a `[[file]]` block with a comment saying what the file is *for*. One key per line — the
   fetch script parses the manifest with awk, and `corpus.rs::manifest_shape_is_parsable` enforces
   the shape and the 800 MB total budget.
4. Fetch, record, and commit `manifest.toml` + `expected.toml` together. Changing `manifest.toml`
   invalidates the CI download cache, so add deliberately.
