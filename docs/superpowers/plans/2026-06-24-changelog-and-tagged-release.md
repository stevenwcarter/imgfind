# CHANGELOG + Signed Tagged Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate `CHANGELOG.md` from Conventional Commits with git-cliff and cut a GPG-signed, tagged release with one command (`just release X.Y.Z`) that triggers the existing installer CI and publishes a GitHub Release whose notes come from the changelog.

**Architecture:** git-cliff (config `cliff.toml`) renders the changelog. A pure `scripts/bump-version.sh` bumps the three crate versions; a `just release` recipe wraps bump → changelog → signed commit → **signed tag** → push. `release.yml`'s publish job runs git-cliff for the new tag and uses that as the GitHub Release body.

**Tech Stack:** git-cliff, just, bash, GitHub Actions (`orhun/git-cliff-action`, `softprops/action-gh-release`), GPG tag signing.

**Spec:** `docs/superpowers/specs/2026-06-24-changelog-and-tagged-release-design.md`

## Global Constraints

- **Conventional Commits** drive the changelog; repo uses `feat`/`fix`/`build`/`ci`/`docs`(+scopes).
- Repo URL: `https://github.com/stevenwcarter/imgfind`. Tag pattern `v[0-9]*` (`vX.Y.Z`).
- **Tags MUST be GPG-signed** via `git tag -s` (global `tag.gpgsign` is **unset**, so `-a` would be unsigned). Signing key `3AECC1A61E1C08A7`, OpenPGP (`gpg.format` unset). Commits auto-sign (`commit.gpgsign = true`).
- **Workspace versions move together**: package `version` in all three of `Cargo.toml`, `imgfind-gui/Cargo.toml`, `imgfind-launcher/Cargo.toml` (currently `0.1.0`). The `clipper` **dependency** version (`clipper = { version = "0.1.0", path = ... }`) must **never** be touched by the bump.
- Explicit version arg (no auto-derivation). Recipe pushes with `--follow-tags`.
- GitHub Release notes come from git-cliff (`--latest`), not `generate_release_notes`.
- No crates.io publishing.
- Tooling/config: full local verification limited to git-cliff smoke + the bump bash test + `actionlint`; nothing in tests pushes/tags the real remote. The real release path is exercised only when the user runs `just release`.

> **Version-sensitivity:** git-cliff's `cliff.toml` template syntax and `orhun/git-cliff-action`'s inputs/outputs are version-specific. Tasks that touch them scaffold/verify against the installed tool / current action docs rather than trusting fixed snippets.

---

### Task 1: git-cliff config + initial CHANGELOG.md

**Files:**
- Create: `cliff.toml`
- Create: `CHANGELOG.md`

**Interfaces:**
- Produces: `cliff.toml` at repo root (consumed by the `just` recipes in Task 3 and `release.yml` in Task 4) and a baseline `CHANGELOG.md`.

- [ ] **Step 1: Install git-cliff and scaffold a version-correct config**

```bash
cargo install git-cliff --locked
git-cliff --version
# Scaffold a default config matching the INSTALLED version's template syntax,
# then we customize it (avoids template-version drift):
git-cliff --init
ls cliff.toml
```

- [ ] **Step 2: Customize `cliff.toml`**

Edit the scaffolded `cliff.toml` so it has these behaviors (keep the version's default `[changelog] body` template structure; only adjust what's below). The commit groups and skips:

```toml
[git]
conventional_commits = true
filter_unconventional = true
tag_pattern = "v[0-9]*"
sort_commits = "oldest"
commit_parsers = [
  { message = "^feat", group = "Features" },
  { message = "^fix", group = "Bug Fixes" },
  { message = "^perf", group = "Performance" },
  { message = "^refactor", group = "Refactor" },
  { message = "^docs", group = "Documentation" },
  { message = "^build", group = "Build & CI" },
  { message = "^ci", group = "Build & CI" },
  { message = "^chore\\(release\\):", skip = true },
  { message = "^chore", skip = true },
  { message = "^style", skip = true },
  { message = "^test", skip = true },
]
```

In `[changelog]`, ensure commit links point at this repo. If the scaffolded body template uses a `remote`/`repository` URL or a `commit.id` link, set the base to `https://github.com/stevenwcarter/imgfind`. (`filter_unconventional = true` already drops merge commits, which are non-conventional.)

- [ ] **Step 3: Generate the baseline CHANGELOG and verify it groups commits**

```bash
git-cliff -o CHANGELOG.md
git-cliff --unreleased
```
Expected: exit 0; `CHANGELOG.md` contains grouped sections (e.g. "Features", "Bug Fixes") populated from the existing history; no `chore(release)`/merge lines. If the template errors, fix the template per `git-cliff --help` / the installed version's docs and re-run.

- [ ] **Step 4: Commit**

```bash
git add cliff.toml CHANGELOG.md
git commit -m "build: git-cliff config + initial CHANGELOG"
```

---

### Task 2: `scripts/bump-version.sh` (pure version bump) + test

**Files:**
- Create: `scripts/bump-version.sh`
- Create: `tests/bump_version_test.sh`

**Interfaces:**
- Produces: `scripts/bump-version.sh X.Y.Z` — edits the package `version` line in all three workspace `Cargo.toml`s (start-of-line anchored, so the `clipper` dep is untouched); rejects non-semver with exit 2. Does NOT touch `Cargo.lock` (the Task 3 recipe refreshes the lock).

- [ ] **Step 1: Write the failing test**

Create `tests/bump_version_test.sh`:

```bash
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `bash tests/bump_version_test.sh`
Expected: FAIL — `scripts/bump-version.sh` does not exist yet (`cp` of a missing file errors under `set -e`, or the script run fails).

- [ ] **Step 3: Write `scripts/bump-version.sh`**

```bash
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
```

Then `chmod +x scripts/bump-version.sh`.

- [ ] **Step 4: Run test to verify it passes**

Run: `bash tests/bump_version_test.sh`
Expected: `PASS: bump-version bumps packages, preserves clipper dep, rejects bad semver`

- [ ] **Step 5: Lint + confirm the real repo Cargo.tomls are unchanged by the test**

Run: `bash -n scripts/bump-version.sh && git diff --quiet -- Cargo.toml imgfind-gui/Cargo.toml imgfind-launcher/Cargo.toml && echo "real Cargo.tomls untouched"`
Expected: prints `real Cargo.tomls untouched` (the test only edits sandbox copies). If shellcheck is installed, `shellcheck scripts/bump-version.sh tests/bump_version_test.sh`.

- [ ] **Step 6: Commit**

```bash
git add scripts/bump-version.sh tests/bump_version_test.sh
git commit -m "build: bump-version.sh (workspace version bump) with test"
```

---

### Task 3: `justfile` release + changelog recipes

**Files:**
- Modify: `justfile` (replace the `changelog` recipe; add a `release` recipe)

**Interfaces:**
- Consumes: `cliff.toml` (Task 1), `scripts/bump-version.sh` (Task 2).
- Produces: `just changelog` (regenerate `CHANGELOG.md`) and `just release X.Y.Z`.

- [ ] **Step 1: Replace the `changelog` recipe and add `release`**

In `justfile`, replace the current `changelog` recipe:

```make
changelog:
    npx standard-version --skip.bump --skip.commit --skip.tag --dry-run=false
```

with:

```make
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
```

(The `release` recipe is a shebang recipe so the whole body runs as one bash script with the guards.)

- [ ] **Step 2: Verify the justfile parses and lists both recipes**

Run: `just --list`
Expected: shows `changelog` and `release version` among the recipes, no parse error.

- [ ] **Step 3: Verify the recipe's guards reject bad input WITHOUT mutating the repo**

Run: `just release 1.2 ; echo "exit=$?"` then `git status --porcelain`
Expected: prints the semver error and `exit=2`; `git status --porcelain` is empty (no files changed, no tag created). Do NOT run `just release` with a valid version here — that would tag and push for real.

- [ ] **Step 4: Commit**

```bash
git add justfile
git commit -m "build: just release recipe (signed tag) + git-cliff changelog recipe"
```

---

### Task 4: `release.yml` — git-cliff release notes

**Files:**
- Modify: `.github/workflows/release.yml` (the `release` job, currently lines 103-116)

**Interfaces:**
- Consumes: `cliff.toml` (Task 1), the pushed `v*` tag.
- Produces: a `release` job that publishes the GitHub Release with the new tag's changelog section as the body.

> **Action interface is version-specific.** Before editing, check the current
> `orhun/git-cliff-action` README (Context7: `orhun/git-cliff-action`, or
> https://github.com/orhun/git-cliff-action) for how it returns the rendered
> changelog (the `OUTPUT` env var and/or `steps.<id>.outputs.content`) and
> reconcile the snippet below with it.

- [ ] **Step 1: Replace the `release` job**

Replace the current `release` job (the block starting at `  release:` through the `generate_release_notes: true` line) with:

```yaml
  release:
    needs: build
    if: startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0   # full history + tags for git-cliff

      - name: Generate release notes (this tag's changelog section)
        id: git-cliff
        uses: orhun/git-cliff-action@v4
        with:
          config: cliff.toml
          args: --latest --strip header
        env:
          OUTPUT: RELEASE_NOTES.md
          GITHUB_REPO: ${{ github.repository }}

      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true

      - uses: softprops/action-gh-release@v2
        with:
          files: dist/*
          body_path: RELEASE_NOTES.md
```

(If the action README shows the rendered notes are exposed as `steps.git-cliff.outputs.content` rather than a file, use `body: ${{ steps.git-cliff.outputs.content }}` instead of `body_path`. Keep whichever the current action documents; do not use both.)

- [ ] **Step 2: Lint the workflow**

Run: `command -v actionlint >/dev/null && actionlint .github/workflows/release.yml || docker run --rm -v "$PWD":/repo -w /repo rhysd/actionlint:latest -color .github/workflows/release.yml`
Expected: exit 0, no errors. Also `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))"` confirms valid YAML. If neither actionlint nor Docker is available, note it — the repo's `actionlint.yml` workflow lints on PR.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: source GitHub Release notes from git-cliff (drop generate_release_notes)"
```

---

### Task 5: Docs

**Files:**
- Modify: `CLAUDE.md` (add a "Releasing" note)
- Modify: `README.md` (one-line pointer)

**Interfaces:** none (docs only).

- [ ] **Step 1: Add the CLAUDE.md "Releasing" note**

Add after the "Packaging / Release" section (or "Build & run" if that's clearer):

```markdown
## Releasing

Cut a release with `just release X.Y.Z`: it bumps the workspace version
(`scripts/bump-version.sh`, all three crates together — the `clipper` dep is
left alone), regenerates `CHANGELOG.md` with **git-cliff** (config `cliff.toml`,
from Conventional Commits), makes a signed `chore(release)` commit, creates a
**GPG-signed** tag `vX.Y.Z` (`git tag -s` — `tag.gpgsign` is unset so signing is
explicit), and pushes. The tag triggers `.github/workflows/release.yml`, which
builds the installers and publishes a GitHub Release whose notes are that tag's
`CHANGELOG.md` section (via `orhun/git-cliff-action`). `just changelog`
regenerates the changelog without releasing. The tag shows "Verified" on GitHub
only if the signing public key (`3AECC1A61E1C08A7`) is uploaded to the GitHub
account. Needs `git-cliff` (`cargo install git-cliff`). No crates.io publishing.
```

- [ ] **Step 2: Add the README pointer**

In `README.md`, near the existing Installation/release context, add:

```markdown
### Cutting a release (maintainers)

`just release X.Y.Z` bumps the version, regenerates `CHANGELOG.md` (git-cliff),
creates a GPG-signed `vX.Y.Z` tag, and pushes — triggering the installer build
and a GitHub Release. Requires `git-cliff` (`cargo install git-cliff`).
```

- [ ] **Step 3: Verify**

Run: `grep -n "## Releasing" CLAUDE.md && grep -n "Cutting a release" README.md`
Expected: both present.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: document the just release / changelog workflow"
```

---

## Final: review + manual acceptance + finish branch

- [ ] Dispatch the final code-reviewer over the whole branch diff.
- [ ] **Manual acceptance (the controller/user, not in CI):** on a clean tree, the real end-to-end is `just release <next-version>` — which signs a tag and pushes, triggering a real release. Flag to the user that this is theirs to run when they want to cut the first real release; optionally a dry validation is `just release 1.2` (must abort) and `git-cliff --latest` locally. CI's git-cliff notes step is only exercised by a real tag push.
- [ ] Invoke `superpowers:finishing-a-development-branch`.

## Self-Review notes

- **Spec coverage:** cliff.toml + CHANGELOG (T1); bump script + clipper-dep invariant test (T2); `just release` signed-tag recipe + `just changelog` (T3); release.yml git-cliff notes (T4); docs incl. signing + Verified note (T5). Signing requirement (`git tag -s`) in Global Constraints + T3. All spec sections map to a task.
- **No-placeholder check:** full file content given for the script, test, recipe, and workflow job; the two version-sensitive surfaces (cliff.toml template, git-cliff-action interface) carry an explicit scaffold/reconcile step rather than a guessed snippet — by design, not a placeholder.
- **Type/name consistency:** `scripts/bump-version.sh` and tag name `vX.Y.Z` consistent across T2/T3/T4/T5; `cliff.toml` path consistent across T1/T3/T4; the `release` recipe refreshes `Cargo.lock` (cargo update), which T2's script deliberately omits.
- **Known soft spots (flagged inline):** git-cliff template + action interface are version-specific (scaffold/reconcile steps); the live `just release` path can't be tested without a real tag/push, so it's review-gated + listed as manual acceptance.
```
