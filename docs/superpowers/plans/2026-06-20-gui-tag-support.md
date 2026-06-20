# GUI Tag Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a tagging system to the imgfind native GUI — free-text tags, five color "brushes" for quick painting, an editable "Most Recent" (`mm`) staging set, per-image tag editing, and tag filtering (AND/OR + enable toggle).

**Architecture:** Core crate gains tag fields on `Filters` (flowing through the single `build_filter_clause` seam) and new persisted UI state on `UiState`. The GUI gains a pure `chords` state machine, thin `Backend` tag methods, two reusable Slint widgets (`toggle`, `tag_editor`), a left rail, a detail-panel editor, a filter-pane tag row, and a tag modal — all wired through the existing `Arc<Mutex<…>>` + background-thread + debounce patterns. Colors are input-only brushes; nothing color is ever stored on an image or tag, so there is **no schema migration**.

**Tech Stack:** Rust 2024, Slint 1.x, rusqlite + r2d2, serde/serde_json, anyhow, tracing.

## Global Constraints

- Rust edition 2024; code must be `cargo clippy --workspace` and `cargo fmt --all` clean.
- Errors use `anyhow` with `.context()`/`.with_context()`.
- No SQLite migration: `LATEST_MIGRATION_VERSION` stays `3`. Tags use the existing `tags`/`image_tags` tables.
- Every new `UiState`/`Filters` field carries `#[serde(default)]`.
- Slint button/label text is ASCII/Latin-1 only (no symbol glyphs — they render as tofu). Use `×`, `<`, `>`, letters.
- Brush color order is fixed and index-stable: `0=red, 1=green, 2=yellow, 3=purple, 4=blue`; mnemonic letters `r g y p b`.
- Dispatch Rust coding to the `rust-developer` agent.
- Per-task: run `cargo test --workspace` (or the targeted module) green before commit.

---

### Task 1: Core — brush colors, `TagBrush`, and `UiState` fields

**Files:**
- Create: `src/colors.rs`
- Modify: `src/lib.rs` (add `pub mod colors;`)
- Modify: `src/ui_state.rs` (add fields + update tests)

**Interfaces:**
- Produces: `colors::BrushColor` enum (`Red,Green,Yellow,Purple,Blue`) with `BrushColor::ALL: [BrushColor;5]`, `from_letter(&str)->Option<BrushColor>`, `letter(self)->&'static str`, `index(self)->usize`, `from_index(usize)->Option<BrushColor>`.
- Produces: `ui_state::TagBrush { tags: Vec<String> }`; `UiState.brushes: [TagBrush;5]`, `UiState.recent_tags: Vec<String>`, `UiState.rail_visible: bool` (defaults to `true`).

- [ ] **Step 1: Write the failing test for `colors`**

Create `src/colors.rs`:

```rust
//! Fixed palette of tag "brush" colors. Colors are an input convenience in the
//! GUI (quick-apply sets of tags); they are never persisted on images or tags.
//! Index order is stable and shared with the persisted `UiState.brushes` array.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushColor {
    Red,
    Green,
    Yellow,
    Purple,
    Blue,
}

impl BrushColor {
    pub const ALL: [BrushColor; 5] = [
        BrushColor::Red,
        BrushColor::Green,
        BrushColor::Yellow,
        BrushColor::Purple,
        BrushColor::Blue,
    ];

    pub fn index(self) -> usize {
        match self {
            BrushColor::Red => 0,
            BrushColor::Green => 1,
            BrushColor::Yellow => 2,
            BrushColor::Purple => 3,
            BrushColor::Blue => 4,
        }
    }

    pub fn from_index(i: usize) -> Option<BrushColor> {
        BrushColor::ALL.get(i).copied()
    }

    pub fn letter(self) -> &'static str {
        match self {
            BrushColor::Red => "r",
            BrushColor::Green => "g",
            BrushColor::Yellow => "y",
            BrushColor::Purple => "p",
            BrushColor::Blue => "b",
        }
    }

    pub fn from_letter(s: &str) -> Option<BrushColor> {
        match s {
            "r" => Some(BrushColor::Red),
            "g" => Some(BrushColor::Green),
            "y" => Some(BrushColor::Yellow),
            "p" => Some(BrushColor::Purple),
            "b" => Some(BrushColor::Blue),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letter_index_roundtrip() {
        for c in BrushColor::ALL {
            assert_eq!(BrushColor::from_letter(c.letter()), Some(c));
            assert_eq!(BrushColor::from_index(c.index()), Some(c));
        }
    }

    #[test]
    fn index_is_stable_order() {
        assert_eq!(BrushColor::Red.index(), 0);
        assert_eq!(BrushColor::Blue.index(), 4);
    }

    #[test]
    fn unknown_letter_is_none() {
        assert_eq!(BrushColor::from_letter("x"), None);
        assert_eq!(BrushColor::from_letter("m"), None);
    }
}
```

Add to `src/lib.rs` near the other `pub mod` lines: `pub mod colors;`

- [ ] **Step 2: Run the colors test, expect pass**

Run: `cargo test -p imgfind colors::`
Expected: 3 tests pass.

- [ ] **Step 3: Add `TagBrush` + fields to `UiState`**

In `src/ui_state.rs`, add above `UiState`:

```rust
/// One color brush: a curated set of tag names quick-applied as a unit. The
/// color is input-only; these tags are assigned to images as ordinary tags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TagBrush {
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Default `rail_visible` is `true` (rail shown on first launch).
fn default_true() -> bool {
    true
}
```

Add these fields to `UiState` (after `scroll_y`):

```rust
    /// Five color brushes, indexed by `colors::BrushColor::index`.
    #[serde(default)]
    pub brushes: [TagBrush; 5],
    /// The live, editable "Most Recent" (`mm`) staging set.
    #[serde(default)]
    pub recent_tags: Vec<String>,
    /// Left-rail visibility (defaults to shown).
    #[serde(default = "default_true")]
    pub rail_visible: bool,
```

Because `rail_visible` now defaults to `true` but `UiState::default()` derives `bool::default()` (`false`), add an explicit `Default` note: keep the derived `Default` (so `UiState::default().rail_visible == false` is acceptable — the GUI uses `get_ui_state()` which goes through serde where the `default_true` applies; on a brand-new DB with no row, the GUI's own startup path sets the rail visible). Leave the derived `Default`.

- [ ] **Step 4: Update `UiState` tests for the new fields**

Replace the body of `round_trips_through_json` so the literal includes the new fields, and add a defaults test:

```rust
    #[test]
    fn round_trips_through_json() {
        let st = UiState {
            search_text: "cat".into(),
            mode: PersistedMode::Text("cat".into()),
            sort: Sort {
                key: SortKey::Size,
                dir: SortDir::Desc,
            },
            filters: crate::filters::Filters::default(),
            result_ids: vec![3, 1, 2],
            selected_index: Some(1),
            detail_open: true,
            scroll_y: 128.5,
            brushes: [
                TagBrush { tags: vec!["beach".into(), "sunset".into()] },
                TagBrush::default(),
                TagBrush::default(),
                TagBrush::default(),
                TagBrush::default(),
            ],
            recent_tags: vec!["beach".into(), "sunset".into()],
            rail_visible: false,
        };
        let json = serde_json::to_string(&st).unwrap();
        let back: UiState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, st);
    }

    #[test]
    fn old_blob_without_tag_fields_deserializes() {
        // A pre-tags JSON blob omits brushes/recent_tags/rail_visible.
        let json = r#"{"search_text":"x","result_ids":[1]}"#;
        let back: UiState = serde_json::from_str(json).unwrap();
        assert_eq!(back.brushes, <[TagBrush; 5]>::default());
        assert!(back.recent_tags.is_empty());
        // Absent rail_visible falls back to the serde default (true).
        assert!(back.rail_visible);
    }
```

Add `use super::TagBrush;` if needed (it is `super::*`, already covered).

- [ ] **Step 5: Run tests, expect pass**

Run: `cargo test -p imgfind ui_state:: colors::`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/colors.rs src/lib.rs src/ui_state.rs
git commit -m "feat(core): brush colors, TagBrush, and tag UI-state fields"
```

---

### Task 2: Core — tag filtering through `Filters`/`build_filter_clause`

**Files:**
- Modify: `src/filters.rs`
- Test: `src/filters.rs` (unit) + `src/database.rs` tests module (integration through `browse_all`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `filters::TagMatch { AllOf, AnyOf }` (default `AllOf`); `Filters.tags: Vec<String>`, `Filters.tag_match: TagMatch`, `Filters.tags_enabled: bool`. `build_filter_clause` emits the tag predicate only when `tags_enabled && !tags.is_empty()`.

- [ ] **Step 1: Add fields + enum to `Filters`**

In `src/filters.rs`, add after the `gps` field in `Filters`:

```rust
    /// Tag names to filter by; empty = no tag filtering.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Whether all tags must match (AND) or any (OR).
    #[serde(default)]
    pub tag_match: TagMatch,
    /// Master enable for the tag filter (`ft`); when false, tags are ignored
    /// but retained.
    #[serde(default)]
    pub tags_enabled: bool,
```

Add the enum after `GpsFilter`:

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagMatch {
    /// Image must have every selected tag (AND).
    #[default]
    AllOf,
    /// Image must have at least one selected tag (OR).
    AnyOf,
}
```

- [ ] **Step 2: Extend `build_filter_clause`**

In `build_filter_clause`, after the `gps` match block and before the `if clauses.is_empty()` check, add:

```rust
    if f.tags_enabled && !f.tags.is_empty() {
        match f.tag_match {
            TagMatch::AllOf => {
                for tag in &f.tags {
                    clauses.push(
                        "EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                         WHERE it.image_id = i.id AND t.name = ?)"
                            .into(),
                    );
                    params.push(Value::Text(tag.clone()));
                }
            }
            TagMatch::AnyOf => {
                let placeholders = vec!["?"; f.tags.len()].join(", ");
                clauses.push(format!(
                    "EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                     WHERE it.image_id = i.id AND t.name IN ({placeholders}))"
                ));
                for tag in &f.tags {
                    params.push(Value::Text(tag.clone()));
                }
            }
        }
    }
```

- [ ] **Step 3: Fix the existing `combined_filters_join_with_and` test literal**

That test builds `Filters { … }` with all fields listed. Add `..Default::default()` so the new fields don't break it:

```rust
    #[test]
    fn combined_filters_join_with_and() {
        let f = Filters {
            size_min: Some(10),
            size_max: None,
            extensions: vec!["nef".into()],
            gps: GpsFilter::HasGps,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND m.file_size >= ? AND (lower(i.path) LIKE ?) AND (m.latitude IS NOT NULL AND m.longitude IS NOT NULL)"
        );
        assert_eq!(params, vec![Value::Integer(10), Value::Text("%.nef".into())]);
    }
```

- [ ] **Step 4: Write failing unit tests for the tag clause**

Add to the `tests` module in `src/filters.rs`:

```rust
    #[test]
    fn tags_disabled_yields_no_clause() {
        let f = Filters {
            tags: vec!["a".into(), "b".into()],
            tags_enabled: false,
            ..Default::default()
        };
        assert_eq!(build_filter_clause(&f).0, "");
    }

    #[test]
    fn tags_all_of_emits_exists_per_tag() {
        let f = Filters {
            tags: vec!["a".into(), "b".into()],
            tag_match: TagMatch::AllOf,
            tags_enabled: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?) AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?)"
        );
        assert_eq!(params, vec![Value::Text("a".into()), Value::Text("b".into())]);
    }

    #[test]
    fn tags_any_of_emits_single_in_clause() {
        let f = Filters {
            tags: vec!["a".into(), "b".into()],
            tag_match: TagMatch::AnyOf,
            tags_enabled: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name IN (?, ?))"
        );
        assert_eq!(params, vec![Value::Text("a".into()), Value::Text("b".into())]);
    }

    #[test]
    fn tags_combine_after_size() {
        let f = Filters {
            size_min: Some(5),
            tags: vec!["x".into()],
            tags_enabled: true,
            ..Default::default()
        };
        let (sql, params) = build_filter_clause(&f);
        assert_eq!(
            sql,
            " AND m.file_size >= ? AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = i.id AND t.name = ?)"
        );
        assert_eq!(params, vec![Value::Integer(5), Value::Text("x".into())]);
    }
```

- [ ] **Step 5: Run unit tests, expect pass**

Run: `cargo test -p imgfind filters::`
Expected: all pass (the `combined` test and the four new tag tests).

- [ ] **Step 6: Add the load-bearing integration test through `browse_all`**

Find the existing `#[cfg(test)] mod tests` in `src/database.rs` and the helper it uses to build a temp DB + insert images (look for an existing helper such as a function that creates a `Database` in a tempdir and inserts rows; reuse it). Add a test that inserts three images, tags them, and asserts filtering:

```rust
    #[test]
    fn browse_all_filters_by_tags_all_and_any() {
        use crate::filters::{Filters, TagMatch};
        use crate::sort::Sort;

        // Reuse the crate's existing temp-DB + image-insert test helper.
        let (db, _tmp) = test_db_with_images(&["a.jpg", "b.jpg", "c.jpg"]);
        db.tag_image(&rel("a.jpg"), "beach").unwrap();
        db.tag_image(&rel("a.jpg"), "sunset").unwrap();
        db.tag_image(&rel("b.jpg"), "beach").unwrap();
        // c.jpg has no tags.

        let all_beach_sunset = Filters {
            tags: vec!["beach".into(), "sunset".into()],
            tag_match: TagMatch::AllOf,
            tags_enabled: true,
            ..Default::default()
        };
        let got = db.browse_all(&all_beach_sunset, &Sort::default()).unwrap();
        let paths: Vec<&str> = got.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["a.jpg"]);

        let any_beach_sunset = Filters {
            tag_match: TagMatch::AnyOf,
            ..all_beach_sunset.clone()
        };
        let mut got = db.browse_all(&any_beach_sunset, &Sort::default()).unwrap();
        got.sort_by(|x, y| x.path.cmp(&y.path));
        let paths: Vec<&str> = got.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["a.jpg", "b.jpg"]);

        let disabled = Filters { tags_enabled: false, ..all_beach_sunset };
        let got = db.browse_all(&disabled, &Sort::default()).unwrap();
        assert_eq!(got.len(), 3);
    }
```

**Note for implementer:** Match the exact helper names/signatures the existing `database.rs` tests use (e.g. for building the temp DB and constructing a `RelativePath`). If no multi-image helper exists, write the rows directly with the same insert calls the other tests use. The assertions (AllOf → only `a.jpg`; AnyOf → `a.jpg`+`b.jpg`; disabled → all 3) are the contract — keep them.

- [ ] **Step 7: Run the integration test, expect pass**

Run: `cargo test -p imgfind database::`
Expected: the new test passes alongside existing ones.

- [ ] **Step 8: Commit**

```bash
git add src/filters.rs src/database.rs
git commit -m "feat(core): tag filtering (AND/OR + enable) via build_filter_clause"
```

---

### Task 3: GUI — pure `chords` keyboard state machine

**Files:**
- Create: `imgfind-gui/src/chords.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod chords;` near the other `mod` declarations)

**Interfaces:**
- Consumes: `imgfind::colors::BrushColor`.
- Produces: `chords::Pending { None, AwaitM, AwaitF }` (default `None`); `chords::Action`; `chords::resolve(pending, key) -> (Pending, Option<Action>)`.

- [ ] **Step 1: Write `chords.rs` with failing tests**

```rust
//! Pure keyboard-chord state machine for tag shortcuts. The Slint side forwards
//! a single key string per press (only when no text input has focus); this
//! module decides the resulting action and the next pending state. No I/O.

use imgfind::colors::BrushColor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pending {
    #[default]
    None,
    AwaitM,
    AwaitF,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleRail,
    OpenTagModal,
    PaintBrush(BrushColor),
    RepeatLast,
    LoadBrushIntoFilter(BrushColor),
    ToggleTagFilter,
}

/// Resolve a key press given the current pending prefix.
/// Returns the next pending state and an optional action to perform.
/// Any key that doesn't complete or start a chord cancels the prefix and yields
/// no action (the caller lets such keys fall through to existing navigation).
pub fn resolve(pending: Pending, key: &str) -> (Pending, Option<Action>) {
    match pending {
        Pending::None => match key {
            "`" => (Pending::None, Some(Action::ToggleRail)),
            "t" => (Pending::None, Some(Action::OpenTagModal)),
            "m" => (Pending::AwaitM, None),
            "f" => (Pending::AwaitF, None),
            _ => (Pending::None, None),
        },
        Pending::AwaitM => match key {
            "m" => (Pending::None, Some(Action::RepeatLast)),
            other => match BrushColor::from_letter(other) {
                Some(c) => (Pending::None, Some(Action::PaintBrush(c))),
                None => (Pending::None, None),
            },
        },
        Pending::AwaitF => match key {
            "t" => (Pending::None, Some(Action::ToggleTagFilter)),
            other => match BrushColor::from_letter(other) {
                Some(c) => (Pending::None, Some(Action::LoadBrushIntoFilter(c))),
                None => (Pending::None, None),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backtick_toggles_rail() {
        assert_eq!(resolve(Pending::None, "`"), (Pending::None, Some(Action::ToggleRail)));
    }

    #[test]
    fn t_opens_modal() {
        assert_eq!(resolve(Pending::None, "t"), (Pending::None, Some(Action::OpenTagModal)));
    }

    #[test]
    fn m_then_color_paints() {
        let (p, a) = resolve(Pending::None, "m");
        assert_eq!((p, a), (Pending::AwaitM, None));
        assert_eq!(
            resolve(p, "r"),
            (Pending::None, Some(Action::PaintBrush(BrushColor::Red)))
        );
    }

    #[test]
    fn mm_repeats_last() {
        let (p, _) = resolve(Pending::None, "m");
        assert_eq!(resolve(p, "m"), (Pending::None, Some(Action::RepeatLast)));
    }

    #[test]
    fn f_then_color_loads_filter() {
        let (p, _) = resolve(Pending::None, "f");
        assert_eq!(
            resolve(p, "g"),
            (Pending::None, Some(Action::LoadBrushIntoFilter(BrushColor::Green)))
        );
    }

    #[test]
    fn ft_toggles_filter() {
        let (p, _) = resolve(Pending::None, "f");
        assert_eq!(resolve(p, "t"), (Pending::None, Some(Action::ToggleTagFilter)));
    }

    #[test]
    fn unknown_after_prefix_cancels_no_action() {
        let (p, _) = resolve(Pending::None, "m");
        assert_eq!(resolve(p, "z"), (Pending::None, None));
        let (p, _) = resolve(Pending::None, "f");
        assert_eq!(resolve(p, "z"), (Pending::None, None));
    }

    #[test]
    fn plain_nav_key_no_chord() {
        assert_eq!(resolve(Pending::None, "j"), (Pending::None, None));
    }
}
```

Add `mod chords;` in `imgfind-gui/src/main.rs` alongside the other `mod` lines.

- [ ] **Step 2: Run tests, expect pass**

Run: `cargo test -p imgfind-gui chords::`
Expected: 8 tests pass.

- [ ] **Step 3: Commit**

```bash
git add imgfind-gui/src/chords.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): pure chords keyboard state machine"
```

---

### Task 4: GUI — `Backend` tag methods

**Files:**
- Modify: `imgfind-gui/src/backend.rs`

**Interfaces:**
- Consumes: `Database::tag_image`, `untag_image`, `tags_for_image`, `list_tags`, and the existing `RelativePath` construction the other `Backend` methods use.
- Produces (on `Backend`): `add_tag(&self, rel_path: &str, tag: &str) -> Result<()>`, `remove_tag(&self, rel_path: &str, tag: &str) -> Result<()>`, `tags_for(&self, rel_path: &str) -> Result<Vec<String>>`, `all_tags(&self) -> Result<Vec<String>>`.

- [ ] **Step 1: Add the four methods**

In `imgfind-gui/src/backend.rs`, mirror how existing methods convert `&str` rel paths into the `RelativePath` the `Database` API expects (reuse the same helper/constructor the `metadata`/`thumbnail` methods use):

```rust
    /// Assign `tag` to the image at `rel_path` (creates the tag if new).
    pub fn add_tag(&self, rel_path: &str, tag: &str) -> Result<()> {
        self.db
            .tag_image(&Self::rel(rel_path), tag)
            .with_context(|| format!("add tag {tag} to {rel_path}"))
    }

    /// Remove `tag` from the image at `rel_path`.
    pub fn remove_tag(&self, rel_path: &str, tag: &str) -> Result<()> {
        self.db
            .untag_image(&Self::rel(rel_path), tag)
            .with_context(|| format!("remove tag {tag} from {rel_path}"))
    }

    /// Tags currently assigned to the image at `rel_path`.
    pub fn tags_for(&self, rel_path: &str) -> Result<Vec<String>> {
        self.db
            .tags_for_image(&Self::rel(rel_path))
            .with_context(|| format!("tags for {rel_path}"))
    }

    /// All tag names in the database (alphabetical).
    pub fn all_tags(&self) -> Result<Vec<String>> {
        self.db.list_tags().context("list all tags")
    }
```

**Note:** If `Backend` has no `rel()` helper, use whatever the existing methods do to make a `RelativePath` from `&str` (e.g. `RelativePath(PathBuf::from(rel_path))` or a dedicated constructor). Match the existing pattern exactly; add a private `fn rel(p: &str) -> RelativePath` helper if it reduces duplication.

- [ ] **Step 2: Add an integration test**

If `backend.rs` already has a `#[cfg(test)]` module with a temp-DB builder, add:

```rust
    #[test]
    fn add_list_remove_tag_roundtrip() {
        let (backend, _tmp) = test_backend_with_image("a.jpg");
        backend.add_tag("a.jpg", "beach").unwrap();
        backend.add_tag("a.jpg", "sunset").unwrap();
        let mut tags = backend.tags_for("a.jpg").unwrap();
        tags.sort();
        assert_eq!(tags, vec!["beach".to_string(), "sunset".to_string()]);
        backend.remove_tag("a.jpg", "beach").unwrap();
        assert_eq!(backend.tags_for("a.jpg").unwrap(), vec!["sunset".to_string()]);
        assert!(backend.all_tags().unwrap().contains(&"sunset".to_string()));
    }
```

If no backend test harness exists, skip the dedicated test (the underlying `Database` methods are covered by Task 2's DB tests and the core crate's own tests) and instead verify with `cargo build -p imgfind-gui`. State which path you took in the commit body.

- [ ] **Step 3: Run tests/build, expect pass**

Run: `cargo test -p imgfind-gui backend::` (or `cargo build -p imgfind-gui` if no harness).
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/src/backend.rs
git commit -m "feat(gui): Backend tag methods (add/remove/tags_for/all_tags)"
```

---

### Task 5: Slint — reusable `toggle` and `tag_editor` widgets

**Files:**
- Create: `imgfind-gui/ui/toggle.slint`
- Create: `imgfind-gui/ui/tag_editor.slint`

**Interfaces:**
- Produces: `Toggle` component (`in property <bool> on; callback toggled(bool);`).
- Produces: `TagEditor` component (`in property <[string]> tags; in property <string> placeholder; in property <color> pill-color; callback committed([string]); callback remove(string);`).

- [ ] **Step 1: Create `toggle.slint`**

```slint
// A web-style slide toggle: a rounded pill track with a circular knob that
// animates left (off) / right (on). Grey track off, dark-green on.
export component Toggle inherits Rectangle {
    in property <bool> on: false;
    callback toggled(bool);

    width: 40px;
    height: 22px;
    border-radius: self.height / 2;
    background: root.on ? #2e7d46 : #3a4255;
    animate background { duration: 120ms; }

    knob := Rectangle {
        width: 16px;
        height: 16px;
        border-radius: self.width / 2;
        background: #f0f0f0;
        y: (parent.height - self.height) / 2;
        x: root.on ? parent.width - self.width - 3px : 3px;
        animate x { duration: 120ms; }
    }

    TouchArea {
        clicked => { root.toggled(!root.on); }
    }
}
```

- [ ] **Step 2: Create `tag_editor.slint`**

```slint
// Text <-> pills tag editor. Display state shows pills (click to remove, click
// empty space to edit). Editing state shows a LineEdit pre-filled with the
// current tags space-joined; committing on blur/Enter splits on whitespace.
import { LineEdit } from "std-widgets.slint";

export component TagEditor inherits Rectangle {
    in property <[string]> tags;
    in property <string> placeholder: "add tags...";
    in property <color> pill-color: #3d4663;
    callback committed([string]);
    callback remove(string);

    property <bool> editing: false;

    min-height: 28px;
    border-radius: 4px;
    background: #232838;
    border-width: 1px;
    border-color: #3a4255;

    // --- Display state: pills + click-to-edit background ---
    if !root.editing: Rectangle {
        edit-area := TouchArea {
            // Click empty space -> edit. Pills sit above and consume their own clicks.
            clicked => { root.editing = true; }
        }
        HorizontalLayout {
            padding: 4px;
            spacing: 4px;
            alignment: start;
            for tag in root.tags: Rectangle {
                width: pill-text.preferred-width + 18px;
                height: 20px;
                border-radius: 10px;
                background: root.pill-color;
                pill-text := Text {
                    text: tag + "  ×";
                    color: #fff;
                    font-size: 11px;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                    width: 100%;
                    height: 100%;
                }
                TouchArea {
                    clicked => { root.remove(tag); }
                }
            }
        }
    }

    // --- Editing state: LineEdit ---
    if root.editing: edit := LineEdit {
        placeholder-text: root.placeholder;
        // Pre-fill handled from Rust by setting an in-out text property is
        // avoided; instead Rust reads `committed` words. Pre-fill via the
        // `init` callback below.
        init => {
            // Join current tags with spaces for editing.
            self.text = root.join-tags();
            self.focus();
        }
        accepted(t) => {
            root.commit(t);
        }
        // Commit on focus loss too.
        changed has-focus => {
            if (!self.has-focus) { root.commit(self.text); }
        }
    }

    // Helper: join tags with a space.
    pure function join-tags() -> string {
        // Slint has no fold; build via a property is overkill. Use Rust-provided
        // pre-join instead: expose `joined` if needed. For simplicity, Rust sets
        // tags and we reconstruct here is not possible without a loop, so the
        // implementer should instead expose an `in property <string> joined`
        // computed in Rust. See note.
        return "";
    }

    function commit(t: string) {
        // Emit raw text; Rust splits on whitespace and dedupes.
        root.committed-from-text(t);
        root.editing = false;
    }

    callback committed-from-text(string);
}
```

**Implementer note (important):** Slint cannot easily fold a `[string]` into one string inside markup. Resolve the pre-fill cleanly by giving `TagEditor` an extra `in property <string> joined;` that **Rust** sets (space-joined tags), and use `self.text = root.joined;` in the `LineEdit`'s `init`. Replace the placeholder `join-tags()`/`committed` design above with this concrete shape:

```slint
import { LineEdit } from "std-widgets.slint";

export component TagEditor inherits Rectangle {
    in property <[string]> tags;
    in property <string> joined;          // space-joined tags, set by Rust
    in property <string> placeholder: "add tags...";
    in property <color> pill-color: #3d4663;
    callback committed-text(string);      // raw text; Rust splits + dedupes
    callback remove(string);

    property <bool> editing: false;
    min-height: 28px;
    border-radius: 4px;
    background: #232838;
    border-width: 1px;
    border-color: #3a4255;

    if !root.editing: Rectangle {
        TouchArea { clicked => { root.editing = true; } }
        HorizontalLayout {
            padding: 4px; spacing: 4px; alignment: start;
            for tag in root.tags: Rectangle {
                width: pill-text.preferred-width + 18px;
                height: 20px; border-radius: 10px;
                background: root.pill-color;
                pill-text := Text {
                    text: tag + "  ×"; color: #fff; font-size: 11px;
                    horizontal-alignment: center; vertical-alignment: center;
                    width: 100%; height: 100%;
                }
                TouchArea { clicked => { root.remove(tag); } }
            }
        }
    }

    if root.editing: LineEdit {
        placeholder-text: root.placeholder;
        init => { self.text = root.joined; self.focus(); }
        accepted(t) => { root.committed-text(t); root.editing = false; }
        changed has-focus => {
            if (!self.has-focus) { root.committed-text(self.text); root.editing = false; }
        }
    }
}
```

Use **this** second version. Delete the first sketch; it exists only to explain the pitfall.

- [ ] **Step 3: Verify both widgets compile**

These files are imported by later tasks. Verify the project still builds (unused exported components are fine in Slint):

Run: `cargo build -p imgfind-gui`
Expected: builds clean (no Slint parse errors). If the compiler rejects `changed has-focus` syntax in this Slint version, use the documented form for reacting to focus changes in this project's Slint (check `range_slider.slint`/existing widgets for the supported callback style) — keep behavior (commit on focus loss) identical.

- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/ui/toggle.slint imgfind-gui/ui/tag_editor.slint
git commit -m "feat(gui): reusable Toggle and TagEditor Slint widgets"
```

---

### Task 6: GUI — left rail (brush editors + Most Recent) with state & persistence

**Files:**
- Modify: `imgfind-gui/ui/app.slint`
- Modify: `imgfind-gui/src/main.rs`
- Modify: `imgfind-gui/src/state.rs` or a small new helper module for brush/recent mutation (pure, testable)

**Interfaces:**
- Consumes: `imgfind::colors::BrushColor`, `imgfind::ui_state::TagBrush`, `TagEditor`/`Toggle` widgets, `Backend` tag methods.
- Produces: pure helpers `tagset::add_words(&mut Vec<String>, &str)` (split on whitespace, trim, dedupe, preserve order) and `tagset::remove(&mut Vec<String>, &str)`; Slint `rail-visible` property + brush/recent models + callbacks.

- [ ] **Step 1: Write pure tagset helpers with failing tests**

Create `imgfind-gui/src/tagset.rs` and add `mod tagset;` to `main.rs`:

```rust
//! Pure helpers for editing ordered, de-duplicated tag lists (brushes and the
//! "Most Recent" buffer).

/// Add whitespace-separated words to `list`, trimming and skipping duplicates,
/// preserving existing order and append order of new words.
pub fn add_words(list: &mut Vec<String>, text: &str) {
    for w in text.split_whitespace() {
        let w = w.trim();
        if !w.is_empty() && !list.iter().any(|t| t == w) {
            list.push(w.to_string());
        }
    }
}

/// Replace the list contents with the whitespace-separated words of `text`
/// (trim, dedupe, preserve order).
pub fn set_words(list: &mut Vec<String>, text: &str) {
    list.clear();
    add_words(list, text);
}

/// Remove `tag` if present.
pub fn remove(list: &mut Vec<String>, tag: &str) {
    list.retain(|t| t != tag);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_words_dedupes_and_trims() {
        let mut v = vec!["beach".to_string()];
        add_words(&mut v, "  sunset beach   john ");
        assert_eq!(v, vec!["beach", "sunset", "john"]);
    }

    #[test]
    fn set_words_replaces() {
        let mut v = vec!["old".to_string()];
        set_words(&mut v, "a b a");
        assert_eq!(v, vec!["a", "b"]);
    }

    #[test]
    fn remove_drops_one() {
        let mut v = vec!["a".to_string(), "b".to_string()];
        remove(&mut v, "a");
        assert_eq!(v, vec!["b"]);
    }
}
```

- [ ] **Step 2: Run helper tests, expect pass**

Run: `cargo test -p imgfind-gui tagset::`
Expected: 3 pass.

- [ ] **Step 3: Add the left rail to `app.slint`**

At the top of `app.slint` add imports:

```slint
import { TagEditor } from "tag_editor.slint";
import { Toggle } from "toggle.slint";
```

Add these properties to the root (near the other `in property` declarations):

```slint
    in property <bool> rail-visible: true;
    // Brush rows: one struct per color, index-aligned with BrushColor.
    in property <[{letter: string, color: color, tags: [string], joined: string}]> brushes;
    in property <[string]> recent-tags;
```

Add callbacks:

```slint
    callback brush-committed(int, string);   // (brush index, raw text)
    callback brush-remove(int, string);      // (brush index, tag)
    callback recent-remove(string);          // remove from mm buffer
    callback toggle-rail();
```

Wrap the existing top-level content so the rail sits to the left. The current layout is a `VerticalLayout` inside `app-keys`. Change `app-keys`'s child to a `HorizontalLayout` containing the rail (conditional) then the existing `VerticalLayout`:

```slint
        HorizontalLayout {
            // --- Left rail ---
            if root.rail-visible: Rectangle {
                width: 240px;
                background: #1a1f29;
                border-width: 1px;
                border-color: #3a4255;
                VerticalLayout {
                    padding: 10px;
                    spacing: 8px;
                    Text { text: "Brushes"; color: #9aa4b2; font-size: 13px; }
                    for brush[i] in root.brushes: HorizontalLayout {
                        spacing: 6px;
                        Rectangle {
                            width: 20px; height: 20px; border-radius: 10px;
                            background: brush.color;
                            Text {
                                text: brush.letter; color: #fff; font-size: 12px;
                                horizontal-alignment: center; vertical-alignment: center;
                                width: 100%; height: 100%;
                            }
                        }
                        TagEditor {
                            horizontal-stretch: 1;
                            tags: brush.tags;
                            joined: brush.joined;
                            pill-color: brush.color;
                            committed-text(t) => { root.brush-committed(i, t); }
                            remove(tag) => { root.brush-remove(i, tag); }
                        }
                    }
                    Rectangle { height: 1px; background: #3a4255; }
                    Text { text: "Most Recent"; color: #9aa4b2; font-size: 13px; }
                    TagEditor {
                        tags: root.recent-tags;
                        joined: "";
                        placeholder: "(applied tags appear here)";
                        remove(tag) => { root.recent-remove(tag); }
                    }
                }
            }

            // --- existing main column ---
            VerticalLayout {
                // ... existing content (search bar, filter bar, grid, etc.) ...
            }
        }
```

**Implementer note:** move the existing `VerticalLayout { padding: 16px; spacing: 12px; … }` (search/filter/grid) verbatim into the second child of this `HorizontalLayout`. Keep the `selected-index` `changed` handler and `capture-key-pressed` on `app-keys` unchanged.

- [ ] **Step 4: Wire rail state in `main.rs`**

Add holders near the other `Arc<Mutex<…>>` declarations:

```rust
    let brushes: Arc<Mutex<[Vec<String>; 5]>> = Arc::new(Mutex::new(Default::default()));
    let mm_buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
```

Add a helper that pushes the brush/recent models into Slint (call after any change and during restore):

```rust
    fn push_rail_models(w: &MainWindow, brushes: &[Vec<String>; 5], recent: &[String]) {
        use imgfind::colors::BrushColor;
        let swatch = |c: BrushColor| -> slint::Color {
            match c {
                BrushColor::Red => slint::Color::from_rgb_u8(0xd0, 0x4a, 0x4a),
                BrushColor::Green => slint::Color::from_rgb_u8(0x3a, 0xa6, 0x60),
                BrushColor::Yellow => slint::Color::from_rgb_u8(0xd2, 0xb0, 0x3a),
                BrushColor::Purple => slint::Color::from_rgb_u8(0x9a, 0x5a, 0xc8),
                BrushColor::Blue => slint::Color::from_rgb_u8(0x4a, 0x7c, 0xd0),
            }
        };
        let rows: Vec<BrushRow> = BrushColor::ALL
            .iter()
            .map(|&c| {
                let tags = &brushes[c.index()];
                BrushRow {
                    letter: c.letter().into(),
                    color: swatch(c),
                    tags: slint::ModelRc::new(slint::VecModel::from(
                        tags.iter().map(|s| s.clone().into()).collect::<Vec<slint::SharedString>>(),
                    )),
                    joined: tags.join(" ").into(),
                }
            })
            .collect();
        w.set_brushes(slint::ModelRc::new(slint::VecModel::from(rows)));
        w.set_recent_tags(slint::ModelRc::new(slint::VecModel::from(
            recent.iter().map(|s| s.clone().into()).collect::<Vec<slint::SharedString>>(),
        )));
    }
```

`BrushRow` is the generated struct for the inline struct in the `brushes` property — Slint generates it; reference it by the name Slint assigns (often the anonymous struct must be declared as a named `struct` in `.slint` to get a stable Rust name). **To get a clean Rust type, declare a named struct in `app.slint`:**

```slint
export struct BrushRow {
    letter: string,
    color: color,
    tags: [string],
    joined: string,
}
```

and change the property to `in property <[BrushRow]> brushes;`.

Wire callbacks:

```rust
    // brush-committed(index, text): set that brush's tags, persist, refresh model.
    {
        let brushes = brushes.clone();
        let w = window.as_weak();
        window.on_brush_committed(move |idx, text| {
            let idx = idx as usize;
            if let Some(w) = w.upgrade() {
                let mut b = brushes.lock().unwrap();
                if let Some(slot) = b.get_mut(idx) {
                    tagset::set_words(slot, text.as_str());
                }
                let recent = mm_buffer_for_push(); // see note
                push_rail_models(&w, &b, &recent);
            }
        });
    }
```

**Note:** the closures need both `brushes` and `mm_buffer`; clone both into each closure as the existing code does for multi-holder callbacks. For `brush-remove`, call `tagset::remove(slot, tag.as_str())`. For `recent-remove`, lock `mm_buffer`, `tagset::remove`, and `push_rail_models`. Persist brushes on change (see Step 6).

- [ ] **Step 5: Wire the rail-visible property + toggle**

Add an `Arc<Mutex<bool>>`? No — rail visibility is pure UI; keep it as a Slint property and a Rust shadow only for persistence. Add a `toggle-rail` callback handler that flips `w.get_rail_visible()` → `w.set_rail_visible(!v)`. (The backtick keypress will call this in Task 9.)

```rust
    {
        let w = window.as_weak();
        window.on_toggle_rail(move || {
            if let Some(w) = w.upgrade() {
                w.set_rail_visible(!w.get_rail_visible());
            }
        });
    }
```

- [ ] **Step 6: Persist + restore brushes/recent/rail**

In `persist_session` (builds the `UiState` before save), add:

```rust
    state.brushes = {
        let b = brushes.lock().unwrap();
        std::array::from_fn(|i| imgfind::ui_state::TagBrush { tags: b[i].clone() })
    };
    state.recent_tags = mm_buffer.lock().unwrap().clone();
    state.rail_visible = window.get_rail_visible();
```

In `restore_session`, after loading `UiState st`:

```rust
    {
        let mut b = brushes.lock().unwrap();
        for i in 0..5 { b[i] = st.brushes[i].tags.clone(); }
        *mm_buffer.lock().unwrap() = st.recent_tags.clone();
        push_rail_models(&window, &b, &st.recent_tags);
    }
    window.set_rail_visible(st.rail_visible);
```

On the **fresh-DB path** (`start_default_browse`, when `get_ui_state()` is `None`), set `window.set_rail_visible(true)` and push empty rail models so the rail shows.

- [ ] **Step 7: Build, run, and visually verify**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui tagset::`
Then run the GUI against a test DB and confirm: the left rail shows five colored swatches (r/g/y/p/b) each with a tag editor; typing `beach sunset` in the red editor and clicking away shows two red pills; clicking a pill removes it; "Most Recent" is empty; relaunching preserves brush contents.

Run: `cargo run -p imgfind-gui -- --dir <a-test-dir-with-.imgfind>`
Expected: rail renders and brush editing persists across restart.

- [ ] **Step 8: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs imgfind-gui/src/tagset.rs
git commit -m "feat(gui): left rail with color brushes and Most Recent buffer"
```

---

### Task 7: GUI — detail-panel per-image tag editor

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (detail panel section)
- Modify: `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes: `TagEditor`, `Backend::tags_for/add_tag/remove_tag`, the existing `detail` holder (`DetailState { path, filename }`).
- Produces: Slint `detail-tags`/`detail-tags-joined` properties + `detail-tag-committed(string)` / `detail-tag-remove(string)` callbacks.

- [ ] **Step 1: Add properties + editor to the detail panel**

Add root properties:

```slint
    in property <[string]> detail-tags;
    in property <string> detail-tags-joined;
```

Add callbacks:

```slint
    callback detail-tag-committed(string);
    callback detail-tag-remove(string);
```

In the detail panel `VerticalLayout` (after the metadata `Text`, before the buttons), insert:

```slint
            Text { text: "Tags"; color: #9aa4b2; font-size: 12px; }
            TagEditor {
                tags: root.detail-tags;
                joined: root.detail-tags-joined;
                committed-text(t) => { root.detail-tag-committed(t); }
                remove(tag) => { root.detail-tag-remove(tag); }
            }
```

- [ ] **Step 2: Load tags when the detail panel opens**

In `on_tile_selected` (and any path that opens/updates the detail panel, e.g. `invoke_tile_selected` used by `on_grid_nav`), after setting the detail path, spawn a background fetch of tags and push them to the UI:

```rust
    // inside the detail-open path, after we know `path`:
    {
        let backend = backend.clone();
        let w = window.as_weak();
        let path = path.clone();
        std::thread::spawn(move || {
            let tags = backend.tags_for(&path).unwrap_or_default();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = w.upgrade() {
                    push_detail_tags(&w, &tags);
                }
            });
        });
    }
```

Add helper:

```rust
    fn push_detail_tags(w: &MainWindow, tags: &[String]) {
        w.set_detail_tags(slint::ModelRc::new(slint::VecModel::from(
            tags.iter().map(|s| s.clone().into()).collect::<Vec<slint::SharedString>>(),
        )));
        w.set_detail_tags_joined(tags.join(" ").into());
    }
```

- [ ] **Step 3: Wire commit/remove callbacks**

```rust
    // detail-tag-committed: diff against current tags; add the new ones.
    {
        let backend = backend.clone();
        let detail = detail.clone();
        let w = window.as_weak();
        window.on_detail_tag_committed(move |text| {
            let Some(path) = detail.lock().unwrap().as_ref().map(|d| d.path.clone()) else { return };
            let backend = backend.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                // set_words semantics: the editor text is the full desired set.
                let mut desired = Vec::new();
                tagset::set_words(&mut desired, text.as_str());
                let current = backend.tags_for(&path).unwrap_or_default();
                for t in &desired {
                    if !current.contains(t) { let _ = backend.add_tag(&path, t); }
                }
                for t in &current {
                    if !desired.contains(t) { let _ = backend.remove_tag(&path, t); }
                }
                let tags = backend.tags_for(&path).unwrap_or_default();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w.upgrade() { push_detail_tags(&w, &tags); }
                });
            });
        });
    }
```

`on_detail_tag_remove` is simpler: `backend.remove_tag(&path, &tag)` then re-fetch + `push_detail_tags`.

- [ ] **Step 4: Build, run, visually verify**

Run: `cargo build -p imgfind-gui`
Then run the GUI: click a tile → detail panel shows a "Tags" editor; type `cat dog`, click away → two pills; reopen the tile → pills persist (loaded from DB); click a pill → removed; navigate with `j`/`k` while panel open → tags update per image.

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): per-image tag editor in detail panel"
```

---

### Task 8: GUI — filter-pane tag row (editor + AND/OR + enable toggle)

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (filter bar area)
- Modify: `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes: `TagEditor`, `Toggle`, `Filters` tag fields, the existing `filters: Arc<Mutex<Filters>>` holder and the existing debounce (`start_debounce`).
- Produces: Slint `filter-tags`/`filter-tags-joined`/`tag-match-and`/`tags-enabled` properties + `filter-tags-committed(string)` / `filter-tag-remove(string)` / `tag-match-toggled()` / `tags-enabled-toggled()` callbacks.

- [ ] **Step 1: Add filter-tag row to `app.slint`**

Root properties:

```slint
    in property <[string]> filter-tags;
    in property <string> filter-tags-joined;
    in property <bool> tag-match-and: true;   // true = AND (AllOf), false = OR
    in property <bool> tags-enabled: false;
```

Callbacks:

```slint
    callback filter-tags-committed(string);
    callback filter-tag-remove(string);
    callback tag-match-toggled();
    callback tags-enabled-toggled();
```

Add a new `HorizontalLayout` row directly beneath the existing filter bar `HorizontalLayout` (still inside the main `VerticalLayout`):

```slint
            HorizontalLayout {
                spacing: 8px;
                alignment: start;
                Text { text: "Tags:"; color: #9aa4b2; vertical-alignment: center; }
                TagEditor {
                    width: 240px;
                    tags: root.filter-tags;
                    joined: root.filter-tags-joined;
                    placeholder: "filter by tags...";
                    committed-text(t) => { root.filter-tags-committed(t); }
                    remove(tag) => { root.filter-tag-remove(tag); }
                }
                // AND/OR toggle button (ASCII text).
                Rectangle {
                    width: andor-text.preferred-width + 16px;
                    height: 24px; border-radius: 4px;
                    background: #2d3446; border-width: 1px; border-color: #3a4255;
                    andor-text := Text {
                        text: root.tag-match-and ? "AND" : "OR";
                        color: #cfd6e2; font-size: 12px;
                        horizontal-alignment: center; vertical-alignment: center;
                        width: 100%; height: 100%;
                    }
                    TouchArea { clicked => { root.tag-match-toggled(); } }
                }
                Toggle {
                    on: root.tags-enabled;
                    toggled(v) => { root.tags-enabled-toggled(); }
                }
                Text {
                    text: "filter on"; color: #9aa4b2; vertical-alignment: center;
                    font-size: 12px;
                }
            }
```

- [ ] **Step 2: Wire callbacks in `main.rs`**

Add a helper to push filter-tag models:

```rust
    fn push_filter_tags(w: &MainWindow, f: &imgfind::filters::Filters) {
        w.set_filter_tags(slint::ModelRc::new(slint::VecModel::from(
            f.tags.iter().map(|s| s.clone().into()).collect::<Vec<slint::SharedString>>(),
        )));
        w.set_filter_tags_joined(f.tags.join(" ").into());
        w.set_tag_match_and(matches!(f.tag_match, imgfind::filters::TagMatch::AllOf));
        w.set_tags_enabled(f.tags_enabled);
    }
```

Wire each callback to mutate `filters`, push the model, and kick the existing debounce. Example:

```rust
    {
        let filters = filters.clone();
        let w = window.as_weak();
        // plus the same captures start_debounce needs elsewhere (timer, state, mode, gen, backend, selected)
        window.on_filter_tags_committed(move |text| {
            if let Some(w) = w.upgrade() {
                let mut f = filters.lock().unwrap();
                tagset::set_words(&mut f.tags, text.as_str());
                push_filter_tags(&w, &f);
                drop(f);
                start_debounce(/* … same args as the other filter callbacks … */);
            }
        });
    }
```

`on_filter_tag_remove` → `tagset::remove(&mut f.tags, tag)`, push, debounce.
`on_tag_match_toggled` → flip `f.tag_match` between `AllOf`/`AnyOf`, push, debounce.
`on_tags_enabled_toggled` → `f.tags_enabled = !f.tags_enabled`, push, debounce.

**Note:** `start_debounce` already exists and takes a specific capture set (timer, weak window, state, mode, grid_gen, backend, filters, selected). Match its exact signature; these callbacks must capture the same holders the existing `on_filters_changed` callback does.

- [ ] **Step 3: Restore filter-tag UI on session restore**

In `restore_session`, after installing `st.filters` into the `filters` holder, call `push_filter_tags(&window, &st.filters)` (under the `restoring` guard so it doesn't fire a query). On fresh DB, push the default (`tags_enabled=false`, AND).

- [ ] **Step 4: Build, run, verify end-to-end**

Run: `cargo build -p imgfind-gui`
Then run the GUI on a DB with tagged images:
- Tag a few images (Task 7 editor). Type a tag in the filter editor, flip the enable Toggle on → grid narrows to images with that tag.
- Add a second tag; AND shows only images with both; click AND→OR shows the union.
- Toggle off → all results return; toggle on → filtered again, tags retained.
- Relaunch → filter tags + mode + enable state restored.

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): filter-pane tag row with AND/OR and enable toggle"
```

---

### Task 9: GUI — tag modal + keyboard chords wiring

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (modal overlay + key forwarding + focus exemption)
- Modify: `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes: `chords::{Pending, Action, resolve}`, `BrushColor`, all action targets built in Tasks 6–8 (paint = `Backend::add_tag` + update `mm_buffer` + push rail; load filter brush; toggle filter; toggle rail; open modal).
- Produces: Slint `tag-modal-open` property + `key(string)` callback + `tag-modal-commit(string)` / `tag-modal-cancel()` callbacks; in-memory `pending_chord` + timeout `Timer`.

- [ ] **Step 1: Add the modal + key callback to `app.slint`**

Root properties + callbacks:

```slint
    in property <bool> tag-modal-open: false;
    callback key(string);
    callback tag-modal-commit(string);
    callback tag-modal-cancel();
```

In `app-keys`'s `capture-key-pressed`, **before** the grid-navigation block (and after the search-input and lightbox blocks), forward unhandled single keys to Rust. The cleanest approach: when the modal is open, let its LineEdit type; otherwise, for keys that may be chord keys, call `root.key(event.text)` and let Rust decide. To avoid disturbing existing nav, forward the specific chord trigger keys plus prefix-completion keys and return `accept` only when Rust consumes them.

Concretely, add near the top of `capture-key-pressed` (after the `search-input.has-focus` block):

```slint
            // Tag modal owns the keyboard while open.
            if (root.tag-modal-open) {
                if (event.text == Key.Escape) { root.tag-modal-cancel(); return accept; }
                return reject; // let the modal's LineEdit type
            }
```

Then, just before `// Grid navigation.`, add chord forwarding for the trigger/prefix keys:

```slint
            // Tag chord keys: backtick, t, m, f, and brush letters/their
            // completions. Forward to Rust, which holds the pending-prefix state.
            if (event.text == "`" || event.text == "t" || event.text == "m" || event.text == "f"
                || event.text == "r" || event.text == "g" || event.text == "y"
                || event.text == "p" || event.text == "b") {
                root.key(event.text);
                return accept;
            }
```

**Important interaction:** the brush letters `r/g/y/p/b` and `t`/`m`/`f` are only meaningful mid-chord or as triggers; forwarding them unconditionally means a bare `r` (no pending prefix) reaches Rust, where `resolve(None, "r")` returns no action — harmless. But bare `t`/`m`/`f`/`` ` `` ARE triggers, handled by Rust. None of these letters are existing grid shortcuts (`h/j/k/l`, arrows, Enter, Space, Esc), so no conflict. Keep the existing nav block exactly as-is after this.

Add the modal overlay at the end of the root (sibling to the lightbox overlay):

```slint
    if root.tag-modal-open: Rectangle {
        background: #000000cc;
        TouchArea { clicked => { root.tag-modal-cancel(); } } // click backdrop cancels
        Rectangle {
            width: 420px; height: 90px;
            background: #252b3a; border-radius: 8px;
            border-width: 1px; border-color: #3a4255;
            VerticalLayout {
                padding: 14px; spacing: 8px;
                Text { text: "Add tags (space-separated)"; color: #cfd6e2; font-size: 13px; }
                modal-input := LineEdit {
                    placeholder-text: "beach sunset ...";
                    init => { self.focus(); }
                    accepted(t) => { root.tag-modal-commit(t); }
                }
            }
        }
    }
```

- [ ] **Step 2: Add chord state + timeout in `main.rs`**

```rust
    use chords::{Action, Pending};
    let pending_chord: Arc<Mutex<Pending>> = Arc::new(Mutex::new(Pending::None));
    let chord_timer = Rc::new(slint::Timer::default());
```

- [ ] **Step 3: Implement the `key` dispatcher**

```rust
    {
        let pending_chord = pending_chord.clone();
        let chord_timer = chord_timer.clone();
        let brushes = brushes.clone();
        let mm_buffer = mm_buffer.clone();
        let filters = filters.clone();
        let detail = detail.clone();
        let selected = selected.clone();
        let lb_index = lb_index.clone();
        let state = state.clone();
        let backend = backend.clone();
        let w = window.as_weak();
        // plus debounce captures (timer, mode, grid_gen) matching start_debounce
        window.on_key(move |key| {
            let Some(w) = w.upgrade() else { return };
            let mut pend = pending_chord.lock().unwrap();
            let (next, action) = chords::resolve(*pend, key.as_str());
            *pend = next;
            drop(pend);

            // Manage prefix timeout: if we entered a prefix, arm an ~800ms reset.
            if matches!(next, Pending::AwaitM | Pending::AwaitF) {
                let pc = pending_chord.clone();
                chord_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(800), move || {
                    *pc.lock().unwrap() = Pending::None;
                });
            } else {
                chord_timer.stop();
            }

            let Some(action) = action else { return };
            match action {
                Action::ToggleRail => { w.set_rail_visible(!w.get_rail_visible()); }
                Action::OpenTagModal => { w.set_tag_modal_open(true); }
                Action::PaintBrush(c) => {
                    let tags = brushes.lock().unwrap()[c.index()].clone();
                    apply_tags_to_focused(&w, &backend, &detail, &selected, &lb_index, &state, &mm_buffer, tags.clone());
                    *mm_buffer.lock().unwrap() = tags;
                    push_rail_models(&w, &brushes.lock().unwrap(), &mm_buffer.lock().unwrap());
                }
                Action::RepeatLast => {
                    let tags = mm_buffer.lock().unwrap().clone();
                    apply_tags_to_focused(&w, &backend, &detail, &selected, &lb_index, &state, &mm_buffer, tags);
                }
                Action::LoadBrushIntoFilter(c) => {
                    let tags = brushes.lock().unwrap()[c.index()].clone();
                    let mut f = filters.lock().unwrap();
                    f.tags = tags;
                    push_filter_tags(&w, &f);
                    drop(f);
                    start_debounce(/* … */);
                }
                Action::ToggleTagFilter => {
                    let mut f = filters.lock().unwrap();
                    f.tags_enabled = !f.tags_enabled;
                    push_filter_tags(&w, &f);
                    drop(f);
                    start_debounce(/* … */);
                }
            }
        });
    }
```

- [ ] **Step 4: Implement `apply_tags_to_focused` + modal commit/cancel**

```rust
    /// Resolve the currently-focused image's relative path: lightbox image if
    /// open, else detail image if open, else the selected grid tile.
    fn focused_path(
        detail: &Arc<Mutex<Option<DetailState>>>,
        selected: &Arc<Mutex<Option<usize>>>,
        lb_index: &Arc<Mutex<Option<usize>>>,
        state: &Arc<Mutex<SearchState>>,
    ) -> Option<String> {
        if let Some(i) = *lb_index.lock().unwrap() {
            return state.lock().unwrap().results.get(i).map(|r| r.path.clone());
        }
        if let Some(d) = detail.lock().unwrap().as_ref() {
            return Some(d.path.clone());
        }
        let i = (*selected.lock().unwrap())?;
        state.lock().unwrap().results.get(i).map(|r| r.path.clone())
    }
```

`apply_tags_to_focused` resolves the path via `focused_path`, then spawns a thread that calls `backend.add_tag(&path, t)` for each tag; if the detail panel is showing that path, re-fetch and `push_detail_tags`. (Pass the captured holders through; keep the signature consistent with the call sites above — adjust arg list to whatever the implementer finds cleanest, but it MUST use `focused_path` for target resolution.)

Modal handlers:

```rust
    {
        let mm_buffer = mm_buffer.clone();
        let backend = backend.clone();
        let detail = detail.clone(); let selected = selected.clone();
        let lb_index = lb_index.clone(); let state = state.clone();
        let brushes = brushes.clone();
        let w = window.as_weak();
        window.on_tag_modal_commit(move |text| {
            let Some(w) = w.upgrade() else { return };
            let mut tags = Vec::new();
            tagset::set_words(&mut tags, text.as_str());
            apply_tags_to_focused(&w, &backend, &detail, &selected, &lb_index, &state, &mm_buffer, tags.clone());
            *mm_buffer.lock().unwrap() = tags;
            push_rail_models(&w, &brushes.lock().unwrap(), &mm_buffer.lock().unwrap());
            w.set_tag_modal_open(false);
            // Return focus to app-keys so chords/nav resume.
        });
    }
    {
        let w = window.as_weak();
        window.on_tag_modal_cancel(move || {
            if let Some(w) = w.upgrade() { w.set_tag_modal_open(false); }
        });
    }
```

- [ ] **Step 5: Build, run, verify the full keyboard flow**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Then run the GUI and verify:
- `` ` `` toggles the left rail.
- Select a tile, press `t` → modal opens; type `beach sunset john`, Enter → image gets 3 tags; "Most Recent" shows all 3.
- Click `john` off in Most Recent; navigate to next tile; `mm` → applies `beach sunset` only.
- Curate the red brush (`beach sunset`); focus a tile; `mr` → paints those, Most Recent updates.
- `fr` → loads red brush into the filter editor; `ft` → toggles filtering on/off; grid responds.
- Open the lightbox (`Space`), press `mg` → green brush paints the lightbox image.
- Typing in the search box, a brush editor, the filter editor, or the modal does NOT trigger chords.

- [ ] **Step 6: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): tag modal and keyboard chord dispatch (t/m_/mm/f_/ft/backtick)"
```

---

### Task 10: Docs — CLAUDE.md, README, USAGE

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md` (if it documents GUI features)
- Modify: `USAGE.md` (if it documents GUI keys)

**Interfaces:** none (documentation).

- [ ] **Step 1: Update `CLAUDE.md`**

In the "Native GUI" architecture bullet, add a sentence describing tag support: free-text tags via the `t` modal / detail panel; five color brushes in a backtick-toggled left rail; the editable "Most Recent" (`mm`) staging buffer; `m`+color paints, `mm` repeats, `f`+color loads the filter, `ft` toggles tag filtering; tag filtering (AND/OR + enable) via the shared `Filters`/`build_filter_clause` seam (no migration; uses existing `tags`/`image_tags`). Persisted in `ui_state` (`brushes`, `recent_tags`, `rail_visible`, plus `Filters.tags/tag_match/tags_enabled`). Link the spec `docs/superpowers/specs/2026-06-20-gui-tag-support-design.md`.

Add `colors.rs` and the GUI `chords.rs`/`tagset.rs` to the relevant module descriptions.

- [ ] **Step 2: Update `README.md` / `USAGE.md`**

If these files document GUI keyboard shortcuts or features, add the tag keys (`` ` ``, `t`, `m`+`r/g/y/p/b`, `mm`, `f`+`r/g/y/p/b`, `ft`) and a short "Tagging" feature paragraph. If a file has no relevant section, skip it and note so in the commit.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md README.md USAGE.md
git commit -m "docs: document GUI tag support (brushes, modal, filtering, keys)"
```

---

## Self-Review

**Spec coverage:**
- Free-text tags via `t` modal → Task 9. ✓
- Most-recent left-rail area = editable `mm` buffer; click removes from buffer → Tasks 6, 9. ✓
- Color brushes (circle + text→pills editor) → Tasks 5, 6. ✓
- `m`+color paint, `mm` repeat → Tasks 3, 9. ✓
- Per-image tagging in detail panel → Task 7. ✓
- Tag filter input in filter pane (same editor) + AND/OR toggle + `ft` + Toggle widget → Tasks 5, 8, 9. ✓
- `f`+color loads brush into filter (keeps mode) → Tasks 3, 9. ✓
- New slide-toggle widget → Task 5. ✓
- Backtick toggles rail → Tasks 3, 9. ✓
- Works in grid/detail/lightbox (`focused_path`) → Task 9. ✓
- Persistence (brushes/recent/rail/filter) → Tasks 1, 6, 8. ✓
- No schema migration → Task 2 (uses existing tables). ✓

**Placeholder scan:** The only deliberate "explanatory sketch" is the first `tag_editor.slint` version in Task 5, explicitly superseded by the second concrete version (instruction: use the second, delete the first). All other steps carry concrete code. The `start_debounce(/* … */)` placeholders point to an existing function whose exact capture set the implementer matches from neighboring callbacks — its signature is in the codebase, not invented here.

**Type consistency:** `BrushColor` (core) reused by `chords` and rail; `TagBrush` (core) used by `UiState` and persistence; `TagMatch` (core) used by filters + `tag-match-and` Slint bool mapping; `tagset::{add_words,set_words,remove}` used consistently in Tasks 6–9; `push_rail_models`/`push_detail_tags`/`push_filter_tags`/`focused_path`/`apply_tags_to_focused` helper names used consistently across tasks. Slint callbacks (`brush-committed`, `committed-text`, `filter-tags-committed`, `key`, `tag-modal-commit`) are named consistently between markup and `on_*` handlers.

**Note for implementers:** Slint identifiers with hyphens map to Rust `snake_case` (`tag-modal-open` → `get_tag_modal_open`/`set_tag_modal_open`; `committed-text` → `on_committed_text`). Follow the project's existing generated-name conventions; consult the `slint` skill for `changed has-focus`, struct models, and threaded `ModelRc` updates if anything doesn't compile.
