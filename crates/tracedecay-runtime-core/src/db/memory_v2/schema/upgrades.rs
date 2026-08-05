//! Fresh-install entry points for the V22/V23 storage shape.

use crate::errors::Result;

use super::super::MemoryV2Executor;
use super::compatibility::{
    install_v22_compatibility_schema, install_v23_compatibility_bank_schema,
    upgrade_v23_fact_relation_schema,
};

/// Installs the retained V22 memory projections over the final baseline.
pub(in crate::db) async fn install_v22_fresh_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await
}

/// Installs V23 over the fresh V22 shape: the constrained relation projection
/// followed by owner-keyed compatibility-bank state.
pub(in crate::db) async fn install_v23_fresh_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    upgrade_v23_fact_relation_schema(conn, operation).await?;
    install_v23_compatibility_bank_schema(conn, operation).await
}
