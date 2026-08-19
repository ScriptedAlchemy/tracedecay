use std::sync::Arc;
use std::time::Duration;

use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationProjectionPageV1,
    GraphPublicationReplayCursorV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayPageRequestV1, GraphPublicationReplayPageV1,
    GraphPublicationReplayRecordV1, GraphPublicationReplayRetirementV1,
    GraphPublicationReplayTombstoneV1, GraphPublicationReplayV1,
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationRetiredCleanupPageV1,
    GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1, GraphPublicationStoreV1,
    GraphReplayAppendOutcomeV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1, MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1,
    MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
};

use crate::exact_sql::{
    ExactSqlHandle, ExactSqlReadSnapshot, ExactSqlRows, ExactSqlStatement, ExactSqlTransaction,
    ExactSqlValue,
};

use super::{
    EncodedProjection, begin_replay_retirement_commit, begin_retired_cleanup_finalize_commit,
    begin_verified_commit, encode_direct_dependency_generations, encode_optional_head,
    ensure_not_interrupted, sequence_from_i64, sequence_to_i64,
};

#[path = "support.rs"]
mod support;
use support::{
    begin, begin_read, commit, ensure_owner, ensure_shard_owner, execute,
    has_active_inbound_dependencies, insert_verified_dependencies, next_replay_metadata,
    next_retired_cleanup_metadata, optional_text, read_by_sequence, read_conflicts, read_exact,
    read_exact_metadata, read_exact_tombstone, read_first_conflict_sequence, read_head,
    read_pending, read_pending_sequence, read_projection_page, read_tombstone_by_sequence,
    read_tombstone_conflicts, rollback, rollback_error, text,
};

const REPLAY_COLUMNS: &str = "sequence, shard_id, namespace, projection, generation,
     idempotency_key, input_digest, dependency_generation_closure_digest,
     direct_dependency_bytes, expected_prior_head, expected_recovered_digest,
     canonical_replay_source_digest, canonical_replay_source";
const REPLAY_METADATA_COLUMNS: &str = "sequence, shard_id, namespace, projection, generation,
     idempotency_key, input_digest, dependency_generation_closure_digest,
     expected_prior_head, expected_recovered_digest";
const TOMBSTONE_COLUMNS: &str = "replay_sequence, shard_id, namespace, projection, generation,
     idempotency_key, input_digest, dependency_generation_closure_digest,
     direct_dependency_bytes, expected_prior_head, expected_recovered_digest,
     canonical_replay_source_digest";
const REPLAY_READER_ACQUIRE_SLICE: Duration = Duration::from_millis(10);

trait ExactQueryAuthority {
    fn exact_query(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ()>;
}

pub(super) enum ExactPublicationRead {
    Snapshot(ExactSqlReadSnapshot),
    Transaction(Option<ExactSqlTransaction>),
}

impl ExactQueryAuthority for ExactPublicationRead {
    fn exact_query(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ()> {
        match self {
            Self::Snapshot(snapshot) => snapshot.query(statement).map_err(|_| ()),
            Self::Transaction(Some(transaction)) => transaction.query(statement).map_err(|_| ()),
            Self::Transaction(None) => Err(()),
        }
    }
}

impl Drop for ExactPublicationRead {
    fn drop(&mut self) {
        if let Self::Transaction(transaction) = self
            && let Some(transaction) = transaction.take()
        {
            let _ = transaction.rollback();
        }
    }
}

impl ExactQueryAuthority for ExactSqlTransaction {
    fn exact_query(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ()> {
        self.query(statement).map_err(|_| ())
    }
}

impl ExactQueryAuthority for ExactSqlReadSnapshot {
    fn exact_query(&self, statement: ExactSqlStatement) -> Result<ExactSqlRows, ()> {
        self.query(statement).map_err(|_| ())
    }
}

/// Relational graph publication authority over one already-attached canonical
/// exact-SQL writer. The handle carries the owner shard's validated locator,
/// binding, and live write authority; no path is accepted or reopened here.
///
/// `()` represents only standalone repository ownership. Daemon-owned
/// databases must call `from_authorized_handle_with_guard` with their counted
/// client guard.
pub(crate) struct ExactSqlRetainedGuard<Guard = ()> {
    _guard: Guard,
}

type ErasedExactSqlRetainedGuard = ExactSqlRetainedGuard<Arc<dyn Send + Sync>>;

impl<Guard> ExactSqlRetainedGuard<Guard>
where
    Guard: Send + Sync + 'static,
{
    pub(crate) fn new(guard: Guard) -> Self {
        Self { _guard: guard }
    }

    fn erase(self) -> ErasedExactSqlRetainedGuard {
        ExactSqlRetainedGuard {
            _guard: Arc::new(self._guard),
        }
    }
}

#[derive(Clone)]
pub struct GraphPublicationExactSqlStorage {
    handle: ExactSqlHandle,
    _retained_guard: Arc<ErasedExactSqlRetainedGuard>,
}

impl GraphPublicationExactSqlStorage {
    pub fn from_authorized_handle(handle: ExactSqlHandle) -> GraphPublicationStoreResultV1<Self> {
        Self::from_authorized_handle_with_guard(handle, ())
    }

    pub fn from_authorized_handle_with_guard<Guard>(
        handle: ExactSqlHandle,
        guard: Guard,
    ) -> GraphPublicationStoreResultV1<Self>
    where
        Guard: Send + Sync + 'static,
    {
        if !matches!(
            &handle.binding().shard_id.scope,
            tracedecay_store::StoreShardScopeV1::Project { .. }
                | tracedecay_store::StoreShardScopeV1::ProfileMemory
        ) {
            return Err(GraphPublicationStoreErrorV1::InvalidRequest(
                tracedecay_store::StorageRuntimeContractErrorV1::OperationScopeMismatch {
                    operation: "attach graph publication exact SQL storage",
                    shard_family: "non-graph-publication",
                },
            ));
        }
        Ok(Self {
            handle,
            _retained_guard: Arc::new(ExactSqlRetainedGuard::new(guard).erase()),
        })
    }
}

pub(super) fn authoritative_verified_head_in_transaction(
    transaction: &ExactSqlTransaction,
    projection: &GraphProjectionIdentityV1,
) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
    let encoded = EncodedProjection::new(projection)?;
    read_head(transaction, &encoded)
}

pub(super) fn active_replay_in_transaction(
    transaction: &ExactSqlTransaction,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
    let encoded = EncodedProjection::new(&key.projection)?;
    read_exact(transaction, &encoded, key)
}

pub(crate) fn retire_replay_in_transaction(
    transaction: &ExactSqlTransaction,
    request: &GraphPublicationReplayRetirementV1,
) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
    request.validate()?;
    let encoded = EncodedProjection::new(&request.key.projection)?;
    let retired_conflicts = read_tombstone_conflicts(transaction, &encoded, &request.key)?;
    if let Some(retired) = retired_conflicts
        .iter()
        .find(|retired| retired.key == request.key)
    {
        return Ok(if retired.retirement() == *request {
            GraphReplayRetirementOutcomeV1::ExactReplay(retired.clone())
        } else {
            GraphReplayRetirementOutcomeV1::Conflict
        });
    }
    if !retired_conflicts.is_empty() {
        return Ok(GraphReplayRetirementOutcomeV1::Conflict);
    }
    let conflicts = read_conflicts(transaction, &encoded, &request.key)?;
    let Some(replay) = conflicts
        .iter()
        .find(|replay| replay.publication.key == request.key)
        .cloned()
    else {
        return Ok(if conflicts.is_empty() {
            GraphReplayRetirementOutcomeV1::Missing
        } else {
            GraphReplayRetirementOutcomeV1::Conflict
        });
    };
    if replay.publication.input_digest != request.input_digest
        || replay.publication.dependency_generation_closure_digest
            != request.dependency_generation_closure_digest
        || replay.publication.direct_dependency_generations != request.direct_dependency_generations
        || replay.publication.expected_prior_head != request.expected_prior_head
        || replay.publication.expected_recovered_digest != request.expected_recovered_digest
        || replay.publication.canonical_replay_source_digest
            != request.canonical_replay_source_digest
    {
        return Ok(GraphReplayRetirementOutcomeV1::Conflict);
    }
    let head = read_head(transaction, &encoded)?;
    if let Some(head) = head
        .as_ref()
        .filter(|head| head.sequence == replay.sequence)
    {
        return Ok(GraphReplayRetirementOutcomeV1::CurrentVerifiedHead { head: head.clone() });
    }
    if let Some(pending) = read_pending(transaction, &encoded, head.as_ref())?
        .filter(|pending| pending.sequence == replay.sequence)
    {
        return Ok(GraphReplayRetirementOutcomeV1::PendingReplay { pending });
    }
    if head
        .as_ref()
        .is_none_or(|head| replay.sequence >= head.sequence)
    {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay retirement target is neither historical nor pending".to_owned(),
        ));
    }
    if has_active_inbound_dependencies(transaction, replay.sequence)? {
        return Ok(GraphReplayRetirementOutcomeV1::Conflict);
    }
    let tombstone = GraphPublicationReplayTombstoneV1::new(
        replay.sequence,
        request.clone(),
        Some(replay.publication.canonical_replay_source.clone()),
    )?;
    execute(
        transaction,
        "INSERT INTO graph_publication_replay_tombstones_v1 (
            replay_sequence, shard_id, namespace, projection, generation,
            idempotency_key, input_digest,
            dependency_generation_closure_digest,
            direct_dependency_bytes, expected_prior_head,
            expected_recovered_digest, canonical_replay_source_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        vec![
            ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?),
            text(encoded.shard_id),
            text(encoded.namespace),
            text(encoded.projection),
            text(request.key.generation.as_str()),
            text(request.key.idempotency_key.as_str()),
            text(request.input_digest.as_str()),
            text(request.dependency_generation_closure_digest.as_str()),
            ExactSqlValue::Integer(
                i64::try_from(
                    encode_direct_dependency_generations(&request.direct_dependency_generations)?
                        .len(),
                )
                .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?,
            ),
            optional_text(encode_optional_head(request.expected_prior_head.as_ref())?),
            text(request.expected_recovered_digest.as_str()),
            text(request.canonical_replay_source_digest.as_str()),
        ],
    )?;
    execute(
        transaction,
        "INSERT INTO graph_publication_replay_tombstone_dependencies_v1 (
            tombstone_replay_sequence, ordinal, shard_id, namespace,
            projection, generation
         )
         SELECT owner_replay_sequence, ordinal, shard_id, namespace,
                projection, generation
         FROM graph_publication_replay_dependencies_v1
         WHERE owner_replay_sequence = ?1",
        vec![ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?)],
    )?;
    execute(
        transaction,
        "DELETE FROM graph_publication_replay_dependencies_v1
         WHERE owner_replay_sequence = ?1",
        vec![ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?)],
    )?;
    Ok(GraphReplayRetirementOutcomeV1::Retired(tombstone))
}

pub(crate) fn append_replay_in_transaction(
    transaction: &ExactSqlTransaction,
    publication: &GraphPublicationReplayV1,
) -> GraphPublicationStoreResultV1<GraphReplayAppendOutcomeV1> {
    publication.validate()?;
    let encoded = EncodedProjection::new(&publication.key.projection)?;
    if let Some(retired) = read_tombstone_conflicts(transaction, &encoded, &publication.key)?
        .into_iter()
        .next()
    {
        return Ok(GraphReplayAppendOutcomeV1::RetiredReplayConflict { retired });
    }
    let conflicts = read_conflicts(transaction, &encoded, &publication.key)?;
    if let Some(exact) = conflicts
        .iter()
        .find(|record| record.publication.key == publication.key)
    {
        return Ok(if exact.publication != *publication {
            GraphReplayAppendOutcomeV1::Conflict {
                existing: exact.clone(),
            }
        } else if let Some(head) =
            read_head(transaction, &encoded)?.filter(|head| head.sequence >= exact.sequence)
        {
            let receipt = GraphVerifiedHeadV1::from_replay(
                exact,
                exact.publication.expected_recovered_digest.clone(),
            )?;
            if head.sequence == exact.sequence && head != receipt {
                return Err(GraphPublicationStoreErrorV1::Corrupt(
                    "verified graph head does not match its exact replay".to_owned(),
                ));
            }
            GraphReplayAppendOutcomeV1::ExactVerifiedReplay {
                replay: exact.clone(),
                receipt: Box::new(receipt),
            }
        } else {
            GraphReplayAppendOutcomeV1::ExactReplay(exact.clone())
        });
    }
    if let Some(existing) = conflicts.into_iter().next() {
        return Ok(GraphReplayAppendOutcomeV1::Conflict { existing });
    }
    let actual = read_head(transaction, &encoded)?;
    if actual != publication.expected_prior_head {
        return Ok(GraphReplayAppendOutcomeV1::VerifiedHeadConflict { actual });
    }
    if let Some(pending) = read_pending(transaction, &encoded, actual.as_ref())? {
        return Ok(GraphReplayAppendOutcomeV1::PendingReplayConflict { pending });
    }
    let inserted = execute(
        transaction,
        "INSERT INTO graph_publication_replay_v1 (
            shard_id, namespace, projection, generation, idempotency_key,
            input_digest, dependency_generation_closure_digest,
            direct_dependency_bytes, expected_prior_head,
            expected_recovered_digest, canonical_replay_source_digest,
            canonical_replay_source
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        vec![
            text(encoded.shard_id),
            text(encoded.namespace),
            text(encoded.projection),
            text(publication.key.generation.as_str()),
            text(publication.key.idempotency_key.as_str()),
            text(publication.input_digest.as_str()),
            text(publication.dependency_generation_closure_digest.as_str()),
            ExactSqlValue::Integer(
                i64::try_from(
                    encode_direct_dependency_generations(
                        &publication.direct_dependency_generations,
                    )?
                    .len(),
                )
                .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?,
            ),
            optional_text(encode_optional_head(
                publication.expected_prior_head.as_ref(),
            )?),
            text(publication.expected_recovered_digest.as_str()),
            text(publication.canonical_replay_source_digest.as_str()),
            ExactSqlValue::Blob(publication.canonical_replay_source.clone()),
        ],
    )?;
    let record = GraphPublicationReplayRecordV1::new(
        sequence_from_i64(inserted.last_insert_rowid)?,
        publication.clone(),
    )?;
    insert_verified_dependencies(transaction, &record)?;
    Ok(GraphReplayAppendOutcomeV1::Appended(record))
}

impl GraphPublicationStoreV1 for GraphPublicationExactSqlStorage {
    fn append_replay(
        &mut self,
        publication: &GraphPublicationReplayV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphReplayAppendOutcomeV1> {
        publication.validate()?;
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, &publication.key.projection)?;
        let transaction = begin(&self.handle, context)?;
        let outcome = append_replay_in_transaction(&transaction, publication)?;
        ensure_not_interrupted(context)?;
        if matches!(outcome, GraphReplayAppendOutcomeV1::Appended(_)) {
            if let Err(error) = begin_verified_commit(context) {
                return rollback_error(transaction, error);
            }
            commit(transaction)?;
            Ok(outcome)
        } else {
            rollback(transaction, outcome)
        }
    }

    fn pending_replay(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, projection)?;
        let encoded = EncodedProjection::new(projection)?;
        let snapshot = begin_read(&self.handle, context)?;
        let actual = read_head(&snapshot, &encoded)?;
        let pending = read_pending(&snapshot, &encoded, actual.as_ref())?;
        ensure_not_interrupted(context)?;
        Ok(pending)
    }

    fn replay(
        &mut self,
        key: &GraphPublicationKeyV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayLookupV1> {
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, &key.projection)?;
        let encoded = EncodedProjection::new(&key.projection)?;
        let snapshot = begin_read(&self.handle, context)?;
        let active = read_exact(&snapshot, &encoded, key)?;
        let retired = read_exact_tombstone(&snapshot, &encoded, key)?;
        let replay = match (active, retired) {
            (_, Some(retired)) => GraphPublicationReplayLookupV1::Retired(retired),
            (Some(replay), None) => GraphPublicationReplayLookupV1::Active(replay),
            (None, None) => GraphPublicationReplayLookupV1::Missing,
        };
        ensure_not_interrupted(context)?;
        Ok(replay)
    }

    fn replay_page(
        &mut self,
        request: &GraphPublicationReplayPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayPageV1> {
        request.validate()?;
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, &request.projection)?;
        let encoded = EncodedProjection::new(&request.projection)?;
        let snapshot = begin_read(&self.handle, context)?;
        let mut after = request
            .after
            .as_ref()
            .map_or(0, |cursor| cursor.sequence.get());
        let mut records: Vec<GraphPublicationReplayRecordV1> =
            Vec::with_capacity(usize::from(request.max_records));
        let mut payload_bytes = 0_usize;
        let mut continuation = None;

        while records.len() < usize::from(request.max_records) {
            ensure_not_interrupted(context)?;
            let Some((sequence, next_payload_bytes)) =
                next_replay_metadata(&snapshot, &encoded, after)?
            else {
                break;
            };
            if next_payload_bytes > MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 {
                return Err(GraphPublicationStoreErrorV1::Corrupt(
                    "graph replay payload exceeds its canonical storage bound".to_owned(),
                ));
            }
            let next_page_bytes =
                payload_bytes
                    .checked_add(next_payload_bytes)
                    .ok_or_else(|| {
                        GraphPublicationStoreErrorV1::Corrupt(
                            "graph replay page payload size overflowed".to_owned(),
                        )
                    })?;
            if !records.is_empty() && next_page_bytes > MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1 {
                continuation = records
                    .last()
                    .map(|record| {
                        GraphPublicationReplayCursorV1::new(
                            request.projection.clone(),
                            record.sequence,
                        )
                    })
                    .transpose()?;
                break;
            }
            let replay =
                read_by_sequence(&snapshot, sequence_to_i64(sequence)?)?.ok_or_else(|| {
                    GraphPublicationStoreErrorV1::Corrupt(
                        "enumerated graph replay disappeared in its read transaction".to_owned(),
                    )
                })?;
            let actual = EncodedProjection::new(&replay.publication.key.projection)?;
            if actual.shard_id != encoded.shard_id
                || actual.namespace != encoded.namespace
                || actual.projection != encoded.projection
            {
                return Err(GraphPublicationStoreErrorV1::Corrupt(
                    "enumerated graph replay escaped its projection".to_owned(),
                ));
            }
            payload_bytes = next_page_bytes;
            after = sequence.get();
            records.push(replay);
        }

        if continuation.is_none()
            && !records.is_empty()
            && next_replay_metadata(&snapshot, &encoded, after)?.is_some()
        {
            continuation = records
                .last()
                .map(|record| {
                    GraphPublicationReplayCursorV1::new(request.projection.clone(), record.sequence)
                })
                .transpose()?;
        }
        ensure_not_interrupted(context)?;
        let page = GraphPublicationReplayPageV1::new(records, continuation)?;
        Ok(page)
    }

    fn projection_page(
        &mut self,
        request: &GraphPublicationProjectionPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationProjectionPageV1> {
        request.validate()?;
        ensure_not_interrupted(context)?;
        ensure_shard_owner(&self.handle, &request.shard_id)?;
        let snapshot = begin_read(&self.handle, context)?;
        let mut projections = read_projection_page(&snapshot, request)?;
        let continuation = if projections.len() > usize::from(request.max_records) {
            projections.truncate(usize::from(request.max_records));
            projections.last().cloned()
        } else {
            None
        };
        ensure_not_interrupted(context)?;
        GraphPublicationProjectionPageV1::new(projections, continuation)
            .map_err(GraphPublicationStoreErrorV1::from)
    }

    fn retire_replay(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
        request.validate()?;
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, &request.key.projection)?;
        let encoded = EncodedProjection::new(&request.key.projection)?;
        let transaction = begin(&self.handle, context)?;
        let retired_conflicts = read_tombstone_conflicts(&transaction, &encoded, &request.key)?;
        if let Some(retired) = retired_conflicts
            .iter()
            .find(|retired| retired.key == request.key)
        {
            let outcome = if retired.retirement() == *request {
                GraphReplayRetirementOutcomeV1::ExactReplay(retired.clone())
            } else {
                GraphReplayRetirementOutcomeV1::Conflict
            };
            ensure_not_interrupted(context)?;
            return rollback(transaction, outcome);
        }
        if !retired_conflicts.is_empty() {
            ensure_not_interrupted(context)?;
            return rollback(transaction, GraphReplayRetirementOutcomeV1::Conflict);
        }
        let conflicts = read_conflicts(&transaction, &encoded, &request.key)?;
        let Some(replay) = conflicts
            .iter()
            .find(|replay| replay.publication.key == request.key)
            .cloned()
        else {
            let outcome = if conflicts.is_empty() {
                GraphReplayRetirementOutcomeV1::Missing
            } else {
                GraphReplayRetirementOutcomeV1::Conflict
            };
            ensure_not_interrupted(context)?;
            return rollback(transaction, outcome);
        };
        if replay.publication.input_digest != request.input_digest
            || replay.publication.dependency_generation_closure_digest
                != request.dependency_generation_closure_digest
            || replay.publication.direct_dependency_generations
                != request.direct_dependency_generations
            || replay.publication.expected_prior_head != request.expected_prior_head
            || replay.publication.expected_recovered_digest != request.expected_recovered_digest
            || replay.publication.canonical_replay_source_digest
                != request.canonical_replay_source_digest
        {
            ensure_not_interrupted(context)?;
            return rollback(transaction, GraphReplayRetirementOutcomeV1::Conflict);
        }
        let head = read_head(&transaction, &encoded)?;
        if let Some(head) = head
            .as_ref()
            .filter(|head| head.sequence == replay.sequence)
        {
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphReplayRetirementOutcomeV1::CurrentVerifiedHead { head: head.clone() },
            );
        }
        if let Some(pending) = read_pending(&transaction, &encoded, head.as_ref())?
            .filter(|pending| pending.sequence == replay.sequence)
        {
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphReplayRetirementOutcomeV1::PendingReplay { pending },
            );
        }
        if head
            .as_ref()
            .is_none_or(|head| replay.sequence >= head.sequence)
        {
            return rollback_error(
                transaction,
                GraphPublicationStoreErrorV1::Corrupt(
                    "graph replay retirement target is neither historical nor pending".to_owned(),
                ),
            );
        }
        if has_active_inbound_dependencies(&transaction, replay.sequence)? {
            ensure_not_interrupted(context)?;
            return rollback(transaction, GraphReplayRetirementOutcomeV1::Conflict);
        }
        let tombstone = GraphPublicationReplayTombstoneV1::new(
            replay.sequence,
            request.clone(),
            Some(replay.publication.canonical_replay_source.clone()),
        )?;
        execute(
            &transaction,
            "INSERT INTO graph_publication_replay_tombstones_v1 (
                replay_sequence, shard_id, namespace, projection, generation,
                idempotency_key, input_digest,
                dependency_generation_closure_digest,
                direct_dependency_bytes, expected_prior_head,
                expected_recovered_digest, canonical_replay_source_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            vec![
                ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?),
                text(encoded.shard_id),
                text(encoded.namespace),
                text(encoded.projection),
                text(request.key.generation.as_str()),
                text(request.key.idempotency_key.as_str()),
                text(request.input_digest.as_str()),
                text(request.dependency_generation_closure_digest.as_str()),
                ExactSqlValue::Integer(
                    i64::try_from(
                        encode_direct_dependency_generations(
                            &request.direct_dependency_generations,
                        )?
                        .len(),
                    )
                    .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?,
                ),
                optional_text(encode_optional_head(request.expected_prior_head.as_ref())?),
                text(request.expected_recovered_digest.as_str()),
                text(request.canonical_replay_source_digest.as_str()),
            ],
        )?;
        execute(
            &transaction,
            "INSERT INTO graph_publication_replay_tombstone_dependencies_v1 (
                tombstone_replay_sequence, ordinal, shard_id, namespace,
                projection, generation
             )
             SELECT owner_replay_sequence, ordinal, shard_id, namespace,
                    projection, generation
             FROM graph_publication_replay_dependencies_v1
             WHERE owner_replay_sequence = ?1",
            vec![ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?)],
        )?;
        execute(
            &transaction,
            "DELETE FROM graph_publication_replay_dependencies_v1
             WHERE owner_replay_sequence = ?1",
            vec![ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?)],
        )?;
        if let Err(error) = ensure_not_interrupted(context) {
            return rollback_error(transaction, error);
        }
        if let Err(error) = begin_replay_retirement_commit(context) {
            return rollback_error(transaction, error);
        }
        commit(transaction)?;
        Ok(GraphReplayRetirementOutcomeV1::Retired(tombstone))
    }

    fn retired_cleanup_page(
        &mut self,
        request: &GraphPublicationRetiredCleanupPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationRetiredCleanupPageV1> {
        request.validate()?;
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, &request.projection)?;
        let encoded = EncodedProjection::new(&request.projection)?;
        let snapshot = begin_read(&self.handle, context)?;
        let mut after = request
            .after
            .as_ref()
            .map_or(0, |cursor| cursor.sequence.get());
        let mut records = Vec::with_capacity(usize::from(request.max_records));
        let mut payload_bytes = 0_usize;
        let mut continuation = None;
        while records.len() < usize::from(request.max_records) {
            ensure_not_interrupted(context)?;
            let Some((sequence, record_bytes)) =
                next_retired_cleanup_metadata(&snapshot, &encoded, after)?
            else {
                break;
            };
            if record_bytes > MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 {
                return Err(GraphPublicationStoreErrorV1::Corrupt(
                    "retired cleanup payload exceeds its canonical storage bound".to_owned(),
                ));
            }
            let next_bytes = payload_bytes.checked_add(record_bytes).ok_or_else(|| {
                GraphPublicationStoreErrorV1::Corrupt(
                    "retired cleanup page payload size overflowed".to_owned(),
                )
            })?;
            if !records.is_empty() && next_bytes > MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1 {
                continuation = records
                    .last()
                    .map(|record: &GraphPublicationReplayTombstoneV1| {
                        GraphPublicationReplayCursorV1::new(
                            request.projection.clone(),
                            record.sequence,
                        )
                    })
                    .transpose()?;
                break;
            }
            let tombstone = read_tombstone_by_sequence(&snapshot, sequence_to_i64(sequence)?)?
                .ok_or_else(|| {
                    GraphPublicationStoreErrorV1::Corrupt(
                        "enumerated retired cleanup replay disappeared in its read transaction"
                            .to_owned(),
                    )
                })?;
            if tombstone.key.projection != request.projection {
                return Err(GraphPublicationStoreErrorV1::Corrupt(
                    "enumerated retired cleanup replay escaped its projection".to_owned(),
                ));
            }
            payload_bytes = next_bytes;
            after = sequence.get();
            records.push(tombstone);
        }
        if continuation.is_none() && !records.is_empty() {
            ensure_not_interrupted(context)?;
        }
        if continuation.is_none()
            && !records.is_empty()
            && next_retired_cleanup_metadata(&snapshot, &encoded, after)?.is_some()
        {
            continuation = records
                .last()
                .map(|record| {
                    GraphPublicationReplayCursorV1::new(request.projection.clone(), record.sequence)
                })
                .transpose()?;
        }
        ensure_not_interrupted(context)?;
        GraphPublicationRetiredCleanupPageV1::new(records, continuation)
            .map_err(GraphPublicationStoreErrorV1::from)
    }

    fn finalize_retired_replay_cleanup(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphRetiredReplayCleanupFinalizeOutcomeV1> {
        request.validate()?;
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, &request.key.projection)?;
        let encoded = EncodedProjection::new(&request.key.projection)?;
        let transaction = begin(&self.handle, context)?;
        let Some(tombstone) = read_exact_tombstone(&transaction, &encoded, &request.key)? else {
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphRetiredReplayCleanupFinalizeOutcomeV1::Missing,
            );
        };
        if tombstone.retirement() != *request {
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphRetiredReplayCleanupFinalizeOutcomeV1::Conflict,
            );
        }
        if tombstone.canonical_replay_source.is_none() {
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphRetiredReplayCleanupFinalizeOutcomeV1::ExactReplay(tombstone),
            );
        }
        let changed = execute(
            &transaction,
            "DELETE FROM graph_publication_replay_v1 WHERE sequence = ?1",
            vec![ExactSqlValue::Integer(sequence_to_i64(tombstone.sequence)?)],
        )?
        .changed_rows;
        if changed != 1 {
            return rollback_error(
                transaction,
                GraphPublicationStoreErrorV1::Corrupt(
                    "retired graph cleanup source disappeared during finalization".to_owned(),
                ),
            );
        }
        if let Err(error) = ensure_not_interrupted(context) {
            return rollback_error(transaction, error);
        }
        if let Err(error) = begin_retired_cleanup_finalize_commit(context) {
            return rollback_error(transaction, error);
        }
        commit(transaction)?;
        let finalized =
            GraphPublicationReplayTombstoneV1::new(tombstone.sequence, request.clone(), None)?;
        Ok(GraphRetiredReplayCleanupFinalizeOutcomeV1::Finalized(
            finalized,
        ))
    }

    fn verified_head(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, projection)?;
        let encoded = EncodedProjection::new(projection)?;
        let snapshot = begin_read(&self.handle, context)?;
        let head = read_head(&snapshot, &encoded)?;
        ensure_not_interrupted(context)?;
        Ok(head)
    }

    fn compare_and_swap_verified_head(
        &mut self,
        request: &GraphVerifiedHeadCompareAndSwapV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphVerifiedHeadCasOutcomeV1> {
        request.validate()?;
        ensure_not_interrupted(context)?;
        ensure_owner(&self.handle, &request.publication_key.projection)?;
        let encoded = EncodedProjection::new(&request.publication_key.projection)?;
        let transaction = begin(&self.handle, context)?;
        let Some(replay) = read_exact_metadata(&transaction, &encoded, &request.publication_key)?
        else {
            if let Some(retired) =
                read_exact_tombstone(&transaction, &encoded, &request.publication_key)?
            {
                ensure_not_interrupted(context)?;
                return rollback(
                    transaction,
                    GraphVerifiedHeadCasOutcomeV1::RetiredReplay(retired),
                );
            }
            let collision =
                read_first_conflict_sequence(&transaction, &encoded, &request.publication_key)?
                    .map(|sequence| read_by_sequence(&transaction, sequence))
                    .transpose()?
                    .flatten();
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                collision.map_or(GraphVerifiedHeadCasOutcomeV1::MissingReplay, |existing| {
                    GraphVerifiedHeadCasOutcomeV1::ReplayInputConflict { existing }
                }),
            );
        };
        if replay.input_digest != request.input_digest
            || replay.dependency_generation_closure_digest
                != request.dependency_generation_closure_digest
            || replay.expected_prior_head != request.expected_prior_head
        {
            let existing = read_by_sequence(&transaction, sequence_to_i64(replay.sequence)?)?
                .ok_or_else(|| {
                    GraphPublicationStoreErrorV1::Corrupt(
                        "graph replay metadata references a missing source".to_owned(),
                    )
                })?;
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphVerifiedHeadCasOutcomeV1::ReplayInputConflict { existing },
            );
        }
        if replay.expected_recovered_digest != request.recovered_digest {
            let expected = replay.expected_recovered_digest.clone();
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphVerifiedHeadCasOutcomeV1::RecoveredDigestMismatch {
                    expected,
                    actual: request.recovered_digest.clone(),
                },
            );
        }
        let actual = read_head(&transaction, &encoded)?;
        let next = replay.verified_head(request.recovered_digest.clone())?;
        if actual
            .as_ref()
            .is_some_and(|head| head.sequence == replay.sequence)
        {
            if actual.as_ref() != Some(&next) {
                return rollback_error(
                    transaction,
                    GraphPublicationStoreErrorV1::Corrupt(
                        "verified graph head does not match its immutable replay".to_owned(),
                    ),
                );
            }
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphVerifiedHeadCasOutcomeV1::ExactReplay(next),
            );
        }
        if actual != request.expected_prior_head {
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphVerifiedHeadCasOutcomeV1::Conflict { actual },
            );
        }
        if read_pending_sequence(&transaction, &encoded, actual.as_ref())? != Some(replay.sequence)
        {
            ensure_not_interrupted(context)?;
            return rollback(
                transaction,
                GraphVerifiedHeadCasOutcomeV1::Conflict { actual },
            );
        }
        match actual {
            None => {
                execute(
                    &transaction,
                    "INSERT INTO graph_verified_heads_v1 (
                        shard_id, namespace, projection, replay_sequence, recovered_digest
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    vec![
                        text(encoded.shard_id),
                        text(encoded.namespace),
                        text(encoded.projection),
                        ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?),
                        text(request.recovered_digest.as_str()),
                    ],
                )?;
            }
            Some(prior) => {
                let changed = execute(
                    &transaction,
                    "UPDATE graph_verified_heads_v1
                     SET replay_sequence = ?4, recovered_digest = ?5
                     WHERE shard_id = ?1 AND namespace = ?2 AND projection = ?3
                       AND replay_sequence = ?6",
                    vec![
                        text(encoded.shard_id),
                        text(encoded.namespace),
                        text(encoded.projection),
                        ExactSqlValue::Integer(sequence_to_i64(replay.sequence)?),
                        text(request.recovered_digest.as_str()),
                        ExactSqlValue::Integer(sequence_to_i64(prior.sequence)?),
                    ],
                )?
                .changed_rows;
                if changed != 1 {
                    return rollback_error(
                        transaction,
                        GraphPublicationStoreErrorV1::Corrupt(
                            "verified-head CAS lost its immediate transaction authority".to_owned(),
                        ),
                    );
                }
            }
        }
        if let Err(error) = ensure_not_interrupted(context) {
            return rollback_error(transaction, error);
        }
        if let Err(error) = begin_verified_commit(context) {
            return rollback_error(transaction, error);
        }
        commit(transaction)?;
        Ok(GraphVerifiedHeadCasOutcomeV1::Advanced(next))
    }
}
