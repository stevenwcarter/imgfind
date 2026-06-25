#!/usr/bin/env bash
# Bump the package version in all three workspace crates to the given semver.
# Pure: edits Cargo.toml files only; the release recipe refreshes Cargo.lock.
set -euo pipefail

VERSION="${1:-}"
if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "usage: bump-version.sh X.Y.Z (semver)" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
for f in Cargo.toml imgfind-gui/Cargo.toml imgfind-launcher/Cargo.toml; do
  # Replace ONLY the first start-of-line `version = "..."` (the package version).
  # The clipper dependency version lives inside an inline table and never starts
  # the line, so it is never matched.
  sed -i -E "0,/^version = \"[^\"]*\"/s//version = \"$VERSION\"/" "$ROOT/$f"
done
echo "bumped package versions to $VERSION"
