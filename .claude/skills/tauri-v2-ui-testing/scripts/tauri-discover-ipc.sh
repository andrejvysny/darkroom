#!/usr/bin/env bash
# Discover the Tauri IPC surface a mock must satisfy. Pass a frontend source dir (default: auto).
# Lists: invoke command names, event names, @tauri-apps imports (plugins/APIs), custom protocols,
# and convertFileSrc usage. Heuristic but a fast starting point for filling in tauriMock.ts.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." 2>/dev/null && pwd || pwd)"
DIR="${1:-}"
if [ -z "$DIR" ]; then
  for d in src app ui frontend client www; do
    [ -d "$ROOT/$d" ] && DIR="$ROOT/$d" && break
  done
  DIR="${DIR:-$ROOT}"
fi
echo "Scanning frontend: $DIR"
INC=(--include=*.ts --include=*.tsx --include=*.js --include=*.jsx --include=*.mjs --include=*.vue --include=*.svelte)
G() { grep -rhoE "${INC[@]}" "$1" "$DIR" 2>/dev/null || true; }

echo
echo "## invoke commands (-> one HANDLERS entry each)"
G "invoke(<[^>]*>)?\(\s*[\"'\`][^\"'\`]+[\"'\`]" \
  | sed -E "s/.*[\"'\`]([^\"'\`]+)[\"'\`].*/\1/" | sort -u

echo
echo "## event names (listen/once/emit -> need shouldMockEvents)"
G "(listen|once|emit)\s*(<[^>]*>)?\(\s*[\"'\`][^\"'\`]+[\"'\`]" \
  | sed -E "s/.*[\"'\`]([^\"'\`]+)[\"'\`].*/\1/" | sort -u

echo
echo "## @tauri-apps imports (plugins/APIs in use -> plugin:* are no-op'd by the mock)"
G "@tauri-apps/[a-zA-Z0-9/_-]+" | sort -u

echo
echo "## custom protocols (scheme:// used as resource URLs -> need a URL-builder hook)"
G "[a-z][a-z0-9.+-]*://[^\"'\`) ]+" \
  | grep -ivE "^https?://|^ws://|^wss://|^file://|w3\.org|schemas\." \
  | sed -E "s#^([a-z][a-z0-9.+-]*://).*#\1#" | sort -u

echo
echo "## convertFileSrc usage (-> mockConvertFileSrc handles it)"
G "convertFileSrc" | sort -u | head -5
