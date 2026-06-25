# CHANGELOG generation + one-command signed tagged release

**Date:** 2026-06-24
**Status:** Approved (brainstorm)
**Topic:** Generate a `CHANGELOG.md` from Conventional Commits and cut a signed, tagged release with one command that triggers the existing installer CI.

## Goal

Make releasing imgfind a one-liner: `just release X.Y.Z` bumps the workspace
version, regenerates `CHANGELOG.md`, commits, creates a **GPG-signed** tag, and
pushes — which triggers the existing `release.yml` to build the macOS/Windows/Linux
installers and publish a GitHub Release whose notes come from the changelog. No
crates.io publishing.

## Decisions (locked in brainstorm)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Changelog tool | **git-cliff** | Rust-native single binary, parses the repo's Conventional Commits, no Node. Replaces the deprecated `standard-version` justfile recipe. |
| Release driver | **`just release X.Y.Z`** | One transparent recipe with an explicit version. Predictable, no crates.io assumptions, easy to read/modify. |
| GitHub Release notes | **git-cliff latest section** | `release.yml` extracts the new tag's section and uses it as the Release body, so the GitHub release page matches `CHANGELOG.md`. Drop `generate_release_notes`. |
| Tag signing | **`git tag -s` (GPG)** | Tags must be signed. The recipe signs explicitly rather than relying on global `tag.gpgsign` (which is unset). |
| Version selection | **Explicit arg** | No auto-derivation from commits. "My own releases" → predictable manual version. |
| Push | **Recipe pushes (`--follow-tags`)** | The tag push is what triggers the release build — cutting the release is the point of the command. Guards prevent accidental/dirty releases. |

## Signing facts (verified against `~/.gitconfig`)

- `commit.gpgsign = true`, `user.signingkey = 3AECC1A61E1C08A7`, `gpg.format`
  unset (OpenPGP/GPG). → The `chore(release)` **commit auto-signs**.
- `tag.gpgsign` is **unset** → annotated tags are NOT auto-signed. Therefore the
  recipe MUST use `git tag -s` (explicit) to produce a signed tag. This is the
  load-bearing requirement of the tag step.
- The "Verified" badge on GitHub requires the public key `3AECC1A61E1C08A7` be
  uploaded to the GitHub account — a one-time account setting, **out of scope**
  for this repo (documented as a note).

## Architecture / components

### 1. `cliff.toml` (git-cliff config)

- Parse Conventional Commits; group by type into sections:
  `feat`→**Features**, `fix`→**Bug Fixes**, `perf`→**Performance**,
  `refactor`→**Refactor**, `docs`→**Documentation**, `build`+`ci`→**Build & CI**.
  Other/unmatched types → grouped under a generic section or skipped (pick one
  in the plan; default: skip `chore`, `style`, `test` from the changelog).
- **Skip** merge commits and `chore(release):` commits (so release commits don't
  appear as changelog entries).
- Commit links to `https://github.com/stevenwcarter/imgfind`.
- Tag pattern `v[0-9]*` (matches `vX.Y.Z`).
- Header + a small footer; date per release.
- Scopes (`feat(launcher): …`) preserved in the rendered line.

### 2. `CHANGELOG.md`

- Generated once from existing history as the baseline (an `[unreleased]`
  section, since no `v*` tags exist yet). The first `just release` stamps the
  unreleased section under its version. "Keep a Changelog"-style header.

### 3. Version-bump script (`scripts/bump-version.sh`)

Factor the pure, testable part out of the justfile so it can be unit-tested:

- **Input:** the new version `X.Y.Z`.
- **Action:** in each of `Cargo.toml`, `imgfind-gui/Cargo.toml`,
  `imgfind-launcher/Cargo.toml`, replace the **package** version line — matched
  with a start-of-line anchor `^version = "..."` so the `clipper` dependency line
  (`clipper = { version = "0.1.0", path = ... }`) is **never** touched — with
  `version = "X.Y.Z"`. Then refresh `Cargo.lock` (`cargo update -p imgfind -p
  imgfind-gui -p imgfind-launcher` or `cargo check`).
- **Validation:** reject a non-semver arg (`^[0-9]+\.[0-9]+\.[0-9]+$`).
- **Invariant this script depends on:** each crate's package `version` is the
  first `version = ` line (start-of-line) in its `Cargo.toml`, and the only other
  `version =` occurrences are inside dependency inline tables (which never start
  at column 0). A test pins this.

### 4. `justfile` recipes

- Replace `changelog:` (was `npx standard-version …`) with a git-cliff preview:
  `changelog: git-cliff -o CHANGELOG.md` (regenerate from all history; for
  previewing unreleased work).
- Add `release X.Y.Z`:
  1. **Guards (fail fast):** working tree clean (`git diff --quiet &&
     git diff --cached --quiet`); arg is valid semver; tag `vX.Y.Z` does not
     already exist (`git rev-parse -q --verify refs/tags/vX.Y.Z` must fail);
     optionally warn if not on `main`.
  2. `scripts/bump-version.sh X.Y.Z`.
  3. `git-cliff --tag vX.Y.Z -o CHANGELOG.md` (stamp the new version section).
  4. `git add -A && git commit -m "chore(release): vX.Y.Z"` (auto-signed via
     `commit.gpgsign`).
  5. `git tag -s vX.Y.Z -m "vX.Y.Z"` (**signed** tag).
  6. `git push --follow-tags` (pushes the branch + the new tag, triggering
     `release.yml`).
- Keep the recipe transparent (each step visible), echoing what it does.

### 4a. Tooling availability

- `git release` recipe and CI both need `git-cliff`. The justfile recipe checks
  for it and prints an install hint (`cargo install git-cliff`) if missing rather
  than failing cryptically.

### 5. `release.yml` change (CI)

The `release` job currently only downloads artifacts and runs
`softprops/action-gh-release` with `generate_release_notes: true`, and has **no
repo checkout**. Change:

- Add `actions/checkout@v4` with `fetch-depth: 0` (full history + tags — git-cliff
  needs them).
- Run git-cliff for the just-pushed tag, extracting only that release's section,
  e.g. `orhun/git-cliff-action@v4` with `args: --latest --strip header` → output
  to `RELEASE_NOTES.md`.
- Pass `body_path: RELEASE_NOTES.md` to `action-gh-release` and **remove**
  `generate_release_notes: true`.
- Everything else (the three build jobs, artifact attach, tag gating) unchanged.

### 6. Docs

- **CLAUDE.md** — a "Releasing" note: the `just release X.Y.Z` flow (bump →
  changelog → signed tag → push triggers installers), that tags are GPG-signed,
  and that `CHANGELOG.md` is git-cliff-generated. Note the GitHub "Verified"
  prerequisite (public key uploaded).
- **README** — a one-line pointer under the existing release/packaging context.

## Data flow

```
just release 0.2.0
  ├─ guards (clean tree, semver, tag absent)
  ├─ scripts/bump-version.sh 0.2.0   → 3× Cargo.toml + Cargo.lock
  ├─ git-cliff --tag v0.2.0          → CHANGELOG.md (new section)
  ├─ git commit (auto-signed)        → "chore(release): v0.2.0"
  ├─ git tag -s v0.2.0               → SIGNED tag
  └─ git push --follow-tags          → triggers release.yml
                                          └─ checkout(full) → git-cliff --latest
                                             → RELEASE_NOTES.md → action-gh-release
                                             (body_path) + 3 installer artifacts
```

## Error handling

- **Dirty tree / bad version / existing tag** → recipe aborts before any mutation.
- **git-cliff not installed** → recipe prints the install hint and exits non-zero.
- **Signing key/agent unavailable** → `git tag -s` fails loudly; the release stops
  (no unsigned tag is produced). The commit step would also fail if signing is
  broken, before tagging.
- **Push rejected** (e.g. behind remote) → standard git error; local commit+tag
  remain so the user can pull/rebase and re-push. (Document: re-running the
  recipe after a failed push would hit the "tag exists" guard — the user pushes
  manually with `git push --follow-tags`.)
- **CI git-cliff finds no section** → release notes empty; not fatal (the Release
  still publishes with the installer artifacts).

## Testing

Tooling/config, so coverage targets the testable seams:

1. **`bump-version.sh`** (load-bearing) — a bash test (mirroring
   `tests/install_sh_test.sh`): on a sandboxed copy of the three `Cargo.toml`s,
   running the script (a) sets the package version in all three to `X.Y.Z`,
   (b) leaves the `clipper` dependency `version = "0.1.0"` **unchanged**, and
   (c) rejects a non-semver arg with a non-zero exit. This pins the
   start-of-line-anchor invariant.
2. **`cliff.toml` smoke** — `git-cliff --unreleased` (or `--bump`) exits 0 and
   produces grouped, non-empty output on the current history; run in the bump
   test's environment or a separate check. (Skipped cleanly if git-cliff is not
   installed locally, with a note — CI is the backstop.)
3. **Workflow lint** — `actionlint` on the modified `release.yml` (the existing
   `actionlint.yml` workflow already guards this on PR).
4. **No test pushes or tags the real remote.** The git commit/tag steps are
   exercised only in a throwaway temp repo if at all; the recipe's push step is
   never run by tests.

## Acceptance criteria

- `cliff.toml` + `git-cliff` generate a grouped `CHANGELOG.md` from the repo's
  Conventional Commits.
- `just release X.Y.Z` (on a clean tree) bumps all three crate versions + lock,
  updates `CHANGELOG.md`, makes a signed commit, creates a **GPG-signed** tag
  `vX.Y.Z` (`git tag -v vX.Y.Z` verifies), and pushes.
- The pushed tag triggers `release.yml`, whose published GitHub Release body is
  the new tag's `CHANGELOG.md` section (not GitHub auto-notes).
- `bump-version.sh` never alters the `clipper` dependency version.
- Re-running with an existing tag, a dirty tree, or a bad version aborts safely.

## Out of scope (YAGNI)

- crates.io publishing.
- Auto version derivation from commit types.
- PR-based release flow (release-please / release-plz).
- CI-side tag signature verification (no public key in CI).
- Changing the global `tag.gpgsign` config (the recipe signs explicitly instead).
- Per-crate independent versioning (the workspace versions move together).

## Open implementation details (decide during planning)

- Exact `cliff.toml` commit-group set and whether to keep an "unmatched" bucket
  vs. skip `chore`/`style`/`test`.
- `Cargo.lock` refresh command (`cargo update -p …` vs `cargo check`) — pick the
  one that updates the three entries without unrelated churn.
- git-cliff CI invocation: `orhun/git-cliff-action@v4` vs `cargo install` step —
  prefer the action for speed/caching.
