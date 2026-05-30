use crate::database::Database;

#[derive(Clone)]
pub struct GraphQLContext {
    pub db: Database,
    pub basepath: String,
}

impl GraphQLContext {
    pub fn new(db: Database, basepath: String) -> Self {
        GraphQLContext { db, basepath }
    }
}

impl juniper::Context for GraphQLContext {}
