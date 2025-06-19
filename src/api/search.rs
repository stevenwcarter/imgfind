use anyhow::Context;
use axum::{Extension, Json, Router, extract::Path, response::IntoResponse, routing::get};
use clipper::ClipEmbedder;
use log::info;

use super::{AppError, middleware};
use crate::{
    context::GraphQLContext,
    search::{SearchEngine, normalize_vector},
    thumbnail::get_or_generate_thumbnail,
};

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
) -> anyhow::Result<impl IntoResponse, AppError> {
    let filename = format!("/{}", filename);
    info!("Size: {}, Filename: {}", size, filename);

    let size = size.parse::<u32>().unwrap_or(300);
    let mut db = context.db.lock().unwrap();
    
    // Get the image hash from the database
    let hash = db.get_image_hash(&filename)
        .with_context(|| format!("Failed to get hash for image: {}", filename))?;
    
    // Generate or retrieve thumbnail
    let thumbnail_bytes = get_or_generate_thumbnail(&mut db, &filename, &hash, size)?;

    Ok(thumbnail_bytes)
}

async fn search(
    Extension(context): Extension<GraphQLContext>,
    Path(search): Path<String>,
) -> anyhow::Result<impl IntoResponse, AppError> {
    let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;

    // Generate embedding for query
    let query_embedding = model
        .get_text_embedding(search.as_str())
        .context("Failed to generate text embedding")?;
    let normalized_query = normalize_vector(&query_embedding);

    let db = context.db.lock().unwrap();
    let search = SearchEngine::new(&db);
    let result = search
        .search(&normalized_query, 30)
        .context("Failed to perform search")?;

    Ok(Json(result))
}

async fn file(
    Extension(_context): Extension<GraphQLContext>,
    Path(filename): Path<String>,
) -> anyhow::Result<impl IntoResponse, AppError> {
    info!("Filename: {}", filename);
    let filename = format!("/{}", filename);

    Ok(std::fs::read(&filename).unwrap())
}
