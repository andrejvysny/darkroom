#!/usr/bin/env bash
# Shared repository checks: the single ordered checklist every green commit must pass.
# Used by .github/workflows/ci.yml (macOS job, `gpu` mode) and the release.yml exact-tag gate.
#
# Usage:
#   bash scripts/repo-checks.sh [headless|gpu]
#
#   headless  default; does not require fixtures or a GPU (skips are allowed, not enforced).
#   gpu       the designated pixel-validation lane: exports DARKROOM_REQUIRE_FIXTURES=1 and
#             DARKROOM_REQUIRE_GPU=1 before the workspace test run, so a missing Metal adapter
#             or committed fixture is a hard failure instead of a silently-skipped test.
#
# No network access, no release/publish commands — this script only checks the tree.
set -euo pipefail

mode="${1:-headless}"
case "$mode" in
  headless | gpu) ;;
  *)
    echo "usage: $(basename "$0") [headless|gpu]" >&2
    exit 2
    ;;
esac

# Run from the repo root regardless of the caller's cwd.
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

step() {
  printf '\n=== %s ===\n' "$*"
}

step "cargo fmt --all -- --check"
cargo fmt --all -- --check

step "npm run e2e:check-native"
npm run e2e:check-native

step "cargo xtask parity"
cargo xtask parity

step "cargo xtask layers"
cargo xtask layers

step "cargo test -p xtask"
cargo test -p xtask

step "cargo clippy --workspace --examples -- -D warnings"
cargo clippy --workspace --examples -- -D warnings

step "cargo clippy -p darkroom-native -p render-wgpu --all-targets -- -D warnings"
cargo clippy -p darkroom-native -p render-wgpu --all-targets -- -D warnings

if [ "$mode" = "gpu" ]; then
  export DARKROOM_REQUIRE_FIXTURES=1 DARKROOM_REQUIRE_GPU=1
else
  # Headless must never inherit a strict environment from the caller: the mode name is the
  # contract, and a "headless" run that accidentally enforced GPU would be indistinguishable
  # from the gpu lane.
  unset DARKROOM_REQUIRE_FIXTURES DARKROOM_REQUIRE_GPU
fi

step "cargo test --workspace (mode: $mode)"
cargo test --workspace

echo
echo "repo-checks: all steps passed (mode: $mode)"
