#!/usr/bin/env bash
# Sandboxed test: install.sh must install prebuilt binaries that sit beside it
# (tarball layout), with no target/release present.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Simulate an extracted tarball: install.sh + three fake binaries side by side.
PKG="$WORK/pkg"
mkdir -p "$PKG/packaging"
cp "$REPO_ROOT/install.sh" "$PKG/install.sh"
cp "$REPO_ROOT/packaging/imgfind-launcher.desktop" "$PKG/packaging/imgfind-launcher.desktop"
for b in imgfind imgfind-gui imgfind-launcher; do
  printf '#!/bin/sh\necho %s\n' "$b" > "$PKG/$b"
  chmod +x "$PKG/$b"
done

# Run install.sh from the package dir with a sandboxed HOME and no target/release.
FAKE_HOME="$WORK/home"
mkdir -p "$FAKE_HOME"
( cd "$PKG" && HOME="$FAKE_HOME" bash ./install.sh >/dev/null )

fail=0
for b in imgfind imgfind-gui imgfind-launcher; do
  if [ ! -x "$FAKE_HOME/.local/bin/$b" ]; then
    echo "MISSING: $b not installed to ~/.local/bin"; fail=1
  fi
done
[ "$fail" -eq 0 ] && echo "PASS: all three binaries installed from tarball layout"
exit "$fail"
