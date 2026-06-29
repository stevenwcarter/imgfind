# Tag-editor inline input polish

**Date:** 2026-06-29
**Status:** Approved
**Scope:** `imgfind-gui/ui/tag_editor.slint` (markup only)

## Problem

The inline `TagEditor` component (used in the brush rail, the "Most Recent"
buffer, the tag-filter row, and the detail panel) shows tag **pills** at rest.
Clicking it enters edit mode, which today swaps the pill display for a
std-widgets `LineEdit`. That `LineEdit` carries its own background, border, and
~32px minimum height baked into the widget — none of which are overridable — so
while editing it renders as a distinct floating sub-box that does **not** match
the resting pill box: it appears centered "in the middle" of the editor and
overflows / mismatches the original bounds. This looks broken.

## Goal

Editing should be **seamless**: clicking turns the pills into an editable
space-separated text caret inside the *same* visible box — same background,
border, radius, and height as the resting state. No nested/floating box, no
size jump.

The editing model is unchanged from today: a single space-joined text line that
Rust splits on whitespace and dedupes on commit. This is a pure visual /
markup polish — no Rust, schema, or behavior-contract changes.

## Approach (chosen: bare `TextInput`)

Replace the `if root.editing: LineEdit { … }` block with a bare Slint builtin
`TextInput`, so the editor's own root `Rectangle` (already `#232838` bg with the
`#3a4255` 1px border and 4px radius) is the only visible box.

Rejected alternatives:
- **Force-style the `LineEdit`** — impossible; the nested box is intrinsic to the
  std widget and not overridable.
- **Extract a reusable styled-input `.slint` component** — premature; `TagEditor`
  is the only consumer.

## Design

1. **Editing branch markup.** `if root.editing:` renders a layout containing:
   - A bare `TextInput` (`input := TextInput`): `single-line: true`,
     `font-size: 11px` (matches pill text), `color: #e0e4ed`, an explicit text
     `selection-background-color`, horizontal padding so the caret aligns with
     where the pills sat (~6px left). `init => { self.text = root.joined;
     self.focus(); }` (unchanged from today).
   - A placeholder `Text` (`#6b7488`, same font-size/position) layered **behind**
     the input, `visible: input.text == ""`. Bare `TextInput` has no
     `placeholder-text`, so this is manual. Uses the existing `root.placeholder`.
2. **Active affordance.** While editing, the editor's border brightens to
   `#4a5573` so the focused field reads as active; reverts to `#3a4255` at rest.
   Expressed as a reactive `border-color: root.editing ? #4a5573 : #3a4255`.
3. **Height.** Editing height stays at the resting **minimum 28px** single-line
   (the existing `height: root.editing ? 28px : …` rule is unchanged). The text
   is single-line and scrolls horizontally past the right edge (`clip: true` is
   already set on the root), so there is no layout jump entering/leaving edit.
4. **Commit semantics — preserved verbatim.** Both commit paths must fire
   `root.editing-changed(false)` **before** `root.committed-text(...)`, exactly as
   the current `LineEdit` does, because committing rebuilds the Rust-side models
   and destroys this element mid-callback (see the existing in-file comment — this
   is the load-bearing ordering that keeps keyboard chords from getting stuck
   suppressed):
   - `accepted => { root.editing-changed(false); root.editing = false;
     root.committed-text(input.text); }`
   - `changed has-focus => { if (!input.has-focus) { root.editing-changed(false);
     root.editing = false; root.committed-text(input.text); } }`

   Note: bare `TextInput`'s `accepted` callback takes **no argument** (unlike
   `LineEdit`'s `accepted(string)`), so the handler reads `input.text` directly.

## Invariants this feature depends on

- **`committed-text` fires once per edit session with the full raw text**, and
  **`editing-changed(false)` fires before it.** The Rust side
  (`editor_editing_changed` / the various `*-committed` handlers in
  `imgfind-gui/src/`) relies on this ordering to clear the chords-suppressed flag
  before the element is destroyed. Any change to the editing branch must keep
  both commit paths (Enter via `accepted`, blur via `changed has-focus`) firing
  `editing-changed(false)` first.
- **The resting/display branch is untouched**, so pill rendering, wrapping
  (`TagWrap.rows`), per-pill remove, and click-to-edit are unchanged.

## Testing

This is pure Slint markup with no extractable pure logic, so there is no unit
test to add (the commit-ordering invariant lives in markup callback order, not
in Rust). Verification is:
1. `cargo build -p imgfind-gui` compiles (Slint markup validated at build time).
2. Manual visual check by the user: click each tag field (brush rail, Most
   Recent, tag filter, detail panel), confirm the edit caret fills the same box
   with no floating sub-box, type + Enter commits, blur commits, and keyboard
   chords (`m`/`f`/`t`/backtick) still work after committing (the load-bearing
   ordering invariant).

## Out of scope

- The centered `t`-key **tag modal** (`modal-input` `LineEdit` at app.slint
  ~1710) — it is a deliberate centered prompt overlay, not an inline editor, and
  is not part of this polish.
- Any change to the editing *model* (pills-stay-visible / type-only-new-tag was
  considered and declined in favor of the seamless text line).
