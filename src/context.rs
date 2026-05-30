use crate::database::Database;
use clipper::ClipEmbedder;
use std::sync::Arc;

#[derive(Clone)]
pub struct GraphQLContext {
    pub db: Database,
    pub basepath: String,
    pub embedder: Arc<ClipEmbedder>,
}

impl GraphQLContext {
    pub fn new(db: Database, basepath: String, embedder: Arc<ClipEmbedder>) -> Self {
        GraphQLContext {
            db,
            basepath,
            embedder,
        }
    }
}

impl juniper::Context for GraphQLContext {}
