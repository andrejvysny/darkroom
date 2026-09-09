#!/usr/bin/env bash
#
# Download the multi-maker RAW test corpus described by `tests/corpus/manifest.toml`.
#
#   scripts/fetch_raw_corpus.sh [--tier 1|2] [--verify-only]
#
# Files land in $DARKROOM_RAW_CORPUS (default `<repo>/target/raw-corpus`), mirroring the upstream
# `<Make>/<Model>/<File>` layout, and are never committed. Nothing is downloaded twice: a file whose
# sha256 already matches the manifest is left alone, so a warm cache is a no-op.
#
# Every sample is CC0, from https://raw.pixls.us/. The manifest path must still be present in the
# LIVE `filelist.sha256` before anything is fetched — that is what stops a typo (or a stale manifest
# pointing at a since-removed sample) from silently pulling the wrong bytes.
#
# macOS ships bash 3.2, so: no associative arrays, no `mapfile`, no `${var^^}`.

set -euo pipefail

TIER=1
VERIFY_ONLY=0

usage() {
    awk 'NR > 1 { if ($0 !~ /^#/) exit; sub(/^# ?/, ""); print }' "$0"
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --tier)
            TIER="${2:-}"
            shift 2
            ;;
        --tier=*)
            TIER="${1#--tier=}"
            shift
            ;;
        --verify-only)
            VERIFY_ONLY=1
            shift
            ;;
        -h | --help) usage 0 ;;
        *)
            echo "unknown argument: $1" >&2
            usage 1
            ;;
    esac
done

case "$TIER" in
    1 | 2) ;;
    *)
        echo "--tier must be 1 or 2 (got '$TIER')" >&2
        exit 2
        ;;
esac

REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
MANIFEST="$REPO_ROOT/tests/corpus/manifest.toml"
CORPUS="${DARKROOM_RAW_CORPUS:-$REPO_ROOT/target/raw-corpus}"

[ -f "$MANIFEST" ] || {
    echo "manifest not found: $MANIFEST" >&2
    exit 2
}

# sha256 tool: coreutils on Linux, BSD-ish `shasum` on macOS.
if command -v sha256sum > /dev/null 2>&1; then
    sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum > /dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    echo "need sha256sum or shasum on PATH" >&2
    exit 2
fi

# Percent-encode a path for the download URL. Upstream names carry spaces and parentheses
# ("Nikon/Z 9/..._(Lossy_High_Efficiency).NEF"); `/` stays a separator.
urlencode() {
    local s=$1 out= i=0 c
    while [ "$i" -lt "${#s}" ]; do
        c=${s:$i:1}
        case "$c" in
            [a-zA-Z0-9._~/-]) out="$out$c" ;;
            *) out="$out$(printf '%%%02X' "'$c")" ;;
        esac
        i=$((i + 1))
    done
    printf '%s' "$out"
}

# `tier<TAB>sha256<TAB>path` per manifest entry. Single-line `key = "value"` only — the shape
# `corpus.rs::manifest_shape_is_parsable` pins.
parse_manifest() {
    awk '
        function unquote(line) {
            sub(/^[^=]*=[ \t]*/, "", line)
            sub(/[ \t]*$/, "", line)
            gsub(/^"|"$/, "", line)
            return line
        }
        function emit() { if (path != "") print tier "\t" sha "\t" path }
        /^\[\[file\]\]/ { emit(); path=""; sha=""; tier=""; next }
        /^[ \t]*path[ \t]*=/   { path = unquote($0); next }
        /^[ \t]*sha256[ \t]*=/ { sha  = unquote($0); next }
        /^[ \t]*tier[ \t]*=/   { tier = unquote($0); next }
        END { emit() }
    ' "$1"
}

SOURCE_BASE=$(awk -F'"' '/^source_base[ \t]*=/ { print $2; exit }' "$MANIFEST")
FILELIST_URL=$(awk -F'"' '/^filelist[ \t]*=/ { print $2; exit }' "$MANIFEST")
[ -n "$SOURCE_BASE" ] && [ -n "$FILELIST_URL" ] || {
    echo "manifest header is missing source_base / filelist" >&2
    exit 2
}

mkdir -p "$CORPUS"
LIVE_LIST="$CORPUS/filelist.sha256"

# One index download per invocation — it is the authority on what upstream actually holds.
if [ "$VERIFY_ONLY" -eq 0 ]; then
    curl -fL --retry 3 --retry-delay 2 --connect-timeout 20 -s -o "$LIVE_LIST.part" "$FILELIST_URL"
    mv "$LIVE_LIST.part" "$LIVE_LIST"
fi

# sha256 the live index records for `path`, or empty when upstream does not have it.
# Lines are `<sha256> *<path>`; the comparison is exact, never a substring.
live_sha_for() {
    awk -v want="*$1" '
        { n = index($0, " *"); if (n > 0 && substr($0, n + 1) == want) { print substr($0, 1, n - 1); exit } }
    ' "$LIVE_LIST"
}

started=$(date +%s)
n_ok=0
n_fetched=0
n_cached=0
n_skipped=0
n_failed=0
bytes_total=0

while IFS="$(printf '\t')" read -r tier sha path; do
    [ -n "${path:-}" ] || continue
    if [ "$tier" -gt "$TIER" ] 2> /dev/null; then
        n_skipped=$((n_skipped + 1))
        continue
    fi

    dest="$CORPUS/$path"

    if [ -f "$dest" ] && [ "$(sha256_of "$dest")" = "$sha" ]; then
        n_cached=$((n_cached + 1))
        n_ok=$((n_ok + 1))
        bytes_total=$((bytes_total + $(wc -c < "$dest")))
        continue
    fi

    if [ "$VERIFY_ONLY" -eq 1 ]; then
        if [ -f "$dest" ]; then
            echo "CORRUPT  $path (sha256 mismatch)" >&2
        else
            echo "MISSING  $path" >&2
        fi
        n_failed=$((n_failed + 1))
        continue
    fi

    if [ ! -f "$LIVE_LIST" ]; then
        echo "no live filelist at $LIVE_LIST" >&2
        exit 1
    fi
    upstream_sha=$(live_sha_for "$path")
    if [ -z "$upstream_sha" ]; then
        echo "REFUSED  $path — not in the live filelist ($FILELIST_URL)" >&2
        n_failed=$((n_failed + 1))
        continue
    fi
    if [ "$upstream_sha" != "$sha" ]; then
        echo "STALE    $path — upstream sha256 is $upstream_sha, manifest says $sha" >&2
        n_failed=$((n_failed + 1))
        continue
    fi

    mkdir -p "$(dirname "$dest")"
    url="$SOURCE_BASE/$(urlencode "$path")"
    echo "fetch    $path"
    if ! curl -fL --retry 3 --retry-delay 2 --connect-timeout 20 -sS -o "$dest.part" "$url"; then
        echo "FAILED   $path — download error" >&2
        rm -f "$dest.part"
        n_failed=$((n_failed + 1))
        continue
    fi
    got=$(sha256_of "$dest.part")
    if [ "$got" != "$sha" ]; then
        echo "FAILED   $path — sha256 $got != $sha" >&2
        rm -f "$dest.part"
        n_failed=$((n_failed + 1))
        continue
    fi
    mv "$dest.part" "$dest"
    n_fetched=$((n_fetched + 1))
    n_ok=$((n_ok + 1))
    bytes_total=$((bytes_total + $(wc -c < "$dest")))
done << EOF
$(parse_manifest "$MANIFEST")
EOF

elapsed=$(($(date +%s) - started))
mb=$((bytes_total / 1000000))
echo "corpus: $n_ok/$((n_ok + n_failed)) files ok (${mb} MB) in ${CORPUS} — fetched $n_fetched, cached $n_cached, tier>${TIER} skipped $n_skipped, failed $n_failed, ${elapsed}s"

[ "$n_failed" -eq 0 ] || exit 1
