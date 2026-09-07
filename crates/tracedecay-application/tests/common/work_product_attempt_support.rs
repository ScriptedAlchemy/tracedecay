//! Product-graph event and durable-row mechanics for the shared attempt fixture.

use super::*;

pub(super) fn digest_char(value: char) -> ManifestDigest {
    digest(&format!("sha256:{}", value.to_string().repeat(64)))
}

pub(super) fn graph_with_task(task_id: TaskId) -> WorkProductGraphV1 {
    WorkProductGraphV1::new(
        WorkGraphVersionV1::initial(),
        vec![
            WorkInitiativeV1::new(
                id::<tracedecay_domain::InitiativeId>("initiative.work-product.fixture"),
                "Fixture initiative".to_owned(),
                UtcMicros(1),
            )
            .expect("fixture initiative is valid"),
        ],
        vec![
            WorkPlanV1::new(
                id::<WorkPlanId>("plan.work-product.fixture"),
                id::<tracedecay_domain::InitiativeId>("initiative.work-product.fixture"),
                "Fixture plan".to_owned(),
                UtcMicros(2),
            )
            .expect("fixture plan is valid"),
        ],
        vec![
            WorkMilestoneV1::new(
                id::<MilestoneId>("milestone.work-product.fixture"),
                id::<WorkPlanId>("plan.work-product.fixture"),
                "Fixture milestone".to_owned(),
                UtcMicros(3),
            )
            .expect("fixture milestone is valid"),
        ],
        vec![work_item(task_id)],
    )
    .expect("fixture Work product graph is valid")
}

pub(super) fn work_item(task_id: TaskId) -> WorkItemV1 {
    WorkItemV1::new(WorkItemInputV1 {
        task_id,
        hierarchy: WorkHierarchyV1::new(
            id::<tracedecay_domain::InitiativeId>("initiative.work-product.fixture"),
            id::<WorkPlanId>("plan.work-product.fixture"),
            id::<MilestoneId>("milestone.work-product.fixture"),
        ),
        title: "Fixture Work task".to_owned(),
        dependencies: BTreeSet::new(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: Vec::new(),
        effort: 1,
        scheduled_at: None,
        deadline: None,
        created_at: UtcMicros(10),
        updated_at: UtcMicros(10),
    })
    .expect("fixture Work item is valid")
}

pub(super) fn append_seed_change(
    rows: &mut WorkProductAttemptRows,
    context: &RequestContext,
    change: WorkGraphChangeV1,
    command: String,
    occurred_at: UtcMicros,
) {
    append_seed_event(
        rows,
        context,
        WorkProductEventPayloadV1::Changed {
            change: Box::new(change),
        },
        command,
        occurred_at,
    );
}

pub(super) fn append_seed_event(
    rows: &mut WorkProductAttemptRows,
    context: &RequestContext,
    payload: WorkProductEventPayloadV1,
    command: String,
    occurred_at: UtcMicros,
) {
    let expected = rows.graph.as_ref().map(WorkProductGraphV1::version);
    let next = match &payload {
        WorkProductEventPayloadV1::Created { graph } => graph.clone(),
        WorkProductEventPayloadV1::Changed { change } => rows
            .graph
            .as_ref()
            .expect("changed seed event follows a graph")
            .clone()
            .apply((**change).clone())
            .expect("seeded Work graph change is legal"),
    };
    let sequence = WorkProductEventSequenceV1::new(
        u64::try_from(rows.events.len() + 1).expect("fixture event count fits u64"),
    )
    .expect("fixture event sequence is nonzero");
    let selection = work_product_selection(context);
    let event = WorkProductEventV1::new(WorkProductEventInputV1 {
        event_id: WorkProductEventId::new(format!("event.work-product.fixture.{}", sequence.get()))
            .expect("fixture event id is valid"),
        sequence,
        actor_id: context.actor().clone(),
        owner_scope: fixture_owner_scope(),
        authorized_relation_scopes: selection
            .relation_scopes()
            .expect("repository selection has relations")
            .iter()
            .cloned()
            .collect(),
        expected_graph_version: expected,
        result_graph_version: next.version(),
        command_id: id(&command),
        canonical_input_digest: digest_char('a'),
        causation_event_id: None,
        evidence: Vec::new(),
        source_watermark: WorkProductSourceWatermarkV1::new(BTreeMap::new())
            .expect("fixture source watermark is valid"),
        occurred_at,
        policy_revision_id: id(context.grant().digest.as_str()),
        configuration_revision_id: id("configuration.work-product.fixture"),
        catalog_generation_id: id("catalog.work-product.fixture"),
        payload,
    })
    .expect("seeded Work product event is valid");
    let verified = VerifiedWorkGraphVersionV1::new(
        next.version(),
        sequence,
        event.source_watermark().clone(),
        digest_char('c'),
    )
    .expect("fixture verified graph version is valid");
    rows.graph = Some(next);
    rows.events.push(StoredProductEvent {
        commit: WorkProductEventCommitV1::new(event, verified)
            .expect("seeded Work product event is committed"),
    });
}

fn fixture_owner_scope() -> WorkProductProfileScopeV1 {
    WorkProductProfileScopeV1 {
        brain_id: id::<BrainId>("brain.work-product.fixture"),
        profile_id: id::<UserProfileId>("profile.work-product.fixture"),
    }
}

pub(super) fn product_commit_for(
    rows: &WorkProductAttemptRows,
    draft: &tracedecay_application::WorkProductEventDraftV1,
) -> Result<(WorkProductGraphV1, WorkProductEventCommitV1), WorkProductAttemptAdmissionErrorV1> {
    let current = rows
        .graph
        .as_ref()
        .ok_or(WorkProductAttemptAdmissionErrorV1::NotFoundOrNotAuthorized)?;
    if draft.expected_graph_version != Some(current.version()) {
        return Err(WorkProductAttemptAdmissionErrorV1::VersionConflict);
    }
    let WorkProductEventPayloadV1::Changed { change } = &draft.payload else {
        return Err(WorkProductAttemptAdmissionErrorV1::InvalidAdmission);
    };
    let next = current
        .clone()
        .apply((**change).clone())
        .map_err(|_| WorkProductAttemptAdmissionErrorV1::InvalidAdmission)?;
    if draft.result_graph_version != next.version() {
        return Err(WorkProductAttemptAdmissionErrorV1::InvalidAdmission);
    }
    let sequence = WorkProductEventSequenceV1::new(
        u64::try_from(rows.events.len() + 1)
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?,
    )
    .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?;
    let event = WorkProductEventV1::new(WorkProductEventInputV1 {
        event_id: WorkProductEventId::new(format!("event.work-product.fixture.{}", sequence.get()))
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?,
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
    .map_err(|_| WorkProductAttemptAdmissionErrorV1::InvalidAdmission)?;
    let verified = VerifiedWorkGraphVersionV1::new(
        next.version(),
        sequence,
        event.source_watermark().clone(),
        digest_char('c'),
    )
    .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?;
    let commit = WorkProductEventCommitV1::new(event, verified)
        .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?;
    Ok((next, commit))
}

pub(super) fn attempt_key(
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> AttemptKey {
    (
        authority.clone(),
        format!(
            "{}/{}/{}",
            identity.task_id().as_str(),
            identity.run_id().as_str(),
            identity.attempt_id().as_str()
        ),
    )
}

pub(super) fn load_attempt(
    rows: &WorkProductAttemptRows,
    authority: &WorkAuthority,
    identity: &WorkAttemptIdentityV1,
) -> Result<WorkAttemptV1, WorkAttemptStorageError> {
    rows.attempts
        .get(&attempt_key(authority, identity))
        .ok_or(WorkAttemptStorageError::NotFoundOrNotAuthorized)
        .and_then(|payload| {
            serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)
        })
}

pub(super) fn insert_attempt(
    store: &Arc<Mutex<WorkProductAttemptRows>>,
    authority: &WorkAuthority,
    attempt: &WorkAttemptV1,
    concurrency: Option<&TopologyConcurrencyPolicyV1>,
) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
    let payload =
        serde_json::to_string(attempt).map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let mut rows = store
        .lock()
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let key = attempt_key(authority, attempt.identity());
    if let Some(existing) = rows.attempts.get(&key) {
        return if existing == &payload {
            serde_json::from_str(existing)
                .map(WorkAttemptInsertOutcome::Replayed)
                .map_err(|_| WorkAttemptStorageError::Unavailable)
        } else {
            Err(WorkAttemptStorageError::AttemptConflict)
        };
    }
    if let Some(concurrency) = concurrency
        && matches!(
            attempt_capacity(&rows, authority, attempt.identity().task_id(), concurrency)?
                .verdict(),
            WorkAttemptCapacityVerdictV1::Exhausted(_)
        )
    {
        return Err(WorkAttemptStorageError::CapacityExceeded);
    }
    rows.attempts.insert(key, payload);
    Ok(WorkAttemptInsertOutcome::Inserted)
}

pub(super) fn insert_synthesis_attempt(
    store: &Arc<Mutex<WorkProductAttemptRows>>,
    authority: &WorkAuthority,
    record: &WorkSynthesisAdmissionRecordV1,
    concurrency: Option<&TopologyConcurrencyPolicyV1>,
) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError> {
    let attempt = &record.result.attempt;
    let payload =
        serde_json::to_string(attempt).map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let mut rows = store
        .lock()
        .map_err(|_| WorkAttemptStorageError::Unavailable)?;
    let key = attempt_key(authority, attempt.identity());
    match (rows.attempts.get(&key), rows.syntheses.get(&key)) {
        (Some(existing_attempt), Some(existing))
            if existing_attempt == &payload && existing.request_digest == record.request_digest =>
        {
            return Ok(WorkSynthesisInsertOutcome::Replayed(Box::new(
                existing.result.clone(),
            )));
        }
        (Some(_), _) | (_, Some(_)) => return Err(WorkAttemptStorageError::AttemptConflict),
        (None, None) => {}
    }
    if let Some(concurrency) = concurrency
        && matches!(
            attempt_capacity(&rows, authority, attempt.identity().task_id(), concurrency)?
                .verdict(),
            WorkAttemptCapacityVerdictV1::Exhausted(_)
        )
    {
        return Err(WorkAttemptStorageError::CapacityExceeded);
    }
    rows.attempts.insert(key.clone(), payload);
    rows.syntheses.insert(key, record.clone());
    Ok(WorkSynthesisInsertOutcome::Inserted)
}

pub(super) fn attempt_capacity(
    rows: &WorkProductAttemptRows,
    authority: &WorkAuthority,
    task_id: &TaskId,
    concurrency: &TopologyConcurrencyPolicyV1,
) -> Result<WorkAttemptCapacityV1, WorkAttemptStorageError> {
    let mut global_active = 0_u64;
    let mut repository_active = 0_u64;
    let mut task_active = 0_u64;
    for ((row_authority, _), payload) in &rows.attempts {
        if row_authority.project_id() != authority.project_id() {
            continue;
        }
        let existing: WorkAttemptV1 =
            serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        if existing.is_terminal() {
            continue;
        }
        global_active += 1;
        if row_authority.repository_id() == authority.repository_id() {
            repository_active += 1;
            if existing.identity().task_id() == task_id {
                task_active += 1;
            }
        }
    }
    Ok(WorkAttemptCapacityV1::new(
        global_active,
        repository_active,
        task_active,
        concurrency.clone(),
    ))
}
