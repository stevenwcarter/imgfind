use crate::database::Database;
use clipper::ClipEmbedder;
use std::sync::Arc;

#[derive(Clone)]
pub struct GraphQLContext {
    pub db: Database,
    pub basepath: String,
    pub embedder: Arc<std::sync::OnceLock<ClipEmbedder>>,
}

impl GraphQLContext {
    pub fn new(
        db: Database,
        basepath: String,
        embedder: Arc<std::sync::OnceLock<ClipEmbedder>>,
    ) -> Self {
        GraphQLContext {
            db,
            basepath,
            embedder,
        }
    }

    /// Returns the loaded embedder, or `None` while the CLIP model is still
    /// loading in the background.
    pub fn embedder_ready(&self) -> Option<&ClipEmbedder> {
        self.embedder.get()
    }
}

impl juniper::Context for GraphQLContext {}
