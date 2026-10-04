use std::collections::BTreeSet;

use tracedecay_runtime_core::db::engine::params;
use tracedecay_store::{SessionStoreError, SessionStoreResult};
use tracedecay_temporal_query::execution::ExecutionControl;
use tracedecay_temporal_query::execution::TemporalPortError;

use super::projection::{base_source_frontier, canonical_parent_message_resolver};
use super::query::{ACTIVATE_OPERATION, now_micros, storage, storage_message};
use super::relation_projection::candidate_session_relation_projection;
use super::relation_receipts::{record_relation_receipt, write_relation_projection};
use super::relations::{SessionRelationError, SessionRelationProjection};
use super::store::execution_control_graph_cancellation;
use crate::handle::{SessionTemporalRegisteredDb, SessionTemporalWriteTxn};

const MAX_REBUILD_RELATION_PROJECTION_ITEMS: usize = 100_000;

/// Leaves the relation receipt pending. The caller acknowledges it in the
/// transaction that activates the generation.
#[tracing::instrument(name = "session_temporal.rebuild.relations", level = "trace", skip_all)]
pub(super) async fn rebuild_candidate_session_relations(
    database: &impl SessionTemporalRegisteredDb,
    session_id: &tracedecay_domain::SessionId,
    generation: tracedecay_domain::SessionProjectionGenerationV1,
    control: &ExecutionControl,
    operation: &'static str,
) -> SessionStoreResult<SessionRelationProjection> {
    checkpoint_relation_rebuild_control(control)?;
    let (scope, relation_store) = database
        .session_relation_store()
        .map_err(|error| storage(operation, error))?;
    let snapshot = database
        .read_snapshot()
        .await
        .map_err(|error| storage(operation, error))?;
    let reconstruction_cancellation = execution_control_graph_cancellation(control);
    checkpoint_relation_rebuild_control(control)?;
    let reconstructed = candidate_session_relation_projection(
        &snapshot,
        &scope,
        &relation_store,
        session_id,
        generation,
        MAX_REBUILD_RELATION_PROJECTION_ITEMS,
        MAX_REBUILD_RELATION_PROJECTION_ITEMS,
        reconstruction_cancellation,
    )
    .await;
    checkpoint_relation_rebuild_control(control)?;
    let reconstructed = reconstructed?;
    drop(snapshot);

    checkpoint_relation_rebuild_control(control)?;
    let receipt = {
        use tracing::Instrument as _;
        {
            database
                .begin_write_transaction()
                .instrument(tracing::trace_span!("session_temporal.txn.begin"))
                .await
                .map_err(|error| storage(operation, error))?
        }
    };
    record_relation_receipt(&receipt, &reconstructed, now_micros(operation)?.0).await?;
    {
        use tracing::Instrument as _;
        {
            receipt
                .commit()
                .instrument(tracing::trace_span!("session_temporal.txn.commit"))
                .await
                .map_err(|error| storage(operation, error))?
        }
    };
    checkpoint_relation_rebuild_control(control)?;

    let apply_cancellation = execution_control_graph_cancellation(control);
    checkpoint_relation_rebuild_control(control)?;
    let applied = write_relation_projection(database, &reconstructed, apply_cancellation).await;
    checkpoint_relation_rebuild_control(control)?;
    applied?;

    let load_cancellation = execution_control_graph_cancellation(control);
    checkpoint_relation_rebuild_control(control)?;
    let loaded = relation_store.load_projection(
        &scope,
        session_id,
        generation.value(),
        MAX_REBUILD_RELATION_PROJECTION_ITEMS,
        MAX_REBUILD_RELATION_PROJECTION_ITEMS,
        load_cancellation,
    );
    checkpoint_relation_rebuild_control(control)?;
    let loaded = loaded.map_err(|error| map_relation_rebuild_error(operation, error))?;
    if loaded != reconstructed {
        return Err(storage_message(
            operation,
            "native session relation graph did not preserve the canonical reconstruction",
        ));
    }
    Ok(loaded)
}

pub(super) fn checkpoint_relation_rebuild_control(
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    control
        .checkpoint()
        .map_err(map_relation_rebuild_control_error)
}

fn map_relation_rebuild_control_error(error: TemporalPortError) -> SessionStoreError {
    match error {
        TemporalPortError::Cancelled => SessionStoreError::Cancelled,
        TemporalPortError::DeadlineExceeded => SessionStoreError::DeadlineExceeded,
        TemporalPortError::BudgetExceeded { resource, .. } => {
            SessionStoreError::BudgetExceeded { resource }
        }
        _ => SessionStoreError::InvalidStateTransition {
            context: "session relation reconstruction execution control checkpoint",
        },
    }
}

fn map_relation_rebuild_error(
    operation: &'static str,
    error: SessionRelationError,
) -> SessionStoreError {
    match error {
        SessionRelationError::Cancelled => SessionStoreError::Cancelled,
        SessionRelationError::BudgetExhausted => SessionStoreError::BudgetExceeded {
            resource: "session relation reconstruction",
        },
        error => storage(operation, error),
    }
}

/// Message ids resolve to one occurrence per session, so a message the
/// candidate introduced must not already have an occurrence in its base.
async fn require_unsettled_message_ids(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &str,
    generation: i64,
    introduced: &super::projection::ParentMessageResolver,
) -> SessionStoreResult<()> {
    let requested = serde_json::to_string(&introduced.message_ids().collect::<Vec<_>>())
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
    let mut settled = conn
        .query(
            "SELECT requested.value
             FROM json_each(?3) AS requested
             WHERE EXISTS (
                 SELECT 1 FROM session_occurrences INDEXED BY idx_session_occurrences_message
                 WHERE session_id = ?1 AND message_id = requested.value AND generation < ?2
             )
             LIMIT 1",
            params![session_id, generation, requested],
        )
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
    match settled
        .next()
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?
    {
        Some(row) => {
            let message_id: String = row
                .get(0)
                .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
            Err(SessionStoreError::AmbiguousMessageOccurrence {
                message_id,
                occurrences: 2,
            })
        }
        None => Ok(()),
    }
}

/// Proves the candidate introduced exactly the canonical outputs of the
/// effects past its base frontier. The base proved its own prefix when it
/// activated, so only the new effects' observations are read.
#[tracing::instrument(
    name = "session_temporal.projection.validate_frontier",
    level = "trace",
    skip_all
)]
pub(super) async fn validate_candidate_frontier(
    conn: &impl crate::handle::SessionTemporalQuery,
    session_id: &str,
    generation: i64,
    source_frontier: u64,
    relation_projection: &SessionRelationProjection,
    control: &ExecutionControl,
) -> SessionStoreResult<()> {
    checkpoint_relation_rebuild_control(control)?;
    if relation_projection.session_id.as_str() != session_id
        || i64::try_from(relation_projection.generation)
            .map_err(|error| storage(ACTIVATE_OPERATION, error))?
            != generation
    {
        return Err(storage_message(
            ACTIVATE_OPERATION,
            "native relation projection identity does not match the candidate generation",
        ));
    }
    let base_frontier = base_source_frontier(
        conn,
        &tracedecay_domain::SessionId::new(session_id)
            .map_err(|error| storage(ACTIVATE_OPERATION, error))?,
        generation,
    )
    .await?;
    let mut expected = BTreeSet::new();
    let mut expected_copies = BTreeSet::new();
    let parent_resolver = canonical_parent_message_resolver(
        conn,
        session_id,
        base_frontier,
        source_frontier,
        ACTIVATE_OPERATION,
        Some(control),
        true,
    )
    .await?;
    checkpoint_relation_rebuild_control(control)?;
    for (occurrence_id, parent_message_id) in parent_resolver.canonical_outputs() {
        checkpoint_relation_rebuild_control(control)?;
        if let Some(parent_message_id) = parent_message_id
            && let Some(parent_occurrence_id) = parent_resolver.resolve(parent_message_id)
        {
            expected_copies.insert((occurrence_id.clone(), parent_occurrence_id.to_owned()));
        }
        expected.insert(occurrence_id.clone());
    }
    if expected.is_empty() && source_frontier != base_frontier {
        return Err(storage_message(
            ACTIVATE_OPERATION,
            "candidate generation has no canonical message outputs past its base frontier",
        ));
    }
    require_unsettled_message_ids(conn, session_id, generation, &parent_resolver).await?;

    let mut actual = BTreeSet::new();
    let mut rows = conn
        .query(
            "SELECT occurrence_id
             FROM session_occurrences INDEXED BY idx_session_occurrences_introduced
             WHERE session_id = ?1 AND generation = ?2",
            params![session_id, generation],
        )
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage(ACTIVATE_OPERATION, error))?
    {
        checkpoint_relation_rebuild_control(control)?;
        actual.insert(
            row.get::<String>(0)
                .map_err(|error| storage(ACTIVATE_OPERATION, error))?,
        );
    }
    if actual != expected {
        return Err(storage_message(
            ACTIVATE_OPERATION,
            "candidate occurrence coverage does not equal the frozen source frontier",
        ));
    }
    let actual_copies = relation_projection
        .logical_copies
        .iter()
        .map(|copy| {
            (
                copy.occurrence_id.as_str().to_owned(),
                copy.copied_from_occurrence_id.as_str().to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    // Parent-message copies are mandatory canonical coverage. Additional copy
    // edges are allowed only because batch persistence already validated their
    // typed retained-evidence proof and the final immutable receipt hashed
    // the complete edge set before activation.
    if !expected_copies.is_subset(&actual_copies) {
        return Err(storage_message(
            ACTIVATE_OPERATION,
            "candidate copy coverage omits canonical parent-message relations",
        ));
    }
    Ok(())
}
