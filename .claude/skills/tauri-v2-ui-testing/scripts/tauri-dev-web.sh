#!/usr/bin/env bash
# Serve a Tauri v2 app's web frontend for browser testing, then print the dev URL. Idempotent.
# Reads the dev URL and the frontend start command from src-tauri/tauri.conf.json
# (build.devUrl / build.beforeDevCommand), falling back to localhost:1420 + `npm run dev`.
#
# A plain-browser visit to the dev URL gets the mock backend (no Tauri runtime in the browser).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
cd "$ROOT"
CONF="src-tauri/tauri.conf.json"

META="$(node -e "try{const c=require('./$CONF');const b=c.build||{};process.stdout.write((b.devUrl||'http://localhost:1420')+'\t'+(b.beforeDevCommand||'npm run dev'))}catch(e){process.stdout.write('http://localhost:1420\tnpm run dev')}" 2>/dev/null || printf 'http://localhost:1420\tnpm run dev')"
DEVURL="${META%%$'\t'*}"
CMD="${META#*$'\t'}"

up() { curl -sf -o /dev/null "$DEVURL/" 2>/dev/null || curl -sf -o /dev/null "$DEVURL" 2>/dev/null; }

if up; then
  echo "$DEVURL (already serving)"
  exit 0
fi

echo "starting frontend: '$CMD'  (in $ROOT)" >&2
nohup sh -c "$CMD" >/tmp/tauri-dev-web.log 2>&1 &

for _ in $(seq 1 90); do
  if up; then
    echo "$DEVURL (started)"
    exit 0
  fi
  sleep 1
done

echo "ERROR: $DEVURL did not come up within 90s; see /tmp/tauri-dev-web.log" >&2
exit 1
