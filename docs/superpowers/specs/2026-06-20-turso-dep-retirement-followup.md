# Dep-retirement follow-up: remove rusqlite / sqlite-vec after migration

**Date:** 2026-06-20
**Status:** Checklist — to be executed in a SEPARATE future PR, NOT in this branch.
**Context:** The Turso migration (Tasks 1–9) leaves `rusqlite`, `r2d2`, `r2d2_sqlite`,
`sqlite-vec`, and `zerocopy` in the workspace solely to support `src/migrate.rs` and its
tests. Once the author's own library has been confirmed migrated cleanly, these deps and the
migration shim can be deleted.

---

## Pre-conditions (verify before opening the PR)

- [ ] Run `imgfind migrate` against your own photo library database and confirm:
  - Exit message reports the correct image / embedding / thumbnail counts.
  - `imgfind status` after migration shows the expected numbers.
  - A spot-check search (`imgfind search "…"`) returns sensible results.
  - `imgfind.db.rusqlite.bak` exists and is byte-for-byte identical to the
    pre-migration file (optional but recommended: `sha256sum` before and after).
- [ ] Confirm the GUI launches and the previous session restores correctly.
- [ ] Keep the backup for at least one week before deleting it.

---

## Checklist (in order)

### 1. Delete `src/migrate.rs`

File: `/home/steve/src/imgfind/src/migrate.rs`

This is the **only** module that imports `rusqlite`, `sqlite_vec`, and `zerocopy`.
Deleting it removes every last use of those crates from the workspace.

### 2. Remove `pub mod migrate;` from `src/lib.rs`

File: `/home/steve/src/imgfind/src/lib.rs`

Remove the line:
```rust
pub mod migrate;
```

### 3. Remove the `Migrate` subcommand and `guard_not_legacy` from `src/main.rs`

File: `/home/steve/src/imgfind/src/main.rs`

- Delete the `Migrate { force: bool }` variant from the `Commands` enum (lines ~140–149
  at time of writing; search for `/// Migrate a legacy`).
- Delete the `guard_not_legacy` function (~lines 193–203; search for `fn guard_not_legacy`).
- Delete every call to `guard_not_legacy(...)` at each subcommand dispatch site.
- Delete the `Commands::Migrate { force } => { … }` match arm (~lines 347–365; search for
  `Commands::Migrate`).
- Remove any `use imgfind::migrate` import that was added.

### 4. Remove the retired deps from `Cargo.toml` (root crate)

File: `/home/steve/src/imgfind/Cargo.toml`

Delete the following entries from `[dependencies]` (exact versions may differ):

```toml
rusqlite = { … }
r2d2 = { … }
r2d2_sqlite = { … }
sqlite-vec = { … }
zerocopy = { … }
```

Also remove any `[features]` entries or `bundled` / `loadable_extension` flags
that were only there to support these deps.

### 5. Verify

```bash
cargo build --workspace
cargo test --workspace
cargo test -p imgfind --features tui
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

All should pass with zero errors or warnings related to the removed code.

### 6. Commit and PR

Suggested commit message:
```
chore: retire rusqlite/sqlite-vec migration shim

The author's library has been confirmed migrated. Remove src/migrate.rs,
the Migrate subcommand, guard_not_legacy, and the rusqlite/r2d2/r2d2_sqlite/
sqlite-vec/zerocopy deps. The workspace is now pure-Rust with no C SQLite deps.
```

---

## Files changed (summary)

| File | Change |
|------|--------|
| `src/migrate.rs` | **Delete** |
| `src/lib.rs` | Remove `pub mod migrate;` |
| `src/main.rs` | Remove `Migrate` variant, `guard_not_legacy`, and all call sites |
| `Cargo.toml` (root) | Remove `rusqlite`, `r2d2`, `r2d2_sqlite`, `sqlite-vec`, `zerocopy` deps |

---

> **Do not execute this checklist on the current branch.** The migration shim must stay
> in place until personal libraries are confirmed migrated.
