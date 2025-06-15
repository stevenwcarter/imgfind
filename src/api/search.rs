use anyhow::Context;
use axum::{Extension, Json, Router, extract::Path, response::IntoResponse, routing::get};
use clipper::ClipEmbedder;
use log::info;

use super::{AppError, middleware};
use crate::{
    context::GraphQLContext,
    search::{SearchEngine, normalize_vector},
};

pub fn routes(context: GraphQLContext) -> Router {
    Router::new()
        .route("/{search}", get(search))
        .route("/file/{*filename}", get(file))
        .layer(Extension(context.clone()))
        .layer(middleware())
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
    // let db = context.db.lock().unwrap();
    // let mut stmt = db
    //     .conn
    //     .prepare("SELECT COUNT(*) FROM images WHERE path = ?1")?;
    //
    // let count: i64 = stmt.query_row(params![filename], |row| row.get(0))?;
    //
    // if count == 0 {
    //     return Err(AppError(anyhow::anyhow!("File not found in database")));
    // }

    Ok(std::fs::read(&filename).unwrap())
}
