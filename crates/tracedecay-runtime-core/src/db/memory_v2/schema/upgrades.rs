//! Fresh-install entry points for the V22/V23 storage shape.

use crate::errors::Result;

use super::super::MemoryV2Executor;
use super::compatibility::{
    install_v22_compatibility_schema, install_v23_compatibility_bank_schema,
    upgrade_v23_fact_relation_schema,
};
use super::proposals::ensure_v22_proposal_schema;

/// Installs the V22 compatibility and proposal shape over the freshly created
/// baseline. Nothing here steps an older database forward: the baseline
/// installer and these two calls together are the only way the shape appears.
pub(in crate::db) async fn install_v22_fresh_schema(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    install_v22_compatibility_schema(conn, operation).await?;
    ensure_v22_proposal_schema(conn, operation).await
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
