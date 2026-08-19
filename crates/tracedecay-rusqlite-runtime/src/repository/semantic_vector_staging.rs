//! Metadata-only semantic-vector staging over one already-open SQLite writer.
//!
//! This adapter accepts no path and opens no connection. The caller retains
//! daemon writer ownership; the durable writer fence is checked on every
//! mutation. The schema intentionally has no BLOB column and no source/vector
//! payload field.

#[path = "semantic_vector_staging/adoption.rs"]
mod adoption;
#[path = "semantic_vector_staging/aggregate.rs"]
mod aggregate;
#[path = "semantic_vector_staging/begin.rs"]
mod begin;
#[path = "semantic_vector_staging/census.rs"]
mod census;
#[path = "semantic_vector_staging/cursors.rs"]
mod cursors;
#[path = "semantic_vector_staging/exact.rs"]
mod exact;
#[path = "semantic_vector_staging/published.rs"]
mod published;
#[path = "semantic_vector_staging/read.rs"]
mod read;
#[path = "semantic_vector_staging/retirement.rs"]
mod retirement;
#[path = "semantic_vector_staging/settle_publication.rs"]
mod settle_publication;
#[path = "semantic_vector_staging/support.rs"]
mod support;
pub use exact::SemanticVectorStagingExactSqlStorage;

#[cfg(test)]
#[path = "semantic_vector_staging/tests.rs"]
mod tests;

pub const SEMANTIC_VECTOR_STAGING_SCHEMA: &str = include_str!("semantic_vector_staging_schema.sql");
