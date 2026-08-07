use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_domain::VectorGenerationIdV1;
use tracedecay_store::{
    GraphPublicationOperationContextV1, GraphPublicationReplayLookupV1,
    GraphPublicationReplayRetirementV1, GraphRetiredReplayCleanupFinalizeOutcomeV1,
    MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS, SemanticVectorCancelledRetirement,
    SemanticVectorCancelledRetirementOutcome, SemanticVectorProjectCensusReceipt,
    SemanticVectorPublicationAuthority, SemanticVectorPublishedRetirement,
    SemanticVectorPublishedRetirementOutcome, SemanticVectorStageCensusCursor,
    SemanticVectorStageCensusRequest, SemanticVectorStageCensusRevision, SemanticVectorStageState,
    SemanticVectorWriterFence, StoreShardIdV1,
};

use super::publication_support::{
    check_all, clear_retiring_fence, locator_from_key, map_publication_error, retain_lease_closure,
};
use super::staging::{map_staging_error, require_authority_binding};
use super::{GraphDbRegistration, GraphDbRegistry, check_registration_request};
use crate::GraphDbError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorRetentionAction {
    None,
    Retired(VectorGenerationIdV1),
    Finalized(VectorGenerationIdV1),
    CancelledRemoved(VectorGenerationIdV1),
    Retained(VectorGenerationIdV1),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorRetentionCensus {
    pub shard_id: StoreShardIdV1,
    pub revision: SemanticVectorStageCensusRevision,
    pub pending: u64,
    pub ready: u64,
    pub published: u64,
    pub cancelled: u64,
    pub complete_receipt: Option<SemanticVectorProjectCensusReceipt>,
    pub continuation: Option<SemanticVectorStageCensusCursor>,
    pub action: SemanticVectorRetentionAction,
}

pub struct SemanticVectorRetirementReservation {
    record: tracedecay_store::SemanticVectorStageRecord,
    revision: SemanticVectorStageCensusRevision,
    kind: SemanticVectorRetirementReservationKind,
    locator: crate::lease::GenerationLocator,
    database: Arc<crate::GraphDb>,
    armed: bool,
}

enum SemanticVectorRetirementReservationKind {
    Cancelled,
    Published(Box<GraphPublicationReplayRetirementV1>),
}

impl SemanticVectorRetirementReservation {
    pub fn generation_id(&self) -> &VectorGenerationIdV1 {
        &self.record.plan.semantic_generation_id
    }

    pub fn census_revision(&self) -> SemanticVectorStageCensusRevision {
        self.revision
    }

    fn release(mut self) -> Result<(), GraphDbError> {
        clear_retiring_fence(&self.database, &self.locator)?;
        self.armed = false;
        Ok(())
    }

    fn preserve_cleanup_fence(mut self) {
        self.armed = false;
    }
}

impl Drop for SemanticVectorRetirementReservation {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self.database.inner.verified_generations.write() {
            Ok(mut state) => {
                state.retiring.remove(&self.locator);
            }
            Err(poisoned) => {
                poisoned.into_inner().retiring.remove(&self.locator);
                self.database.inner.poisoned.store(true, Ordering::Release);
            }
        }
        self.armed = false;
    }
}

pub enum SemanticVectorRetentionStep {
    Census(SemanticVectorRetentionCensus),
    Reserved {
        census: SemanticVectorRetentionCensus,
        reservation: Box<SemanticVectorRetirementReservation>,
    },
}

impl GraphDbRegistry {
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_one_semantic_vector_generation(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        after: Option<SemanticVectorStageCensusCursor>,
        writer_fence: &SemanticVectorWriterFence,
    ) -> Result<SemanticVectorRetentionStep, GraphDbError> {
        check_all(&registration, context)?;
        require_authority_binding(&registration, authority)?;
        if &writer_fence.binding != registration.binding() {
            return Err(GraphDbError::Conflict);
        }
        let shard_id = &registration.binding().shard_id;
        let database = self.registered_database(&registration)?;
        if let Some(action) =
            converge_retired_cleanup(&registration, authority, context, writer_fence, &database)?
        {
            return Ok(SemanticVectorRetentionStep::Census(empty_census(
                shard_id.clone(),
                after,
                action,
            )));
        }

        let request = SemanticVectorStageCensusRequest::for_shard(
            shard_id.clone(),
            after,
            MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS,
        )
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let page = authority
            .stage_census(&request, context)
            .map_err(map_staging_error)?;
        let mut census = summarize_page(&page);

        if let Some(census_record) = page
            .records
            .iter()
            .find(|record| record.stage.state == SemanticVectorStageState::Cancelled)
        {
            let record = &census_record.stage;
            let reservation = reserve_cancelled(&database, record, page.revision)?;
            census = summarize_through_action(&page, &census_record.cursor)?;
            return Ok(SemanticVectorRetentionStep::Reserved {
                census,
                reservation: Box::new(reservation),
            });
        }

        for census_record in &page.records {
            let record = &census_record.stage;
            if record.state != SemanticVectorStageState::Published
                || authority
                    .generation_has_live_base_reference(
                        shard_id,
                        &record.plan.semantic_generation_id,
                        page.revision,
                        context,
                    )
                    .map_err(map_staging_error)?
            {
                continue;
            }
            let reservation = reserve_published(
                &registration,
                authority,
                context,
                &database,
                record,
                page.revision,
            )?;
            census = summarize_through_action(&page, &census_record.cursor)?;
            return match reservation {
                Ok(reservation) => Ok(SemanticVectorRetentionStep::Reserved {
                    census,
                    reservation: Box::new(reservation),
                }),
                Err(action) => {
                    census.action = action;
                    Ok(SemanticVectorRetentionStep::Census(census))
                }
            };
        }
        Ok(SemanticVectorRetentionStep::Census(census))
    }

    pub fn release_semantic_vector_retirement(
        &self,
        reservation: SemanticVectorRetirementReservation,
    ) -> Result<(), GraphDbError> {
        reservation.release()
    }

    pub fn finalize_semantic_vector_retirement(
        &self,
        registration: GraphDbRegistration,
        authority: &mut dyn SemanticVectorPublicationAuthority,
        context: &GraphPublicationOperationContextV1<'_>,
        writer_fence: &SemanticVectorWriterFence,
        reservation: SemanticVectorRetirementReservation,
    ) -> Result<SemanticVectorRetentionAction, GraphDbError> {
        let database = self.registered_database(&registration)?;
        let validation = check_all(&registration, context)
            .and_then(|()| require_authority_binding(&registration, authority))
            .and_then(|()| {
                (&writer_fence.binding == registration.binding())
                    .then_some(())
                    .ok_or(GraphDbError::Conflict)
            })
            .and_then(|()| {
                authority
                    .validate_project_census_revision(
                        &registration.binding().shard_id,
                        reservation.revision,
                        context,
                    )
                    .map_err(map_staging_error)
            });
        if let Err(error) = validation {
            reservation.release()?;
            return Err(error);
        }
        let cancelled = matches!(
            &reservation.kind,
            SemanticVectorRetirementReservationKind::Cancelled
        );
        if cancelled {
            finish_reserved_cancelled(
                &registration,
                authority,
                context,
                writer_fence,
                &database,
                reservation,
            )
        } else {
            finish_reserved_published(
                &registration,
                authority,
                context,
                writer_fence,
                &database,
                reservation,
            )
        }
    }
}

fn continuation_after_action(
    page: &tracedecay_store::SemanticVectorStageCensusPage,
    cursor: &SemanticVectorStageCensusCursor,
) -> Option<SemanticVectorStageCensusCursor> {
    let action_index = page
        .records
        .iter()
        .position(|record| &record.cursor == cursor)?;
    (action_index + 1 < page.records.len() || page.continuation.is_some()).then(|| cursor.clone())
}

fn summarize_page(
    page: &tracedecay_store::SemanticVectorStageCensusPage,
) -> SemanticVectorRetentionCensus {
    summarize_records(
        &page.records,
        page.continuation.clone(),
        page.complete_receipt.clone(),
        page,
    )
}

fn summarize_through_action(
    page: &tracedecay_store::SemanticVectorStageCensusPage,
    cursor: &SemanticVectorStageCensusCursor,
) -> Result<SemanticVectorRetentionCensus, GraphDbError> {
    let Some(action_index) = page
        .records
        .iter()
        .position(|record| &record.cursor == cursor)
    else {
        return Err(GraphDbError::Corrupt {
            message: "semantic vector retention action escaped its census page".to_owned(),
        });
    };
    Ok(summarize_records(
        &page.records[..=action_index],
        continuation_after_action(page, cursor),
        None,
        page,
    ))
}

fn summarize_records(
    records: &[tracedecay_store::SemanticVectorStageCensusRecord],
    continuation: Option<SemanticVectorStageCensusCursor>,
    complete_receipt: Option<SemanticVectorProjectCensusReceipt>,
    page: &tracedecay_store::SemanticVectorStageCensusPage,
) -> SemanticVectorRetentionCensus {
    let mut census = SemanticVectorRetentionCensus {
        shard_id: page.shard_id.clone(),
        revision: page.revision,
        pending: 0,
        ready: 0,
        published: 0,
        cancelled: 0,
        complete_receipt,
        continuation,
        action: SemanticVectorRetentionAction::None,
    };
    for record in records {
        match record.stage.state {
            SemanticVectorStageState::Pending => census.pending += 1,
            SemanticVectorStageState::ReadyToPublish => census.ready += 1,
            SemanticVectorStageState::Published => census.published += 1,
            SemanticVectorStageState::Cancelled => {
                census.cancelled += 1;
                continue;
            }
        }
    }
    census
}

fn empty_census(
    shard_id: StoreShardIdV1,
    continuation: Option<SemanticVectorStageCensusCursor>,
    action: SemanticVectorRetentionAction,
) -> SemanticVectorRetentionCensus {
    SemanticVectorRetentionCensus {
        revision: continuation
            .as_ref()
            .map_or(SemanticVectorStageCensusRevision::INITIAL, |cursor| {
                cursor.revision
            }),
        shard_id,
        pending: 0,
        ready: 0,
        published: 0,
        cancelled: 0,
        complete_receipt: None,
        continuation,
        action,
    }
}

fn reserve_published(
    _registration: &GraphDbRegistration,
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    database: &Arc<crate::GraphDb>,
    record: &tracedecay_store::SemanticVectorStageRecord,
    revision: SemanticVectorStageCensusRevision,
) -> Result<Result<SemanticVectorRetirementReservation, SemanticVectorRetentionAction>, GraphDbError>
{
    let replay = match authority
        .replay(&record.plan.publication_key, context)
        .map_err(map_publication_error)?
    {
        GraphPublicationReplayLookupV1::Active(replay) => replay,
        GraphPublicationReplayLookupV1::Retired(_) | GraphPublicationReplayLookupV1::Missing => {
            return Err(GraphDbError::Corrupt {
                message: "published semantic vector stage lost its active replay".to_owned(),
            });
        }
    };
    let locator = locator_from_key(&replay.publication.key)?;
    let retirement = GraphPublicationReplayRetirementV1::new(
        replay.publication.key.clone(),
        replay.publication.input_digest.clone(),
        replay
            .publication
            .dependency_generation_closure_digest
            .clone(),
        replay.publication.direct_dependency_generations.clone(),
        replay.publication.expected_prior_head.clone(),
        replay.publication.expected_recovered_digest.clone(),
        replay.publication.canonical_replay_source_digest.clone(),
    )
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    {
        let mut state = database.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        let retained_by_head = state.heads.values().any(|head| {
            let mut retained = BTreeSet::new();
            retain_lease_closure(head, &mut retained);
            retained.contains(&locator)
        });
        let retained_by_reader = state
            .known
            .get(&locator)
            .and_then(std::sync::Weak::upgrade)
            .is_some();
        if retained_by_head || retained_by_reader {
            return Ok(Err(SemanticVectorRetentionAction::Retained(
                record.plan.semantic_generation_id.clone(),
            )));
        }
        state.retiring.insert(locator.clone());
    }
    Ok(Ok(SemanticVectorRetirementReservation {
        record: record.clone(),
        revision,
        kind: SemanticVectorRetirementReservationKind::Published(Box::new(retirement)),
        locator,
        database: Arc::clone(database),
        armed: true,
    }))
}

fn reserve_cancelled(
    database: &Arc<crate::GraphDb>,
    record: &tracedecay_store::SemanticVectorStageRecord,
    revision: SemanticVectorStageCensusRevision,
) -> Result<SemanticVectorRetirementReservation, GraphDbError> {
    database.reserve_staged_generation_retirement(&record.plan)?;
    Ok(SemanticVectorRetirementReservation {
        record: record.clone(),
        revision,
        kind: SemanticVectorRetirementReservationKind::Cancelled,
        locator: locator_from_key(&record.plan.publication_key)?,
        database: Arc::clone(database),
        armed: true,
    })
}

fn finish_reserved_cancelled(
    registration: &GraphDbRegistration,
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    writer_fence: &SemanticVectorWriterFence,
    database: &crate::GraphDb,
    reservation: SemanticVectorRetirementReservation,
) -> Result<SemanticVectorRetentionAction, GraphDbError> {
    let record = reservation.record.clone();
    if record.state != SemanticVectorStageState::Cancelled {
        reservation.release()?;
        return Err(GraphDbError::Conflict);
    }
    if let Err(error) = database.delete_cancelled_staged_generation(&record.plan, &|| {
        check_registration_request(registration)
    }) {
        reservation.preserve_cleanup_fence();
        return Err(error);
    }
    let outcome = authority
        .remove_cancelled_generation(
            &SemanticVectorCancelledRetirement {
                stage: record.plan.key.clone(),
                writer_fence: writer_fence.clone(),
            },
            context,
        )
        .map_err(map_staging_error);
    match outcome {
        Ok(
            SemanticVectorCancelledRetirementOutcome::Removed
            | SemanticVectorCancelledRetirementOutcome::ExactMissing,
        ) => {
            reservation.release()?;
            Ok(SemanticVectorRetentionAction::CancelledRemoved(
                record.plan.semantic_generation_id,
            ))
        }
        Ok(SemanticVectorCancelledRetirementOutcome::NotCancelled(_)) => {
            reservation.release()?;
            Err(GraphDbError::Conflict)
        }
        Err(error) => {
            reservation.preserve_cleanup_fence();
            Err(error)
        }
    }
}

fn finish_reserved_published(
    registration: &GraphDbRegistration,
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    writer_fence: &SemanticVectorWriterFence,
    database: &crate::GraphDb,
    reservation: SemanticVectorRetirementReservation,
) -> Result<SemanticVectorRetentionAction, GraphDbError> {
    let record = reservation.record.clone();
    let SemanticVectorRetirementReservationKind::Published(replay) = &reservation.kind else {
        reservation.release()?;
        return Err(GraphDbError::Conflict);
    };
    let replay = (**replay).clone();
    let locator = reservation.locator.clone();
    {
        let state = database.inner.verified_generations.write().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        if !state.retiring.contains(&locator) {
            return Err(GraphDbError::Conflict);
        }
        let retained_by_head = state.heads.values().any(|head| {
            let mut retained = BTreeSet::new();
            retain_lease_closure(head, &mut retained);
            retained.contains(&locator)
        });
        let retained_by_reader = state
            .known
            .get(&locator)
            .and_then(std::sync::Weak::upgrade)
            .is_some();
        if retained_by_head || retained_by_reader {
            drop(state);
            reservation.release()?;
            return Ok(SemanticVectorRetentionAction::Retained(
                record.plan.semantic_generation_id,
            ));
        }
    }
    let outcome = match authority.retire_published_generation(
        &SemanticVectorPublishedRetirement {
            stage: record.plan.key.clone(),
            semantic_generation_id: record.plan.semantic_generation_id.clone(),
            replay,
            writer_fence: writer_fence.clone(),
        },
        context,
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            reservation.release()?;
            return Err(map_staging_error(error));
        }
    };
    match outcome {
        SemanticVectorPublishedRetirementOutcome::Retired(_)
        | SemanticVectorPublishedRetirementOutcome::ExactReplay(_) => {
            // Relational retirement is the linearization point. Native cleanup
            // failures must retain this fence so replay can converge without a
            // reader reopening bytes whose authority has already retired.
            reservation.preserve_cleanup_fence();
            database.delete_generation_contents(&locator, &|| {
                check_registration_request(registration)
            })?;
            Ok(SemanticVectorRetentionAction::Retired(
                record.plan.semantic_generation_id.clone(),
            ))
        }
        SemanticVectorPublishedRetirementOutcome::CurrentVerifiedHead { .. }
        | SemanticVectorPublishedRetirementOutcome::PendingReplay => {
            reservation.release()?;
            Ok(SemanticVectorRetentionAction::Retained(
                record.plan.semantic_generation_id.clone(),
            ))
        }
        SemanticVectorPublishedRetirementOutcome::Conflict
        | SemanticVectorPublishedRetirementOutcome::Missing => {
            reservation.release()?;
            Err(GraphDbError::Conflict)
        }
    }
}

fn converge_retired_cleanup(
    registration: &GraphDbRegistration,
    authority: &mut dyn SemanticVectorPublicationAuthority,
    context: &GraphPublicationOperationContextV1<'_>,
    writer_fence: &SemanticVectorWriterFence,
    database: &crate::GraphDb,
) -> Result<Option<SemanticVectorRetentionAction>, GraphDbError> {
    let Some(mut cleanup) = authority
        .pending_retirement_cleanup(&registration.binding().shard_id, context)
        .map_err(map_staging_error)?
    else {
        return Ok(None);
    };
    cleanup.retirement.writer_fence = writer_fence.clone();
    let locator = locator_from_key(&cleanup.retirement.replay.key)?;
    database.delete_generation_contents(&locator, &|| check_registration_request(registration))?;
    match authority
        .finalize_retired_replay_cleanup(&cleanup.retirement.replay, context)
        .map_err(map_publication_error)?
    {
        GraphRetiredReplayCleanupFinalizeOutcomeV1::Finalized(_) => Ok(Some(
            SemanticVectorRetentionAction::Finalized(cleanup.retirement.semantic_generation_id),
        )),
        GraphRetiredReplayCleanupFinalizeOutcomeV1::ExactReplay(_) => {
            if !authority
                .complete_retirement_cleanup(&cleanup.retirement, context)
                .map_err(map_staging_error)?
            {
                return Err(GraphDbError::Corrupt {
                    message: "semantic vector retirement cleanup journal disappeared".to_owned(),
                });
            }
            Ok(Some(SemanticVectorRetentionAction::Finalized(
                cleanup.retirement.semantic_generation_id,
            )))
        }
        GraphRetiredReplayCleanupFinalizeOutcomeV1::Conflict
        | GraphRetiredReplayCleanupFinalizeOutcomeV1::Missing => Err(GraphDbError::Conflict),
    }
}
