//! The immutable Work product event journal, its idempotency replay, and the
//! publication outbox one append enqueues.

use tracedecay_application::{
    WorkProductEventAppendOutcomeV1, WorkProductEventDraftV1, WorkProductEventPortErrorV1,
    WorkProductEventPortV1, WorkProductPortContextV1,
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
    ) -> Result<Option<WorkProductEventV1>, PortError> {
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
        Ok(Some(event))
    }

    fn append_with_outbox(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventAppendOutcomeV1, PortError> {
        let scope = context.authorized_scope();
        // The draft carries the owner scope the caller believes it is writing
        // for. It must be the scope the authorization port actually resolved,
        // or the append would attribute a change to a profile the request never
        // proved it owns.
        if draft.owner_scope.brain_id != *scope.owner_brain_id()
            || draft.owner_scope.profile_id != *scope.owner_profile_id()
        {
            return Err(PortError::NotFoundOrNotAuthorized);
        }

        let transaction = self
            .handle
            .begin_immediate()
            .map_err(|_| PortError::Unavailable)?;

        let replayed = replay_in_transaction(&transaction, context, draft);
        match replayed {
            Ok(Some(event)) => {
                let _ = transaction.rollback();
                return Ok(WorkProductEventAppendOutcomeV1::Replayed(event));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = transaction.rollback();
                return Err(error);
            }
        }

        let tail = match load_journal_tail(&transaction, scope) {
            Some(tail) => tail,
            None => {
                let _ = transaction.rollback();
                return Err(PortError::Unavailable);
            }
        };
        // The journal is the compare-and-swap authority: the draft's expected
        // version must be exactly the tail it claims to extend. A first event
        // expects no prior graph; every later one expects the stored tail.
        let expected_matches = match (&tail, draft.expected_graph_version) {
            (None, None) => true,
            (Some((_, stored)), Some(expected)) => *stored == expected,
            _ => false,
        };
        if !expected_matches {
            let _ = transaction.rollback();
            return Err(PortError::VersionConflict);
        }

        let next_sequence = tail
            .map_or(Some(1), |(sequence, _)| sequence.get().checked_add(1))
            .and_then(|next| WorkProductEventSequenceV1::new(next).ok());
        let Some(sequence) = next_sequence else {
            let _ = transaction.rollback();
            return Err(PortError::Unavailable);
        };

        let event = match mint_event(draft, sequence) {
            Some(event) => event,
            None => {
                let _ = transaction.rollback();
                // A draft that cannot form a canonical event is the caller's
                // contract violation, surfaced as the version-progression
                // refusal the domain itself raised.
                return Err(PortError::VersionConflict);
            }
        };

        if let Err(error) = insert_event(&transaction, context, &event, sequence) {
            let _ = transaction.rollback();
            return Err(error);
        }
        if let Err(error) = enqueue_outbox(&transaction, context, sequence) {
            let _ = transaction.rollback();
            return Err(error);
        }
        transaction.commit().map_err(|_| PortError::Unavailable)?;
        Ok(WorkProductEventAppendOutcomeV1::Appended(event))
    }
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
    let expected = event
        .expected_graph_version()
        .map_or(ExactSqlValue::Null, |version| {
            ExactSqlValue::Integer(i64::try_from(version.get()).unwrap_or(i64::MAX))
        });
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

/// Enqueue the appended event for publication.
///
/// The outbox row is written in the same transaction as the event, so an event
/// can never exist without a pending publication, and `publish_event` is the
/// only writer that can settle it.
fn enqueue_outbox(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    context: &WorkProductPortContextV1,
    sequence: WorkProductEventSequenceV1,
) -> Result<(), PortError> {
    transaction
        .execute(
            exact_sql_statement(
                "INSERT INTO work_product_event_outbox_v1 (
                    owner_brain_id, owner_profile_id, sequence, enqueued_at, published_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL)",
                owner_params(context.authorized_scope())
                    .into_iter()
                    .chain([
                        ExactSqlValue::Integer(
                            i64::try_from(sequence.get()).map_err(|_| PortError::Unavailable)?,
                        ),
                        ExactSqlValue::Integer(context.observed_at().0),
                    ])
                    .collect(),
            )
            .map_err(|_| PortError::Unavailable)?,
        )
        .map_err(|_| PortError::Unavailable)?;
    Ok(())
}
