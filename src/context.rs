use crate::database::Database;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct GraphQLContext {
    pub db: Arc<Mutex<Database>>,
    pub basepath: String,
}

impl GraphQLContext {
    pub fn new(db: Database, basepath: String) -> Self {
        GraphQLContext {
            db: Arc::new(Mutex::new(db)),
            basepath,
        }
    }

    pub async fn get_db(&self) -> Arc<Mutex<Database>> {
        self.db.clone()
    }
}

impl juniper::Context for GraphQLContext {}
