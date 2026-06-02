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
pub struct ImageResult {
    pub path: String,
    pub distance: f64,
    pub file_size: Option<i32>,
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
    pub fn search(
        context: &GraphQLContext,
        query: String,
        limit: Option<i32>,
        offset: Option<i32>,
    ) -> FieldResult<Vec<ImageResult>> {
        let emb = context
            .embedder_ready()
            .ok_or_else(|| juniper::FieldError::new("model still loading", juniper::Value::null()))?
            .get_text_embedding(&query)?;
        let sc = crate::config::SearchConfig::default();
        let rows = crate::search::SearchEngine::new(&context.db).search_meta(
            emb,
            limit.unwrap_or(80) as usize,
            offset.unwrap_or(0) as usize,
            sc.distance_threshold,
            sc.max_k,
        )?;
        Ok(rows
            .into_iter()
            .map(|(path, distance, file_size)| ImageResult {
                path,
                distance: distance as f64,
                file_size: file_size.map(|s| s as i32),
            })
            .collect())
    }

    #[graphql(name = "tags")]
    pub fn tags(context: &GraphQLContext) -> FieldResult<Vec<String>> {
        Ok(context.db.list_tags()?)
    }

    #[graphql(name = "tagsForImage")]
    pub fn tags_for_image(context: &GraphQLContext, path: String) -> FieldResult<Vec<String>> {
        Ok(context
            .db
            .tags_for_image(&RelativePath(std::path::PathBuf::from(path)))?)
    }

    #[graphql(name = "collections")]
    pub fn collections(context: &GraphQLContext) -> FieldResult<Vec<String>> {
        Ok(context.db.list_collections()?)
    }

    #[graphql(name = "collectionImages")]
    pub fn collection_images(context: &GraphQLContext, name: String) -> FieldResult<Vec<String>> {
        Ok(context
            .db
            .collection_images(&name)?
            .into_iter()
            .map(|p| p.as_str().to_string())
            .collect())
    }

    #[graphql(name = "imagesByTag")]
    pub fn images_by_tag(context: &GraphQLContext, name: String) -> FieldResult<Vec<String>> {
        Ok(context
            .db
            .images_by_tag(&name)?
            .into_iter()
            .map(|p| p.as_str().to_string())
            .collect())
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

    #[graphql(name = "createTag")]
    pub fn create_tag(context: &GraphQLContext, name: String) -> FieldResult<bool> {
        context.db.create_tag(&name)?;
        Ok(true)
    }

    #[graphql(name = "tagImage")]
    pub fn tag_image(context: &GraphQLContext, path: String, tag: String) -> FieldResult<bool> {
        context
            .db
            .tag_image(&RelativePath(std::path::PathBuf::from(path)), &tag)?;
        Ok(true)
    }

    #[graphql(name = "untagImage")]
    pub fn untag_image(context: &GraphQLContext, path: String, tag: String) -> FieldResult<bool> {
        context
            .db
            .untag_image(&RelativePath(std::path::PathBuf::from(path)), &tag)?;
        Ok(true)
    }

    #[graphql(name = "createCollection")]
    pub fn create_collection(context: &GraphQLContext, name: String) -> FieldResult<bool> {
        context.db.create_collection(&name)?;
        Ok(true)
    }

    #[graphql(name = "addToCollection")]
    pub fn add_to_collection(
        context: &GraphQLContext,
        name: String,
        path: String,
    ) -> FieldResult<bool> {
        context
            .db
            .add_to_collection(&name, &RelativePath(std::path::PathBuf::from(path)))?;
        Ok(true)
    }

    #[graphql(name = "removeFromCollection")]
    pub fn remove_from_collection(
        context: &GraphQLContext,
        name: String,
        path: String,
    ) -> FieldResult<bool> {
        context
            .db
            .remove_from_collection(&name, &RelativePath(std::path::PathBuf::from(path)))?;
        Ok(true)
    }
}

pub type Schema = RootNode<Query, Mutation, EmptySubscription<GraphQLContext>>;

pub fn create_schema() -> Schema {
    Schema::new(Query, Mutation, EmptySubscription::<GraphQLContext>::new())
}
