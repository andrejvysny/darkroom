#!/usr/bin/env bash
# Stage the libheif dylib closure for .app/.dmg bundling (macOS only; no-op elsewhere).
#
# Why this exists: tauri-bundler copies `bundle.macOS.frameworks` entries into
# Contents/Frameworks and code-signs them, but it never rewrites install names — a binary
# linked against Homebrew's /opt/homebrew/... (or /usr/local/... on Intel) paths would still
# dlopen the machine-local copies (and fail on machines without Homebrew libheif). Additionally,
# tauri-build VALIDATES the `frameworks` paths at compile time, so the staged dylibs must exist
# for any `cargo check/test/build` of src-tauri, not just for bundling. Hence two modes:
#
#   stage   — copy the closure into src-tauri/frameworks/ under VERSION-STRIPPED stable names
#             (libx265.215.dylib → libx265.dylib, so the static `frameworks` list survives
#             Homebrew version bumps and the arm64/x86_64 runner split) and rewrite each copy's
#             id + inter-references to @executable_path/../Frameworks/<stable>. Needs no built
#             binary; wired into `build.beforeBuildCommand` and the CI test job.
#   bundle  — stage (idempotent), then repoint the freshly linked release binary at the staged
#             names. Wired into `build.beforeBundleCommand`; the bundler signs inside-out AFTER
#             this hook, so the rewrites end up under a valid (ad-hoc) signature.
#
# If Homebrew ever changes the closure membership, staging fails loudly — update EXPECTED here
# and `bundle.macOS.frameworks` in src-tauri/tauri.conf.json together.
#   fix-adhoc — AFTER a local (unsigned) `tauri build --bundles app`: re-sign the bundled .app
#             ad-hoc WITHOUT the hardened runtime. tauri-bundler's ad-hoc signature sets the
#             hardened-runtime flag, and library validation then refuses the ad-hoc Frameworks
#             dylibs at launch (dyld: "code signature ... different Team IDs"). CI Developer ID
#             builds are unaffected (every Mach-O carries the same real Team ID) and must NOT run
#             this — notarization requires the hardened runtime.
set -euo pipefail

[ "$(uname)" = "Darwin" ] || exit 0
MODE="${1:-bundle}"

if [ "$MODE" = "fix-adhoc" ]; then
  APP="${2:-target/release/bundle/macos/Darkroom.app}"
  [ -d "$APP" ] || { echo "macos-bundle-dylibs: no .app at $APP" >&2; exit 1; }
  if codesign -dv "$APP/Contents/MacOS/darkroom" 2>&1 | grep -q 'Signature=adhoc'; then
    for lib in "$APP"/Contents/Frameworks/*.dylib; do codesign --force -s - "$lib"; done
    codesign --force -s - "$APP"
    codesign --verify --strict "$APP"
    echo "macos-bundle-dylibs: re-signed $APP ad-hoc without hardened runtime"
  else
    echo "macos-bundle-dylibs: $APP is not ad-hoc signed — leaving it alone" >&2
  fi
  exit 0
fi

# Version-stripped stable name: libheif.1.dylib → libheif.dylib.
stable() { basename "$1" | sed -E 's/\.[0-9]+(\.[0-9]+)*\.dylib$/.dylib/'; }

# Resolve the real (versioned) libheif dylib from Homebrew.
heif_root=""
if command -v brew >/dev/null 2>&1; then
  prefix="$(brew --prefix libheif 2>/dev/null || true)"
  if [ -n "$prefix" ]; then
    heif_root="$(find "$prefix/lib" -name 'libheif.*.dylib' -type f 2>/dev/null | head -1)"
  fi
fi
if [ -z "$heif_root" ]; then
  echo "macos-bundle-dylibs: Homebrew libheif not found (brew install libheif)" >&2
  exit 1
fi

# BFS the non-system closure (skip /usr/lib + /System). Dedup by STABLE name — the same lib can
# appear both as its on-disk path (libheif.1.21.1.dylib) and as its install id (libheif.1.dylib).
queue=("$heif_root")
closure=()
seen=""
while [ ${#queue[@]} -gt 0 ]; do
  lib="${queue[0]}"; queue=("${queue[@]:1}")
  s="$(stable "$lib")"
  case " $seen " in *" $s "*) continue ;; esac
  seen="$seen $s"
  closure+=("$lib")
  while IFS= read -r dep; do
    case "$dep" in /usr/lib/*|/System/*|"$lib") continue ;; esac
    queue+=("$dep")
  done < <(otool -L "$lib" | tail -n +2 | awk '{print $1}')
done

EXPECTED="libaom.dylib libde265.dylib libheif.dylib libsharpyuv.dylib libvmaf.dylib libx265.dylib"
actual=$(for l in "${closure[@]}"; do stable "$l"; done | sort | tr '\n' ' ' | sed 's/ $//')
if [ "$actual" != "$EXPECTED" ]; then
  echo "macos-bundle-dylibs: dylib closure changed." >&2
  echo "  expected: $EXPECTED" >&2
  echo "  actual:   $actual" >&2
  echo "Update EXPECTED in this script AND bundle.macOS.frameworks in src-tauri/tauri.conf.json." >&2
  exit 1
fi

STAGE="src-tauri/frameworks"
mkdir -p "$STAGE"

# Repoint every non-system reference EMBEDDED in the given Mach-O to the staged stable names.
# Iterating the file's actual otool -L strings (rather than the closure's first-seen paths)
# matters: two closure members can reference the same lib under different version paths, and
# `install_name_tool -change` only rewrites an exact string match.
repoint() { # $1 = mach-o file
  while IFS= read -r dep; do
    case "$dep" in /usr/lib/*|/System/*|@*) continue ;; esac
    install_name_tool -change "$dep" "@executable_path/../Frameworks/$(stable "$dep")" "$1" 2>/dev/null
  done < <(otool -L "$1" | tail -n +2 | awk '{print $1}')
}

for lib in "${closure[@]}"; do
  out="$STAGE/$(stable "$lib")"
  cp -f "$lib" "$out"
  chmod u+w "$out"
  install_name_tool -id "@executable_path/../Frameworks/$(stable "$lib")" "$out" 2>/dev/null
  repoint "$out"
  # The rewrites invalidate Homebrew's signature, and an ad-hoc ("-") bundle does NOT re-sign
  # Frameworks — dyld then kills the .app at launch ("code signature not valid for use in
  # process"). Leave a fresh ad-hoc signature; the CI Developer ID pass force-re-signs over it.
  codesign --force -s - "$out" 2>/dev/null
done
echo "macos-bundle-dylibs: staged ${#closure[@]} dylibs into $STAGE"

[ "$MODE" = "stage" ] && exit 0

# bundle mode: repoint the freshly linked binary (it links libheif directly; -change on absent
# refs is a no-op).
BIN="target/${TAURI_ENV_TARGET_TRIPLE:-}/release/darkroom"
[ -f "$BIN" ] || BIN="target/release/darkroom"
[ -f "$BIN" ] || { echo "macos-bundle-dylibs: binary not found (target/*/release/darkroom)" >&2; exit 1; }
repoint "$BIN"
echo "macos-bundle-dylibs: rewrote $BIN"
