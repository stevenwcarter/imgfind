# LAION ViT-L/14 Selectable Embedding Model — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `laion/CLIP-ViT-L-14-laion2B-s32B-b82K` (dim 768) as a second, user-selectable CLIP embedding model alongside the default `openai/clip-vit-base-patch32` (dim 512), with the model catalog owned by the `../clipper` crate.

**Architecture:** `clipper` vendors candle's CLIP module (because candle hardcodes `QuickGelu` and the LAION model needs `gelu`), adds a config-driven `Gelu` activation, and exposes a small model catalog + `from_model(name)` API. `imgfind` stays thin: it resolves the active model name (from the W52 `models` DB registry / `--model` flag), auto-registers clipper-supported models on first use, validates dims, and constructs the embedder via `from_model`.

**Tech Stack:** Rust 2024, candle-core/candle-nn 0.9, hf-hub, tokenizers, rusqlite + sqlite-vec (imgfind). Local path dep `../clipper`.

**Spec:** `docs/superpowers/specs/2026-06-02-laion-vit-l14-model-design.md`

---

## File Structure

**clipper (`../clipper`):**
- Create `src/clip/mod.rs`, `src/clip/text_model.rs`, `src/clip/vision_model.rs` — vendored & extended candle CLIP (config-driven `Gelu`).
- Create `src/catalog.rs` — model catalog (`ModelSpec`, `supported_models()`, name→config).
- Modify `src/lib.rs` — use vendored `crate::clip`; add `from_model`, `supported_models` re-export, `model_name()`/`dim()` accessors; delegate `new()` to the default.
- Modify `Cargo.toml` — drop `candle-transformers` dep (no longer used).

**imgfind (`.`):**
- Create `src/models.rs` — `ensure_and_activate_model(db, name)` helper (auto-register + dim guard) and `available_models()` view for listing.
- Modify `src/main.rs` — call sites for `--model` (index/search), `Models` subcommand (`Use` auto-registers, `List` shows available), and the four `ClipEmbedder::new` → `from_model(active_name)` sites (index, search, serve, plus tui below).
- Modify `src/tui/app/search.rs` — `ClipEmbedder::new` → `from_model(active_name)`.
- Modify `USAGE.md`, `CLAUDE.md` — document model selection + re-index-per-model.

---

## Notes for the implementer (read once)

- **candle import alias:** candle-transformers' source writes `use candle::{...}`. clipper depends on the crate as `candle_core`. When vendoring, every `use candle::` and `candle::` path becomes `candle_core::`. `candle_nn` is unchanged.
- **gelu flavor is load-bearing:** HF `hidden_act: "gelu"` is the *exact* erf GELU → `Tensor::gelu_erf()`. Do **not** use `Tensor::gelu()` (candle's tanh approximation). Getting this wrong silently degrades embeddings.
- **Preprocessing is unchanged:** clipper's `preprocess_dynamic_image` maps pixels to `[-1, 1]` via `.affine(2./255., -1.)` and is parameterized by `image_size`. Both models use `image_size = 224`, so the existing pipeline is reused as-is. (The mandatory spike in Task 3 validates that this preprocessing yields sane L/14 rankings; if it does not, switching to per-channel CLIP mean/std is the documented fallback — out of scope unless the spike fails.)
- **Online test is `#[ignore]`d:** the L/14 weights are ~1.7 GB; the real-load test must be `#[ignore]`d so routine `cargo test` does not download it.

---

## Task 1: Vendor candle's CLIP module into clipper with a config-driven `Gelu`

**Files:**
- Create: `../clipper/src/clip/mod.rs` (from candle `clip/mod.rs`)
- Create: `../clipper/src/clip/text_model.rs` (from candle `clip/text_model.rs`)
- Create: `../clipper/src/clip/vision_model.rs` (from candle `clip/vision_model.rs`)
- Modify: `../clipper/src/lib.rs` (declare `mod clip;`, switch the embedder to it — full switch happens in Task 2; here just make it compile alongside)
- Test: `../clipper/src/clip/text_model.rs` (inline `#[cfg(test)]`)

Source to copy verbatim (then edit):
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/candle-transformers-0.9.1/src/models/clip/{mod,text_model,vision_model}.rs`

- [ ] **Step 1: Copy the three files**

```bash
SRC=~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/candle-transformers-0.9.1/src/models/clip
mkdir -p ../clipper/src/clip
cp "$SRC/mod.rs"          ../clipper/src/clip/mod.rs
cp "$SRC/text_model.rs"   ../clipper/src/clip/text_model.rs
cp "$SRC/vision_model.rs" ../clipper/src/clip/vision_model.rs
```

- [ ] **Step 2: Fix candle import alias in all three files**

In each of `../clipper/src/clip/{mod,text_model,vision_model}.rs`, replace the crate alias `candle` with `candle_core`:
- `use candle::{...}` → `use candle_core::{...}`
- any bare `candle::` path (e.g. none expected beyond the `use`) → `candle_core::`

(`mod.rs` line 15: `use candle::{Result, Tensor, D};` → `use candle_core::{Result, Tensor, D};`
`text_model.rs` line 9: `use candle::{DType, Device, IndexOp, Result, Tensor, D};` → `candle_core::...`
`vision_model.rs` line 9: `use candle::{Context, IndexOp, Result, Shape, Tensor, D};` → `candle_core::...`)

- [ ] **Step 3: Add the `Gelu` activation variant (text_model.rs)**

In `../clipper/src/clip/text_model.rs`, change the enum and its `Module` impl:

```rust
#[derive(Debug, Clone, Copy)]
pub enum Activation {
    QuickGelu,
    Gelu,
}

impl Module for Activation {
    fn forward(&self, xs: &Tensor) -> Result<Tensor> {
        match self {
            Activation::QuickGelu => xs * nn::ops::sigmoid(&(xs * 1.702f64)?)?,
            // HF `hidden_act: "gelu"` is the exact (erf) GELU, NOT the tanh approximation.
            Activation::Gelu => xs.gelu_erf(),
        }
    }
}
```

- [ ] **Step 4: Make the text-tower activation config-driven (mod.rs)**

In `../clipper/src/clip/mod.rs`, change `EncoderConfig::activation()` so the `Text` arm reads the configured activation instead of hardcoding `QuickGelu`:

```rust
    pub fn activation(&self) -> Activation {
        match self {
            Self::Text(c) => c.activation,
            Self::Vision(c) => c.activation,
        }
    }
```

- [ ] **Step 5: Declare the module in lib.rs**

In `../clipper/src/lib.rs`, add near the top (after the existing `use` lines):

```rust
mod clip;
```

Leave the existing `use candle_transformers::models::clip;` import in place for now (Task 2 removes it). To avoid a name clash, the new module is private and unused until Task 2 — so temporarily allow it:

```rust
#[allow(unused)]
mod clip;
```

- [ ] **Step 6: Add unit tests for the activation (text_model.rs, inline)**

Append to `../clipper/src/clip/text_model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{Device, Tensor};
    use candle_nn::Module;

    #[test]
    fn gelu_matches_gelu_erf() {
        let d = Device::Cpu;
        let xs = Tensor::new(&[-2.0f32, -0.5, 0.0, 0.5, 2.0], &d).unwrap();
        let got = Activation::Gelu.forward(&xs).unwrap().to_vec1::<f32>().unwrap();
        let want = xs.gelu_erf().unwrap().to_vec1::<f32>().unwrap();
        for (a, b) in got.iter().zip(want.iter()) {
            assert!((a - b).abs() < 1e-6, "got {a} want {b}");
        }
    }

    #[test]
    fn gelu_differs_from_quickgelu() {
        let d = Device::Cpu;
        let xs = Tensor::new(&[1.5f32], &d).unwrap();
        let g = Activation::Gelu.forward(&xs).unwrap().to_vec1::<f32>().unwrap()[0];
        let q = Activation::QuickGelu.forward(&xs).unwrap().to_vec1::<f32>().unwrap()[0];
        assert!((g - q).abs() > 1e-4, "gelu {g} should differ from quickgelu {q}");
    }
}
```

- [ ] **Step 7: Run the tests (expect compile + pass)**

Run: `cd ../clipper && cargo test clip::text_model::tests -- --nocapture`
Expected: both tests PASS. If the crate does not compile, fix import/alias issues from Steps 2–4 before proceeding.

- [ ] **Step 8: Commit**

```bash
cd ../clipper && git add src/clip src/lib.rs && \
git commit -m "feat(clip): vendor candle CLIP module with config-driven Gelu activation"
```

---

## Task 2: clipper model catalog + `from_model` API

**Files:**
- Create: `../clipper/src/catalog.rs`
- Modify: `../clipper/src/lib.rs` (use `crate::clip`; add `from_model`, accessors, `supported_models` re-export; delegate `new`)
- Modify: `../clipper/Cargo.toml` (remove `candle-transformers`)
- Test: `../clipper/src/catalog.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing catalog test**

Create `../clipper/src/catalog.rs`:

```rust
//! The catalog of CLIP models clipper knows how to load.

use crate::clip::{ClipConfig, text_model::ClipTextConfig, text_model::Activation, vision_model::ClipVisionConfig};

/// Public, lightweight description of a supported model (name + embedding dim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: String,
    pub dim: usize,
}

/// Internal full spec used to actually load a model.
pub(crate) struct ModelSpec {
    pub name: &'static str,
    pub hf_repo: &'static str,
    pub revision: &'static str,
    pub weights_file: &'static str,
    pub tokenizer_file: &'static str,
    pub image_size: usize,
    pub dim: usize,
    pub config: fn() -> ClipConfig,
}

pub(crate) const DEFAULT_MODEL: &str = "openai/clip-vit-base-patch32";

pub(crate) fn specs() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            name: "openai/clip-vit-base-patch32",
            hf_repo: "openai/clip-vit-base-patch32",
            revision: "refs/pr/15",
            weights_file: "model.safetensors",
            tokenizer_file: "tokenizer.json",
            image_size: 224,
            dim: 512,
            config: clip_vit_base_patch32,
        },
        ModelSpec {
            name: "laion/CLIP-ViT-L-14-laion2B-s32B-b82K",
            hf_repo: "laion/CLIP-ViT-L-14-laion2B-s32B-b82K",
            revision: "main",
            weights_file: "model.safetensors",
            tokenizer_file: "tokenizer.json",
            image_size: 224,
            dim: 768,
            config: laion_vit_l14_224,
        },
    ]
}

pub(crate) fn find_spec(name: &str) -> Option<ModelSpec> {
    specs().into_iter().find(|s| s.name == name)
}

/// All models clipper can load, as `(name, dim)`.
pub fn supported_models() -> Vec<ModelInfo> {
    specs()
        .into_iter()
        .map(|s| ModelInfo { name: s.name.to_string(), dim: s.dim })
        .collect()
}

fn clip_vit_base_patch32() -> ClipConfig {
    ClipConfig {
        text_config: ClipTextConfig {
            vocab_size: 49408,
            embed_dim: 512,
            activation: Activation::QuickGelu,
            intermediate_size: 2048,
            max_position_embeddings: 77,
            pad_with: None,
            num_hidden_layers: 12,
            num_attention_heads: 8,
            projection_dim: 512,
        },
        vision_config: ClipVisionConfig {
            embed_dim: 768,
            activation: Activation::QuickGelu,
            intermediate_size: 3072,
            num_hidden_layers: 12,
            num_attention_heads: 12,
            projection_dim: 512,
            num_channels: 3,
            image_size: 224,
            patch_size: 32,
        },
        logit_scale_init_value: 2.6592,
        image_size: 224,
    }
}

fn laion_vit_l14_224() -> ClipConfig {
    ClipConfig {
        text_config: ClipTextConfig {
            vocab_size: 49408,
            embed_dim: 768,
            activation: Activation::Gelu,
            intermediate_size: 3072,
            max_position_embeddings: 77,
            pad_with: None,
            num_hidden_layers: 12,
            num_attention_heads: 12,
            projection_dim: 768,
        },
        vision_config: ClipVisionConfig {
            embed_dim: 1024,
            activation: Activation::Gelu,
            intermediate_size: 4096,
            num_hidden_layers: 24,
            num_attention_heads: 16,
            projection_dim: 768,
            num_channels: 3,
            image_size: 224,
            patch_size: 14,
        },
        logit_scale_init_value: 2.6592,
        image_size: 224,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lists_both_models_with_dims() {
        let models = supported_models();
        let by_name = |n: &str| models.iter().find(|m| m.name == n).map(|m| m.dim);
        assert_eq!(by_name("openai/clip-vit-base-patch32"), Some(512));
        assert_eq!(by_name("laion/CLIP-ViT-L-14-laion2B-s32B-b82K"), Some(768));
    }

    #[test]
    fn laion_config_has_l14_shape_and_gelu() {
        let c = laion_vit_l14_224();
        assert_eq!(c.vision_config.embed_dim, 1024);
        assert_eq!(c.vision_config.num_hidden_layers, 24);
        assert_eq!(c.vision_config.patch_size, 14);
        assert_eq!(c.text_config.projection_dim, 768);
        assert!(matches!(c.text_config.activation, Activation::Gelu));
        assert!(matches!(c.vision_config.activation, Activation::Gelu));
    }
}
```

This requires `ClipConfig`, `ClipTextConfig`, `ClipVisionConfig`, `Activation` to be importable from `crate::clip`. The vendored `clip/mod.rs` already declares `pub mod text_model;`/`pub mod vision_model;` and `pub struct ClipConfig`. Ensure `Activation` is `pub` in `text_model.rs` (it is) and re-export is reachable.

- [ ] **Step 2: Run it to verify it fails to compile (module not wired)**

Run: `cd ../clipper && cargo test catalog:: 2>&1 | head -20`
Expected: FAIL — `catalog` not declared as a module yet (and `clip` is private).

- [ ] **Step 3: Wire the modules and make `clip` accessible to the catalog**

In `../clipper/src/lib.rs`:
- Change `#[allow(unused)] mod clip;` to `mod clip;` (drop the allow).
- Add `mod catalog;` and re-export: `pub use catalog::{supported_models, ModelInfo};`
- Remove `use candle_transformers::models::clip;` (the vendored module shadows it).

- [ ] **Step 4: Switch `ClipEmbedder` to the vendored clip + add `from_model`**

In `../clipper/src/lib.rs`, replace the `model`/`config` field types and constructors. The struct becomes:

```rust
pub struct ClipEmbedder {
    model: crate::clip::ClipModel,
    tokenizer: Tokenizer,
    config: crate::clip::ClipConfig,
    device: Device,
    model_name: String,
    dim: usize,
}
```

Add the new constructor and accessors (keep all existing `get_*_embedding*` methods unchanged — they reference `self.model` / `self.config.image_size`, which still resolve):

```rust
impl ClipEmbedder {
    /// Load a model from clipper's catalog by name, downloading weights and
    /// tokenizer from HuggingFace (cached) on first use.
    pub fn from_model(name: &str, use_cpu: bool) -> Result<Self> {
        let spec = crate::catalog::find_spec(name)
            .with_context(|| {
                let known: Vec<_> = crate::catalog::supported_models()
                    .into_iter().map(|m| m.name).collect();
                format!("unknown model '{name}'; supported models: {known:?}")
            })?;
        let device = get_device(use_cpu)?;

        let api = hf_hub::api::sync::Api::new()?;
        let repo = api.repo(hf_hub::Repo::with_revision(
            spec.hf_repo.to_string(),
            hf_hub::RepoType::Model,
            spec.revision.to_string(),
        ));
        let model_file = repo
            .get(spec.weights_file)
            .with_context(|| format!("download {} weights", spec.name))?;
        let tokenizer_file = repo
            .get(spec.tokenizer_file)
            .with_context(|| format!("download {} tokenizer", spec.name))?;
        let tokenizer = Tokenizer::from_file(tokenizer_file).map_err(anyhow::Error::msg)?;

        let config = (spec.config)();
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[model_file], DType::F32, &device)?
        };
        let model = crate::clip::ClipModel::new(vb, &config)?;

        Ok(ClipEmbedder {
            model,
            tokenizer,
            config,
            device,
            model_name: spec.name.to_string(),
            dim: spec.dim,
        })
    }

    /// The catalog name of the loaded model.
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// The embedding dimension of the loaded model.
    pub fn dim(&self) -> usize {
        self.dim
    }
}
```

Add `use anyhow::Context;` at the top of `lib.rs` if not already present (it uses `Result` from anyhow; ensure `Context` is imported for `with_context`).

- [ ] **Step 5: Make `new()` delegate to the default model (back-compat)**

Replace the body of the existing `new(model_path, tokenizer_path, use_cpu)` so that when both paths are `None` it delegates to `from_model(DEFAULT_MODEL, use_cpu)`. Preserve the explicit-path branch (used by tests/tools that pass a local file):

```rust
    pub fn new(
        model_path: Option<String>,
        tokenizer_path: Option<String>,
        use_cpu: bool,
    ) -> Result<Self> {
        if model_path.is_none() && tokenizer_path.is_none() {
            return Self::from_model(crate::catalog::DEFAULT_MODEL, use_cpu);
        }
        // Explicit local-file path: load the base-patch32 architecture from the
        // given files (preserves prior behavior).
        let device = get_device(use_cpu)?;
        let model_file: std::path::PathBuf = match model_path {
            None => {
                let api = hf_hub::api::sync::Api::new()?;
                let api = api.repo(hf_hub::Repo::with_revision(
                    "openai/clip-vit-base-patch32".to_string(),
                    hf_hub::RepoType::Model,
                    "refs/pr/15".to_string(),
                ));
                api.get("model.safetensors")?
            }
            Some(model) => model.into(),
        };
        let tokenizer = get_tokenizer(tokenizer_path)?;
        let config = (crate::catalog::find_spec(crate::catalog::DEFAULT_MODEL).unwrap().config)();
        let dim = config.text_config.projection_dim;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[model_file], DType::F32, &device)?
        };
        let model = crate::clip::ClipModel::new(vb, &config)?;
        Ok(ClipEmbedder {
            model,
            tokenizer,
            config,
            device,
            model_name: crate::catalog::DEFAULT_MODEL.to_string(),
            dim,
        })
    }
```

- [ ] **Step 6: Remove the candle-transformers dependency**

In `../clipper/Cargo.toml`, delete the `candle-transformers = { ... }` line. (The vendored `clip` module replaces its only use.)

- [ ] **Step 7: Build + run catalog tests**

Run: `cd ../clipper && cargo build && cargo test catalog:: clip:: -- --nocapture`
Expected: build succeeds; `catalog_lists_both_models_with_dims`, `laion_config_has_l14_shape_and_gelu`, and the Task 1 activation tests all PASS.

- [ ] **Step 8: Commit**

```bash
cd ../clipper && git add src/lib.rs src/catalog.rs Cargo.toml && \
git commit -m "feat(catalog): model catalog + ClipEmbedder::from_model + accessors"
```

---

## Task 3: Correctness spike — real L/14 load + ranking sanity (MANDATORY GATE)

**Files:**
- Test: `../clipper/tests/laion_l14_online.rs` (integration test, `#[ignore]`d)
- Asset: reuse an existing image under `../clipper/assets/` (list them first)

This task verifies the whole approach before imgfind work proceeds. **Do not start Task 4 until this passes.**

- [ ] **Step 1: Identify a test image**

Run: `ls ../clipper/assets`
Pick one obviously-describable image (e.g. a photo of an animal/object). Note its path and an accurate caption + a clearly-wrong caption.

- [ ] **Step 2: Write the `#[ignore]`d online test**

Create `../clipper/tests/laion_l14_online.rs` (adjust the image path + captions to the asset chosen):

```rust
use clipper::ClipEmbedder;

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

#[test]
#[ignore = "downloads ~1.7GB LAION ViT-L/14 weights; run manually"]
fn laion_l14_ranks_matching_caption_higher() {
    let m = ClipEmbedder::from_model("laion/CLIP-ViT-L-14-laion2B-s32B-b82K", true)
        .expect("load L/14");
    assert_eq!(m.dim(), 768);

    // EDIT these to match the chosen asset:
    let img = m.get_image_embedding("assets/<CHOSEN_IMAGE>").expect("image embed");
    assert_eq!(img.len(), 768);

    let matching = m.get_text_embedding("<ACCURATE CAPTION>").expect("text embed");
    let wrong = m.get_text_embedding("<CLEARLY WRONG CAPTION>").expect("text embed");

    let s_match = cosine(&img, &matching);
    let s_wrong = cosine(&img, &wrong);
    eprintln!("match={s_match:.4} wrong={s_wrong:.4}");
    assert!(
        s_match > s_wrong,
        "expected matching caption to score higher: match={s_match} wrong={s_wrong}"
    );
}
```

- [ ] **Step 3: Run the spike (manual, downloads weights)**

Run: `cd ../clipper && cargo test --test laion_l14_online -- --ignored --nocapture`
Expected: PASS, with `match` cosine clearly above `wrong`. Note the printed values.

- [ ] **Step 4: Decision gate**

If the matching caption scores higher with a sensible margin → the gelu path + weight naming + preprocessing are correct. Proceed.
If rankings are wrong or scores look degenerate (e.g. near-identical, NaN): STOP and debug — most likely the HF tensor names did not map (check `VarBuilder` errors), the activation is wrong, or preprocessing needs CLIP mean/std (see implementer notes). Do not proceed to imgfind changes until resolved.

- [ ] **Step 5: Commit the test**

```bash
cd ../clipper && git add tests/laion_l14_online.rs && \
git commit -m "test(clip): ignored online L/14 ranking sanity check"
```

---

## Task 4: imgfind — `ensure_and_activate_model` helper (auto-register + dim guard)

**Files:**
- Create: `src/models.rs`
- Modify: `src/lib.rs` (add `pub mod models;` if lib re-exports modules — otherwise declare `mod models;` in `main.rs`)
- Test: `src/models.rs` (inline `#[cfg(test)]`)

Check first whether modules are declared in `src/lib.rs` or `src/main.rs`:
Run: `grep -n "^mod \|^pub mod " src/lib.rs src/main.rs | head`
Declare `models` in the same place the other modules live (likely `src/lib.rs`). The examples below assume `pub mod models;` in `src/lib.rs`.

- [ ] **Step 1: Write the failing test**

Create `src/models.rs`:

```rust
//! Glue between imgfind's DB model registry (W52) and clipper's model catalog.

use crate::database::Database;
use anyhow::{Context, Result};

/// Resolve `name` to an active model, auto-registering it if clipper supports it
/// but the DB has not seen it yet, and validating that any already-registered
/// model's dimension matches clipper's.
pub fn ensure_and_activate_model(db: &Database, name: &str) -> Result<()> {
    let registered = db.list_models()?; // Vec<(name, dim, is_active)>
    let clipper_dim = clipper::supported_models()
        .into_iter()
        .find(|m| m.name == name)
        .map(|m| m.dim);

    if let Some((_, db_dim, _)) = registered.iter().find(|(n, _, _)| n == name) {
        if let Some(cd) = clipper_dim {
            anyhow::ensure!(
                *db_dim == cd,
                "model '{name}' dim mismatch: db={db_dim}, clipper={cd}"
            );
        }
        db.set_active_model(name)?;
        return Ok(());
    }

    let dim = clipper_dim.with_context(|| {
        let known: Vec<_> = clipper::supported_models().into_iter().map(|m| m.name).collect();
        format!("unknown model '{name}'; supported models: {known:?}")
    })?;
    db.register_model(name, dim)?;
    db.set_active_model(name)?;
    Ok(())
}

/// Rows for `models list`: registered models plus clipper-supported models that
/// are not yet registered (marked not-indexed).
pub struct ModelRow {
    pub name: String,
    pub dim: usize,
    pub active: bool,
    pub indexed: bool,
}

pub fn list_rows(db: &Database) -> Result<Vec<ModelRow>> {
    let registered = db.list_models()?;
    let names: std::collections::HashSet<&str> =
        registered.iter().map(|(n, _, _)| n.as_str()).collect();
    let mut rows: Vec<ModelRow> = registered
        .iter()
        .map(|(n, d, a)| ModelRow { name: n.clone(), dim: *d, active: *a, indexed: true })
        .collect();
    for m in clipper::supported_models() {
        if !names.contains(m.name.as_str()) {
            rows.push(ModelRow { name: m.name, dim: m.dim, active: false, indexed: false });
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    fn temp_db() -> Database {
        // Mirror the pattern used in database.rs tests for an in-memory/temp DB.
        Database::new_in_memory().expect("temp db")
    }

    #[test]
    fn auto_registers_clipper_model_and_activates() {
        let db = temp_db();
        ensure_and_activate_model(&db, "laion/CLIP-ViT-L-14-laion2B-s32B-b82K").unwrap();
        let active = db.active_model().unwrap();
        assert_eq!(active.name, "laion/CLIP-ViT-L-14-laion2B-s32B-b82K");
        assert_eq!(active.dim, 768);
    }

    #[test]
    fn unknown_model_errors() {
        let db = temp_db();
        let err = ensure_and_activate_model(&db, "nope/not-a-model").unwrap_err();
        assert!(err.to_string().contains("unknown model"));
    }

    #[test]
    fn dim_mismatch_errors() {
        let db = temp_db();
        // Register the LAION name with the WRONG dim, then try to activate it.
        db.register_model("laion/CLIP-ViT-L-14-laion2B-s32B-b82K", 512).unwrap();
        let err = ensure_and_activate_model(&db, "laion/CLIP-ViT-L-14-laion2B-s32B-b82K")
            .unwrap_err();
        assert!(err.to_string().contains("dim mismatch"));
    }
}
```

**Before writing tests:** confirm the temp-DB helper. Run `grep -n "fn new_in_memory\|fn test_db\|Database::new(" src/database.rs | head`. If there is no `new_in_memory`, use the existing test pattern from `database.rs` (e.g. a `tempfile` path) for `temp_db()` and adjust accordingly.

- [ ] **Step 2: Declare the module + run the test to see it fail**

Add `pub mod models;` to `src/lib.rs` (next to the other module declarations).
Run: `cargo test models:: -- --nocapture`
Expected: FAIL to compile/resolve until `clipper::supported_models` is available (it is, from Task 2) and the temp-DB helper resolves. Fix the temp-DB helper, then the three tests should drive the implementation (which is already written above) to PASS.

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test models:: -- --nocapture`
Expected: `auto_registers_clipper_model_and_activates`, `unknown_model_errors`, `dim_mismatch_errors` PASS.

- [ ] **Step 4: Commit**

```bash
git add src/models.rs src/lib.rs && \
git commit -m "feat(models): ensure_and_activate_model helper + list rows (clipper catalog glue)"
```

---

## Task 5: imgfind — wire `--model` and `models use` to the helper

**Files:**
- Modify: `src/main.rs` (Index/Search `--model` handling at ~196-198 and ~213-215; `Models` subcommand at ~254-267)

- [ ] **Step 1: Route `--model` on `index` through the helper**

In `src/main.rs`, in `Commands::Index { .. model .. }`, replace:

```rust
            if let Some(m) = model {
                db.set_active_model(&m)?;
            }
```

with:

```rust
            if let Some(m) = model {
                imgfind::models::ensure_and_activate_model(&db, &m)?;
            }
```

(Use the correct crate path for the binary — if `main.rs` already does `use imgfind::...`, match that; otherwise `crate::models::ensure_and_activate_model` if `models` is declared in `main.rs`. Confirm with the module-declaration check from Task 4.)

- [ ] **Step 2: Route `--model` on `search` through the helper**

Same replacement in `Commands::Search { .. model .. }` (the second `if let Some(m) = model { db.set_active_model(&m)?; }` block).

- [ ] **Step 3: Make `models use` auto-register**

In `Commands::Models { action }`, change the `Use` arm:

```rust
                ModelsAction::Use { name } => {
                    imgfind::models::ensure_and_activate_model(&db, &name)?;
                    println!("Active model: {name}");
                }
```

- [ ] **Step 4: Build + run the full test suite**

Run: `cargo build && cargo test`
Expected: builds; all existing tests + Task 4 tests PASS. (No new behavior test here — this is wiring; covered by Task 4's helper tests and the manual workflow in verification.)

- [ ] **Step 5: Commit**

```bash
git add src/main.rs && \
git commit -m "feat(cli): --model and 'models use' auto-register clipper-supported models"
```

---

## Task 6: imgfind — embedder construction uses the active model + `models list` shows availability

**Files:**
- Modify: `src/main.rs` (serve spawn ~288; index ~371; search ~701; `models list` ~258-262)
- Modify: `src/tui/app/search.rs` (~148)

- [ ] **Step 1: serve — load the active model**

In `serve()` (`src/main.rs` ~282-296), resolve the active model name before spawning, and pass it in:

```rust
    let model_name = db.active_model()?.name;
    let cell: Arc<std::sync::OnceLock<ClipEmbedder>> = Arc::new(std::sync::OnceLock::new());
    {
        let cell = cell.clone();
        let model_name = model_name.clone();
        tokio::task::spawn_blocking(move || match ClipEmbedder::from_model(&model_name, false) {
            Ok(m) => {
                let _ = cell.set(m);
                info!("CLIP model loaded");
            }
            Err(e) => log::error!("CLIP model load failed: {e:#}"),
        });
    }
```

- [ ] **Step 2: index path — load the active model**

In the index flow (`src/main.rs` ~371), replace:

```rust
    let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;
```

with (the function has `db` in scope as `&mut Database`):

```rust
    let model_name = db.active_model()?.name;
    let model = ClipEmbedder::from_model(&model_name, false)
        .context("Failed to create ClipEmbedder")?;
```

- [ ] **Step 3: search path — load the active model**

In the search flow (`src/main.rs` ~701), same replacement using the `db` in scope:

```rust
    let model_name = db.active_model()?.name;
    let model = ClipEmbedder::from_model(&model_name, false)
        .context("Failed to create ClipEmbedder")?;
```

- [ ] **Step 4: TUI search — load the active model**

In `src/tui/app/search.rs` (~148), replace `ClipEmbedder::new(None, None, false)` with the active model. The `db` is moved into the async task; resolve the name before constructing the embedder:

```rust
                let model_name = db.active_model()?.name;
                let model = ClipEmbedder::from_model(&model_name, false)
                    .context("Failed to create ClipEmbedder")?;
```

- [ ] **Step 5: `models list` — show available-not-indexed**

In `Commands::Models { action }`, change the `List` arm to use `list_rows`:

```rust
                ModelsAction::List => {
                    for row in imgfind::models::list_rows(&db)? {
                        let mark = if row.active { "*" } else { " " };
                        let tag = if row.indexed { "" } else { " [available, not indexed]" };
                        println!("{} {} (dim {}){}", mark, row.name, row.dim, tag);
                    }
                }
```

- [ ] **Step 6: Build + test**

Run: `cargo build --release && cargo test`
Expected: builds (TUI feature included by default); all tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/tui/app/search.rs && \
git commit -m "feat: construct embedder from the active model; models list shows available models"
```

---

## Task 7: Documentation

**Files:**
- Modify: `USAGE.md`
- Modify: `CLAUDE.md` (Embeddings section)

- [ ] **Step 1: Document model selection in USAGE.md**

Add a "Embedding models" section to `USAGE.md` describing:
- `imgfind models list` (now shows indexed + available models),
- `imgfind models use <name>` (auto-registers a clipper-supported model, creates its vector table, sets it active),
- `--model <name>` on `index`/`search`,
- the two catalog entries (`openai/clip-vit-base-patch32` dim 512 default; `laion/CLIP-ViT-L-14-laion2B-s32B-b82K` dim 768, ~1.7 GB first download, slower indexing, higher quality),
- **the re-index requirement:** each model has its own vector table, so images must be (re)indexed under a model before they are searchable under it.

Use the workflow block from the spec:

```bash
imgfind models use laion/CLIP-ViT-L-14-laion2B-s32B-b82K
imgfind index ~/Pictures
imgfind search "a dog on a beach" --model laion/CLIP-ViT-L-14-laion2B-s32B-b82K
```

- [ ] **Step 2: Update CLAUDE.md Embeddings note**

In `CLAUDE.md`, update the **Embeddings** bullet: clipper is no longer hardwired to a single model — it vendors candle's CLIP module (with a config-driven `Gelu` activation) and exposes a catalog via `clipper::supported_models()` and `ClipEmbedder::from_model(name, use_cpu)`. imgfind resolves the active model from the W52 `models` registry. Note that the LAION model uses dim 768 and that adding further models is a clipper-catalog change.

- [ ] **Step 3: Commit**

```bash
git add USAGE.md CLAUDE.md && \
git commit -m "docs: document selectable embedding models (LAION ViT-L/14)"
```

---

## Final verification (run after all tasks)

- [ ] `cd ../clipper && cargo test` — clipper unit tests pass (online L/14 test stays ignored).
- [ ] `cargo build --release && cargo test` — imgfind builds and tests pass.
- [ ] Manual: the Task 3 spike passed (matching caption ranks higher under L/14).
- [ ] Manual end-to-end on a small directory:
  ```bash
  imgfind models list                                       # shows base (default) + laion [available, not indexed]
  imgfind models use laion/CLIP-ViT-L-14-laion2B-s32B-b82K  # auto-registers dim 768
  imgfind index <small-dir>                                 # indexes under L/14
  imgfind search "<obvious query for that dir>"             # sensible results
  imgfind models use openai/clip-vit-base-patch32           # switch back works
  ```
- [ ] `cargo clippy` (clipper + imgfind) clean, matching repo convention.
```
