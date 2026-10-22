//! Chunked, publication-bound caches of an immutable journal prefix.

use tracedecay_contracts::AuthorizedWorkProductScopeV1;
use tracedecay_domain::{ManifestDigest, WorkProductEventSequenceV1, WorkProductGraphV1};

use super::{StoreFault, cursor_value, owner_params, recovered_graph_digest};
use crate::exact_sql::ExactSqlValue;
use crate::work::{
    RegisteredWorkQuery, RegisteredWorkWrite, exact_sql_integer, exact_sql_statement,
    exact_sql_text, registered_work_query,
};

// A chunk fits one exact-SQL request and a page stays below its 64 MiB result
// ceiling, regardless of the legal graph's total serialized size.
const CHUNK_BYTES: usize = 1024 * 1024;
const PAGE_CHUNKS: u64 = 32;

pub(super) struct GraphCheckpoint {
    pub(super) sequence: u64,
    pub(super) graph: WorkProductGraphV1,
}

/// Historical writes record the earliest affected version even when no
/// checkpoint covers it. Later events may overwrite the changed value, so a
/// matching current digest alone cannot verify an invalidated history.
pub(super) fn invalidated_from(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    through: WorkProductEventSequenceV1,
) -> Result<Option<WorkProductEventSequenceV1>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT from_sequence FROM work_product_journal_invalidations_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND from_sequence <= ?3",
        owner_params(scope)
            .into_iter()
            .chain([cursor_value(through.get(), "journal invalidation bound")?])
            .collect(),
    )?;
    rows.rows
        .first()
        .map(|row| {
            exact_sql_integer(&row.values, 0)
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| WorkProductEventSequenceV1::new(value).ok())
                .ok_or(StoreFault::Corrupt("journal invalidation row"))
        })
        .transpose()
}

/// Only the transaction that verified the complete affected history can
/// clear its marker, together with the new verified publication.
pub(super) fn clear_invalidation(
    transaction: &impl RegisteredWorkWrite,
    scope: &AuthorizedWorkProductScopeV1,
    from: WorkProductEventSequenceV1,
) -> Result<(), StoreFault> {
    let result = transaction.work_execute(exact_sql_statement(
        "DELETE FROM work_product_journal_invalidations_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND from_sequence = ?3",
        owner_params(scope)
            .into_iter()
            .chain([cursor_value(from.get(), "verified journal invalidation")?])
            .collect(),
    )?)?;
    if result.changed_rows != 1 {
        return Err(StoreFault::Corrupt("journal invalidation changed"));
    }
    Ok(())
}

pub(super) fn load(
    source: &impl RegisteredWorkQuery,
    scope: &AuthorizedWorkProductScopeV1,
    through: u64,
) -> Result<Option<GraphCheckpoint>, StoreFault> {
    let rows = registered_work_query(
        source,
        "SELECT checkpoint.event_sequence, checkpoint.graph_version,
                checkpoint.recovered_graph_digest, checkpoint.payload_bytes, checkpoint.chunk_count,
                published.graph_version, published.recovered_graph_digest
         FROM work_product_graph_checkpoints_v1 AS checkpoint
         LEFT JOIN work_product_graph_versions_v1 AS published
           ON published.owner_brain_id = checkpoint.owner_brain_id
          AND published.owner_profile_id = checkpoint.owner_profile_id
          AND published.event_sequence = checkpoint.event_sequence
         WHERE checkpoint.owner_brain_id = ?1 AND checkpoint.owner_profile_id = ?2
           AND checkpoint.event_sequence <= ?3
         ORDER BY checkpoint.event_sequence DESC LIMIT 1",
        owner_params(scope)
            .into_iter()
            .chain([cursor_value(through, "checkpoint bound")?])
            .collect(),
    )?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let corrupt = || StoreFault::Corrupt("graph checkpoint row");
    let positive = |index| {
        exact_sql_integer(&row.values, index)
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(corrupt)
    };
    let sequence = positive(0)?;
    let version = positive(1)?;
    let digest = exact_sql_text(&row.values, 2).ok_or_else(corrupt)?;
    let payload_bytes = usize::try_from(positive(3)?).map_err(|_| corrupt())?;
    let chunk_count = positive(4)?;
    if positive(5)? != version
        || exact_sql_text(&row.values, 6) != Some(digest)
        || payload_bytes.div_ceil(CHUNK_BYTES) as u64 != chunk_count
    {
        return Err(corrupt());
    }

    let mut payload = Vec::new();
    let mut next_chunk = 0_u64;
    loop {
        let rows = registered_work_query(
            source,
            "SELECT chunk_index, chunk_payload FROM work_product_graph_checkpoint_chunks_v1
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
               AND event_sequence = ?3 AND chunk_index >= ?4
             ORDER BY chunk_index LIMIT ?5",
            owner_params(scope)
                .into_iter()
                .chain([
                    cursor_value(sequence, "checkpoint sequence")?,
                    cursor_value(next_chunk, "checkpoint chunk cursor")?,
                    cursor_value(PAGE_CHUNKS, "checkpoint chunk page")?,
                ])
                .collect(),
        )?;
        if rows.rows.is_empty() {
            break;
        }
        for row in rows.rows {
            let index = exact_sql_integer(&row.values, 0)
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(corrupt)?;
            let Some(ExactSqlValue::Blob(chunk)) = row.values.get(1) else {
                return Err(corrupt());
            };
            if index != next_chunk || next_chunk >= chunk_count {
                return Err(corrupt());
            }
            let expected_bytes = payload_bytes.saturating_sub(payload.len()).min(CHUNK_BYTES);
            if chunk.len() != expected_bytes || chunk.is_empty() {
                return Err(corrupt());
            }
            payload.extend_from_slice(chunk);
            next_chunk += 1;
        }
    }
    if next_chunk != chunk_count || payload.len() != payload_bytes {
        return Err(corrupt());
    }
    let graph: WorkProductGraphV1 = serde_json::from_slice(&payload).map_err(|_| corrupt())?;
    if graph.version().get() != version
        || recovered_graph_digest(&graph)
            .as_ref()
            .map(ManifestDigest::as_str)
            != Some(digest)
    {
        return Err(corrupt());
    }
    Ok(Some(GraphCheckpoint { sequence, graph }))
}

pub(super) fn write(
    transaction: &impl RegisteredWorkWrite,
    scope: &AuthorizedWorkProductScopeV1,
    sequence: WorkProductEventSequenceV1,
    graph: &WorkProductGraphV1,
    digest: &ManifestDigest,
) -> Result<(), StoreFault> {
    let payload = serde_json::to_vec(graph)
        .map_err(|_| StoreFault::Corrupt("graph checkpoint serialization"))?;
    for (index, chunk) in payload.chunks(CHUNK_BYTES).enumerate() {
        transaction.work_execute(exact_sql_statement(
            "INSERT INTO work_product_graph_checkpoint_chunks_v1 (
                owner_brain_id, owner_profile_id, event_sequence, chunk_index, chunk_payload
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            owner_params(scope)
                .into_iter()
                .chain([
                    cursor_value(sequence.get(), "checkpoint sequence")?,
                    cursor_value(index as u64, "checkpoint chunk index")?,
                    ExactSqlValue::Blob(chunk.to_vec()),
                ])
                .collect(),
        )?)?;
    }
    // The header and every chunk become visible only with the event/version
    // commit. Any refusal rolls all of them back together.
    transaction.work_execute(exact_sql_statement(
        "INSERT INTO work_product_graph_checkpoints_v1 (
            owner_brain_id, owner_profile_id, event_sequence, graph_version,
            recovered_graph_digest, payload_bytes, chunk_count
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        owner_params(scope)
            .into_iter()
            .chain([
                cursor_value(sequence.get(), "checkpoint sequence")?,
                cursor_value(graph.version().get(), "checkpoint graph version")?,
                ExactSqlValue::Text(digest.as_str().to_owned()),
                cursor_value(payload.len() as u64, "checkpoint payload size")?,
                cursor_value(
                    payload.len().div_ceil(CHUNK_BYTES) as u64,
                    "checkpoint chunk count",
                )?,
            ])
            .collect(),
    )?)?;
    Ok(())
}
