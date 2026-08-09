//! The immutable Work product event journal and its atomic verified projection.

use tracedecay_application::{
    WorkProductEventCommitOutcomeV1, WorkProductEventCommitV1, WorkProductEventDraftV1,
    WorkProductEventPortErrorV1, WorkProductEventPortV1, WorkProductPortContextV1,
};
use tracedecay_domain::{
    ManifestDigest, WorkCommandId, WorkProductEventId, WorkProductEventInputV1,
    WorkProductEventSequenceV1, WorkProductEventV1, canonical_sha256,
};

use super::{WORK_PRODUCT_EVENT_ID_DOMAIN, load_journal_tail, owner_params, selection_covers};
use crate::exact_sql::ExactSqlValue;
use crate::work::{WorkSqliteStorage, exact_sql_statement, exact_sql_text, registered_work_query};

type PortError = WorkProductEventPortErrorV1;

impl WorkProductEventPortV1 for WorkSqliteStorage {
    fn replay(
        &self,
        context: &WorkProductPortContextV1,
        command_id: &WorkCommandId,
        canonical_input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventCommitV1>, PortError> {
        let scope = context.authorized_scope();
        let rows = registered_work_query(
            &self.handle,
            "SELECT canonical_input_digest, event_payload FROM work_product_events_v1
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND command_id = ?3",
            owner_params(scope)
                .into_iter()
                .chain([ExactSqlValue::Text(command_id.as_str().to_owned())])
                .collect(),
        )
        .map_err(|_| PortError::Unavailable)?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let stored_digest = exact_sql_text(&row.values, 0).ok_or(PortError::Unavailable)?;
        if stored_digest != canonical_input_digest.as_str() {
            // The same command id with different canonical input is a reused
            // idempotency key, never a replay of this request.
            return Err(PortError::IdempotencyConflict);
        }
        let event: WorkProductEventV1 =
            serde_json::from_str(exact_sql_text(&row.values, 1).ok_or(PortError::Unavailable)?)
                .map_err(|_| PortError::Unavailable)?;
        // A replayed event must still be one this selection is authorized to
        // see; otherwise its existence would leak through the idempotency
        // channel.
        if !selection_covers(scope.selection(), &event) {
            return Err(PortError::NotFoundOrNotAuthorized);
        }
        let published = super::load_published_versions(&self.handle, scope)
            .ok_or(PortError::Unavailable)?
            .into_iter()
            .find(|published| published.event_sequence == event.sequence())
            .ok_or(PortError::Unavailable)?;
        let verified = super::verified_version(&published, &event).ok_or(PortError::Unavailable)?;
        WorkProductEventCommitV1::new(event, verified)
            .map(Some)
            .map_err(|_| PortError::Unavailable)
    }

    fn append_atomically(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventCommitOutcomeV1, PortError> {
        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| PortError::Unavailable)?;
        let outcome = append_in_transaction(&transaction, context, draft);
        match outcome {
            Ok(WorkProductEventCommitOutcomeV1::Appended(commit)) => {
                transaction.commit().map_err(|_| PortError::Unavailable)?;
                Ok(WorkProductEventCommitOutcomeV1::Appended(commit))
            }
            Ok(WorkProductEventCommitOutcomeV1::Replayed(commit)) => {
                transaction.rollback().map_err(|_| PortError::Unavailable)?;
                Ok(WorkProductEventCommitOutcomeV1::Replayed(commit))
            }
            Err(error) => {
                transaction.rollback().map_err(|_| PortError::Unavailable)?;
                Err(error)
            }
        }
    }
}

pub(super) fn append_in_transaction(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    context: &WorkProductPortContextV1,
    draft: &WorkProductEventDraftV1,
) -> Result<WorkProductEventCommitOutcomeV1, PortError> {
    let scope = context.authorized_scope();
    if draft.owner_scope.brain_id != *scope.owner_brain_id()
        || draft.owner_scope.profile_id != *scope.owner_profile_id()
    {
        return Err(PortError::NotFoundOrNotAuthorized);
    }
    if let Some(event) = replay_in_transaction(transaction, context, draft)? {
        let published = super::load_published_versions(transaction, scope)
            .ok_or(PortError::Unavailable)?
            .into_iter()
            .find(|published| published.event_sequence == event.sequence())
            .ok_or(PortError::Unavailable)?;
        let verified = super::verified_version(&published, &event).ok_or(PortError::Unavailable)?;
        return WorkProductEventCommitV1::new(event, verified)
            .map(WorkProductEventCommitOutcomeV1::Replayed)
            .map_err(|_| PortError::Unavailable);
    }
    let tail = load_journal_tail(transaction, scope).ok_or(PortError::Unavailable)?;
    let expected_matches = match (&tail, draft.expected_graph_version) {
        (None, None) => true,
        (Some((_, stored)), Some(expected)) => *stored == expected,
        _ => false,
    };
    if !expected_matches {
        return Err(PortError::VersionConflict);
    }
    let sequence = tail
        .map_or(Some(1), |(sequence, _)| sequence.get().checked_add(1))
        .and_then(|next| WorkProductEventSequenceV1::new(next).ok())
        .ok_or(PortError::Unavailable)?;
    let event = mint_event(draft, sequence).ok_or(PortError::VersionConflict)?;
    insert_event(transaction, context, &event, sequence)?;
    let verified = super::publication::publish_in_transaction(transaction, context, &event)?;
    WorkProductEventCommitV1::new(event, verified)
        .map(WorkProductEventCommitOutcomeV1::Appended)
        .map_err(|_| PortError::Unavailable)
}

fn replay_in_transaction(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    context: &WorkProductPortContextV1,
    draft: &WorkProductEventDraftV1,
) -> Result<Option<WorkProductEventV1>, PortError> {
    let scope = context.authorized_scope();
    let rows = registered_work_query(
        transaction,
        "SELECT canonical_input_digest, event_payload FROM work_product_events_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND command_id = ?3",
        owner_params(scope)
            .into_iter()
            .chain([ExactSqlValue::Text(draft.command_id.as_str().to_owned())])
            .collect(),
    )
    .map_err(|_| PortError::Unavailable)?;
    let Some(row) = rows.rows.first() else {
        return Ok(None);
    };
    let stored_digest = exact_sql_text(&row.values, 0).ok_or(PortError::Unavailable)?;
    if stored_digest != draft.canonical_input_digest.as_str() {
        return Err(PortError::IdempotencyConflict);
    }
    serde_json::from_str(exact_sql_text(&row.values, 1).ok_or(PortError::Unavailable)?)
        .map(Some)
        .map_err(|_| PortError::Unavailable)
}

/// Mint the canonical event this draft becomes at `sequence`.
///
/// The identity is derived from the owner scope, the assigned sequence, and the
/// command id, so the same draft at the same journal position always yields the
/// same event id — an identity that is reproducible from the journal rather
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

fn insert_event(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    context: &WorkProductPortContextV1,
    event: &WorkProductEventV1,
    sequence: WorkProductEventSequenceV1,
) -> Result<(), PortError> {
    let payload = serde_json::to_string(event).map_err(|_| PortError::Unavailable)?;
    let expected = match event.expected_graph_version() {
        Some(version) => ExactSqlValue::Integer(
            i64::try_from(version.get()).map_err(|_| PortError::Unavailable)?,
        ),
        None => ExactSqlValue::Null,
    };
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_product_events_v1 (
                    owner_brain_id, owner_profile_id, sequence, event_id, command_id,
                    canonical_input_digest, expected_graph_version, result_graph_version,
                    occurred_at, event_payload
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                owner_params(context.authorized_scope())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(
                            i64::try_from(sequence.get()).map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Text(event.event_id().as_str().to_owned()),
                        ExactSqlValue::Text(event.command_id().as_str().to_owned()),
                        ExactSqlValue::Text(event.canonical_input_digest().as_str().to_owned()),
                        expected,
                        ExactSqlValue::Integer(
                            i64::try_from(event.result_graph_version().get())
                                .map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Integer(event.occurred_at().0),
                        ExactSqlValue::Text(payload),
                    ])
                    .collect(),
            )
            .map_err(|_| PortError::Unavailable)?,
        )
        .map_err(|_| PortError::VersionConflict)?;
    Ok(())
}
