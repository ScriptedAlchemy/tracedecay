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

pub(super) fn begin(
    handle: &ExactSqlHandle,
    context: &GraphPublicationOperationContextV1<'_>,
) -> GraphPublicationStoreResultV1<ExactSqlTransaction> {
    loop {
        ensure_not_interrupted(context)?;
        match handle.begin_immediate() {
            Ok(transaction) => {
                ensure_not_interrupted(context)?;
                return Ok(transaction);
            }
            Err(ExactSqlError::Busy) => {
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
    if !matches!(&shard_id.scope, StoreShardScopeV1::Project { .. }) {
        return Err(GraphPublicationStoreErrorV1::InvalidRequest(
            tracedecay_store::StorageRuntimeContractErrorV1::OperationScopeMismatch {
                operation: "graph publication exact SQL attachment",
                shard_family: "non-project",
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
    loop {
        ensure_not_interrupted(context)?;
        match handle.begin_read_snapshot(REPLAY_READER_ACQUIRE_SLICE) {
            Ok(snapshot) => {
                ensure_not_interrupted(context)?;
                return Ok(ExactPublicationRead::Snapshot(snapshot));
            }
            Err(ExactSqlError::Busy) => {
                ensure_not_interrupted(context)?;
            }
            Err(_) => {
                ensure_not_interrupted(context)?;
                return handle
                    .begin_deferred()
                    .map(|transaction| ExactPublicationRead::Transaction(Some(transaction)))
                    .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure);
            }
        }
    }
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
    .map(|row| {
        Ok(GraphProjectionIdentityV1 {
            shard_id: request.shard_id.clone(),
            namespace: GraphNamespaceV1::new(text_at(&row, 0)?).map_err(corrupt)?,
            projection: GraphProjectionIdV1::new(text_at(&row, 1)?).map_err(corrupt)?,
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
    let Some(row) = rows.pop() else {
        return Ok(None);
    };
    let head = decode_verified_head(RawVerifiedHead {
        sequence: integer_at(&row, 0)?,
        recovered_digest: text_at(&row, 1)?,
        shard_id: text_at(&row, 2)?,
        namespace: text_at(&row, 3)?,
        projection: text_at(&row, 4)?,
        generation: text_at(&row, 5)?,
        idempotency_key: text_at(&row, 6)?,
        input_digest: text_at(&row, 7)?,
        dependency_generation_closure_digest: text_at(&row, 8)?,
        expected_recovered_digest: text_at(&row, 9)?,
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

pub(super) fn next_replay_metadata(
    transaction: &impl ExactQueryAuthority,
    encoded: &EncodedProjection,
    after: u64,
) -> GraphPublicationStoreResultV1<Option<(tracedecay_store::GraphPublicationSequenceV1, usize)>> {
    let after = sqlite_sequence_from_u64(after)?;
    let mut rows = query(
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
            "graph replay page metadata returned duplicate rows".to_owned(),
        ));
    }
    let Some(row) = rows.pop() else {
        return Ok(None);
    };
    let sequence = sequence_from_i64(integer_at(&row, 0)?)?;
    let payload_bytes = usize::try_from(integer_at(&row, 1)?).map_err(|_| {
        GraphPublicationStoreErrorV1::Corrupt(
            "graph replay payload length is negative or exceeds usize".to_owned(),
        )
    })?;
    Ok(Some((sequence, payload_bytes)))
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
    for row in rows {
        dependencies.push(GraphDependencyGenerationIdentityV1::new(
            GraphProjectionIdentityV1 {
                shard_id: serde_json::from_str::<StoreShardIdV1>(&text_at(&row, 1)?)
                    .map_err(corrupt)?,
                namespace: GraphNamespaceV1::new(text_at(&row, 2)?).map_err(corrupt)?,
                projection: GraphProjectionIdV1::new(text_at(&row, 3)?).map_err(corrupt)?,
            },
            GraphGenerationIdV1::new(text_at(&row, 4)?).map_err(corrupt)?,
        ));
    }
    Ok(dependencies)
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
    rows.pop().map(|row| blob_at(&row, 0)).transpose()
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
    decode_replay(
        RawReplay {
            sequence,
            shard_id: text_at(&row, 1)?,
            namespace: text_at(&row, 2)?,
            projection: text_at(&row, 3)?,
            generation: text_at(&row, 4)?,
            idempotency_key: text_at(&row, 5)?,
            input_digest: text_at(&row, 6)?,
            dependency_generation_closure_digest: text_at(&row, 7)?,
            direct_dependency_bytes: integer_at(&row, 8)?,
            expected_prior_head: optional_text_at(&row, 9)?,
            expected_recovered_digest: text_at(&row, 10)?,
            canonical_replay_source_digest: text_at(&row, 11)?,
            canonical_replay_source: blob_at(&row, 12)?,
        },
        read_dependencies(transaction, sequence, false)?,
    )
}

pub(super) fn decode_tombstone_row(
    transaction: &impl ExactQueryAuthority,
    row: ExactSqlRow,
) -> GraphPublicationStoreResultV1<GraphPublicationReplayTombstoneV1> {
    let sequence = integer_at(&row, 0)?;
    decode_tombstone(
        RawReplayTombstone {
            sequence,
            shard_id: text_at(&row, 1)?,
            namespace: text_at(&row, 2)?,
            projection: text_at(&row, 3)?,
            generation: text_at(&row, 4)?,
            idempotency_key: text_at(&row, 5)?,
            input_digest: text_at(&row, 6)?,
            dependency_generation_closure_digest: text_at(&row, 7)?,
            direct_dependency_bytes: integer_at(&row, 8)?,
            expected_prior_head: optional_text_at(&row, 9)?,
            expected_recovered_digest: text_at(&row, 10)?,
            canonical_replay_source_digest: text_at(&row, 11)?,
            canonical_replay_source: read_retained_source(transaction, sequence)?,
        },
        read_dependencies(transaction, sequence, true)?,
    )
}

pub(super) fn decode_metadata_row(
    row: ExactSqlRow,
) -> GraphPublicationStoreResultV1<ReplayMetadata> {
    decode_replay_metadata(RawReplayMetadata {
        sequence: integer_at(&row, 0)?,
        shard_id: text_at(&row, 1)?,
        namespace: text_at(&row, 2)?,
        projection: text_at(&row, 3)?,
        generation: text_at(&row, 4)?,
        idempotency_key: text_at(&row, 5)?,
        input_digest: text_at(&row, 6)?,
        dependency_generation_closure_digest: text_at(&row, 7)?,
        expected_prior_head: optional_text_at(&row, 8)?,
        expected_recovered_digest: text_at(&row, 9)?,
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

pub(super) fn text_at(row: &ExactSqlRow, index: usize) -> GraphPublicationStoreResultV1<String> {
    match value_at(row, index)? {
        ExactSqlValue::Text(value) => Ok(value.clone()),
        _ => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication text column has the wrong type".to_owned(),
        )),
    }
}

pub(super) fn optional_text_at(
    row: &ExactSqlRow,
    index: usize,
) -> GraphPublicationStoreResultV1<Option<String>> {
    match value_at(row, index)? {
        ExactSqlValue::Null => Ok(None),
        ExactSqlValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication optional text column has the wrong type".to_owned(),
        )),
    }
}

pub(super) fn blob_at(row: &ExactSqlRow, index: usize) -> GraphPublicationStoreResultV1<Vec<u8>> {
    match value_at(row, index)? {
        ExactSqlValue::Blob(value) => Ok(value.clone()),
        _ => Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph publication blob column has the wrong type".to_owned(),
        )),
    }
}

pub(super) fn text(value: impl Into<String>) -> ExactSqlValue {
    ExactSqlValue::Text(value.into())
}

pub(super) fn optional_text(value: Option<String>) -> ExactSqlValue {
    value.map_or(ExactSqlValue::Null, ExactSqlValue::Text)
}
