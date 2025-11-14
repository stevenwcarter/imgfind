use crate::context::GraphQLContext;

use juniper::{EmptyMutation, EmptySubscription, FieldError, FieldResult, RootNode};
use tracing::error;

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
}

pub struct Mutation;

pub type Schema = RootNode<Query, EmptyMutation<GraphQLContext>, EmptySubscription<GraphQLContext>>;

pub fn create_schema() -> Schema {
    Schema::new(
        Query,
        EmptyMutation::<GraphQLContext>::new(),
        EmptySubscription::<GraphQLContext>::new(),
    )
}

pub fn graphql_translate<T>(res: Result<T, anyhow::Error>) -> FieldResult<T> {
    match res {
        Ok(t) => Ok(t),
        Err(e) => {
            error!("graphql error: {:#?}", e);
            Err(FieldError::from(e))
        }
    }
}
