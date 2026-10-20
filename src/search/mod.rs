//! Search-pagination context: bind a cursor to the query that produced it so a
//! follow-up call carrying only the cursor can restore the full filters.

pub mod store;

pub use store::{SearchCtx, SearchQuery, SearchStore};
