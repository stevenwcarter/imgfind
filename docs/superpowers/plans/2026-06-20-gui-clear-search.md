# GUI Clear-Text-Search Button Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "Clear text search" button left of the GUI search box that drops the text query and browses the full set matching the remaining filters (all images when no filters).

**Architecture:** A new Slint `clear-search` callback + button invokes a shared Rust `clear_to_browse` helper. The same helper replaces the body of `on_search`'s empty-query branch, so "Enter on empty field" and the button behave identically and always browse (no idle prompt).

**Tech Stack:** Rust (edition 2024), Slint 1.x, rusqlite. Crates: `imgfind` (core, `src/`) and `imgfind-gui` (`imgfind-gui/`).

## Global Constraints

- Rust edition 2024; code must be `cargo clippy --workspace`-clean and `cargo fmt`-clean.
- Slint button/label text must be ASCII/Latin-1 only (default-font glyph safety — no `✕`, `…` in Button text; `set_status` Text may use `\u{2026}` as existing code does).
- Errors via `anyhow` (`Context`/`with_context`).
- Dispatch Rust coding to the `rust-developer` agent.

---

### Task 1: Pin the browse-all invariant (test-only)

The "show all images on no-filter clear" behavior depends on
`browse_all(&Filters::default(), ..)` returning every image. Make that
dependency explicit and greppable.

**Files:**
- Test: `src/database.rs` (the `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Database::browse_all(&Filters, &Sort) -> Result<Vec<RowMeta>>`, the existing test helpers in `src/database.rs` tests (look at `browse_all_sorts_by_size_then_name_nulls_last` ~line 2456 for the in-memory DB + insert pattern).
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Write the test**

Mirror the setup of the existing `browse_all_*` tests (same in-memory DB + image insert helper they use). Insert 3 images, then:

```rust
#[test]
fn browse_all_default_filters_returns_all() {
    let db = /* same test-DB construction as browse_all_sorts_by_size_then_name_nulls_last */;
    // insert 3 images via the same helper those tests use
    let rows = db
        .browse_all(&Filters::default(), &crate::sort::Sort::default())
        .unwrap();
    assert_eq!(rows.len(), 3, "default filters must return every image");
}
```

Use the exact helper names/imports already present in that test module (read them first; do not invent a new fixture).

- [ ] **Step 2: Run it to verify it passes**

Run: `cargo test -p imgfind browse_all_default_filters_returns_all`
Expected: PASS (this characterizes existing behavior — it should pass on first run; if it fails, stop and report, as it contradicts the spec's assumption).

- [ ] **Step 3: Commit**

```bash
git add src/database.rs
git commit -m "test: pin browse_all(default filters) returns all images [T1]"
```

---

### Task 2: `clear_to_browse` helper + collapse `on_search` empty branch

Extract the empty-query browse logic into a reusable helper and make the
empty-query path always browse (drop the idle special-case; reset sort so a
stale Relevance sort can't leak into browse).

**Files:**
- Modify: `imgfind-gui/src/main.rs` — add `clear_to_browse` (near `spawn_browse`, ~line 2953); replace the `if query.is_empty() { … }` block in `on_search` (~lines 463-517).

**Interfaces:**
- Consumes: `spawn_browse(weak, state, grid_gen, backend, filters, sel)`; `SelectionPolicy { selected, after, selection, selection_dirty }`; `SelectAfter::Clear`; `SearchMode::Text`; `make_sort_options_model(bool)`; `Sort::default()`; `SearchState::start_search(String)`; `Filters`.
- Produces: `fn clear_to_browse(weak: &Weak<MainWindow>, state: &Arc<Mutex<SearchState>>, detail: &Arc<Mutex<Option<DetailState>>>, lb: &Arc<Mutex<Option<usize>>>, mode: &Arc<Mutex<SearchMode>>, grid_gen: &Arc<AtomicU64>, backend: &Backend, filters: Filters, sel: SelectionPolicy)` — consumed by Task 3.

- [ ] **Step 1: Add the `clear_to_browse` helper**

Place immediately above `fn spawn_browse` in `imgfind-gui/src/main.rs`:

```rust
/// Drop the text query and browse the full set matching `filters` (an empty
/// `Filters::default()` browses the whole library). Shared by `on_search`'s
/// empty-query branch and the "Clear text search" button. Resets sort to the
/// browse default — a prior search may have left sort = Relevance, invalid for
/// browse — and the sort selector to browse mode (no Relevance option).
fn clear_to_browse(
    weak: &Weak<MainWindow>,
    state: &Arc<Mutex<SearchState>>,
    detail: &Arc<Mutex<Option<DetailState>>>,
    lb: &Arc<Mutex<Option<usize>>>,
    mode: &Arc<Mutex<SearchMode>>,
    grid_gen: &Arc<AtomicU64>,
    backend: &Backend,
    filters: Filters,
    sel: SelectionPolicy,
) {
    *lb.lock().unwrap() = None;
    *detail.lock().unwrap() = None;
    *mode.lock().unwrap() = SearchMode::Text(String::new());
    {
        let mut s = state.lock().unwrap();
        s.sort = Sort::default();
        s.start_search(String::new());
    }
    if let Some(w) = weak.upgrade() {
        w.set_lightbox_open(false);
        w.set_detail_open(false);
        w.set_status("Searching\u{2026}".into());
        w.set_can_search(false);
        w.set_sort_options(make_sort_options_model(false));
        w.set_sort_index(0);
        w.set_sort_desc(false);
    }
    spawn_browse(
        weak.clone(),
        Arc::clone(state),
        Arc::clone(grid_gen),
        backend.clone(),
        filters,
        sel,
    );
}
```

(Confirm the `Sort` import path used elsewhere in the file; match it. `state.sort` and `start_search` are used identically in the current `on_search` filters-active branch.)

- [ ] **Step 2: Replace the `on_search` empty-query block**

In `on_search` (~line 463), replace the entire `if query.is_empty() { … return; }` block (both the `Filters::default()` idle sub-branch and the filters-active sub-branch) with:

```rust
            if query.is_empty() {
                clear_to_browse(
                    &weak,
                    &state_ref,
                    &detail_ref,
                    &lb_ref,
                    &mode_ref,
                    &grid_gen_ref,
                    &backend_search,
                    current_filters,
                    SelectionPolicy {
                        selected: Arc::clone(&selected_ref),
                        after: SelectAfter::Clear,
                        selection: Arc::clone(&selection_ref),
                        selection_dirty: Arc::clone(&selection_dirty_ref),
                    },
                );
                return;
            }
```

Leave the `restoring_ref` early-return, the `query.trim()`, and the
`current_filters` binding above it unchanged. Everything after the empty-query
block (the non-empty search path) is unchanged.

- [ ] **Step 3: Build + clippy + fmt**

Run: `cargo clippy -p imgfind-gui && cargo fmt --check`
Expected: clean. Fix any warning the change introduced.

- [ ] **Step 4: Run the workspace tests**

Run: `cargo test --workspace`
Expected: PASS (no behavior under test regressed; this path is GUI wiring).

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): always browse on empty query via clear_to_browse helper [T2]"
```

---

### Task 3: "Clear text search" button + `clear-search` callback

Add the UI button and its Rust handler, both wired to `clear_to_browse`.

**Files:**
- Modify: `imgfind-gui/ui/app.slint` — add `callback clear-search();` (near the other callbacks ~line 71) and a `Button` in the search `HorizontalLayout` (~line 404).
- Modify: `imgfind-gui/src/main.rs` — add an `on_clear_search` handler block (place right after the `on_search` block, ~line 630).

**Interfaces:**
- Consumes: `clear_to_browse(..)` from Task 2; `MainWindow::on_clear_search`, `set_query_text`, `as_weak`; the same shared Arcs cloned for `on_search` (`state`, `lb_index`, `detail`, `search_mode`, `filters`, `selected`, `selection`, `selection_dirty`, `grid_generation`, `restoring`, `backend`).
- Produces: user-visible button; nothing consumed by later tasks.

- [ ] **Step 1: Declare the Slint callback**

In `imgfind-gui/ui/app.slint`, near the existing `callback search(string);` (~line 71), add:

```slint
    callback clear-search();
```

- [ ] **Step 2: Add the button left of the input**

In the search `HorizontalLayout` (~line 404), add a `Button` **before**
`search-input :=`:

```slint
                HorizontalLayout {
                    spacing: 8px;
                    Button {
                        text: "Clear text search";
                        enabled: root.query-text != "";
                        clicked => { root.clear-search(); app-keys.focus(); }
                    }
                    search-input := LineEdit {
                        text <=> root.query-text;
                        placeholder-text: root.can-search ? "Search images..." : "Loading model...";
                        enabled: root.can-search;
                        accepted(text) => { root.search(text); app-keys.focus(); }
                    }
                }
```

- [ ] **Step 3: Add the `on_clear_search` Rust handler**

In `imgfind-gui/src/main.rs`, immediately after the `on_search` block closes
(~line 630), add a new block mirroring `on_search`'s capture-clones:

```rust
    // --- clear-search callback: drop the text query, browse remaining filters ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let detail_ref = Arc::clone(&detail);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_ref = Arc::clone(&selected);
        let selection_ref = Arc::clone(&selection);
        let selection_dirty_ref = Arc::clone(&selection_dirty);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let restoring_ref = Arc::clone(&restoring);
        let backend_clear = backend.clone();
        window.on_clear_search(move || {
            if restoring_ref.load(Ordering::SeqCst) {
                return;
            }
            let Some(w) = weak.upgrade() else { return };
            w.set_query_text("".into());
            let current_filters = filters_ref.lock().unwrap().clone();
            clear_to_browse(
                &weak,
                &state_ref,
                &detail_ref,
                &lb_ref,
                &mode_ref,
                &grid_gen_ref,
                &backend_clear,
                current_filters,
                SelectionPolicy {
                    selected: Arc::clone(&selected_ref),
                    after: SelectAfter::Clear,
                    selection: Arc::clone(&selection_ref),
                    selection_dirty: Arc::clone(&selection_dirty_ref),
                },
            );
        });
    }
```

Match the actual `Ordering` import already used in the file (`Ordering::SeqCst`
appears throughout). If `lb_index`/`grid_generation`/etc. have slightly
different binding names, use the names as defined where `on_search`'s clones are
made (read ~lines 440-452).

- [ ] **Step 4: Build + clippy + fmt**

Run: `cargo clippy -p imgfind-gui && cargo fmt --check`
Expected: clean.

- [ ] **Step 5: Run the workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Manual-smoke note (no automated test)**

The button wiring is interactive — verify manually per the spec's smoke
checklist (search → button enables → click → grid browses, Relevance gone,
button greys; filters set → returns filtered set; inner X → text clears, grid
unchanged). Record as a manual-smoke residual; do not invent a unit test for
Slint callback wiring.

- [ ] **Step 7: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): add Clear text search button [T3]"
```

---

## Self-Review

**Spec coverage:**
- "New button left of input" → Task 3 Step 2. ✓
- "Inner X unchanged" → search-input untouched (Task 3 Step 2 keeps it as-is). ✓
- "Clearing always browses; no-filter → all images" → Task 2 (collapsed branch) + Task 1 (invariant pinned). ✓
- "Approach A: dedicated callback + shared helper" → Task 2 helper + Task 3 callback. ✓
- "Reset sort (Relevance invalid for browse)" → Task 2 `clear_to_browse`. ✓
- "Enabled only when query present" → Task 3 `enabled: root.query-text != ""`. ✓
- "browse_all(default) = all invariant test" → Task 1. ✓

**Placeholder scan:** none — all code blocks are concrete; the only deferred-to-implementer items are reading exact pre-existing test-fixture and binding names (explicitly flagged), which is correct since those names already exist in the file.

**Type consistency:** `clear_to_browse` signature in Task 2 (Produces) matches its call in Task 3 Step 3 and the on_search call in Task 2 Step 2. `SelectionPolicy` fields, `SelectAfter::Clear`, `SearchMode::Text(String)`, `Sort::default()` all match the code read during planning.
