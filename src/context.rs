use crate::database::Database;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct GraphQLContext {
    pub db: Arc<Mutex<Database>>,
}

impl GraphQLContext {
    pub fn new(db: Database) -> Self {
        GraphQLContext {
            db: Arc::new(Mutex::new(db)),
        }
    }

    pub async fn get_db(&self) -> Arc<Mutex<Database>> {
        self.db.clone()
    }
}

impl juniper::Context for GraphQLContext {}
