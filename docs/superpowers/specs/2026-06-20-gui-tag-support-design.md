# GUI Tag Support — Design

Date: 2026-06-20
Crate: `imgfind-gui` (+ small additions to core `imgfind`)
Status: Approved (brainstorm → spec)

## Summary

Add a tagging system to the native Slint GUI. Users assign free-text tags to
images several ways (a text modal, color "brush" shortcuts, a per-image editor),
manage five color brushes and a live "Most Recent" staging set in a toggleable
left rail, and filter results by tags (AND/OR, with an enable toggle) in the
filter pane.

Colors are **brushes** — a pure input convenience. A color is a curated *set of
tags*; "painting" with it assigns those tags as ordinary (colorless) tags. No
color is ever stored on an image or on a tag. Editing a brush only changes what a
future paint with that brush will apply; it never retroactively changes anything
already assigned.

## Goals

- Assign/remove free-text tags on images from the GUI.
- Five color brushes (red/green/yellow/purple/blue), each a persisted set of tags.
- Keyboard-driven painting: `t` modal, `m`+color, `mm` repeat — working wherever
  an image is focused (grid, detail panel, lightbox).
- A toggleable left rail: the five brush editors + an editable "Most Recent"
  (`mm`) staging set.
- Per-image tag editing in the right-side detail panel.
- Filter results by tags (AND/OR), combinable with existing size/type/GPS
  filters, with a slide-toggle to enable/disable without losing the chosen tags.
- A new reusable slide-toggle widget and a reusable text⇄pills tag-editor widget.

## Non-goals

- No per-image color labels (Finder/Lightroom-style colored flags). Colors are
  brushes only.
- No tag rename/merge/delete-from-registry management UI.
- No tag autocomplete/suggestions in this iteration.
- No CLI tag commands (the core methods exist already; CLI surface is out of
  scope here).
- No tag column in search results / no tag-based sort.

## Data model & persistence

### SQLite — no migration required

The baseline schema already contains the tables and (currently unused) methods:

- `tags (id, name UNIQUE, created_at)`
- `image_tags (image_id, tag_id, PRIMARY KEY(image_id, tag_id))` — both FKs
  cascade on delete.
- Existing `Database` methods: `create_tag`, `tag_image(&RelativePath, &str)`,
  `untag_image(&RelativePath, &str)`, `tags_for_image(&RelativePath)`,
  `list_tags()`, `images_by_tag(&str)`.

Because colors are not persisted on images, **no schema change is needed.**
`LATEST_MIGRATION_VERSION` stays at 3.

### Session state — extend `ui_state` JSON

All new persisted UI state lives in the existing single-row `ui_state` JSON blob
(per-DB, auto round-trips via `get_ui_state`/`set_ui_state`). Every new field gets
`#[serde(default)]` for forward/backward compatibility.

New struct (in `imgfind` core, alongside `UiState`):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TagBrush {
    pub tags: Vec<String>,   // curated set for this color
}
```

`UiState` gains:

```rust
#[serde(default)] pub brushes: [TagBrush; 5],   // red,green,yellow,purple,blue
#[serde(default)] pub recent_tags: Vec<String>, // the live "Most Recent" / mm buffer
#[serde(default)] pub rail_visible: bool,        // left-rail show/hide (default true)
```

Note: `recent_tags` is the persisted form of the `mm` staging buffer (see below),
not a rolling history.

Brush color order is fixed and indexed `0..5` = red, green, yellow, purple, blue.
A small shared constant maps color → index → mnemonic letter (`r g y p b`) and →
swatch RGB, defined once in core so both Rust and the Slint bridge agree.

### Tag filter — extend `Filters`

`Filters` (in `src/filters.rs`, serialized inside `ui_state.filters`) gains:

```rust
#[serde(default)] pub tags: Vec<String>,
#[serde(default)] pub tag_match: TagMatch,   // AllOf (AND) | AnyOf (OR)
#[serde(default)] pub tags_enabled: bool,    // the `ft` toggle

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TagMatch { #[default] AllOf, AnyOf }
```

These persist automatically (they live on `Filters`, already in `ui_state`).

## Filtering logic

`build_filter_clause(&Filters) -> (String, Vec<Value>)` is extended. The tag
clause is appended (ANDed) to the existing size/type/GPS clauses, so **both**
`browse_all` and `search_similar_images_meta` inherit it unchanged (they already
splice the returned fragment + params).

Tag clause is emitted only when `tags_enabled && !tags.is_empty()`:

- **AllOf (AND):** one correlated `EXISTS` per tag —
  ```sql
  AND EXISTS (SELECT 1 FROM image_tags it JOIN tags t ON t.id = it.tag_id
              WHERE it.image_id = i.id AND t.name = ?)
  ```
  repeated per tag (one bound param each).
- **AnyOf (OR):** a single `EXISTS` with `t.name IN (?, ?, …)` (one param per tag).

`i` is the `images` alias already used by both queries. Params are pushed in clause
order to match the existing `params_from_iter` binding.

When `tags_enabled` is false the clause is empty (results unfiltered by tags) even
if `tags` is non-empty — so `ft` toggles filtering on/off without discarding the
chosen tags.

## Keyboard chords

All keys already funnel through the single `app-keys` `capture-key-pressed`
FocusScope in `app.slint`. The chord logic is a **pure, unit-tested Rust module**
(`imgfind-gui/src/chords.rs`); the Slint side only forwards a key string to an
`on_key(string)` callback, and only when no text input has focus.

State: `enum Pending { None, AwaitM, AwaitF }`. Pure fn
`resolve(pending, key) -> (next_pending, Option<Action>)`:

| Input | Result |
|-------|--------|
| `` ` `` | ToggleRail |
| `t` (Pending::None) | OpenTagModal |
| `m` (None) | → AwaitM |
| `f` (None) | → AwaitF |
| `r/g/y/p/b` (AwaitM) | PaintBrush(color) |
| `m` (AwaitM) | RepeatLast (`mm`) |
| `r/g/y/p/b` (AwaitF) | LoadBrushIntoFilter(color) |
| `t` (AwaitF) | ToggleTagFilter (`ft`) |
| any other key | cancel pending → None, no action (key falls through to existing nav) |

A pending prefix also auto-cancels after a short timeout (~800ms, Slint `Timer`)
so a stray `m`/`f` doesn't strand the state. Existing `h/j/k/l`/arrow nav and all
current shortcuts are unchanged (they only fire when `Pending::None` and the key
isn't consumed above).

**Focus exemption:** the current logic passes keys through while
`search-input.has-focus`. This is generalized so chords are suppressed whenever
*any* text input has focus — the search field, the tag-modal input, a brush
editor's text field, the detail editor's text field, or the filter editor's text
field. Escape blurs back to `app-keys`.

### Actions (Rust handlers)

- **OpenTagModal** — show the centered modal `LineEdit`, focused. Enter splits the
  text on whitespace into words; each becomes a tag on the focused image; the set
  replaces the `mm` buffer (= "Most Recent"); modal closes. Esc cancels.
- **PaintBrush(color)** — apply that brush's tags to the focused image; replace the
  `mm` buffer with the brush's tags.
- **RepeatLast** — apply the current `mm` buffer to the focused image. No-op on
  empty buffer.
- **LoadBrushIntoFilter(color)** — set `Filters.tags` to the brush's tags (keep the
  current `tag_match` mode); trigger the debounced re-query if `tags_enabled`.
- **ToggleTagFilter** — flip `Filters.tags_enabled`; trigger debounced re-query.
- **ToggleRail** — flip `rail_visible`.

"Focused image" = the lightbox image when the lightbox is open, else the detail
panel's image when open, else the selected grid tile. If nothing is focused, paint
actions are a no-op.

## UI components (Slint)

### New reusable widgets

- **`ui/toggle.slint`** — a slide toggle. A rounded-pill track with a circle knob
  that animates to the left (off) or right (on). Track is dark grey
  (`#3a4255`-ish) when off, dark green (`#2e7d46`-ish) when on. `in property
  <bool> on`, `callback toggled(bool)`. Follows the `range_slider.slint`
  convention (Rectangle base, TouchArea, callback out).
- **`ui/tag_editor.slint`** — text⇄pills editor. Two visual states:
  - **Editing:** a `LineEdit` with the tags as space-separated text.
  - **Display:** the tags rendered as round button pills (clickable).
  Blur (focus lost) commits the text → pills (split on whitespace, dedupe). Click
  a pill → emits `remove(string)`. Click empty area of the display → return to the
  editing state, pre-filled with the *current* tags (any pill removed meanwhile is
  not re-shown). Props: `in property <[string]> tags`, optional `placeholder`,
  optional `pill-color`. Callbacks: `committed([string])`, `remove(string)`,
  `edit-requested()`. Reused in brushes, detail panel, and filter pane.

### Left rail

A fixed-width panel (left side), shown/hidden by `rail_visible` (backtick).
Contents top→bottom:

- Five brush rows, each: a colored swatch circle showing its mnemonic letter
  (`r/g/y/p/b`) + a `tag_editor` bound to that brush's tags (`pill-color` = the
  swatch color). Editing a brush updates `UiState.brushes[i]` and persists. This
  does **not** alter any already-assigned image tags.
- A divider + a **"Most Recent"** label + a `tag_editor` (non-colored pills) bound
  to the `mm` buffer. Clicking a pill removes it from the buffer only (does not
  touch images or brushes). This area is *display-only for pills* (its commit path
  is unused; the buffer is populated by paint actions), but reuses the same widget
  for visual consistency.

### Detail panel

The existing right-side panel gains one `tag_editor` (plain, non-colored) bound to
the selected image's tags. Commit adds new tags / removes dropped ones on that
image; click-pill removes that tag from the image. Edits here also refresh the
visible tags if the filter would now exclude the image (handled via the normal
debounced re-query when tag filtering is active).

### Filter pane

Beneath the existing filter bar, a tag-filter row:

- A `tag_editor` bound to `Filters.tags`.
- An **AND/OR** text button toggling `tag_match` (label shows current mode, ASCII
  text only per the Slint glyph constraint).
- A **`Toggle`** (the new widget) bound to `Filters.tags_enabled` with a small
  label; this is the clickable equivalent of `ft`.

Any change here (tags, mode, enable) runs through the **existing 250ms filter
debounce** → re-query (browse / text-search / similarity, per current mode).

### Tag modal

A centered overlay (semi-transparent backdrop) with a single `LineEdit`, opened by
`t`. Enter commits (whitespace-split → tags on focused image, replace `mm`
buffer), Esc cancels. While open it owns keyboard focus; chords are suppressed.

## Rust wiring (`imgfind-gui/src/main.rs` + helpers)

New `Arc<Mutex<…>>` / holder state:

- `brushes: Arc<Mutex<[Vec<String>; 5]>>`
- `mm_buffer: Arc<Mutex<Vec<String>>>` (the "Most Recent" staging set)
- `pending_chord: Arc<Mutex<chords::Pending>>` (+ a Slint `Timer` for timeout)
- `rail_visible` reflected to a Slint `in property <bool>`

New `Backend` methods (thin wrappers over existing `Database` calls, run on
background threads like the others): `add_tag`, `remove_tag`, `tags_for`,
`all_tags`.

New Slint properties/callbacks (names illustrative):

- Properties: `rail-visible`, `brush-tags-0..4` (or a struct model
  `[{letter,color,tags}]`), `recent-tags`, `detail-tags`, `filter-tags`,
  `tag-match-label`, `tags-enabled`, `tag-modal-open`, `tag-modal-text`.
- Callbacks: `key(string)`, `tag-modal-commit(string)`, `tag-modal-cancel()`,
  `brush-committed(int,[string])`, `brush-remove(int,string)`,
  `detail-tag-committed([string])`, `detail-tag-remove(string)`,
  `filter-tags-committed([string])`, `filter-tag-remove(string)`,
  `tag-match-toggled()`, `tags-enabled-toggled()`, `recent-remove(string)`.

Session restore/persist: extend `restore_session`/`persist_session` to load/save
`brushes`, `recent_tags`, `rail_visible` (and the already-persisted `Filters`
tag fields). During restore the `restoring` guard suppresses re-query as today.

## Testing

Pure/unit tests (Rust):

- **`chords::resolve`** — full transition table: every `(Pending, key)` pair,
  including cancel-on-other-key and the `mm`/`ft` completions.
- **`build_filter_clause`** with tags — AllOf emits N `EXISTS`, AnyOf emits one
  `EXISTS … IN`, disabled emits nothing, params bound in order, and tags combine
  (AND) with size/type/GPS clauses. Assert SQL fragment + param vector.
- **Integration (DB):** seed images + `tag_image`; assert `browse_all` with an
  AllOf filter returns only images having *all* tags, AnyOf returns the union, and
  `tags_enabled=false` returns the unfiltered set. (This is the load-bearing test
  that pins the filter end-to-end through the real query, not just the helper.)
- **mm-buffer / recents helper** — paint replaces buffer; remove trims buffer;
  serialization round-trips through `UiState`.
- **brush helper** — add/remove/dedupe; round-trips through `UiState`.

Slint visual behavior (rail toggle, pill editing, toggle widget animation, modal)
is verified by building and running the GUI.

## Invariants this feature depends on

- **Colors are never persisted on images or tags.** Any future feature that adds
  real color labels must not assume the brush sets reflect image state.
- **`build_filter_clause` is the single filter seam** for both `browse_all` and
  similarity search. The tag-filter tests pin AND/OR/disabled behavior at this
  seam so a later refactor that splits or reorders clause assembly will fail
  loudly. The `images` alias is `i` in both queries; the EXISTS subquery
  correlates on `i.id`.
- **All keyboard shortcuts funnel through `app-keys` `capture-key-pressed`**, and
  chords are suppressed while any text input has focus. A new text input added
  later must join the focus-exemption set or it will eat chord keys.
- **`#[serde(default)]` on every new `ui_state`/`Filters` field** so older session
  blobs deserialize and newer fields are simply absent until first write.

## Out-of-scope / future

- Tag autocomplete, rename/merge, a dedicated tag-management view.
- Per-image color labels.
- CLI tag subcommands and tag-based sorting.
