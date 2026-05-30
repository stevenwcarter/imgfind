use axum::response::{IntoResponse, Response};
use axum::{Router, http::StatusCode};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;

use self::search::routes as search_routes;
use crate::context::GraphQLContext;

mod search;

pub fn middleware() -> tower::ServiceBuilder<
    tower::layer::util::Stack<
        tower_http::compression::CompressionLayer,
        tower::layer::util::Identity,
    >,
> {
    ServiceBuilder::new().layer(CompressionLayer::new())
}

pub fn api_routes(context: GraphQLContext) -> Router {
    Router::new().nest("/search", search_routes(context.clone()))
}

// Make our own error that wraps `anyhow::Error`.
pub struct AppError(anyhow::Error);

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {:?}", self.0),
        )
            .into_response()
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
