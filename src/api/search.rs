use anyhow::{Context, Result};
use axum::{
    Extension, Json, Router,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use log::info;
use tracing::{debug, error, warn};

use super::{AppError, middleware};
use crate::{
    context::GraphQLContext,
    search::SearchEngine,
    thumbnail::get_or_generate_thumbnail,
};

/// Join `filename` onto `base`, returning the path only if it stays within `base`.
/// Rejects absolute paths and any `..` that would climb above `base`.
fn safe_join(base: &std::path::Path, filename: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;

    let rel = std::path::Path::new(filename);
    if rel.is_absolute() {
        return None;
    }

    let mut result = base.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => result.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() || !result.starts_with(base) {
                    return None;
                }
            }
            // RootDir / Prefix => absolute or platform prefix: reject.
            _ => return None,
        }
    }

    if result.starts_with(base) {
        Some(result)
    } else {
        None
    }
}

pub fn routes(context: GraphQLContext) -> Router {
    Router::new()
        .route("/{search}", get(search))
        .route("/file/{*filename}", get(file))
        .route("/thumb:{size}/{*filename}", get(thumb))
        .layer(Extension(context.clone()))
        .layer(middleware())
}

async fn thumb(
    Extension(context): Extension<GraphQLContext>,
    Path((size, filename)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    info!("Size: {}, Filename: {}", size, filename);

    let size = size.parse::<u32>().unwrap_or(300);
    let db = &context.db;

    // Get the image hash from the database
    let hash = db
        .get_image_hash(&filename)
        .with_context(|| format!("Failed to get hash for image: {}", filename))?;

    // Generate or retrieve thumbnail
    let thumbnail_bytes = get_or_generate_thumbnail(db, &filename, &hash, size)?;

    Ok(thumbnail_bytes)
}

async fn search(
    Extension(context): Extension<GraphQLContext>,
    Path(search): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    // Generate embedding for query using the cached model.
    let query_embedding = context
        .embedder
        .get_text_embedding(search.as_str())
        .context("Failed to generate text embedding")?;

    let search = SearchEngine::new(&context.db);
    let result = search
        .search_with_thumbnails(&query_embedding, 80)
        .context("Failed to perform search")?;

    Ok(Json(result))
}

async fn file(
    Extension(context): Extension<GraphQLContext>,
    Path(filename): Path<String>,
) -> Response {
    // Canonicalize the base so containment checks compare real paths.
    let base = std::fs::canonicalize(&context.basepath)
        .unwrap_or_else(|_| std::path::PathBuf::from(&context.basepath));

    let Some(full_path) = safe_join(&base, &filename) else {
        warn!("rejected path traversal attempt: {filename}");
        return StatusCode::NOT_FOUND.into_response();
    };
    debug!("Serving file: {:?}", full_path);

    match std::fs::read(&full_path) {
        Ok(data) => data.into_response(),
        Err(e) => {
            error!("Error reading file {}: {}", filename, e);
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::safe_join;
    use std::path::{Path, PathBuf};

    #[test]
    fn allows_simple_relative() {
        let base = Path::new("/srv/images");
        assert_eq!(
            safe_join(base, "a/b.jpg"),
            Some(PathBuf::from("/srv/images/a/b.jpg"))
        );
    }

    #[test]
    fn allows_internal_dotdot() {
        let base = Path::new("/srv/images");
        assert_eq!(
            safe_join(base, "a/../b.jpg"),
            Some(PathBuf::from("/srv/images/b.jpg"))
        );
    }

    #[test]
    fn rejects_parent_traversal() {
        let base = Path::new("/srv/images");
        assert_eq!(safe_join(base, "../../etc/passwd"), None);
    }

    #[test]
    fn rejects_absolute_path() {
        let base = Path::new("/srv/images");
        assert_eq!(safe_join(base, "/etc/passwd"), None);
    }

    #[test]
    fn rejects_climb_then_descend_escape() {
        let base = Path::new("/srv/images");
        assert_eq!(safe_join(base, "a/../../b"), None);
    }
}
