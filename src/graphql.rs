use base64::{Engine, engine::general_purpose};
use juniper::{EmptySubscription, FieldResult, GraphQLObject, RootNode};
use tracing::info;

use crate::RelativePath;
use crate::context::GraphQLContext;

#[derive(GraphQLObject)]
pub struct ImageBoundsResult {
    images: Vec<ImageLocation>,
    original_count: i32,
}

#[derive(GraphQLObject)]
pub struct ImageLocation {
    pub path: String,
    pub latitude: f64,
    pub longitude: f64,
    pub thumbnail_base64: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub datetime_taken: Option<String>,
}

pub struct Query;

#[juniper::graphql_object(Context = GraphQLContext)]
impl Query {
    #[graphql(name = "search")]
    pub fn search(_context: &GraphQLContext) -> FieldResult<Vec<String>> {
        Ok(vec![
            "Search result 1".to_string(),
            "Search result 2".to_string(),
            "Search result 3".to_string(),
        ])
    }

    #[graphql(name = "favorites")]
    pub fn favorites(context: &GraphQLContext) -> FieldResult<Vec<String>> {
        // Boundary: serialize stored relative paths back to plain strings.
        Ok(context
            .db
            .list_favorites()?
            .into_iter()
            .map(|p| p.as_str().into_owned())
            .collect())
    }

    #[graphql(name = "isFavorite")]
    pub fn is_favorite(context: &GraphQLContext, path: String) -> FieldResult<bool> {
        Ok(context.db.is_favorite(&RelativePath(path.into()))?)
    }

    #[graphql(name = "imagesByBounds")]
    pub async fn images_by_bounds(
        context: &GraphQLContext,
        north: f64,
        south: f64,
        east: f64,
        west: f64,
    ) -> FieldResult<ImageBoundsResult> {
        let db = &context.db;

        let (images, original_count) = db.get_images_by_bounds(north, south, east, west)?;
        info!("Found {} images", images.len());
        let mut result = Vec::new();

        let original_count = original_count as i32;
        for image in images {
            // Skip images without GPS coordinates
            if let (Some(lat), Some(lon)) = (image.latitude, image.longitude) {
                // Try to get existing thumbnail
                let thumbnail_base64 = match db.get_thumbnail(&image.hash, 300) {
                    Ok(thumbnail_data) => Some(general_purpose::STANDARD.encode(&thumbnail_data)),
                    Err(_) => {
                        // For now, return None if thumbnail doesn't exist
                        // In a real implementation, we might want to generate it
                        None
                    }
                };

                result.push(ImageLocation {
                    path: image.path,
                    latitude: lat,
                    longitude: lon,
                    thumbnail_base64,
                    width: image.width.map(|w| w as i32),
                    height: image.height.map(|h| h as i32),
                    datetime_taken: image.datetime_taken,
                });
            }
        }

        Ok(ImageBoundsResult {
            images: result,
            original_count,
        })
    }
}

pub struct Mutation;

#[juniper::graphql_object(Context = GraphQLContext)]
impl Mutation {
    #[graphql(name = "toggleFavorite")]
    pub fn toggle_favorite(context: &GraphQLContext, path: String) -> FieldResult<bool> {
        Ok(context.db.toggle_favorite(&RelativePath(path.into()))?)
    }
}

pub type Schema = RootNode<Query, Mutation, EmptySubscription<GraphQLContext>>;

pub fn create_schema() -> Schema {
    Schema::new(
        Query,
        Mutation,
        EmptySubscription::<GraphQLContext>::new(),
    )
}
