//! Final owner-keyed holographic bank projection writers (dirty marking,
//! upsert, delete, and dirty clearing) plus their owner-key helper.

use tracedecay_domain::{FactOwnerV1, UtcMicros};

use crate::db::engine::params;
use crate::errors::Result;

use super::super::types::OwnerKey;
use super::super::{
    BANK_VECTOR_BYTES, BANK_VECTOR_HEADER, MemoryV2Executor, OPERATION, db_error, db_message,
    owner_key,
};

/// Marks one owner-bound bank projection dirty inside the caller's
/// authoritative writer transaction.
pub(in crate::db) async fn mark_memory_v2_bank_dirty_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    bank_name: &str,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = bank_owner_key(owner, bank_name)?;
    conn.execute(
        "INSERT INTO memory_v2_bank_dirty(
            owner_kind, project_id, owner_json, bank_name, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(owner_kind, project_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            updated_at = max(
                excluded.updated_at,
                memory_v2_bank_dirty.updated_at + 1
            )",
        params![
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            bank_name,
            updated_at.0
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Replaces one owner-bound bank projection inside the caller's authoritative
/// writer transaction. The strict binary shape is the canonical f32-2048 FHRR
/// encoding.
pub(in crate::db) async fn upsert_memory_v2_bank_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    bank_name: &str,
    vector: &[u8],
    fact_count: u64,
    updated_at: UtcMicros,
) -> Result<()> {
    let owner = bank_owner_key(owner, bank_name)?;
    if vector.len() != BANK_VECTOR_BYTES || vector[..8] != BANK_VECTOR_HEADER {
        return Err(db_message(
            OPERATION,
            "bank vector is not canonical f32-2048 FHRR data",
        ));
    }
    let fact_count = i64::try_from(fact_count)
        .map_err(|_| db_message(OPERATION, "bank fact count is out of range"))?;
    if fact_count == 0 {
        return Err(db_message(OPERATION, "bank fact count must be positive"));
    }
    conn.execute(
        "INSERT INTO memory_v2_banks(
            owner_kind, project_id, owner_json, bank_name,
            vector, hrr_algebra, hrr_dim, fact_count, updated_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, 'amari_fhrr', 2048, ?6, ?7)
         ON CONFLICT(owner_kind, project_id, bank_name) DO UPDATE SET
            owner_json = excluded.owner_json,
            vector = excluded.vector,
            hrr_algebra = excluded.hrr_algebra,
            hrr_dim = excluded.hrr_dim,
            fact_count = excluded.fact_count,
            updated_at = excluded.updated_at
         WHERE excluded.updated_at >= memory_v2_banks.updated_at",
        params![
            owner.kind,
            owner.project_id.as_str(),
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

/// Deletes an empty owner-bound bank projection inside the caller's
/// authoritative writer transaction.
pub(in crate::db) async fn delete_memory_v2_bank_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    bank_name: &str,
) -> Result<()> {
    let owner = bank_owner_key(owner, bank_name)?;
    conn.execute(
        "DELETE FROM memory_v2_banks
         WHERE owner_kind = ?1 AND project_id = ?2
           AND owner_json = ?3 AND bank_name = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
            bank_name
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| db_error(OPERATION, error))
}

/// Clears a dirty projection only when the caller rebuilt the exact owner
/// generation it observed. A concurrent mark therefore remains pending.
pub(in crate::db) async fn clear_memory_v2_bank_dirty_in_transaction(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    bank_name: &str,
    expected_updated_at: UtcMicros,
) -> Result<bool> {
    let owner = bank_owner_key(owner, bank_name)?;
    let changed = conn
        .execute(
            "DELETE FROM memory_v2_bank_dirty
             WHERE owner_kind = ?1 AND project_id = ?2
               AND owner_json = ?3 AND bank_name = ?4 AND updated_at = ?5",
            params![
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                bank_name,
                expected_updated_at.0
            ],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    Ok(changed == 1)
}

fn bank_owner_key(owner: &FactOwnerV1, bank_name: &str) -> Result<OwnerKey> {
    owner
        .validate()
        .map_err(|_| db_message(OPERATION, "fact owner is invalid"))?;
    if !matches!(
        bank_name,
        "all" | "general" | "user_pref" | "project" | "tool" | "decision" | "code_area"
    ) {
        return Err(db_message(OPERATION, "bank name is unsupported"));
    }
    owner_key(owner)
}
