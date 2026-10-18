//! The immutable Work product event journal and its atomic verified projection.
//!
//! An append reads the journal's head, applies the event to it, and writes the
//! event, the verified version, the new head and, on a checkpoint version, the
//! checkpoint, in one transaction. Its cost is the head graph plus one event
//! row, whatever the journal's length.

use tracedecay_contracts::{
    WorkProductEventCommitOutcomeV1, WorkProductEventCommitV1, WorkProductEventDraftV1,
    WorkProductEventPortErrorV1, WorkProductEventPortV1, WorkProductPortContextV1,
};
use tracedecay_domain::{
    ManifestDigest, WorkCommandId, WorkProductEventId, WorkProductEventInputV1,
    WorkProductEventSequenceV1, WorkProductEventV1, canonical_sha256,
};

use super::journal::{
    CHECKPOINT_INTERVAL, WriteFault, fold_event, insert_checkpoint_row, insert_event_row,
    insert_version_row, load_head, load_published, register_scope_set, require_current_journal,
    selection_covers, write_head_row,
};
use super::{
    FromStoreFault, StoreFault, WORK_PRODUCT_EVENT_ID_DOMAIN, WorkProductPublishedVersionV1,
    owner_params, recovered_graph_digest, verified_version,
};
use crate::exact_sql::ExactSqlValue;
use crate::work::{
    RegisteredWorkQuery, RegisteredWorkWrite, WorkSqliteStorage, exact_sql_text,
    registered_work_query,
};

use crate::work_run_control::WorkSqliteJobStorage;

type PortError = WorkProductEventPortErrorV1;

/// The writer job commits the event and its verified version together, or
/// rolls both back with any refusal the job returns.
impl WorkProductEventPortV1 for WorkSqliteJobStorage<'_> {
    fn replay(
        &self,
        context: &WorkProductPortContextV1,
        command_id: &WorkCommandId,
        canonical_input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventCommitV1>, PortError> {
        replay_from(
            self.transaction,
            context,
            command_id,
            canonical_input_digest,
        )
    }

    fn append_atomically(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventCommitOutcomeV1, PortError> {
        append_in_transaction(self.transaction, context, draft)
    }
}

impl WorkProductEventPortV1 for WorkSqliteStorage {
    fn replay(
        &self,
        context: &WorkProductPortContextV1,
        command_id: &WorkCommandId,
        canonical_input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventCommitV1>, PortError> {
        replay_from(self.handle(), context, command_id, canonical_input_digest)
    }

    fn append_atomically(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventCommitOutcomeV1, PortError> {
        let transaction = self
            .handle()
            .begin_immediate()
            .map_err(PortError::from_fault)?;
        let outcome = append_in_transaction(&transaction, context, draft);
        match outcome {
            Ok(WorkProductEventCommitOutcomeV1::Appended(commit)) => {
                transaction.commit().map_err(PortError::from_fault)?;
                Ok(WorkProductEventCommitOutcomeV1::Appended(commit))
            }
            Ok(WorkProductEventCommitOutcomeV1::Replayed(commit)) => {
                transaction.rollback().map_err(PortError::from_fault)?;
                Ok(WorkProductEventCommitOutcomeV1::Replayed(commit))
            }
            Err(error) => {
                transaction.rollback().map_err(PortError::from_fault)?;
                Err(error)
            }
        }
    }
}

/// The event a command id already recorded. Command ids are unique per owner,
/// so a retry is recognised whichever selection sends it.
struct RecordedCommand {
    canonical_input_digest: String,
    event: WorkProductEventV1,
}

fn recorded_command(
    source: &impl RegisteredWorkQuery,
    context: &WorkProductPortContextV1,
    command_id: &WorkCommandId,
) -> Result<Option<RecordedCommand>, PortError> {
    let rows = registered_work_query(
        source,
        "SELECT canonical_input_digest, event_payload FROM work_product_journal_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND command_id = ?3",
        owner_params(context.authorized_scope())
            .into_iter()
            .chain([ExactSqlValue::Text(command_id.as_str().to_owned())])
            .collect(),
    )
    .map_err(PortError::from_fault)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let corrupt = || PortError::from_fault(StoreFault::Corrupt("replayed event row"));
    Ok(Some(RecordedCommand {
        canonical_input_digest: exact_sql_text(&row.values, 0)
            .ok_or_else(corrupt)?
            .to_owned(),
        event: serde_json::from_str(exact_sql_text(&row.values, 1).ok_or_else(corrupt)?)
            .map_err(|_| corrupt())?,
    }))
}

/// The commit a recorded command replays as, once it is known to carry the same
/// input and to be inside this selection.
fn replayed_commit(
    source: &impl RegisteredWorkQuery,
    context: &WorkProductPortContextV1,
    recorded: RecordedCommand,
    canonical_input_digest: &ManifestDigest,
) -> Result<WorkProductEventCommitV1, PortError> {
    if recorded.canonical_input_digest != canonical_input_digest.as_str() {
        // The same command id with different canonical input is a reused
        // idempotency key, never a replay of this request.
        return Err(PortError::IdempotencyConflict);
    }
    let event = recorded.event;
    // A replayed event must still be one this selection is authorized to see;
    // otherwise its existence would leak through the idempotency channel.
    if !selection_covers(
        context.authorized_scope().selection(),
        event.authorized_relation_scopes(),
    ) {
        return Err(PortError::NotFoundOrNotAuthorized);
    }
    let published = load_published(
        source,
        context.authorized_scope(),
        u64::MAX,
        event.result_graph_version(),
    )
    .map_err(PortError::from_fault)?
    .filter(|published| published.event_sequence == event.sequence())
    .ok_or(PortError::Unavailable)?;
    let verified = verified_version(&published).ok_or(PortError::Unavailable)?;
    WorkProductEventCommitV1::new(event, verified).map_err(|_| PortError::Unavailable)
}

fn replay_from(
    source: &impl RegisteredWorkQuery,
    context: &WorkProductPortContextV1,
    command_id: &WorkCommandId,
    canonical_input_digest: &ManifestDigest,
) -> Result<Option<WorkProductEventCommitV1>, PortError> {
    require_current_journal(source).map_err(PortError::from_fault)?;
    recorded_command(source, context, command_id)?
        .map(|recorded| replayed_commit(source, context, recorded, canonical_input_digest))
        .transpose()
}

pub(super) fn append_in_transaction(
    transaction: &impl RegisteredWorkWrite,
    context: &WorkProductPortContextV1,
    draft: &WorkProductEventDraftV1,
) -> Result<WorkProductEventCommitOutcomeV1, PortError> {
    let scope = context.authorized_scope();
    if draft.owner_scope.brain_id != *scope.owner_brain_id()
        || draft.owner_scope.profile_id != *scope.owner_profile_id()
    {
        return Err(PortError::NotFoundOrNotAuthorized);
    }
    require_current_journal(transaction).map_err(PortError::from_fault)?;
    if let Some(recorded) = recorded_command(transaction, context, &draft.command_id)? {
        return replayed_commit(
            transaction,
            context,
            recorded,
            &draft.canonical_input_digest,
        )
        .map(WorkProductEventCommitOutcomeV1::Replayed);
    }
    let head = load_head(transaction, scope).map_err(PortError::from_fault)?;
    let expected_matches = match (&head, draft.expected_graph_version) {
        (None, None) => true,
        (Some(head), Some(expected)) => head.version == expected,
        _ => false,
    };
    if !expected_matches {
        return Err(PortError::VersionConflict);
    }
    let sequence = head
        .as_ref()
        .map_or(Some(1), |head| head.version.get().checked_add(1))
        .and_then(|next| WorkProductEventSequenceV1::new(next).ok())
        .ok_or(PortError::Unavailable)?;
    // A draft that cannot become an event is not a stale revision: the head
    // matched, so nothing another writer did could have caused this.
    let event = mint_event(draft, sequence).ok_or(PortError::Unavailable)?;
    if !selection_covers(scope.selection(), event.authorized_relation_scopes()) {
        return Err(PortError::NotFoundOrNotAuthorized);
    }
    // An observation earlier than the change it verifies would make forensic
    // and as-of reads disagree, so the entire event transaction is refused.
    if context.observed_at() < event.occurred_at() {
        return Err(PortError::Unavailable);
    }
    let graph = fold_event(head.as_ref().map(|head| head.graph.clone()), &event)
        .ok_or(PortError::Unavailable)?;
    let digest = recovered_graph_digest(&graph).ok_or(PortError::Unavailable)?;
    let graph_json = serde_json::to_string(&graph).map_err(|_| PortError::Unavailable)?;
    let published = WorkProductPublishedVersionV1 {
        graph_version: event.result_graph_version(),
        event_sequence: sequence,
        valid_at: event.occurred_at(),
        observed_at: context.observed_at(),
        source_watermark: event.source_watermark().clone(),
        recovered_graph_digest: digest.as_str().to_owned(),
    };

    let owner = owner_params(scope);
    let written = (|| -> Result<(), WriteFault> {
        insert_event_row(transaction, &owner, &event)?;
        register_scope_set(transaction, &owner, &event)?;
        insert_version_row(transaction, &owner, &published)?;
        write_head_row(
            transaction,
            &owner,
            head.as_ref().map(|head| head.version),
            graph.version(),
            &graph_json,
        )?;
        if sequence.get().is_multiple_of(CHECKPOINT_INTERVAL) {
            insert_checkpoint_row(transaction, &owner, graph.version(), &graph_json)?;
        }
        Ok(())
    })();
    match written {
        Ok(()) => {}
        Err(WriteFault::Conflict) => return Err(PortError::VersionConflict),
        Err(WriteFault::Store(fault)) => return Err(PortError::from_fault(fault)),
    }

    let verified = verified_version(&published).ok_or(PortError::Unavailable)?;
    WorkProductEventCommitV1::new(event, verified)
        .map(WorkProductEventCommitOutcomeV1::Appended)
        .map_err(|_| PortError::Unavailable)
}

/// Mint the canonical event this draft becomes at `sequence`.
///
/// The identity is derived from the owner scope, the assigned sequence, and the
/// command id, so the same draft at the same journal position always yields the
/// same event id, an identity that is reproducible from the journal rather
/// than drawn from a clock or a counter the caller cannot see.
fn mint_event(
    draft: &WorkProductEventDraftV1,
    sequence: WorkProductEventSequenceV1,
) -> Option<WorkProductEventV1> {
    let event_id = canonical_sha256(&(
        WORK_PRODUCT_EVENT_ID_DOMAIN,
        draft.owner_scope.brain_id.as_str(),
        draft.owner_scope.profile_id.as_str(),
        sequence.get(),
        draft.command_id.as_str(),
    ))
    .ok()
    .and_then(|digest| WorkProductEventId::new(digest.as_str()).ok())?;
    WorkProductEventV1::new(WorkProductEventInputV1 {
        event_id,
        sequence,
        actor_id: draft.actor_id.clone(),
        owner_scope: draft.owner_scope.clone(),
        authorized_relation_scopes: draft.authorized_relation_scopes.clone(),
        expected_graph_version: draft.expected_graph_version,
        result_graph_version: draft.result_graph_version,
        command_id: draft.command_id.clone(),
        canonical_input_digest: draft.canonical_input_digest.clone(),
        causation_event_id: draft.causation_event_id.clone(),
        evidence: draft.evidence.clone(),
        source_watermark: draft.source_watermark.clone(),
        occurred_at: draft.occurred_at,
        policy_revision_id: draft.policy_revision_id.clone(),
        configuration_revision_id: draft.configuration_revision_id.clone(),
        catalog_generation_id: draft.catalog_generation_id.clone(),
        payload: draft.payload.clone(),
    })
    .ok()
}
