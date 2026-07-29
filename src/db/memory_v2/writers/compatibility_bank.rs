//! V23 compatibility-bank projection writers (dirty marking, upsert, delete,
//! and dirty clearing) plus their owner-key helper.

use tracedecay_domain::{FactOwnerV1, SourceStoreId, UtcMicros};

use crate::db::engine::params;
use crate::errors::Result;

use super::super::types::OwnerKey;
use super::super::{
    MemoryV2Executor, OPERATION, V23_COMPATIBILITY_BANK_VECTOR_BYTES,
    V23_COMPATIBILITY_BANK_VECTOR_HEADER, db_error, db_message, owner_key, validate_scope,
    validate_v1_compatibility_source,
};

/// Marks one owner-bound V23 compatibility-bank projection dirty inside the
/// caller's authoritative writer transaction.
pub(in crate::db) async fn mark_memory_v2_compatibility_bank_dirty_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    conn.execute(
        "INSERT INTO memory_v2_compatibility_bank_dirty(
            owner_kind, project_id, source_store_id, owner_json, bank_name, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(owner_kind, project_id, source_store_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            updated_at = max(
                excluded.updated_at,
                memory_v2_compatibility_bank_dirty.updated_at + 1
            )",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name,
            updated_at.0
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Replaces one owner-bound V23 compatibility-bank projection inside the
/// caller's authoritative writer transaction. The strict binary shape is the
/// canonical f32-2048 FHRR encoding, never a legacy global-bank payload.
pub(in crate::db) async fn upsert_memory_v2_compatibility_bank_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    vector: &[u8],
    fact_count: u64,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    if vector.len() != V23_COMPATIBILITY_BANK_VECTOR_BYTES
        || vector[..8] != V23_COMPATIBILITY_BANK_VECTOR_HEADER
    {
        return Err(db_message(
            OPERATION,
            "compatibility bank vector is not canonical f32-2048 FHRR data",
        ));
    }
    let fact_count = i64::try_from(fact_count)
        .map_err(|_| db_message(OPERATION, "compatibility bank fact count is out of range"))?;
    if fact_count == 0 {
        return Err(db_message(
            OPERATION,
            "compatibility bank fact count must be positive",
        ));
    }
    conn.execute(
        "INSERT INTO memory_v2_compatibility_banks(
            owner_kind, project_id, source_store_id, owner_json, bank_name,
            vector, hrr_algebra, hrr_dim, fact_count, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 'amari_fhrr', 2048, ?7, ?8)
         ON CONFLICT(owner_kind, project_id, source_store_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            vector = excluded.vector,
            hrr_algebra = excluded.hrr_algebra,
            hrr_dim = excluded.hrr_dim,
            fact_count = excluded.fact_count,
            updated_at = excluded.updated_at
         WHERE excluded.updated_at >= memory_v2_compatibility_banks.updated_at",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name,
            vector,
            fact_count,
            updated_at.0
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Deletes an empty owner-bound V23 compatibility-bank projection inside the
/// caller's authoritative writer transaction.
pub(in crate::db) async fn delete_memory_v2_compatibility_bank_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
) -> Result<()> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    conn.execute(
        "DELETE FROM memory_v2_compatibility_banks
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND owner_json = ?4 AND bank_name = ?5",
        params![
            owner.kind,
            owner.project_id.as_str(),
            source_store_id.as_str(),
            owner.json.as_str(),
            bank_name
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Clears a V23 dirty projection only when the caller rebuilt the exact owner
/// generation it observed. A concurrent mark therefore remains pending.
pub(in crate::db) async fn clear_memory_v2_compatibility_bank_dirty_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
    expected_updated_at: UtcMicros,
) -> Result<bool> {
    let owner = compatibility_bank_owner_key(owner, source_store_id, bank_name)?;
    let changed = conn
        .execute(
            "DELETE FROM memory_v2_compatibility_bank_dirty
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
               AND owner_json = ?4 AND bank_name = ?5 AND updated_at = ?6",
            params![
                owner.kind,
                owner.project_id.as_str(),
                source_store_id.as_str(),
                owner.json.as_str(),
                bank_name,
                expected_updated_at.0
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(changed == 1)
}

fn compatibility_bank_owner_key(
    owner: &FactOwnerV1,
    source_store_id: &SourceStoreId,
    bank_name: &str,
) -> Result<OwnerKey> {
    validate_scope(owner, source_store_id)?;
    validate_v1_compatibility_source(source_store_id)?;
    if !matches!(
        bank_name,
        "all" | "general" | "user_pref" | "project" | "tool" | "decision" | "code_area"
    ) {
        return Err(db_message(
            OPERATION,
            "compatibility bank name is unsupported",
        ));
    }
    owner_key(owner)
}
