test:
    watchexec -e rs,toml cargo test

cover:
    cargo llvm-cov --lcov --output-path lcov.info

# Regenerate CHANGELOG.md from the full history (preview / refresh).
changelog:
    git-cliff -o CHANGELOG.md

# Cut a signed, tagged release: bump version, regenerate changelog, signed
# commit + signed tag, push (triggers the installer release workflow).
# Usage: just release 0.2.0
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{version}}"
    if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      echo "error: version must be semver X.Y.Z (got '$VERSION')" >&2; exit 2
    fi
    command -v git-cliff >/dev/null || { echo "error: git-cliff not found — install: cargo install git-cliff" >&2; exit 1; }
    if ! git diff --quiet || ! git diff --cached --quiet; then
      echo "error: working tree not clean; commit or stash first" >&2; exit 1
    fi
    if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
      echo "error: tag v$VERSION already exists" >&2; exit 1
    fi
    scripts/bump-version.sh "$VERSION"
    cargo update -p imgfind -p imgfind-gui -p imgfind-launcher
    git-cliff --tag "v$VERSION" -o CHANGELOG.md
    git add -A
    git commit -m "chore(release): v$VERSION"
    git tag -s "v$VERSION" -m "v$VERSION"
    git push --follow-tags
    echo "released v$VERSION — signed tag pushed; release.yml will build installers"
