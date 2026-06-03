# LAION ViT-L/14 as a selectable embedding model

**Date:** 2026-06-02
**Status:** Approved (brainstorming → spec)
**Branch:** `feat/laion-vit-l14-model`

## Goal

Add a second, higher-quality CLIP embedding model — **LAION ViT-L/14**
(`laion/CLIP-ViT-L-14-laion2B-s32B-b82K`, dim 768) — selectable alongside the
existing default `openai/clip-vit-base-patch32` (dim 512). The driving motivation
is **better English search relevance**. The W52 multi-model storage infrastructure
(per-model `vec0` tables, `models` registry, `--model` flag, `models list/use`)
already exists on the imgfind side; the work here is mostly in the `../clipper`
crate, which currently hardwires a single model.

## Decisions captured during brainstorming

- **Goal:** quality-first, English (not multilingual / speed / image-to-image).
- **Model:** LAION ViT-L/14 specifically (`laion/CLIP-ViT-L-14-laion2B-s32B-b82K`),
  dim 768. The single best bang-for-buck quality tier; ~1.7 GB; ~3–4× slower to
  index than B/32. This is acceptable — it is opt-in via `--model`, and the
  existing B/32 model stays the default.
- **Catalog ownership:** the model catalog (name → repo/config/dim/tokenizer)
  lives in **`clipper`**. imgfind stays thin: it passes the active model name and
  trusts clipper for the dim, validating the DB-recorded dim against clipper's
  reported dim as a safety check.
- **Loading approach (Approach A):** candle's `clip` module **cannot** load this
  model correctly as-is, so clipper **vendors and extends** candle's CLIP module.

## Critical technical finding (why Approach A is required)

candle-transformers 0.9.1 **and** 0.9.2 define the CLIP `Activation` enum with
**only `QuickGelu`** — there is no `Gelu` variant, and the text tower's activation
is **hardcoded** to `QuickGelu` (`ClipEncoderConfig::activation()` returns
`Activation::QuickGelu` for `Self::Text(_)` regardless of config; vision reads
config but the enum still only offers `QuickGelu`).

The LAION ViT-L/14 HF config specifies `hidden_act = "gelu"` for **both** the text
and vision towers (verified from the repo's `config.json`). Running it through
candle's QuickGelu path would **not crash** — it would silently produce subtly
wrong embeddings and degrade relevance, defeating the purpose.

Therefore candle's `clip` module is vendored into clipper and extended:
- Add a `Gelu` variant to the `Activation` enum, implemented with
  `Tensor::gelu_erf()`. **Important:** HF `hidden_act: "gelu"` is the *exact*
  erf-based GELU, which in candle is `gelu_erf()`, **not** `gelu()` (candle's
  `gelu()` is the tanh approximation). Using the wrong one reintroduces the
  same silent-degradation bug.
- Make the **text** tower's activation config-driven (read from config like the
  vision tower), removing the hardcoded `QuickGelu`.

Both candidate models still work through this generalized loader:
`openai/clip-vit-base-patch32` selects `QuickGelu`; the LAION model selects `Gelu`.

The HF-`CLIPModel`-format weights (`model.safetensors`, 1.71 GB) are confirmed
present in the LAION repo, so candle's `VarBuilder::from_mmaped_safetensors` can
load them with the standard HF tensor naming
(`text_model.*`, `vision_model.*`, `visual_projection`, `text_projection`,
`logit_scale`).

## Architecture

### `clipper` crate (owns the catalog)

**1. Vendored CLIP module (`clipper/src/clip/`)**

Copy candle's `clip/{mod.rs, text_model.rs, vision_model.rs}` into clipper. Edits:

- `Activation` enum gains `Gelu` → `xs.gelu_erf()`.
- `ClipTextConfig` activation becomes config-driven; the encoder config's
  `activation()` returns the configured activation for both Text and Vision.
- Public config constructors/fields remain so specs can be hand-built.

**2. Model catalog**

A static table mapping a model **name** to a `ModelSpec`:

```rust
struct ModelSpec {
    name: &'static str,        // e.g. "laion/CLIP-ViT-L-14-laion2B-s32B-b82K"
    hf_repo: &'static str,     // HF repo id (== name here)
    weights_file: &'static str,// "model.safetensors"
    tokenizer_file: &'static str, // "tokenizer.json"
    config: ClipConfig,        // hand-built candle config
    dim: usize,                // projection dim (512 or 768)
    image_size: usize,         // 224 for both
}
```

Two entries:

| name | activation | dim | image | vision (w/L/H/proj) | text (w/L/H/proj) |
|---|---|---|---|---|---|
| `openai/clip-vit-base-patch32` (default) | QuickGelu | 512 | 224 | 768 / 12 / 12 / 512, patch 32 | 512 / 12 / 8 / 512 |
| `laion/CLIP-ViT-L-14-laion2B-s32B-b82K` | Gelu | 768 | 224 | 1024 / 24 / 16 / 768, patch 14 | 768 / 12 / 12 / 768 |

(Exact text-tower head/intermediate counts for B/32 come from candle's existing
`vit_base_patch32()` constructor; L/14 text config mirrors the HF config values.)

**3. API surface**

- `ClipEmbedder::from_model(name: &str, use_cpu: bool) -> Result<ClipEmbedder>` —
  resolves the spec, downloads weights+tokenizer via hf-hub (cached), builds the
  model. Errors clearly on an unknown name.
- `ClipEmbedder::new(model_path, tokenizer_path, use_cpu)` — kept for back-compat;
  delegates to the base-patch32 default (preserving today's behavior). Existing
  callers and the `--model`-less path are unaffected.
- `clipper::supported_models() -> Vec<ModelInfo { name: String, dim: usize }>` —
  lets imgfind enumerate/validate.
- `ClipEmbedder::model_name(&self) -> &str` and `.dim(&self) -> usize` accessors.
- Preprocessing uses `image_size` from the spec (both 224 here). Normalization
  constants are unchanged — LAION-2B models use the OpenAI CLIP mean/std, same as
  the current model. Tokenization is the CLIP BPE tokenizer for both; the
  tokenizer is downloaded per-repo.

### `imgfind` (thin consumer)

- **Embedder construction:** replace `ClipEmbedder::new(...)` call sites
  (index/search/serve/tui as applicable) with
  `ClipEmbedder::from_model(active_model_name, use_cpu)`. The active model name is
  resolved from the DB `models` registry (W52 `active_model()`), overridden by
  `--model`.
- **`models use <name>` auto-registration:** if `<name>` is not in the DB registry
  but **is** in `clipper::supported_models()`, auto-register it: read the dim from
  clipper, call the existing W52 `register_model(name, dim)` (creates the per-model
  `vec0` table at that dim), then set it active. (Chosen default — avoids adding a
  separate `models add` verb.) If the name is neither registered nor
  clipper-supported, error with the list of supported names.
- **Dim-validation guard:** when activating/using a model already in the DB,
  assert `db.models.dim == clipper dim for that name`; error on mismatch (guards
  against a future catalog change diverging from indexed data).
- **`models list`:** show DB-registered (indexed) models; additionally flag
  clipper-supported models that are not yet indexed (e.g. a `available` marker).
- **`serve` (W46 lazy init):** the background loader uses the model that is
  **active at serve startup**. Switching the active model in a running server is
  out of scope.

## User workflow

```bash
imgfind models use laion/CLIP-ViT-L-14-laion2B-s32B-b82K   # auto-registers dim 768, creates vec0 table, sets active
imgfind index ~/Pictures                                   # embeds with L/14 into the new table (downloads ~1.7GB once)
imgfind search "a dog on a beach" --model laion/CLIP-ViT-L-14-laion2B-s32B-b82K
imgfind models use openai/clip-vit-base-patch32            # switch back to the fast default
```

The two models' embeddings live in separate `vec0` tables (W52), so they coexist.
A given query only searches the active/`--model` table, so images must be indexed
under L/14 before they are searchable under L/14.

## Error handling

- Unknown model name (CLI or `from_model`): error listing `supported_models()`.
- Dim mismatch (DB vs clipper): hard error; do not silently re-create tables.
- Weight/tokenizer download failure: propagated via `anyhow` context
  (`with_context`), consistent with the codebase convention.
- Server search before model load (W46): unchanged — 503 + `Retry-After`.

## Testing

**clipper**
- `supported_models()` returns both entries with dims 512 and 768.
- `Activation::Gelu` forward output equals `Tensor::gelu_erf()` on a sample tensor
  (and is distinct from `QuickGelu`).
- Config builders produce the expected layer/dim counts for both models.
- An `#[ignore]`d "online" integration test that actually calls
  `from_model("laion/...")`, embeds a bundled test image and a matching/non-matching
  caption, and asserts the matching caption has higher cosine similarity. (Ignored
  by default because the 1.7 GB download is too heavy for routine CI; run manually
  in the correctness spike and on demand.)

**imgfind**
- `models use <laion-name>` on a DB that lacks it: auto-registers dim 768, creates
  the `vec0` table, sets active (extends W52 `register_model`/`set_active_model`
  tests).
- Dim-mismatch guard errors when DB dim ≠ clipper dim.
- Unknown model name errors with the supported list.
- index/search route to the active model's table (reuse W52 `vectors_table()`
  resolution tests).

## Implementation order / risk

1. **Correctness spike (gates everything else).** Vendor candle's clip into
   clipper with the `Gelu` + config-driven-activation edits; load
   `laion/CLIP-ViT-L-14-laion2B-s32B-b82K`; embed a known image + matching and
   non-matching captions; confirm sane cosine rankings and that `gelu_erf` (not
   quickgelu, not tanh-`gelu`) is the correct path. Verify HF tensor names map onto
   candle's `vs.pp(...)` prefixes. **Do not proceed until embeddings look correct.**
2. clipper catalog + `from_model` + `supported_models` + accessors; back-compat
   `new()` delegation; unit tests.
3. imgfind: `from_model` call sites; `models use` auto-registration; dim guard;
   `models list` availability flag; tests.
4. Docs: `USAGE.md` / `CLAUDE.md` notes on selecting the L/14 model and the
   re-index-per-model requirement.

## Out of scope

- Hot-swapping the active model in a running `serve` process.
- Automatic migration/re-embedding of existing data when switching models (the
  user re-indexes under the new model).
- Additional models beyond these two (SigLIP, MetaCLIP, MobileCLIP, DINOv2). The
  generalized loader makes them straightforward follow-ups, but they are not built
  here.

## Verification

- `cargo build --release` (imgfind, with the local `../clipper` path dep).
- `cargo test` (clipper unit tests + imgfind unit tests; the online L/14 test stays
  `#[ignore]`d).
- Manual: run the correctness spike; then the user workflow above end-to-end on a
  small image directory; confirm `models list` shows both and search returns
  sensible results under L/14.
