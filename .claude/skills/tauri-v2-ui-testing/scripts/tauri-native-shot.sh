#!/usr/bin/env bash
# Tier 2: screenshot the REAL running Tauri app window on macOS (real render), no extra deps.
# Usage: tauri-native-shot.sh [output.png] [target]
#   output.png : default /tmp/tauri-native.png
#   target     : optional override — a bundle id (built .app) OR a process name (dev binary).
#
# IMPORTANT: a `tauri dev` build is a BARE binary (target/debug/<name>), NOT a .app bundle, so it has
# NO bundle identifier — System Events sees it only by process name (= the cargo `[package] name`).
# A `tauri build` .app DOES have the bundle id. So this script tries the bundle id first, then falls
# back to process-name candidates (cargo bin name, productName). It targets a process precisely (no
# `tell application "<name>"`, which could launch a same-named app like the App Store "Darkroom").
#
# Requires the app running, and macOS permissions for your terminal: Automation→System Events and
# Screen Recording (System Settings > Privacy & Security). Without Screen Recording the capture is
# blank/black; without Automation the bounds query returns empty.
set -uo pipefail

OUT="${1:-/tmp/tauri-native.png}"
OVERRIDE="${2:-}"
ST="src-tauri"
[ -d "$ST" ] || ST="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)/src-tauri"

BID=""
NAMES=()
if [ -n "$OVERRIDE" ]; then
  case "$OVERRIDE" in *.*) BID="$OVERRIDE" ;; *) NAMES+=("$OVERRIDE") ;; esac
fi
# bundle id (built .app) + product name from tauri.conf.json
CONF_BID="$(node -e "try{process.stdout.write(require('$ST/tauri.conf.json').identifier||'')}catch(e){}" 2>/dev/null || true)"
PRODUCT="$(node -e "try{process.stdout.write(require('$ST/tauri.conf.json').productName||'')}catch(e){}" 2>/dev/null || true)"
# dev binary/process name = cargo [package] name
CARGO_NAME="$(grep -m1 '^name = ' "$ST/Cargo.toml" 2>/dev/null | sed -E 's/.*"([^"]+)".*/\1/')"
[ -z "$BID" ] && BID="$CONF_BID"
[ -n "$CARGO_NAME" ] && NAMES+=("$CARGO_NAME")
[ -n "$PRODUCT" ] && NAMES+=("$PRODUCT")

# Find the running process (by bundle id, then by each name), focus it, return "x,y,w,h".
bounds_for() { # $1 = AppleScript predicate
  osascript -e "tell application \"System Events\"
    set ps to (every process whose $1)
    if (count of ps) is 0 then return \"\"
    set p to item 1 of ps
    set frontmost of p to true
    delay 0.3
    set b to {position, size} of front window of p
    return ((item 1 of (item 1 of b)) as text) & \",\" & ((item 2 of (item 1 of b)) as text) & \",\" & ((item 1 of (item 2 of b)) as text) & \",\" & ((item 2 of (item 2 of b)) as text)
  end tell" 2>/dev/null
}

COORDS=""
[ -n "$BID" ] && COORDS="$(bounds_for "bundle identifier is \"$BID\"")"
if [ -z "$COORDS" ]; then
  for n in "${NAMES[@]}"; do
    COORDS="$(bounds_for "name is \"$n\"")"
    [ -n "$COORDS" ] && break
  done
fi

if [ -z "$COORDS" ]; then
  echo "Could not find/read the app window. Tried bundle id '$BID' and names: ${NAMES[*]:-<none>}." >&2
  echo "Ensure the app is running (npm run tauri dev) and your terminal has Automation (System" >&2
  echo "Events) + Screen Recording permission (System Settings > Privacy & Security)." >&2
  exit 1
fi

IFS=',' read -r X Y W H <<<"$COORDS"
screencapture -o -x -R"${X},${Y},${W},${H}" "$OUT" # -o no shadow, -x silent, -R region
echo "$OUT"
