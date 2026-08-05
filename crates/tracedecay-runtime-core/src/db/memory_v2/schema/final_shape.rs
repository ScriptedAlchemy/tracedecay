//! Final compatibility projections and holographic derived state.

use crate::errors::Result;

use super::super::MemoryV2Executor;
use super::compatibility::{install_compatibility_bank_schema, install_compatibility_schema};

pub(in crate::db) async fn install_final_shape(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    install_compatibility_schema(conn, operation).await?;
    install_compatibility_bank_schema(conn, operation).await
}
