//! Glue between imgfind's DB model registry (W52) and clipper's model catalog.

use crate::database::Database;
use anyhow::{Context, Result};
use std::path::Path;

/// Open the database at `db_path`, creating it if needed. If the database did
/// **not** already exist and `default_model` is `Some`, seed that model as the
/// active one (auto-registering it via [`ensure_and_activate_model`]). Existing
/// databases are returned untouched — the default only applies to brand-new
/// databases, so a per-database `models use` choice is never overridden.
pub fn open_db_seeding_default(db_path: &Path, default_model: Option<&str>) -> Result<Database> {
    let existed = db_path.exists();
    let db = Database::new(db_path)?;
    if let (false, Some(name)) = (existed, default_model) {
        ensure_and_activate_model(&db, name)
            .with_context(|| format!("seeding new database with default model '{name}'"))?;
    }
    Ok(db)
}

/// Resolve `name` to the active model, auto-registering it if clipper supports
/// it but the DB has not seen it yet, and validating that an already-registered
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
        let known: Vec<_> = clipper::supported_models()
            .into_iter()
            .map(|m| m.name)
            .collect();
        format!("unknown model '{name}'; supported models: {known:?}")
    })?;
    db.register_model(name, dim)?;
    db.set_active_model(name)?;
    Ok(())
}

/// A row for `models list`: registered models plus clipper-supported models
/// that are not yet registered (marked not-indexed).
pub struct ModelRow {
    pub name: String,
    pub dim: usize,
    pub active: bool,
    pub indexed: bool,
}

/// Build the rows for `models list`: every registered (indexed) model, plus any
/// clipper-supported model that isn't registered yet (marked not-indexed).
pub fn list_rows(db: &Database) -> Result<Vec<ModelRow>> {
    let registered = db.list_models()?;
    let names: std::collections::HashSet<&str> =
        registered.iter().map(|(n, _, _)| n.as_str()).collect();
    let mut rows: Vec<ModelRow> = registered
        .iter()
        .map(|(n, d, a)| ModelRow {
            name: n.clone(),
            dim: *d,
            active: *a,
            indexed: true,
        })
        .collect();
    for m in clipper::supported_models() {
        if !names.contains(m.name.as_str()) {
            rows.push(ModelRow {
                name: m.name,
                dim: m.dim,
                active: false,
                indexed: false,
            });
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("imgfind_models_test_{}_{n}", std::process::id()));
        dir.join(".imgfind").join("imgfind.db")
    }

    fn cleanup(p: &Path) {
        let _ = std::fs::remove_dir_all(p.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn auto_registers_clipper_model_and_activates() {
        let path = temp_db_path();
        let db = Database::new(&path).expect("create db");
        ensure_and_activate_model(&db, "laion/CLIP-ViT-L-14-laion2B-s32B-b82K").unwrap();
        let active = db.active_model().unwrap();
        assert_eq!(active.name, "laion/CLIP-ViT-L-14-laion2B-s32B-b82K");
        assert_eq!(active.dim, 768);
        cleanup(&path);
    }

    #[test]
    fn unknown_model_errors() {
        let path = temp_db_path();
        let db = Database::new(&path).expect("create db");
        let err = ensure_and_activate_model(&db, "nope/not-a-model").unwrap_err();
        assert!(err.to_string().contains("unknown model"), "got: {err}");
        cleanup(&path);
    }

    #[test]
    fn dim_mismatch_errors() {
        let path = temp_db_path();
        let db = Database::new(&path).expect("create db");
        // Register the LAION name with the WRONG dim, then try to activate it.
        db.register_model("laion/CLIP-ViT-L-14-laion2B-s32B-b82K", 512)
            .unwrap();
        let err = ensure_and_activate_model(&db, "laion/CLIP-ViT-L-14-laion2B-s32B-b82K")
            .unwrap_err();
        assert!(err.to_string().contains("dim mismatch"), "got: {err}");
        cleanup(&path);
    }

    #[test]
    fn open_db_seeds_default_only_on_creation() {
        let path = temp_db_path();

        // Brand-new DB + a default -> the default becomes active.
        let db = open_db_seeding_default(&path, Some("laion/CLIP-ViT-L-14-laion2B-s32B-b82K"))
            .expect("create + seed");
        assert_eq!(
            db.active_model().unwrap().name,
            "laion/CLIP-ViT-L-14-laion2B-s32B-b82K"
        );
        drop(db);

        // Re-opening an EXISTING DB must not reseed, even with a different default.
        let db = open_db_seeding_default(&path, Some("openai/clip-vit-base-patch32"))
            .expect("reopen existing");
        assert_eq!(
            db.active_model().unwrap().name,
            "laion/CLIP-ViT-L-14-laion2B-s32B-b82K",
            "existing DB's active model must be preserved"
        );
        cleanup(&path);
    }

    #[test]
    fn open_db_without_default_uses_baseline() {
        let path = temp_db_path();
        let db = open_db_seeding_default(&path, None).expect("create");
        // Fresh DB with no default keeps the migration-seeded baseline.
        assert_eq!(
            db.active_model().unwrap().name,
            "openai/clip-vit-base-patch32"
        );
        cleanup(&path);
    }
}
