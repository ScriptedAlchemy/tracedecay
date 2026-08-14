//! Canonical tag curation and fact merge commands.

use std::collections::BTreeSet;

use serde_json::Value;
use tracedecay_domain::{
    ActorId, FactAssertionId, FactAssertionKindV1, FactCurationActionV1, FactEventId, FactId,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, PayloadAccessState,
    UtcMicros,
};
use tracedecay_store::{
    FactCommitConflict, FactCommitReceipt, FactStoreError, FactStoreResult, FactWriteBatch,
    ProjectMemoryFactCurationBatchV1, ProjectMemoryFactCurationOperationEffectV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactMergeCommandV1, ProjectMemoryFactMergeOutcomeV1,
};

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;

use super::super::crud::{
    add_project_memory_fact_tx, commit_batch_tx, remove_project_memory_fact_tx, sanitize_payload,
    update_project_memory_fact_tx,
};
use super::super::envelope::{
    ProjectMemoryOperationReceiptV1, project_memory_lookup_operation_receipt_tx,
    project_memory_record_operation_receipt_tx,
};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, project_memory_event_time, project_memory_now,
    row_i64, row_optional_string, row_string, storage_error, storage_message,
};
use super::relations::normalize_tags;
use super::review::verify_curation_review_tx;
use super::{
    available_curation_fact_tx, curated_correction_batch, link_facts_tx, normalize_tags_tx,
};

fn canonical_fact_ids(
    owner: &FactOwnerV1,
    values: &[Value],
    field: &'static str,
) -> FactStoreResult<Vec<ProjectMemoryFactIdV1>> {
    let mut facts = Vec::with_capacity(values.len());
    let mut seen = BTreeSet::new();
    for value in values {
        let fact_id = value.as_str().ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                format!("operation receipt {field} contains a non-string fact id"),
            )
        })?;
        let fact_id = FactId::new(fact_id.to_owned())?;
        if !seen.insert(fact_id.clone()) {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                format!("operation receipt {field} contains duplicate fact ids"),
            ));
        }
        facts.push(ProjectMemoryFactIdV1::new(owner.clone(), fact_id)?);
    }
    Ok(facts)
}

fn curation_receipt_value(receipt: &ProjectMemoryFactCurationReceiptV1) -> FactStoreResult<Value> {
    serde_json::to_value(receipt)
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))
}

pub(super) fn curation_receipt_from_value(
    value: &Value,
) -> FactStoreResult<ProjectMemoryFactCurationReceiptV1> {
    serde_json::from_value(value.clone())
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))
}

async fn replay_curation(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactCurationBatchV1,
    envelope: &ProjectMemoryOperationReceiptV1,
    input_digest: &str,
) -> FactStoreResult<ProjectMemoryFactCurationReceiptV1> {
    let receipt = curation_receipt_from_value(&envelope.receipt)?;
    if receipt.owner() != request.owner()
        || receipt.operation_id() != request.operation_id()
        || receipt.input_digest() != input_digest
        || receipt.automation_run_id() != request.automation_run_id()
        || envelope.fact_id.as_ref() != receipt.replay_fact_id()
        || envelope.event_id.as_ref() != receipt.replay_event_id()
    {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "curation operation receipt material does not match the request",
        ));
    }
    verify_curation_replay_events_tx(transaction, request, &receipt).await?;
    Ok(receipt.into_replayed())
}

async fn verify_curation_replay_events_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactCurationBatchV1,
    receipt: &ProjectMemoryFactCurationReceiptV1,
) -> FactStoreResult<()> {
    if receipt.operation_effects().len() != request.operations().len() {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "curation commit receipt count does not match the immutable request",
        ));
    }
    let key = OwnerKey::new(request.owner())?;
    for (operation, effect) in request.operations().iter().zip(receipt.operation_effects()) {
        let direct_matches = match operation {
            ProjectMemoryFactCurationOperationV1::Add(operation) => {
                Some(effect.matches_add_outcome(
                    &add_project_memory_fact_tx(transaction, operation.command()).await?,
                ))
            }
            ProjectMemoryFactCurationOperationV1::Update(operation) => {
                Some(effect.matches_update_outcome(
                    &update_project_memory_fact_tx(transaction, operation.command()).await?,
                ))
            }
            ProjectMemoryFactCurationOperationV1::Merge(operation) => {
                Some(effect.matches_merge_outcome(
                    &merge_project_memory_facts_tx(transaction, operation.command()).await?,
                ))
            }
            ProjectMemoryFactCurationOperationV1::Remove(operation) => {
                Some(effect.matches_remove_outcome(
                    operation.command().target(),
                    &remove_project_memory_fact_tx(transaction, operation.command()).await?,
                ))
            }
            ProjectMemoryFactCurationOperationV1::NormalizeTags(_)
            | ProjectMemoryFactCurationOperationV1::LinkFacts(_) => None,
        };
        if let Some(matches) = direct_matches {
            if !matches {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation effect does not match the canonical child operation replay",
                ));
            }
            continue;
        }
        if let (
            ProjectMemoryFactCurationOperationV1::LinkFacts(operation),
            ProjectMemoryFactCurationOperationEffectV1::LinkFacts {
                relation,
                disposition:
                    tracedecay_store::ProjectMemoryFactCurationLinkDispositionV1::AlreadyLinked,
                commit: None,
            },
        ) = (operation, effect)
        {
            if !relation.matches_relation(operation.relation())
                || !super::relations::exact_relation_exists_tx(
                    transaction,
                    request.owner(),
                    operation.relation(),
                )
                .await?
            {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation no-op link receipt does not match canonical relation state",
                ));
            }
            continue;
        }
        let commits = effect.commit_receipts();
        let [commit] = commits.as_slice() else {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "curation commit receipt boundary does not match the immutable operation",
            ));
        };
        let (expected_fact_id, expected_event_count) = match operation {
            ProjectMemoryFactCurationOperationV1::NormalizeTags(operation)
                if matches!(
                    effect,
                    ProjectMemoryFactCurationOperationEffectV1::NormalizeTags { fact, .. }
                        if fact == operation.fact().fact()
                ) =>
            {
                (operation.fact().fact().fact_id(), 2_usize)
            }
            ProjectMemoryFactCurationOperationV1::LinkFacts(operation)
                if matches!(
                    effect,
                    ProjectMemoryFactCurationOperationEffectV1::LinkFacts { relation, .. }
                        if relation.matches_relation(operation.relation())
                ) =>
            {
                (operation.relation().source_fact_id(), 1_usize)
            }
            ProjectMemoryFactCurationOperationV1::Add(_)
            | ProjectMemoryFactCurationOperationV1::Update(_)
            | ProjectMemoryFactCurationOperationV1::Merge(_)
            | ProjectMemoryFactCurationOperationV1::Remove(_) => {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation direct operation reached lineage-only replay verification",
                ));
            }
            _ => {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation effect does not match the immutable request operation",
                ));
            }
        };
        if commit.owner() != request.owner()
            || commit.fact_id() != expected_fact_id
            || commit.committed_event_ids().len() != expected_event_count
        {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "curation commit receipt boundary does not match the immutable operation",
            ));
        }
        let events = load_commit_events_tx(transaction, &key, commit).await?;
        let last_sequence = events
            .last()
            .map(|(sequence, _)| *sequence)
            .ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation commit receipt has no canonical events",
                )
            })?;
        if events.windows(2).any(|pair| pair[0].0 >= pair[1].0)
            || active_assertion_at_event_tx(
                transaction,
                &key,
                request.owner(),
                commit.fact_id(),
                last_sequence,
            )
            .await?
            .as_ref()
                != commit.active_assertion_id()
        {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "curation commit receipt does not match canonical projection history",
            ));
        }
        match (operation, events.as_slice()) {
            (
                ProjectMemoryFactCurationOperationV1::NormalizeTags(operation),
                [(_, recorded), (_, normalized)],
            ) => {
                let mut expected_evidence_fact_ids = operation
                    .evidence_facts()
                    .iter()
                    .map(|evidence| evidence.fact().fact_id().clone())
                    .collect::<Vec<_>>();
                expected_evidence_fact_ids.sort_unstable();
                let assertion_id = match recorded.kind() {
                    FactLineageEventKindV1::AssertionRecorded { assertion_id }
                        if Some(assertion_id) == commit.active_assertion_id() =>
                    {
                        assertion_id
                    }
                    _ => {
                        return Err(storage_message(
                            PROJECT_MEMORY_WRITE_OPERATION,
                            "curation receipt is not bound to the committed normalization events",
                        ));
                    }
                };
                if recorded.fact_id() != operation.fact().fact().fact_id()
                    || normalized.fact_id() != operation.fact().fact().fact_id()
                    || recorded.actor_id() != request.actor()
                    || normalized.actor_id() != request.actor()
                    || recorded.occurred_at().0.checked_add(1) != Some(normalized.occurred_at().0)
                    || !matches!(
                        normalized.kind(),
                        FactLineageEventKindV1::Curated {
                            action: FactCurationActionV1::TagsNormalized {
                                evidence_fact_ids,
                                confidence,
                            },
                            evidence_ids,
                        } if evidence_fact_ids == &expected_evidence_fact_ids
                            && *confidence == operation.confidence()
                            && evidence_ids.is_empty()
                    )
                {
                    return Err(storage_message(
                        PROJECT_MEMORY_WRITE_OPERATION,
                        "curation receipt is not bound to the committed normalization events",
                    ));
                }
                let payload = correction_assertion_payload_tx(
                    transaction,
                    &key,
                    operation.fact().fact().fact_id(),
                    assertion_id,
                    request.actor(),
                    recorded.occurred_at(),
                )
                .await?;
                if payload.tags() != normalize_tags(operation.tags()) {
                    return Err(storage_message(
                        PROJECT_MEMORY_WRITE_OPERATION,
                        "curation correction payload tags do not match the immutable request",
                    ));
                }
            }
            (ProjectMemoryFactCurationOperationV1::LinkFacts(operation), [(_, event)]) => {
                if event.fact_id() != operation.relation().source_fact_id()
                    || event.actor_id() != request.actor()
                    || !matches!(
                        event.kind(),
                        FactLineageEventKindV1::Curated {
                            action: FactCurationActionV1::Linked { relation },
                            evidence_ids,
                        } if relation.as_ref() == operation.relation() && evidence_ids.is_empty()
                    )
                {
                    return Err(storage_message(
                        PROJECT_MEMORY_WRITE_OPERATION,
                        "curation receipt is not bound to the committed relation event",
                    ));
                }
            }
            _ => {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation commit receipt event partition is malformed",
                ));
            }
        }
    }
    Ok(())
}

async fn load_commit_events_tx(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    receipt: &FactCommitReceipt,
) -> FactStoreResult<Vec<(i64, FactLineageEventV1)>> {
    let mut events = Vec::with_capacity(receipt.committed_event_ids().len());
    for event_id in receipt.committed_event_ids() {
        let mut rows = transaction
            .query(
                "SELECT fact_id, event_json, event_sequence
                 FROM memory_v2_lineage_events
                 WHERE owner_kind = ?1 AND project_id = ?2 AND event_id = ?3
                 LIMIT 1",
                params![owner.kind, owner.project_id.as_str(), event_id.as_str()],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
            .ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation receipt references a missing canonical event",
                )
            })?;
        let stored_fact_id = FactId::new(row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?)?;
        let event = serde_json::from_str::<FactLineageEventV1>(&row_string(
            &row,
            1,
            PROJECT_MEMORY_WRITE_OPERATION,
        )?)
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        if event.event_id() != event_id
            || event.fact_id() != &stored_fact_id
            || event.fact_id() != receipt.fact_id()
            || event.owner() != receipt.owner()
        {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "curation receipt event does not match canonical storage",
            ));
        }
        events.push((row_i64(&row, 2, PROJECT_MEMORY_WRITE_OPERATION)?, event));
    }
    Ok(events)
}

async fn active_assertion_at_event_tx(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    expected_owner: &FactOwnerV1,
    fact_id: &FactId,
    last_sequence: i64,
) -> FactStoreResult<Option<FactAssertionId>> {
    let mut rows = transaction
        .query(
            "SELECT event_json
             FROM memory_v2_lineage_events
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
               AND event_sequence <= ?4
             ORDER BY event_sequence",
            params![
                owner.kind,
                owner.project_id.as_str(),
                fact_id.as_str(),
                last_sequence,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut active = None;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        let event = serde_json::from_str::<FactLineageEventV1>(&row_string(
            &row,
            0,
            PROJECT_MEMORY_WRITE_OPERATION,
        )?)
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        if event.owner() != expected_owner || event.fact_id() != fact_id {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "curation projection history does not match canonical storage",
            ));
        }
        match event.kind() {
            FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
                active = Some(assertion_id.clone());
            }
            FactLineageEventKindV1::PayloadAccessChanged { current, .. }
                if matches!(
                    current,
                    PayloadAccessState::Quarantined | PayloadAccessState::Deleted
                ) =>
            {
                active = None;
            }
            _ => {}
        }
    }
    Ok(active)
}

async fn correction_assertion_payload_tx(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
    actor: Option<&ActorId>,
    asserted_at: UtcMicros,
) -> FactStoreResult<FactPayloadV1> {
    let mut rows = transaction
        .query(
            "SELECT assertions.kind_json, assertions.asserted_at, assertions.actor_id,
                    payloads.payload_json
             FROM memory_v2_assertions AS assertions
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = assertions.assertion_id
              AND payloads.fact_id = assertions.fact_id
              AND payloads.owner_kind = assertions.owner_kind
              AND payloads.project_id = assertions.project_id
             WHERE assertions.assertion_id = ?1
               AND assertions.fact_id = ?2
               AND assertions.owner_kind = ?3
               AND assertions.project_id = ?4
               AND assertions.owner_json = ?5",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "curation correction assertion payload is missing",
            )
        })?;
    let kind = serde_json::from_str::<FactAssertionKindV1>(&row_string(
        &row,
        0,
        PROJECT_MEMORY_WRITE_OPERATION,
    )?)
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let stored_actor = row_optional_string(&row, 2, PROJECT_MEMORY_WRITE_OPERATION)?;
    if !matches!(kind, FactAssertionKindV1::Correction { .. })
        || row_i64(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)? != asserted_at.0
        || stored_actor.as_deref() != actor.map(ActorId::as_str)
    {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "curation correction assertion does not match its recorded event",
        ));
    }
    let payload = serde_json::from_str::<FactPayloadV1>(&row_string(
        &row,
        3,
        PROJECT_MEMORY_WRITE_OPERATION,
    )?)
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    Ok(payload)
}

pub(in crate::store::memory) async fn apply_project_memory_fact_curation_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactCurationBatchV1,
) -> FactStoreResult<ProjectMemoryFactCurationReceiptV1> {
    let input_digest = request.input_digest()?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "curation",
        &input_digest,
    )
    .await?
    {
        return replay_curation(transaction, request, &receipt, &input_digest).await;
    }

    verify_curation_review_tx(transaction, request).await?;

    let now = project_memory_now()?;
    let mut changed_ids = Vec::new();
    let mut effects = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, operation) in request.operations().iter().enumerate() {
        let offset = i64::try_from(index)
            .ok()
            .and_then(|value| value.checked_mul(2))
            .ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "curation event timestamp offset overflowed",
                )
            })?;
        let operation_time = project_memory_event_time(now, offset)?;
        match operation {
            ProjectMemoryFactCurationOperationV1::Add(operation) => {
                let outcome = add_project_memory_fact_tx(transaction, operation.command()).await?;
                if outcome.commit_receipt().is_some()
                    && seen.insert(outcome.fact().fact_id().clone())
                {
                    changed_ids.push(outcome.fact().fact_id().clone());
                }
                effects.push(ProjectMemoryFactCurationOperationEffectV1::add(&outcome)?);
            }
            ProjectMemoryFactCurationOperationV1::Update(operation) => {
                let outcome =
                    update_project_memory_fact_tx(transaction, operation.command()).await?;
                if seen.insert(outcome.fact().fact_id().clone()) {
                    changed_ids.push(outcome.fact().fact_id().clone());
                }
                effects.push(ProjectMemoryFactCurationOperationEffectV1::update(
                    &outcome,
                )?);
            }
            ProjectMemoryFactCurationOperationV1::Merge(operation) => {
                let outcome =
                    merge_project_memory_facts_tx(transaction, operation.command()).await?;
                if outcome.content_updated() && seen.insert(outcome.winner().fact_id().clone()) {
                    changed_ids.push(outcome.winner().fact_id().clone());
                }
                for loser in outcome.deleted_losers() {
                    if seen.insert(loser.fact_id().clone()) {
                        changed_ids.push(loser.fact_id().clone());
                    }
                }
                effects.push(ProjectMemoryFactCurationOperationEffectV1::merge(outcome));
            }
            ProjectMemoryFactCurationOperationV1::Remove(operation) => {
                let outcome =
                    remove_project_memory_fact_tx(transaction, operation.command()).await?;
                if outcome.was_removed()
                    && seen.insert(operation.command().target().fact_id().clone())
                {
                    changed_ids.push(operation.command().target().fact_id().clone());
                }
                effects.push(ProjectMemoryFactCurationOperationEffectV1::remove(
                    operation.command().target().clone(),
                    &outcome,
                )?);
            }
            ProjectMemoryFactCurationOperationV1::NormalizeTags(operation) => {
                let (fact_id, receipt) = normalize_tags_tx(
                    transaction,
                    request.owner(),
                    request.actor(),
                    operation,
                    operation_time,
                )
                .await?;
                if seen.insert(fact_id.clone()) {
                    changed_ids.push(fact_id);
                }
                effects.push(ProjectMemoryFactCurationOperationEffectV1::normalize_tags(
                    operation.fact().fact().clone(),
                    receipt,
                )?);
            }
            ProjectMemoryFactCurationOperationV1::LinkFacts(operation) => {
                let (fact_ids, commit) = link_facts_tx(
                    transaction,
                    request.owner(),
                    request.actor(),
                    operation,
                    operation_time,
                )
                .await?;
                for fact_id in fact_ids {
                    if seen.insert(fact_id.clone()) {
                        changed_ids.push(fact_id);
                    }
                }
                effects.push(match commit {
                    Some(receipt) => ProjectMemoryFactCurationOperationEffectV1::link_facts(
                        operation.relation().clone(),
                        receipt,
                    )?,
                    None => ProjectMemoryFactCurationOperationEffectV1::already_linked(
                        operation.relation().clone(),
                    )?,
                });
            }
        }
    }
    let changed = canonical_fact_ids(
        request.owner(),
        &changed_ids
            .iter()
            .map(|fact_id| Value::String(fact_id.as_str().to_owned()))
            .collect::<Vec<_>>(),
        "changed_fact_ids",
    )?;
    let receipt = ProjectMemoryFactCurationReceiptV1::new(
        request.owner().clone(),
        request.operation_id().clone(),
        input_digest.clone(),
        request.automation_run_id().cloned(),
        effects,
        changed,
    )?;
    let durable_receipt = curation_receipt_value(&receipt)?;
    project_memory_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "curation",
        &input_digest,
        receipt.replay_fact_id(),
        receipt.replay_event_id(),
        &durable_receipt,
        now,
    )
    .await?;
    Ok(receipt)
}

fn merge_removal_batch(
    fact_id: &FactId,
    owner: &FactOwnerV1,
    previous: PayloadAccessState,
    expected_last_event_id: &FactEventId,
    winner: &FactId,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let curated = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::MergedInto {
                fact_id: winner.clone(),
            },
            evidence_ids: Vec::new(),
        },
        now,
        actor.clone(),
    )?;
    let deleted = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous,
            current: PayloadAccessState::Deleted,
        },
        project_memory_event_time(now, 1)?,
        actor,
    )?;
    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![curated, deleted],
        Vec::new(),
        Vec::new(),
        Some(expected_last_event_id.clone()),
    )
}

fn merge_outcome_value(outcome: &ProjectMemoryFactMergeOutcomeV1) -> FactStoreResult<Value> {
    serde_json::to_value(outcome)
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))
}

fn merge_outcome_from_value(value: &Value) -> FactStoreResult<ProjectMemoryFactMergeOutcomeV1> {
    serde_json::from_value(value.clone())
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))
}

async fn verified_merge_commit_events_tx(
    transaction: &Transaction<'_>,
    key: &OwnerKey,
    owner: &FactOwnerV1,
    commit: &FactCommitReceipt,
) -> FactStoreResult<Vec<(i64, FactLineageEventV1)>> {
    let events = load_commit_events_tx(transaction, key, commit).await?;
    let last_sequence = events
        .last()
        .map(|(sequence, _)| *sequence)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "merge commit receipt has no canonical events",
            )
        })?;
    if events.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        || active_assertion_at_event_tx(transaction, key, owner, commit.fact_id(), last_sequence)
            .await?
            .as_ref()
            != commit.active_assertion_id()
    {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "merge commit receipt does not match canonical projection history",
        ));
    }
    Ok(events)
}

async fn verify_merge_replay_events_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactMergeCommandV1,
    outcome: &ProjectMemoryFactMergeOutcomeV1,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(request.owner())?;
    let mut commits = outcome.commit_receipts().iter();
    if request.merged_content().is_some() {
        let commit = commits.next().ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "merge winner commit receipt is missing",
            )
        })?;
        let events =
            verified_merge_commit_events_tx(transaction, &key, request.owner(), commit).await?;
        match events.as_slice() {
            [(_, recorded), (_, retained)]
                if recorded.fact_id() == request.winner().fact_id()
                    && retained.fact_id() == request.winner().fact_id()
                    && recorded.actor_id() == request.actor()
                    && retained.actor_id() == request.actor()
                    && matches!(
                        recorded.kind(),
                        FactLineageEventKindV1::AssertionRecorded { assertion_id }
                            if Some(assertion_id) == commit.active_assertion_id()
                    )
                    && matches!(
                        retained.kind(),
                        FactLineageEventKindV1::Curated {
                            action: FactCurationActionV1::Retained,
                            evidence_ids,
                        } if evidence_ids.is_empty()
                    ) => {}
            _ => {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "merge winner receipt is not bound to its canonical correction events",
                ));
            }
        }
    }
    for (loser, commit) in request.loser_facts().zip(commits.by_ref()) {
        let events =
            verified_merge_commit_events_tx(transaction, &key, request.owner(), commit).await?;
        match events.as_slice() {
            [(_, merged), (_, deleted)]
                if merged.fact_id() == loser.fact_id()
                    && deleted.fact_id() == loser.fact_id()
                    && merged.actor_id() == request.actor()
                    && deleted.actor_id() == request.actor()
                    && matches!(
                        merged.kind(),
                        FactLineageEventKindV1::Curated {
                            action: FactCurationActionV1::MergedInto { fact_id },
                            evidence_ids,
                        } if fact_id == request.winner().fact_id() && evidence_ids.is_empty()
                    )
                    && matches!(
                        deleted.kind(),
                        FactLineageEventKindV1::PayloadAccessChanged {
                            previous,
                            current: PayloadAccessState::Deleted,
                        } if previous != &PayloadAccessState::Deleted
                    ) => {}
            _ => {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "merge loser receipt is not bound to its canonical removal events",
                ));
            }
        }
    }
    if commits.next().is_some() {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "merge receipt contains excess canonical commits",
        ));
    }
    Ok(())
}

async fn replay_merge(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactMergeCommandV1,
    envelope: &ProjectMemoryOperationReceiptV1,
    input_digest: &str,
) -> FactStoreResult<ProjectMemoryFactMergeOutcomeV1> {
    let outcome = merge_outcome_from_value(&envelope.receipt)?;
    let first_commit = outcome.commit_receipts().first().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "merge receipt has no canonical commit",
        )
    })?;
    if outcome.owner() != request.owner()
        || outcome.operation_id() != request.operation_id()
        || outcome.input_digest() != input_digest
        || outcome.winner() != request.winner()
        || !outcome.deleted_losers().iter().eq(request.loser_facts())
        || outcome.content_updated() != request.merged_content().is_some()
        || envelope.fact_id.as_ref() != Some(first_commit.fact_id())
        || envelope.event_id.as_ref() != Some(first_commit.last_event_id())
    {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "merge operation receipt material does not match the immutable request",
        ));
    }
    verify_merge_replay_events_tx(transaction, request, &outcome).await?;
    Ok(outcome.into_replayed())
}

pub(in crate::store::memory) async fn merge_project_memory_facts_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactMergeCommandV1,
) -> FactStoreResult<ProjectMemoryFactMergeOutcomeV1> {
    let request_digest = request.input_digest()?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "merge",
        &request_digest,
    )
    .await?
    {
        return replay_merge(transaction, request, &receipt, &request_digest).await;
    }

    let now = project_memory_now()?;
    let winner_fact = available_curation_fact_tx(transaction, request.winner()).await?;
    if winner_fact.last_event_id() != request.winner_target().expected_last_event_id() {
        return Err(FactStoreError::CommitConflict {
            conflict: FactCommitConflict::LastEventMismatch {
                expected: Some(request.winner_target().expected_last_event_id().clone()),
                actual: Some(winner_fact.last_event_id().clone()),
            },
        });
    }
    let mut loser_facts = Vec::with_capacity(request.loser_targets().len());
    for target in request.loser_targets() {
        let loser = available_curation_fact_tx(transaction, target.fact()).await?;
        if loser.last_event_id() != target.expected_last_event_id() {
            return Err(FactStoreError::CommitConflict {
                conflict: FactCommitConflict::LastEventMismatch {
                    expected: Some(target.expected_last_event_id().clone()),
                    actual: Some(loser.last_event_id().clone()),
                },
            });
        }
        loser_facts.push((target, loser));
    }
    let mut commits = Vec::new();
    let content_updated = if let Some(content) = request.merged_content() {
        let payload = winner_fact
            .payload()
            .ok_or(FactStoreError::PayloadAccessMismatch)?;
        let Some(sanitized) = sanitize_payload(
            content,
            payload.category(),
            payload.tags(),
            payload.entities(),
            payload.metadata(),
            payload.source_label(),
        )?
        else {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "merged content was rejected by the privacy sanitizer",
            ));
        };
        let batch = curated_correction_batch(
            &winner_fact,
            sanitized.payload,
            request.actor().cloned(),
            now,
        )?;
        commits.push(commit_batch_tx(transaction, &batch).await?.0);
        true
    } else {
        false
    };

    let mut deleted_losers = Vec::with_capacity(request.loser_targets().len());
    for (target, loser) in loser_facts {
        if loser.fact_id() == winner_fact.fact_id() {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "merge winner cannot also be a loser",
            ));
        }
        let batch = merge_removal_batch(
            loser.fact_id(),
            request.owner(),
            loser.payload_access(),
            target.expected_last_event_id(),
            winner_fact.fact_id(),
            request.actor().cloned(),
            now,
        )?;
        commits.push(commit_batch_tx(transaction, &batch).await?.0);
        deleted_losers.push(target.fact().clone());
    }

    let outcome = ProjectMemoryFactMergeOutcomeV1::new(
        request.owner().clone(),
        request.operation_id().clone(),
        request_digest.clone(),
        request.winner().clone(),
        content_updated,
        deleted_losers,
        commits,
    )?;
    let durable_receipt = merge_outcome_value(&outcome)?;
    let first_commit = outcome.commit_receipts().first().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "merge outcome has no canonical commit",
        )
    })?;
    project_memory_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "merge",
        &request_digest,
        Some(first_commit.fact_id()),
        Some(first_commit.last_event_id()),
        &durable_receipt,
        now,
    )
    .await?;
    Ok(outcome)
}
