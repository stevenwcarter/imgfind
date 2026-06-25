#!/usr/bin/env bash
# Sandboxed test for scripts/bump-version.sh: bumps all three package versions,
# leaves the clipper dependency version untouched, rejects bad semver.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Mirror the workspace layout with the real Cargo.toml files + the script.
mkdir -p "$WORK/scripts" "$WORK/imgfind-gui" "$WORK/imgfind-launcher"
cp "$REPO_ROOT/scripts/bump-version.sh" "$WORK/scripts/"
cp "$REPO_ROOT/Cargo.toml" "$WORK/Cargo.toml"
cp "$REPO_ROOT/imgfind-gui/Cargo.toml" "$WORK/imgfind-gui/Cargo.toml"
cp "$REPO_ROOT/imgfind-launcher/Cargo.toml" "$WORK/imgfind-launcher/Cargo.toml"

bash "$WORK/scripts/bump-version.sh" 9.9.9

fail=0
for f in Cargo.toml imgfind-gui/Cargo.toml imgfind-launcher/Cargo.toml; do
  if ! grep -Eq '^version = "9\.9\.9"' "$WORK/$f"; then
    echo "FAIL: package version not bumped in $f"; fail=1
  fi
done
# The clipper dependency version must be unchanged (still 0.1.0, inside an inline table).
if ! grep -Eq 'clipper = \{ version = "0\.1\.0"' "$WORK/Cargo.toml"; then
  echo "FAIL: clipper dependency version was modified in Cargo.toml"; fail=1
fi
# Bad semver must be rejected with non-zero exit.
if bash "$WORK/scripts/bump-version.sh" 1.2 2>/dev/null; then
  echo "FAIL: bad semver '1.2' was accepted"; fail=1
fi

[ "$fail" -eq 0 ] && echo "PASS: bump-version bumps packages, preserves clipper dep, rejects bad semver"
exit "$fail"
