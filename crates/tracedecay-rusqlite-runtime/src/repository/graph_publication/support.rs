use std::collections::HashMap;
use std::time::Duration;

use tracedecay_store::{
    GraphDependencyGenerationIdentityV1, GraphGenerationIdV1, GraphNamespaceV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationKeyV1,
    GraphPublicationOperationContextV1, GraphPublicationProjectionPageRequestV1,
    GraphPublicationReplayRecordV1, GraphPublicationReplayTombstoneV1,
    GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1, GraphVerifiedHeadV1,
    MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1, StorageRuntimeContractErrorV1, StoreShardIdV1,
    StoreShardScopeV1,
};

use crate::exact_sql::{
    ExactSqlError, ExactSqlExecuteResult, ExactSqlHandle, ExactSqlRow, ExactSqlStatement,
    ExactSqlTransaction, ExactSqlValue,
};

use super::super::{
    EncodedProjection, RawReplay, RawReplayMetadata, RawReplayTombstone, RawVerifiedHead,
    ReplayMetadata, corrupt, decode_replay, decode_replay_metadata, decode_tombstone,
    decode_verified_head, ensure_not_interrupted, sequence_from_i64, sequence_to_i64,
};
use super::{
    ExactPublicationRead, ExactQueryAuthority, REPLAY_COLUMNS, REPLAY_METADATA_COLUMNS,
    REPLAY_READER_ACQUIRE_SLICE, TOMBSTONE_COLUMNS,
};

const BEGIN_BUSY_ATTEMPT_BUDGET: u32 = 64;

/// Maximum owner sequences bound into one `IN (...)` dependency lookup.
///
/// Mirrors `REFERENCED_ANCHOR_BATCH` in
/// `tracedecay-runtime-core/src/store/memory/crud/commit.rs`: chunking keeps
/// every batched query under SQLite's bound-parameter limit regardless of
/// how large a replay page or sequence set becomes.
const GRAPH_REPLAY_DEPENDENCY_BATCH: usize = 500;

pub(super) fn begin(
    handle: &ExactSqlHandle,
    context: &GraphPublicationOperationContextV1<'_>,
) -> GraphPublicationStoreResultV1<ExactSqlTransaction> {
    let mut busy_attempts = 0_u32;
    loop {
        ensure_not_interrupted(context)?;
        match handle.begin_immediate() {
            Ok(transaction) => {
                ensure_not_interrupted(context)?;
                return Ok(transaction);
            }
            Err(ExactSqlError::Busy) => {
                busy_attempts = busy_attempts.saturating_add(1);
                if busy_attempts >= BEGIN_BUSY_ATTEMPT_BUDGET {
                    return Err(GraphPublicationStoreErrorV1::Infrastructure);
                }
                std::thread::sleep(Duration::from_millis(1));
                ensure_not_interrupted(context)?;
            }
            Err(_) => {
                ensure_not_interrupted(context)?;
                return Err(GraphPublicationStoreErrorV1::Infrastructure);
            }
        }
    }
}

pub(super) fn ensure_owner(
    handle: &ExactSqlHandle,
    projection: &GraphProjectionIdentityV1,
) -> GraphPublicationStoreResultV1<()> {
    ensure_shard_owner(handle, &projection.shard_id)
}

pub(super) fn ensure_shard_owner(
    handle: &ExactSqlHandle,
    shard_id: &StoreShardIdV1,
) -> GraphPublicationStoreResultV1<()> {
    if !matches!(
        &shard_id.scope,
        StoreShardScopeV1::Project { .. } | StoreShardScopeV1::ProfileMemory
    ) {
        return Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: "graph publication exact SQL attachment",
                shard_family: "non-graph-publication",
            },
        ));
    }
    if shard_id == &handle.binding().shard_id {
        Ok(())
    } else {
        Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::ShardMismatch {
                field: "graph publication projection",
            },
        ))
    }
}

pub(super) fn begin_read(
    handle: &ExactSqlHandle,
    context: &GraphPublicationOperationContextV1<'_>,
) -> GraphPublicationStoreResultV1<ExactPublicationRead> {
    let mut busy_attempts = 0_u32;
    loop {
        ensure_not_interrupted(context)?;
        match handle.begin_read_snapshot(REPLAY_READER_ACQUIRE_SLICE) {
            Ok(snapshot) => {
                ensure_not_interrupted(context)?;
                return Ok(ExactPublicationRead::Snapshot(snapshot));
            }
            Err(ExactSqlError::Busy) => {
                busy_attempts = busy_attempts.saturating_add(1);
                if busy_attempts >= BEGIN_BUSY_ATTEMPT_BUDGET {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
                ensure_not_interrupted(context)?;
            }
            Err(_) => {
                ensure_not_interrupted(context)?;
                break;
            }
        }
    }
    handle
        .begin_deferred()
        .map(|transaction| ExactPublicationRead::Transaction(Some(transaction)))
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
}

pub(super) fn commit(transaction: ExactSqlTransaction) -> GraphPublicationStoreResultV1<()> {
    transaction
        .commit()
        .map(|_| ())
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
}

pub(super) fn rollback<T>(
    transaction: ExactSqlTransaction,
    value: T,
) -> GraphPublicationStoreResultV1<T> {
    transaction
        .rollback()
        .map(|_| value)
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
}

pub(super) fn rollback_error<T>(
    transaction: ExactSqlTransaction,
    error: GraphPublicationStoreErrorV1,
) -> GraphPublicationStoreResultV1<T> {
    transaction
        .rollback()
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?;
    Err(error)
}

pub(super) fn statement(
    sql: impl Into<String>,
    params: Vec<ExactSqlValue>,
) -> GraphPublicationStoreResultV1<ExactSqlStatement> {
    ExactSqlStatement::new(sql.into(), params)
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
}

pub(super) fn execute(
    transaction: &ExactSqlTransaction,
    sql: &str,
    params: Vec<ExactSqlValue>,
) -> GraphPublicationStoreResultV1<ExactSqlExecuteResult> {
    transaction
        .execute(statement(sql, params)?)
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
}

pub(super) fn query(
    authority: &impl ExactQueryAuthority,
    sql: String,
    params: Vec<ExactSqlValue>,
) -> GraphPublicationStoreResultV1<Vec<ExactSqlRow>> {
    authority
        .exact_query(statement(sql, params)?)
        .map(|rows| rows.rows)
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
}

pub(super) fn read_exact(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
    one_replay(
        transaction,
        query(
            transaction,
            format!(
                "SELECT {REPLAY_COLUMNS} FROM graph_publication_replay_v1 AS replay
                 WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
                   AND generation = ?4 AND idempotency_key = ?5
                   AND NOT EXISTS (
                       SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                       WHERE retired.replay_sequence = replay.sequence
                   )"
            ),
            vec![
                text(&encoded.shard_id),
                text(&encoded.namespace),
                text(&encoded.projection),
                text(key.generation.as_str()),
                text(key.idempotency_key.as_str()),
            ],
        )?,
    )
}

pub(super) fn read_exact_metadata(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Option<ReplayMetadata>> {
    let mut rows = query(
        transaction,
        format!(
            "SELECT {REPLAY_METADATA_COLUMNS} FROM graph_publication_replay_v1 AS replay
             WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
               AND generation = ?4 AND idempotency_key = ?5
               AND NOT EXISTS (
                   SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                   WHERE retired.replay_sequence = replay.sequence
               )"
        ),
        vec![
            text(&encoded.shard_id),
            text(&encoded.namespace),
            text(&encoded.projection),
            text(key.generation.as_str()),
            text(key.idempotency_key.as_str()),
        ],
    )?;
    if rows.len() > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay metadata identity is not unique".to_owned(),
        ));
    }
    rows.pop().map(decode_metadata_row).transpose()
}

pub(super) fn read_exact_tombstone(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayTombstoneV1>> {
    one_tombstone(
        transaction,
        query(
            transaction,
            format!(
                "SELECT {TOMBSTONE_COLUMNS}
                 FROM graph_publication_replay_tombstones_v1
                 WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
                   AND generation = ?4 AND idempotency_key = ?5"
            ),
            vec![
                text(&encoded.shard_id),
                text(&encoded.namespace),
                text(&encoded.projection),
                text(key.generation.as_str()),
                text(key.idempotency_key.as_str()),
            ],
        )?,
    )
}

pub(super) fn read_tombstone_conflicts(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Vec<GraphPublicationReplayTombstoneV1>> {
    let rows = query(
        transaction,
        format!(
            "SELECT {TOMBSTONE_COLUMNS}
             FROM graph_publication_replay_tombstones_v1
             WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
               AND (generation = ?4 OR idempotency_key = ?5)
             ORDER BY replay_sequence ASC"
        ),
        vec![
            text(&encoded.shard_id),
            text(&encoded.namespace),
            text(&encoded.projection),
            text(key.generation.as_str()),
            text(key.idempotency_key.as_str()),
        ],
    )?;
    rows.into_iter()
        .map(|row| decode_tombstone_row(transaction, row))
        .collect()
}

pub(super) fn read_projection_page(
    transaction: &impl ExactQueryAuthority,
    request: &GraphPublicationProjectionPageRequestV1,
) -> GraphPublicationStoreResultV1<Vec<GraphProjectionIdentityV1>> {
    let shard_id = serde_json::to_string(&request.shard_id)
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?;
    let (after_namespace, after_projection) = request.after.as_ref().map_or_else(
        || (String::new(), String::new()),
        |after| {
            (
                after.namespace.as_str().to_owned(),
                after.projection.as_str().to_owned(),
            )
        },
    );
    let limit = i64::from(request.max_records) + 1;
    query(
        transaction,
        "SELECT namespace, projection
         FROM (
             SELECT namespace, projection
             FROM graph_publication_replay_v1
             WHERE shard_id = ?1
             UNION
             SELECT namespace, projection
             FROM graph_publication_replay_tombstones_v1
             WHERE shard_id = ?1
         )
         WHERE namespace > ?2 OR (namespace = ?2 AND projection > ?3)
         ORDER BY namespace ASC, projection ASC
         LIMIT ?4"
            .to_owned(),
        vec![
            text(shard_id),
            text(after_namespace),
            text(after_projection),
            ExactSqlValue::Integer(limit),
        ],
    )?
    .into_iter()
    .map(|mut row| {
        Ok(GraphProjectionIdentityV1 {
            shard_id: request.shard_id.clone(),
            namespace: GraphNamespaceV1::new(text_at(&mut row, 0)?).map_err(corrupt)?,
            projection: GraphProjectionIdV1::new(text_at(&mut row, 1)?).map_err(corrupt)?,
        })
    })
    .collect()
}

pub(super) fn read_first_conflict_sequence(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Option<i64>> {
    let mut rows = query(
        transaction,
        "SELECT sequence FROM graph_publication_replay_v1
         WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
           AND (generation = ?4 OR idempotency_key = ?5)
         ORDER BY sequence ASC
         LIMIT 1"
            .to_owned(),
        vec![
            text(&encoded.shard_id),
            text(&encoded.namespace),
            text(&encoded.projection),
            text(key.generation.as_str()),
            text(key.idempotency_key.as_str()),
        ],
    )?;
    if rows.len() > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay conflict probe returned duplicate rows".to_owned(),
        ));
    }
    rows.pop().map(|row| integer_at(&row, 0)).transpose()
}

pub(super) fn read_conflicts(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Vec<GraphPublicationReplayRecordV1>> {
    let rows = query(
        transaction,
        format!(
            "SELECT {REPLAY_COLUMNS} FROM graph_publication_replay_v1 AS replay
             WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
               AND (generation = ?4 OR idempotency_key = ?5)
               AND NOT EXISTS (
                   SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                   WHERE retired.replay_sequence = replay.sequence
               )
             ORDER BY sequence ASC"
        ),
        vec![
            text(&encoded.shard_id),
            text(&encoded.namespace),
            text(&encoded.projection),
            text(key.generation.as_str()),
            text(key.idempotency_key.as_str()),
        ],
    )?;
    rows.into_iter()
        .map(|row| decode_row(transaction, row))
        .collect()
}

pub(super) fn read_head(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
    let mut rows = query(
        transaction,
        "SELECT h.replay_sequence, h.recovered_digest,
                r.shard_id, r.namespace, r.projection, r.generation,
                r.idempotency_key, r.input_digest,
                r.dependency_generation_closure_digest,
                r.expected_recovered_digest
         FROM graph_verified_heads_v1 AS h
         LEFT JOIN graph_publication_replay_v1 AS r
           ON r.sequence = h.replay_sequence
         WHERE h.shard_id = ?1 AND h.namespace = ?2 AND h.projection = ?3"
            .to_owned(),
        vec![
            text(&encoded.shard_id),
            text(&encoded.namespace),
            text(&encoded.projection),
        ],
    )?;
    if rows.len() > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "verified graph projection has duplicate heads".to_owned(),
        ));
    }
    let Some(mut row) = rows.pop() else {
        return Ok(None);
    };
    let head = decode_verified_head(RawVerifiedHead {
        sequence: integer_at(&row, 0)?,
        recovered_digest: text_at(&mut row, 1)?,
        shard_id: text_at(&mut row, 2)?,
        namespace: text_at(&mut row, 3)?,
        projection: text_at(&mut row, 4)?,
        generation: text_at(&mut row, 5)?,
        idempotency_key: text_at(&mut row, 6)?,
        input_digest: text_at(&mut row, 7)?,
        dependency_generation_closure_digest: text_at(&mut row, 8)?,
        expected_recovered_digest: text_at(&mut row, 9)?,
    })?;
    let actual = EncodedProjection::new(&head.key.projection)?;
    if actual.shard_id != encoded.shard_id
        || actual.namespace != encoded.namespace
        || actual.projection != encoded.projection
    {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "verified graph head references a foreign projection replay".to_owned(),
        ));
    }
    Ok(Some(head))
}

pub(super) fn read_pending(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    actual: Option<&GraphVerifiedHeadV1>,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
    let Some(sequence) = read_pending_sequence(transaction, encoded, actual)? else {
        return Ok(None);
    };
    read_by_sequence(transaction, sequence_to_i64(sequence)?)?.map_or_else(
        || {
            Err(GraphPublicationStoreErrorV1::Corrupt(
                "pending graph publication references a missing replay".to_owned(),
            ))
        },
        |replay| Ok(Some(replay)),
    )
}

pub(super) fn read_pending_sequence(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    actual: Option<&GraphVerifiedHeadV1>,
) -> GraphPublicationStoreResultV1<Option<tracedecay_store::GraphPublicationSequenceV1>> {
    let after = actual.map_or(0, |head| head.sequence.get());
    let after = i64::try_from(after).map_err(|_| {
        GraphPublicationStoreErrorV1::Corrupt(
            "verified graph sequence exceeds SQLite integer range".to_owned(),
        )
    })?;
    let row = exactly_one(
        query(
            transaction,
            "SELECT MIN(sequence), COUNT(*) FROM (
                 SELECT sequence
                 FROM graph_publication_replay_v1 AS replay
                 WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
                   AND sequence > ?4
                   AND NOT EXISTS (
                       SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                       WHERE retired.replay_sequence = replay.sequence
                   )
                 ORDER BY sequence ASC
                 LIMIT 2
             )"
            .to_owned(),
            vec![
                text(&encoded.shard_id),
                text(&encoded.namespace),
                text(&encoded.projection),
                ExactSqlValue::Integer(after),
            ],
        )?,
        "pending graph replay aggregate",
    )?;
    let count = integer_at(&row, 1)?;
    if count > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph projection has more than one pending replay".to_owned(),
        ));
    }
    optional_integer_at(&row, 0)?
        .map(sequence_from_i64)
        .transpose()
}

pub(super) fn read_by_sequence(
    transaction: &impl ExactQueryAuthority,
    sequence: i64,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
    one_replay(
        transaction,
        query(
            transaction,
            format!(
                "SELECT {REPLAY_COLUMNS} FROM graph_publication_replay_v1 AS replay
             WHERE sequence = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                   WHERE retired.replay_sequence = replay.sequence
               )"
            ),
            vec![ExactSqlValue::Integer(sequence)],
        )?,
    )
}

pub(super) fn replay_metadata_page(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    after: u64,
    limit: u16,
) -> GraphPublicationStoreResultV1<Vec<(tracedecay_store::GraphPublicationSequenceV1, usize)>> {
    let after = sqlite_sequence_from_u64(after)?;
    let rows = query(
        transaction,
        "SELECT sequence,
                length(canonical_replay_source) + direct_dependency_bytes
         FROM graph_publication_replay_v1 AS replay
         WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
           AND sequence > ?4
           AND NOT EXISTS (
               SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
               WHERE retired.replay_sequence = replay.sequence
           )
         ORDER BY sequence ASC
         LIMIT ?5"
            .to_owned(),
        vec![
            text(&encoded.shard_id),
            text(&encoded.namespace),
            text(&encoded.projection),
            ExactSqlValue::Integer(after),
            ExactSqlValue::Integer(i64::from(limit)),
        ],
    )?;
    rows.into_iter()
        .map(|row| {
            let sequence = sequence_from_i64(integer_at(&row, 0)?)?;
            let payload_bytes = usize::try_from(integer_at(&row, 1)?).map_err(|_| {
                GraphPublicationStoreErrorV1::Corrupt(
                    "graph replay payload length is negative or exceeds usize".to_owned(),
                )
            })?;
            Ok((sequence, payload_bytes))
        })
        .collect()
}

pub(super) fn read_replays_by_sequences(
    transaction: &impl ExactQueryAuthority,
    sequences: &[i64],
) -> GraphPublicationStoreResultV1<Vec<GraphPublicationReplayRecordV1>> {
    if sequences.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (1..=sequences.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = query(
        transaction,
        format!(
            "SELECT {REPLAY_COLUMNS} FROM graph_publication_replay_v1 AS replay
             WHERE sequence IN ({placeholders})
               AND NOT EXISTS (
                   SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                   WHERE retired.replay_sequence = replay.sequence
               )
             ORDER BY sequence ASC"
        ),
        sequences
            .iter()
            .copied()
            .map(ExactSqlValue::Integer)
            .collect(),
    )?;
    if rows.len() != sequences.len() {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "enumerated graph replay disappeared in its read transaction".to_owned(),
        ));
    }
    let mut dependencies_by_owner = read_dependencies_batch(transaction, sequences, false)?;
    rows.into_iter()
        .map(|row| {
            let sequence = integer_at(&row, 0)?;
            let dependencies = dependencies_by_owner.remove(&sequence).unwrap_or_default();
            decode_row_with_dependencies(row, dependencies)
        })
        .collect()
}

pub(super) fn insert_verified_dependencies(
    transaction: &ExactSqlTransaction,
    owner: &GraphPublicationReplayRecordV1,
) -> GraphPublicationStoreResultV1<()> {
    let owner_sequence = sequence_to_i64(owner.sequence)?;
    for (ordinal, dependency) in owner
        .publication
        .direct_dependency_generations
        .iter()
        .enumerate()
    {
        let encoded = EncodedProjection::new(&dependency.projection)?;
        let mut rows = query(
            transaction,
            "SELECT replay.sequence, head.replay_sequence
             FROM graph_publication_replay_v1 AS replay
             JOIN graph_verified_heads_v1 AS head
               ON head.shard_id = replay.shard_id
              AND head.namespace = replay.namespace
              AND head.projection = replay.projection
             WHERE replay.shard_id = ?1 AND replay.namespace = ?2
               AND replay.projection = ?3 AND replay.generation = ?4
               AND NOT EXISTS (
                   SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                   WHERE retired.replay_sequence = replay.sequence
               )"
            .to_owned(),
            vec![
                text(&encoded.shard_id),
                text(&encoded.namespace),
                text(&encoded.projection),
                text(dependency.generation.as_str()),
            ],
        )?;
        if rows.len() != 1 {
            return Err(GraphPublicationStoreErrorV1::InvalidRequest(
                tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "graph replay dependency generation",
                },
            ));
        }
        let row = rows.pop().ok_or_else(|| {
            GraphPublicationStoreErrorV1::Corrupt(
                "verified graph dependency row disappeared".to_owned(),
            )
        })?;
        let dependency_sequence = integer_at(&row, 0)?;
        if integer_at(&row, 1)? < dependency_sequence {
            return Err(GraphPublicationStoreErrorV1::InvalidRequest(
                tracedecay_store::StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "graph replay dependency verified head",
                },
            ));
        }
        execute(
            transaction,
            "INSERT INTO graph_publication_replay_dependencies_v1 (
                owner_replay_sequence, ordinal, dependency_replay_sequence,
                shard_id, namespace, projection, generation
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            vec![
                ExactSqlValue::Integer(owner_sequence),
                ExactSqlValue::Integer(
                    i64::try_from(ordinal)
                        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?,
                ),
                ExactSqlValue::Integer(dependency_sequence),
                text(encoded.shard_id),
                text(encoded.namespace),
                text(encoded.projection),
                text(dependency.generation.as_str()),
            ],
        )?;
    }
    Ok(())
}

pub(super) fn has_active_inbound_dependencies(
    transaction: &impl ExactQueryAuthority,
    dependency_sequence: tracedecay_store::GraphPublicationSequenceV1,
) -> GraphPublicationStoreResultV1<bool> {
    let row = exactly_one(
        query(
            transaction,
            "SELECT COUNT(*)
             FROM graph_publication_replay_dependencies_v1 AS dependency
             WHERE dependency.dependency_replay_sequence = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM graph_publication_replay_tombstones_v1 AS retired
                   WHERE retired.replay_sequence = dependency.owner_replay_sequence
               )"
            .to_owned(),
            vec![ExactSqlValue::Integer(sequence_to_i64(
                dependency_sequence,
            )?)],
        )?,
        "graph replay inbound dependency count",
    )?;
    Ok(integer_at(&row, 0)? != 0)
}

pub(super) fn next_retired_cleanup_metadata(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    after: u64,
) -> GraphPublicationStoreResultV1<Option<(tracedecay_store::GraphPublicationSequenceV1, usize)>> {
    let after = sqlite_sequence_from_u64(after)?;
    let mut rows = query(
        transaction,
        "SELECT retired.replay_sequence,
                length(replay.canonical_replay_source)
                    + retired.direct_dependency_bytes
         FROM graph_publication_replay_tombstones_v1 AS retired
         JOIN graph_publication_replay_v1 AS replay
           ON replay.sequence = retired.replay_sequence
         WHERE retired.shard_id = ?1 AND retired.namespace = ?2
           AND retired.projection = ?3 AND retired.replay_sequence > ?4
         ORDER BY retired.replay_sequence ASC
         LIMIT 1"
            .to_owned(),
        vec![
            text(&encoded.shard_id),
            text(&encoded.namespace),
            text(&encoded.projection),
            ExactSqlValue::Integer(after),
        ],
    )?;
    if rows.len() > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "retired cleanup metadata returned duplicate rows".to_owned(),
        ));
    }
    let Some(row) = rows.pop() else {
        return Ok(None);
    };
    let sequence = sequence_from_i64(integer_at(&row, 0)?)?;
    let payload_bytes = usize::try_from(integer_at(&row, 1)?).map_err(|_| {
        GraphPublicationStoreErrorV1::Corrupt(
            "retired cleanup payload length is negative or exceeds usize".to_owned(),
        )
    })?;
    Ok(Some((sequence, payload_bytes)))
}

pub(super) fn read_tombstone_by_sequence(
    transaction: &impl ExactQueryAuthority,
    sequence: i64,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayTombstoneV1>> {
    one_tombstone(
        transaction,
        query(
            transaction,
            format!(
                "SELECT {TOMBSTONE_COLUMNS}
                 FROM graph_publication_replay_tombstones_v1
                 WHERE replay_sequence = ?1"
            ),
            vec![ExactSqlValue::Integer(sequence)],
        )?,
    )
}

fn sqlite_sequence_from_u64(value: u64) -> GraphPublicationStoreResultV1<i64> {
    i64::try_from(value).map_err(|_| {
        GraphPublicationStoreErrorV1::InvalidRequest(StorageRuntimeContractErrorV1::LimitExceeded {
            field: "graph publication sequence",
            actual: value,
            max: i64::MAX.unsigned_abs(),
        })
    })
}

fn read_dependencies(
    transaction: &impl ExactQueryAuthority,
    sequence: i64,
    retired: bool,
) -> GraphPublicationStoreResultV1<Vec<GraphDependencyGenerationIdentityV1>> {
    let (table, owner_column) = if retired {
        (
            "graph_publication_replay_tombstone_dependencies_v1",
            "tombstone_replay_sequence",
        )
    } else {
        (
            "graph_publication_replay_dependencies_v1",
            "owner_replay_sequence",
        )
    };
    let rows = query(
        transaction,
        format!(
            "SELECT ordinal, shard_id, namespace, projection, generation
             FROM {table}
             WHERE {owner_column} = ?1
             ORDER BY ordinal ASC
             LIMIT {}",
            MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1 + 1
        ),
        vec![ExactSqlValue::Integer(sequence)],
    )?;
    if rows.len() > MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay dependency count exceeds the contract limit".to_owned(),
        ));
    }
    for (expected, row) in rows.iter().enumerate() {
        if usize::try_from(integer_at(row, 0)?).ok() != Some(expected) {
            return Err(GraphPublicationStoreErrorV1::Corrupt(
                "graph replay dependency ordinals are not contiguous".to_owned(),
            ));
        }
    }
    let mut dependencies = Vec::with_capacity(rows.len());
    for mut row in rows {
        dependencies.push(GraphDependencyGenerationIdentityV1::new(
            GraphProjectionIdentityV1 {
                shard_id: serde_json::from_str::<StoreShardIdV1>(&text_at(&mut row, 1)?)
                    .map_err(corrupt)?,
                namespace: GraphNamespaceV1::new(text_at(&mut row, 2)?).map_err(corrupt)?,
                projection: GraphProjectionIdV1::new(text_at(&mut row, 3)?).map_err(corrupt)?,
            },
            GraphGenerationIdV1::new(text_at(&mut row, 4)?).map_err(corrupt)?,
        ));
    }
    Ok(dependencies)
}

/// Batched form of [`read_dependencies`] for a whole replay set: one
/// chunked `IN (...)` query per [`GRAPH_REPLAY_DEPENDENCY_BATCH`]-sized
/// slice of owner sequences instead of one query per replay.
///
/// Returns every owner's dependencies keyed by its sequence. An owner with
/// no dependency rows is simply absent from the map (callers treat a
/// missing entry the same as an empty `Vec`, matching what a per-sequence
/// `read_dependencies` call would have returned).
///
/// A `ROW_NUMBER() OVER (PARTITION BY owner ORDER BY ordinal ASC)` window
/// reproduces the per-owner `LIMIT {MAX + 1}` guard from
/// `read_dependencies` inside the single batched query, so a corrupt owner
/// with a runaway dependency count is still caught (and still bounded to
/// `MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1 + 1` rows read for that owner)
/// rather than only being caught once all of its rows are buffered.
fn read_dependencies_batch(
    transaction: &impl ExactQueryAuthority,
    sequences: &[i64],
    retired: bool,
) -> GraphPublicationStoreResultV1<HashMap<i64, Vec<GraphDependencyGenerationIdentityV1>>> {
    let mut dependencies_by_owner: HashMap<i64, Vec<GraphDependencyGenerationIdentityV1>> =
        HashMap::with_capacity(sequences.len());
    if sequences.is_empty() {
        return Ok(dependencies_by_owner);
    }
    let (table, owner_column) = if retired {
        (
            "graph_publication_replay_tombstone_dependencies_v1",
            "tombstone_replay_sequence",
        )
    } else {
        (
            "graph_publication_replay_dependencies_v1",
            "owner_replay_sequence",
        )
    };
    let dependency_rank_limit = i64::try_from(MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1 + 1)
        .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?;
    for chunk in sequences.chunks(GRAPH_REPLAY_DEPENDENCY_BATCH) {
        let placeholders = (1..=chunk.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let limit_placeholder = chunk.len() + 1;
        let rows = query(
            transaction,
            format!(
                "SELECT owner, ordinal, shard_id, namespace, projection, generation
                 FROM (
                     SELECT {owner_column} AS owner, ordinal, shard_id, namespace,
                            projection, generation,
                            ROW_NUMBER() OVER (
                                PARTITION BY {owner_column} ORDER BY ordinal ASC
                            ) AS dependency_rank
                     FROM {table}
                     WHERE {owner_column} IN ({placeholders})
                 )
                 WHERE dependency_rank <= ?{limit_placeholder}
                 ORDER BY owner ASC, ordinal ASC"
            ),
            chunk
                .iter()
                .copied()
                .map(ExactSqlValue::Integer)
                .chain(std::iter::once(ExactSqlValue::Integer(
                    dependency_rank_limit,
                )))
                .collect(),
        )?;
        for mut row in rows {
            let owner = integer_at(&row, 0)?;
            let ordinal = integer_at(&row, 1)?;
            let entry = dependencies_by_owner.entry(owner).or_default();
            if usize::try_from(ordinal).ok() != Some(entry.len()) {
                return Err(GraphPublicationStoreErrorV1::Corrupt(
                    "graph replay dependency ordinals are not contiguous".to_owned(),
                ));
            }
            entry.push(GraphDependencyGenerationIdentityV1::new(
                GraphProjectionIdentityV1 {
                    shard_id: serde_json::from_str::<StoreShardIdV1>(&text_at(&mut row, 2)?)
                        .map_err(corrupt)?,
                    namespace: GraphNamespaceV1::new(text_at(&mut row, 3)?).map_err(corrupt)?,
                    projection: GraphProjectionIdV1::new(text_at(&mut row, 4)?).map_err(corrupt)?,
                },
                GraphGenerationIdV1::new(text_at(&mut row, 5)?).map_err(corrupt)?,
            ));
            if entry.len() > MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1 {
                return Err(GraphPublicationStoreErrorV1::Corrupt(
                    "graph replay dependency count exceeds the contract limit".to_owned(),
                ));
            }
        }
    }
    Ok(dependencies_by_owner)
}

fn read_retained_source(
    transaction: &impl ExactQueryAuthority,
    sequence: i64,
) -> GraphPublicationStoreResultV1<Option<Vec<u8>>> {
    let mut rows = query(
        transaction,
        "SELECT canonical_replay_source
         FROM graph_publication_replay_v1
         WHERE sequence = ?1"
            .to_owned(),
        vec![ExactSqlValue::Integer(sequence)],
    )?;
    if rows.len() > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "retired graph replay source identity is not unique".to_owned(),
        ));
    }
    rows.pop().map(|mut row| blob_at(&mut row, 0)).transpose()
}

pub(super) fn one_replay(
    transaction: &impl ExactQueryAuthority,
    mut rows: Vec<ExactSqlRow>,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
    if rows.len() > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay identity is not unique".to_owned(),
        ));
    }
    rows.pop()
        .map(|row| decode_row(transaction, row))
        .transpose()
}

pub(super) fn one_tombstone(
    transaction: &impl ExactQueryAuthority,
    mut rows: Vec<ExactSqlRow>,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayTombstoneV1>> {
    if rows.len() > 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay tombstone identity is not unique".to_owned(),
        ));
    }
    rows.pop()
        .map(|row| decode_tombstone_row(transaction, row))
        .transpose()
}

pub(super) fn decode_row(
    transaction: &impl ExactQueryAuthority,
    row: ExactSqlRow,
) -> GraphPublicationStoreResultV1<GraphPublicationReplayRecordV1> {
    let sequence = integer_at(&row, 0)?;
    let dependencies = read_dependencies(transaction, sequence, false)?;
    decode_row_with_dependencies(row, dependencies)
}

/// Same decode as [`decode_row`], but takes already-fetched dependencies
/// instead of querying for them. Lets a caller batch-fetch dependencies for
/// a whole replay set (see [`read_dependencies_batch`]) and then decode
/// each row without an additional per-row query.
fn decode_row_with_dependencies(
    mut row: ExactSqlRow,
    dependencies: Vec<GraphDependencyGenerationIdentityV1>,
) -> GraphPublicationStoreResultV1<GraphPublicationReplayRecordV1> {
    let sequence = integer_at(&row, 0)?;
    decode_replay(
        RawReplay {
            sequence,
            shard_id: text_at(&mut row, 1)?,
            namespace: text_at(&mut row, 2)?,
            projection: text_at(&mut row, 3)?,
            generation: text_at(&mut row, 4)?,
            idempotency_key: text_at(&mut row, 5)?,
            input_digest: text_at(&mut row, 6)?,
            dependency_generation_closure_digest: text_at(&mut row, 7)?,
            direct_dependency_bytes: integer_at(&row, 8)?,
            expected_prior_head: optional_text_at(&mut row, 9)?,
            expected_recovered_digest: text_at(&mut row, 10)?,
            canonical_replay_source_digest: text_at(&mut row, 11)?,
            canonical_replay_source: blob_at(&mut row, 12)?,
        },
        dependencies,
    )
}

pub(super) fn decode_tombstone_row(
    transaction: &impl ExactQueryAuthority,
    mut row: ExactSqlRow,
) -> GraphPublicationStoreResultV1<GraphPublicationReplayTombstoneV1> {
    let sequence = integer_at(&row, 0)?;
    decode_tombstone(
        RawReplayTombstone {
            sequence,
            shard_id: text_at(&mut row, 1)?,
            namespace: text_at(&mut row, 2)?,
            projection: text_at(&mut row, 3)?,
            generation: text_at(&mut row, 4)?,
            idempotency_key: text_at(&mut row, 5)?,
            input_digest: text_at(&mut row, 6)?,
            dependency_generation_closure_digest: text_at(&mut row, 7)?,
            direct_dependency_bytes: integer_at(&row, 8)?,
            expected_prior_head: optional_text_at(&mut row, 9)?,
            expected_recovered_digest: text_at(&mut row, 10)?,
            canonical_replay_source_digest: text_at(&mut row, 11)?,
            canonical_replay_source: read_retained_source(transaction, sequence)?,
        },
        read_dependencies(transaction, sequence, true)?,
    )
}

pub(super) fn decode_metadata_row(
    mut row: ExactSqlRow,
) -> GraphPublicationStoreResultV1<ReplayMetadata> {
    decode_replay_metadata(RawReplayMetadata {
        sequence: integer_at(&row, 0)?,
        shard_id: text_at(&mut row, 1)?,
        namespace: text_at(&mut row, 2)?,
        projection: text_at(&mut row, 3)?,
        generation: text_at(&mut row, 4)?,
        idempotency_key: text_at(&mut row, 5)?,
        input_digest: text_at(&mut row, 6)?,
        dependency_generation_closure_digest: text_at(&mut row, 7)?,
        expected_prior_head: optional_text_at(&mut row, 8)?,
        expected_recovered_digest: text_at(&mut row, 9)?,
    })
}

pub(super) fn exactly_one(
    mut rows: Vec<ExactSqlRow>,
    subject: &str,
) -> GraphPublicationStoreResultV1<ExactSqlRow> {
    if rows.len() != 1 {
        return Err(GraphPublicationStoreErrorV1::Corrupt(format!(
            "{subject} returned {} rows",
            rows.len()
        )));
    }
    rows.pop()
        .ok_or_else(|| GraphPublicationStoreErrorV1::Corrupt(format!("{subject} row disappeared")))
}

pub(super) fn value_at(
    row: &ExactSqlRow,
    index: usize,
) -> GraphPublicationStoreResultV1<&ExactSqlValue> {
    row.values.get(index).ok_or_else(|| {
        GraphPublicationStoreErrorV1::Corrupt("graph publication row is truncated".to_owned())
    })
}

pub(super) fn integer_at(row: &ExactSqlRow, index: usize) -> GraphPublicationStoreResultV1<i64> {
    match value_at(row, index)? {
        ExactSqlValue::Integer(value) => Ok(*value),
        _ => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication integer column has the wrong type".to_owned(),
        )),
    }
}

pub(super) fn optional_integer_at(
    row: &ExactSqlRow,
    index: usize,
) -> GraphPublicationStoreResultV1<Option<i64>> {
    match value_at(row, index)? {
        ExactSqlValue::Null => Ok(None),
        ExactSqlValue::Integer(value) => Ok(Some(*value)),
        _ => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication optional integer column has the wrong type".to_owned(),
        )),
    }
}

pub(super) fn text_at(
    row: &mut ExactSqlRow,
    index: usize,
) -> GraphPublicationStoreResultV1<String> {
    match row.values.get_mut(index) {
        Some(ExactSqlValue::Text(value)) => Ok(std::mem::take(value)),
        Some(_) => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication text column has the wrong type".to_owned(),
        )),
        None => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication row is truncated".to_owned(),
        )),
    }
}

pub(super) fn optional_text_at(
    row: &mut ExactSqlRow,
    index: usize,
) -> GraphPublicationStoreResultV1<Option<String>> {
    match row.values.get_mut(index) {
        Some(ExactSqlValue::Null) => Ok(None),
        Some(ExactSqlValue::Text(value)) => Ok(Some(std::mem::take(value))),
        Some(_) => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication optional text column has the wrong type".to_owned(),
        )),
        None => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication row is truncated".to_owned(),
        )),
    }
}

pub(super) fn blob_at(
    row: &mut ExactSqlRow,
    index: usize,
) -> GraphPublicationStoreResultV1<Vec<u8>> {
    match row.values.get_mut(index) {
        Some(ExactSqlValue::Blob(value)) => Ok(std::mem::take(value)),
        Some(_) => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication blob column has the wrong type".to_owned(),
        )),
        None => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication row is truncated".to_owned(),
        )),
    }
}

pub(super) fn text(value: impl Into<String>) -> ExactSqlValue {
    ExactSqlValue::Text(value.into())
}

pub(super) fn optional_text(value: Option<String>) -> ExactSqlValue {
    value.map_or(ExactSqlValue::Null, ExactSqlValue::Text)
}

/// Falsifiable RED/GREEN coverage for [`read_dependencies_batch`]: asserts
/// on SQL query counts and returned data, never on elapsed time.
///
/// This lives inside `support` (rather than the richer `graph_publication`
/// test tree) because [`ExactQueryAuthority`] and the per-replay dependency
/// helpers it exercises are private to the `exact`/`support` module pair.
#[cfg(test)]
mod dependency_batch_tests {
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    use rusqlite::Savepoint;
    use tempfile::TempDir;
    use tracedecay_domain::{BrainId, LocatorDigest, ProjectId, UserProfileId, UtcMicros};
    use tracedecay_store::{
        AdmissionConfigV1, GraphDependencyGenerationClosureDigestV1,
        GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1, GraphPublicationReplayV1,
        GraphPublicationStoreV1, GraphRecoveredGenerationDigestV1, GraphReplayAppendOutcomeV1,
        GraphVerifiedHeadCasOutcomeV1, GraphVerifiedHeadCompareAndSwapV1, RepositoryWritePayloadV1,
        RuntimeCancellationIdV1, RuntimeCancellationIdentityV1, RuntimeDeadlineIdV1,
        RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1, RuntimeRequestProbeV1,
        StoreIncarnationV1, StoreRuntimeBindingV1, VerifiedStoreLocatorV1,
    };

    use super::*;
    use crate::exact_sql::ExactSqlRows;
    use crate::reader::{ExactSqlOnlyReaderV1, ExistingReaderLocator, ReaderPool};
    use crate::repository::{GRAPH_PUBLICATION_SCHEMA_V1, GraphPublicationExactSqlStorage};
    use crate::{ExistingWriterLocator, PersistentWriter, StorageOperationExecutor};

    struct NoWrites;

    impl StorageOperationExecutor for NoWrites {
        fn execute(
            &mut self,
            _savepoint: &Savepoint<'_>,
            _payload: &RepositoryWritePayloadV1,
        ) -> rusqlite::Result<()> {
            Ok(())
        }
    }

    struct Fixture {
        _directory: TempDir,
        _writer: PersistentWriter,
        _readers: ReaderPool<ExactSqlOnlyReaderV1>,
        handle: ExactSqlHandle,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = TempDir::new().unwrap();
            let path = directory
                .path()
                .join("graph-publication-dependency-batch.sqlite3");
            drop(rusqlite::Connection::open(&path).unwrap());
            let path = path.canonicalize().unwrap();
            let shard_id = StoreShardIdV1::project(
                BrainId::new("brain.dependency-batch").unwrap(),
                UserProfileId::new("profile.dependency-batch").unwrap(),
                ProjectId::new("project.dependency-batch").unwrap(),
            );
            let binding = StoreRuntimeBindingV1::new(
                shard_id,
                StoreIncarnationV1::new(3).unwrap(),
                tracedecay_store::StoreAuthorityEpochV1::new(11).unwrap(),
            );
            let locator = VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                StoreIncarnationV1::new(3).unwrap(),
                LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            );
            let writer = PersistentWriter::start(
                ExistingWriterLocator::new(binding.clone(), locator.clone(), path.clone()).unwrap(),
                AdmissionConfigV1::default(),
                NoWrites,
            )
            .unwrap();
            let readers = ReaderPool::start(
                ExistingReaderLocator::new(binding, locator, path).unwrap(),
                AdmissionConfigV1::default().readers,
                ExactSqlOnlyReaderV1,
            )
            .unwrap();
            let handle = ExactSqlHandle::attach(&writer, &readers).unwrap();
            handle
                .execute_batch(GRAPH_PUBLICATION_SCHEMA_V1.to_owned())
                .unwrap();
            Self {
                _directory: directory,
                _writer: writer,
                _readers: readers,
                handle,
            }
        }

        fn storage(&self) -> GraphPublicationExactSqlStorage {
            GraphPublicationExactSqlStorage::from_authorized_handle(self.handle.clone()).unwrap()
        }
    }

    struct Probe {
        cancellation: RuntimeCancellationIdentityV1,
        deadline: RuntimeDeadlineV1,
        commit_started: AtomicBool,
    }

    impl RuntimeRequestProbeV1 for Probe {
        fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
            &self.cancellation
        }

        fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
            &self.deadline
        }

        fn interruption(&self) -> Option<RuntimeInterruptionV1> {
            None
        }

        fn try_begin_commit(&self) -> bool {
            self.commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }
    }

    fn context(suffix: &str) -> GraphPublicationOperationContextV1<'static> {
        let cancellation = RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{suffix}"))
                .unwrap(),
            generation: 1,
        };
        let deadline = RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{suffix}")).unwrap(),
        };
        let control = RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        };
        // Leaked deliberately: the operation context's probe reference must
        // outlive this helper call, and this is test-only setup, never
        // production code.
        let probe: &'static Probe = &*Box::leak(Box::new(Probe {
            cancellation,
            deadline,
            commit_started: AtomicBool::new(false),
        }));
        GraphPublicationOperationContextV1::new(&control, probe).unwrap()
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn projection(name: &str) -> GraphProjectionIdentityV1 {
        GraphProjectionIdentityV1 {
            shard_id: StoreShardIdV1::project(
                BrainId::new("brain.dependency-batch").unwrap(),
                UserProfileId::new("profile.dependency-batch").unwrap(),
                ProjectId::new("project.dependency-batch").unwrap(),
            ),
            namespace: GraphNamespaceV1::new("project").unwrap(),
            projection: GraphProjectionIdV1::new(name).unwrap(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn replay(
        name: &str,
        generation: &str,
        input_byte: char,
        recovered_byte: char,
        dependencies: Vec<GraphDependencyGenerationIdentityV1>,
        source: &[u8],
    ) -> GraphPublicationReplayV1 {
        GraphPublicationReplayV1::new(
            GraphPublicationKeyV1::new(
                projection(name),
                GraphGenerationIdV1::new(generation).unwrap(),
                GraphPublicationIdempotencyKeyV1::new(format!("publish.{name}")).unwrap(),
            ),
            GraphPublicationInputDigestV1::new(digest(input_byte)).unwrap(),
            GraphDependencyGenerationClosureDigestV1::new(digest('d')).unwrap(),
            dependencies,
            None,
            GraphRecoveredGenerationDigestV1::new(digest(recovered_byte)).unwrap(),
            source.to_vec(),
        )
        .unwrap()
    }

    fn append_and_verify(
        storage: &mut GraphPublicationExactSqlStorage,
        name: &str,
        generation: &str,
        byte: char,
    ) -> GraphPublicationReplayV1 {
        let publication = replay(name, generation, byte, byte, Vec::new(), name.as_bytes());
        let append_context = context(&format!("{name}.append"));
        assert!(matches!(
            storage
                .append_replay(&publication, &append_context)
                .unwrap(),
            GraphReplayAppendOutcomeV1::Appended(_)
        ));
        let cas_context = context(&format!("{name}.cas"));
        let request = GraphVerifiedHeadCompareAndSwapV1 {
            publication_key: publication.key.clone(),
            input_digest: publication.input_digest.clone(),
            dependency_generation_closure_digest: publication
                .dependency_generation_closure_digest
                .clone(),
            recovered_digest: publication.expected_recovered_digest.clone(),
            expected_prior_head: publication.expected_prior_head.clone(),
        };
        assert!(matches!(
            storage
                .compare_and_swap_verified_head(&request, &cas_context)
                .unwrap(),
            GraphVerifiedHeadCasOutcomeV1::Advanced(_)
        ));
        publication
    }

    fn append_owner(
        storage: &mut GraphPublicationExactSqlStorage,
        name: &str,
        byte: char,
        dependencies: Vec<GraphDependencyGenerationIdentityV1>,
    ) -> i64 {
        let publication = replay(
            name,
            &format!("generation.{name}"),
            byte,
            byte,
            dependencies,
            name.as_bytes(),
        );
        let append_context = context(&format!("{name}.append"));
        match storage
            .append_replay(&publication, &append_context)
            .unwrap()
        {
            GraphReplayAppendOutcomeV1::Appended(record) => {
                sequence_to_i64(record.sequence).unwrap()
            }
            other => panic!("unexpected append outcome for {name}: {other:?}"),
        }
    }

    /// Wraps a real [`ExactSqlTransaction`] and counts every query whose SQL
    /// text touches `needle` (the dependency table name), while still
    /// executing the query for real against the underlying transaction.
    struct QueryCounter<'a> {
        inner: &'a ExactSqlTransaction,
        needle: &'static str,
        hits: Cell<usize>,
    }

    impl<'a> QueryCounter<'a> {
        fn new(inner: &'a ExactSqlTransaction, needle: &'static str) -> Self {
            Self {
                inner,
                needle,
                hits: Cell::new(0),
            }
        }

        fn hits(&self) -> usize {
            self.hits.get()
        }
    }

    impl ExactQueryAuthority for QueryCounter<'_> {
        fn exact_query(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ()> {
            if statement.sql.contains(self.needle) {
                self.hits.set(self.hits.get() + 1);
            }
            self.inner.exact_query(statement)
        }
    }

    /// RED (pre-fix behaviour, reproduced explicitly below): calling
    /// `read_dependencies` once per replay in a set of N replays issues N
    /// queries against `graph_publication_replay_dependencies_v1`.
    ///
    /// GREEN (this crate's current behaviour): `read_dependencies_batch`
    /// issues exactly one query for the same replay set and returns exactly
    /// the same per-owner dependency data, in the same per-owner ordinal
    /// order, as the looped per-replay reads.
    #[test]
    fn read_dependencies_batch_matches_looped_reads_in_one_query() {
        let fixture = Fixture::new();
        let mut storage = fixture.storage();

        // Two verified targets that every owner below depends on. Ordinal
        // order inside a replay's dependency list is the domain's own
        // ascending-`Ord` order for `GraphDependencyGenerationIdentityV1`
        // (projection then generation), so listing "dep-a" before "dep-b"
        // here is already canonical.
        let dep_a = append_and_verify(&mut storage, "dep-a", "generation.dep-a", 'a');
        let dep_b = append_and_verify(&mut storage, "dep-b", "generation.dep-b", 'b');
        let dependencies = vec![
            GraphDependencyGenerationIdentityV1::new(
                dep_a.key.projection.clone(),
                dep_a.key.generation.clone(),
            ),
            GraphDependencyGenerationIdentityV1::new(
                dep_b.key.projection.clone(),
                dep_b.key.generation.clone(),
            ),
        ];

        let owner_sequences: Vec<i64> = ["owner-1", "owner-2", "owner-3"]
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let byte = char::from(b'c' + u8::try_from(index).unwrap());
                append_owner(&mut storage, name, byte, dependencies.clone())
            })
            .collect();
        assert_eq!(owner_sequences.len(), 3);

        let transaction = fixture.handle.begin_immediate().unwrap();

        let batch_counter =
            QueryCounter::new(&transaction, "graph_publication_replay_dependencies_v1");
        let mut batched = read_dependencies_batch(&batch_counter, &owner_sequences, false).unwrap();
        assert_eq!(
            batch_counter.hits(),
            1,
            "batched dependency read must issue exactly one query for the whole replay set, \
             regardless of how many replays it covers"
        );

        let loop_counter =
            QueryCounter::new(&transaction, "graph_publication_replay_dependencies_v1");
        let mut looped: HashMap<i64, Vec<GraphDependencyGenerationIdentityV1>> = HashMap::new();
        for sequence in &owner_sequences {
            let dependencies = read_dependencies(&loop_counter, *sequence, false).unwrap();
            looped.insert(*sequence, dependencies);
        }
        assert_eq!(
            loop_counter.hits(),
            owner_sequences.len(),
            "looped per-replay reads reproduce the pre-fix N+1 query shape this change removes"
        );

        for sequence in &owner_sequences {
            assert_eq!(
                batched.remove(sequence),
                looped.remove(sequence),
                "batched dependency read must return the same ordinal-ordered dependencies \
                 for sequence {sequence} as a per-replay read_dependencies call"
            );
        }
        assert!(batched.is_empty());
        assert!(looped.is_empty());

        transaction.rollback().unwrap();
    }
}
