use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use tracedecay_application::{
    AcceptWorkTaskRequestV1, AuthorizedWorkProductScopeV1, CancellationContext,
    CapabilityGrantSnapshot, CreateWorkProductRequestV1, Deadline, DisclosureClass, OpaqueCursor,
    RequestContext, RequestId, ResolvedScope, SelectedWorkEvidenceV1,
    VerifiedWorkEvidenceExpansionV1, VerifiedWorkGraphVersionV1, WorkEvidenceExpandRequestV1,
    WorkEvidenceReadPortErrorV1, WorkEvidenceReadPortV1, WorkEvidenceSelectRequestV1,
    WorkGraphPublishPortErrorV1, WorkGraphPublishPortV1, WorkGraphReadModeV1,
    WorkGraphReadPortErrorV1, WorkGraphReadPortV1, WorkGraphReadRequestV1, WorkGraphReadV1,
    WorkGraphTimelineV1, WorkGraphVersionEntryV1, WorkHistoryCoverageV1, WorkHistoryReadPortV1,
    WorkHistoryRequestV1, WorkHistoryServiceV1, WorkHistoryV1, WorkProductApplicationErrorV1,
    WorkProductBindingV1, WorkProductEventAppendOutcomeV1, WorkProductEventDraftV1,
    WorkProductEventPortErrorV1, WorkProductEventPortV1, WorkProductEvidenceServiceV1,
    WorkProductExpectedAuthorityV1, WorkProductMutationIdentityV1, WorkProductMutationServiceV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductReadServiceV1, WorkProductRevisionPinsV1, WorkProductSelectionScopeV1,
    WorkRelationScopeV1,
};
use tracedecay_domain::{
    ActorId, BrainId, CatalogGenerationId, ConfigurationRevisionId, ManifestDigest,
    PolicyRevisionId, ProjectId, ProjectionGenerationId, RepositoryId, RetrievalAnchorId,
    SourceStoreId, TaskId, UserProfileId, UtcMicros, WorkCommandId, WorkGraphChangeV1,
    WorkGraphVersionV1, WorkProductEventEvidenceV1, WorkProductEventId, WorkProductEventInputV1,
    WorkProductEventPayloadV1, WorkProductEventSequenceV1, WorkProductEventV1, WorkProductGraphV1,
    WorkProductProjectionBundleV1, WorkProductSourceWatermarkV1, WorkProjectionSequenceV1,
    WorkRuntimeProjectionCoverageV1, WorkRuntimeProjectionV1, WorkTaskEvidenceCoverageV1,
    WorkTaskEvidenceV1, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn binding() -> WorkProductBindingV1 {
    WorkProductBindingV1::new(
        CapabilityId::new("capability.work.graph.read").unwrap(),
        UseCaseId::new("use-case.work.graph.read").unwrap(),
    )
}

fn repository_selection() -> WorkProductSelectionScopeV1 {
    WorkProductSelectionScopeV1::relations(BTreeSet::from([WorkRelationScopeV1::Repository {
        project_id: id("project.work.fixture"),
        repository_id: id("repository.work.fixture"),
    }]))
    .unwrap()
}

fn context(authorized: bool) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.work.fixture"),
        id::<RepositoryId>("repository.work.fixture"),
        id::<WorktreeId>("worktree.work.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new(if authorized {
        "capability.work.graph.read"
    } else {
        "capability.unrelated.read"
    })
    .unwrap();
    let use_case = UseCaseId::new(if authorized {
        "use-case.work.graph.read"
    } else {
        "use-case.unrelated.read"
    })
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.fixture"),
        1,
        digest('a'),
        id::<ActorId>("actor.work.issuer"),
        UtcMicros(-1_000),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.work.requester"),
        scope,
        grant,
        RequestId::new("request.work.fixture").unwrap(),
        Deadline::new(UtcMicros(500)).unwrap(),
        CancellationContext::active("cancel.work.fixture").unwrap(),
    )
    .unwrap()
}

#[derive(Default)]
struct RegisteredOwner {
    selections: Mutex<Vec<WorkProductSelectionScopeV1>>,
}

impl WorkProductOwnerAuthorizationPortV1 for RegisteredOwner {
    fn authorize_scope(
        &self,
        context: &RequestContext,
        selection: &WorkProductSelectionScopeV1,
        _observed_at: UtcMicros,
    ) -> Result<AuthorizedWorkProductScopeV1, WorkProductOwnerAuthorizationErrorV1> {
        let admitted = match selection {
            WorkProductSelectionScopeV1::ProfileOwnedNoGit => true,
            WorkProductSelectionScopeV1::Relations { relation_scopes } => {
                relation_scopes.iter().all(|relation| match relation {
                    WorkRelationScopeV1::Project { project_id } => {
                        project_id == &context.scope().project_id
                    }
                    WorkRelationScopeV1::Repository {
                        project_id,
                        repository_id,
                    } => {
                        project_id == &context.scope().project_id
                            && repository_id == &context.scope().repository_id
                    }
                })
            }
        };
        if !admitted {
            return Err(WorkProductOwnerAuthorizationErrorV1::NotAuthorized);
        }
        self.selections.lock().unwrap().push(selection.clone());
        AuthorizedWorkProductScopeV1::new(
            id::<BrainId>("brain.work.registered"),
            id::<UserProfileId>("profile.work.registered"),
            selection.clone(),
        )
        .map_err(|_| WorkProductOwnerAuthorizationErrorV1::Unavailable)
    }
}

fn graph(version: u64) -> WorkProductGraphV1 {
    WorkProductGraphV1::new(
        WorkGraphVersionV1::new(version).unwrap(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap()
}

fn verified_version(version: u64) -> VerifiedWorkGraphVersionV1 {
    verified_version_with(
        version,
        WorkProductSourceWatermarkV1::new(BTreeMap::new()).unwrap(),
        'b',
    )
}

fn verified_version_with(
    version: u64,
    source_watermark: WorkProductSourceWatermarkV1,
    digest_byte: char,
) -> VerifiedWorkGraphVersionV1 {
    VerifiedWorkGraphVersionV1::new(
        WorkGraphVersionV1::new(version).unwrap(),
        WorkProductEventSequenceV1::new(version).unwrap(),
        source_watermark,
        digest(digest_byte),
    )
    .unwrap()
}

fn entry(
    version: u64,
    valid_at: UtcMicros,
    observed_at: UtcMicros,
    projected_at: UtcMicros,
) -> WorkGraphVersionEntryV1 {
    let graph = graph(version);
    let runtime = WorkRuntimeProjectionV1::new(
        graph.version(),
        ProjectionGenerationId::new(format!("generation.work.fixture.{version}")).unwrap(),
        WorkProjectionSequenceV1::new(version),
        projected_at,
        Vec::new(),
        WorkRuntimeProjectionCoverageV1::Complete,
    )
    .unwrap();
    let projections =
        WorkProductProjectionBundleV1::from_graph(&graph, &runtime, projected_at).unwrap();
    WorkGraphVersionEntryV1::new(
        valid_at,
        observed_at,
        projected_at,
        verified_version(version),
        graph,
        runtime,
        projections,
    )
    .unwrap()
}

fn mutation_identity(
    expected_authority: WorkProductExpectedAuthorityV1,
) -> WorkProductMutationIdentityV1 {
    WorkProductMutationIdentityV1 {
        expected_authority,
        command_id: id::<WorkCommandId>("command.work.fixture"),
        causation_event_id: None,
        evidence: Vec::new(),
        occurred_at: UtcMicros(100),
        revisions: WorkProductRevisionPinsV1 {
            policy_revision_id: id::<PolicyRevisionId>("policy.work.fixture"),
            configuration_revision_id: id::<ConfigurationRevisionId>("config.work.fixture"),
            catalog_generation_id: id::<CatalogGenerationId>("catalog.work.fixture"),
        },
    }
}

fn event_from_draft(draft: &WorkProductEventDraftV1) -> WorkProductEventV1 {
    WorkProductEventV1::new(WorkProductEventInputV1 {
        event_id: WorkProductEventId::new("event.work.fixture").unwrap(),
        sequence: WorkProductEventSequenceV1::new(1).unwrap(),
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
    .unwrap()
}

fn event_for_replay(
    context: &tracedecay_application::WorkProductPortContextV1,
    mutation: &WorkProductMutationIdentityV1,
    payload: WorkProductEventPayloadV1,
    canonical_input_digest: ManifestDigest,
) -> WorkProductEventV1 {
    let result_graph_version = match &payload {
        WorkProductEventPayloadV1::Created { .. } => WorkGraphVersionV1::initial(),
        WorkProductEventPayloadV1::Changed { .. } => match &mutation.expected_authority {
            WorkProductExpectedAuthorityV1::Verified { verified_version } => {
                verified_version.graph_version().next().unwrap()
            }
            WorkProductExpectedAuthorityV1::NoPriorGraph => panic!("change requires authority"),
        },
    };
    let (expected_graph_version, source_watermark) = match &mutation.expected_authority {
        WorkProductExpectedAuthorityV1::NoPriorGraph => (
            None,
            WorkProductSourceWatermarkV1::new(BTreeMap::new()).unwrap(),
        ),
        WorkProductExpectedAuthorityV1::Verified { verified_version } => (
            Some(verified_version.graph_version()),
            verified_version.source_watermark().clone(),
        ),
    };
    event_from_draft(&WorkProductEventDraftV1 {
        actor_id: context.actor().clone(),
        owner_scope: tracedecay_domain::WorkProductProfileScopeV1 {
            brain_id: context.authorized_scope().owner_brain_id().clone(),
            profile_id: context.authorized_scope().owner_profile_id().clone(),
        },
        authorized_relation_scopes: context
            .authorized_scope()
            .selection()
            .relation_scopes()
            .map_or_else(Vec::new, |relations| relations.iter().cloned().collect()),
        expected_graph_version,
        result_graph_version,
        command_id: mutation.command_id.clone(),
        canonical_input_digest,
        causation_event_id: mutation.causation_event_id.clone(),
        evidence: mutation.evidence.clone(),
        source_watermark,
        occurred_at: mutation.occurred_at,
        policy_revision_id: mutation.revisions.policy_revision_id.clone(),
        configuration_revision_id: mutation.revisions.configuration_revision_id.clone(),
        catalog_generation_id: mutation.revisions.catalog_generation_id.clone(),
        payload,
    })
}

#[derive(Default)]
struct RecordingEventPort {
    replay: Mutex<Option<(WorkProductMutationIdentityV1, WorkProductEventPayloadV1)>>,
    replay_calls: AtomicUsize,
    append_calls: AtomicUsize,
}

impl WorkProductEventPortV1 for RecordingEventPort {
    fn replay(
        &self,
        context: &tracedecay_application::WorkProductPortContextV1,
        _command_id: &WorkCommandId,
        canonical_input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventV1>, WorkProductEventPortErrorV1> {
        self.replay_calls.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .replay
            .lock()
            .unwrap()
            .clone()
            .map(|(mutation, payload)| {
                event_for_replay(context, &mutation, payload, canonical_input_digest.clone())
            }))
    }

    fn append_with_outbox(
        &self,
        _context: &tracedecay_application::WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventAppendOutcomeV1, WorkProductEventPortErrorV1> {
        self.append_calls.fetch_add(1, Ordering::Relaxed);
        Ok(WorkProductEventAppendOutcomeV1::Appended(event_from_draft(
            draft,
        )))
    }
}

#[derive(Default)]
struct RecordingPublisher {
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl WorkGraphPublishPortV1 for RecordingPublisher {
    fn publish_event(
        &self,
        _context: &tracedecay_application::WorkProductPortContextV1,
        event: &WorkProductEventV1,
    ) -> Result<VerifiedWorkGraphVersionV1, WorkGraphPublishPortErrorV1> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.fail.load(Ordering::Relaxed) {
            return Err(WorkGraphPublishPortErrorV1::Unavailable);
        }
        VerifiedWorkGraphVersionV1::new(
            event.result_graph_version(),
            event.sequence(),
            event.source_watermark().clone(),
            digest('d'),
        )
        .map_err(|_| WorkGraphPublishPortErrorV1::Unavailable)
    }
}

struct FixedEvidencePort {
    evidence: WorkTaskEvidenceV1,
    verified_version: VerifiedWorkGraphVersionV1,
}

#[derive(Default)]
struct PagingHistoryPort {
    calls: AtomicUsize,
}

impl WorkHistoryReadPortV1 for PagingHistoryPort {
    fn read_history(
        &self,
        context: &tracedecay_application::WorkProductPortContextV1,
        request: &WorkHistoryRequestV1,
    ) -> Result<WorkHistoryV1, WorkProductApplicationErrorV1> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let coverage = if request.continuation.is_none() {
            WorkHistoryCoverageV1::Partial {
                returned: 0,
                continuation: OpaqueCursor::new("cursor.work.history.next").unwrap(),
            }
        } else {
            WorkHistoryCoverageV1::Complete { returned: 0 }
        };
        Ok(WorkHistoryV1 {
            authorized_scope: context.authorized_scope().clone(),
            events: Vec::new(),
            coverage,
        })
    }
}

impl WorkEvidenceReadPortV1 for FixedEvidencePort {
    fn select_task_evidence(
        &self,
        _context: &tracedecay_application::WorkProductPortContextV1,
        _request: &WorkEvidenceSelectRequestV1,
    ) -> Result<SelectedWorkEvidenceV1, WorkEvidenceReadPortErrorV1> {
        Ok(SelectedWorkEvidenceV1 {
            verified_version: self.verified_version.clone(),
            evidence: self.evidence.clone(),
        })
    }

    fn expand_task_evidence(
        &self,
        _context: &tracedecay_application::WorkProductPortContextV1,
        _request: &WorkEvidenceExpandRequestV1,
    ) -> Result<VerifiedWorkEvidenceExpansionV1, WorkEvidenceReadPortErrorV1> {
        Err(WorkEvidenceReadPortErrorV1::NotFoundOrNotAuthorized)
    }
}

#[derive(Default)]
struct RecordingGraphPort {
    calls: AtomicUsize,
    requests: Mutex<Vec<WorkGraphReadRequestV1>>,
    return_wrong_owner: AtomicBool,
    paginate: AtomicBool,
}

impl WorkGraphReadPortV1 for RecordingGraphPort {
    fn read_graph(
        &self,
        context: &tracedecay_application::WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, WorkGraphReadPortErrorV1> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.requests.lock().unwrap().push(request.clone());
        let scope = if self.return_wrong_owner.load(Ordering::Relaxed) {
            AuthorizedWorkProductScopeV1::new(
                id("brain.work.foreign"),
                id("profile.work.foreign"),
                request.selection.clone(),
            )
            .unwrap()
        } else {
            context.authorized_scope().clone()
        };
        Ok(match request.mode {
            WorkGraphReadModeV1::Current => WorkGraphReadV1::Current {
                authorized_scope: scope,
                snapshot: entry(1, UtcMicros(-10), UtcMicros(0), request.observed_at),
            },
            WorkGraphReadModeV1::AsOf { valid_at } => WorkGraphReadV1::AsOf {
                authorized_scope: scope,
                snapshot: entry(1, UtcMicros(valid_at.0 - 1), valid_at, request.observed_at),
            },
            WorkGraphReadModeV1::Evolution {
                from_valid_at,
                through_valid_at,
            } => WorkGraphReadV1::Evolution {
                authorized_scope: scope,
                timeline: if self.paginate.load(Ordering::Relaxed) && request.continuation.is_none()
                {
                    WorkGraphTimelineV1::partial(
                        vec![entry(1, from_valid_at, from_valid_at, request.observed_at)],
                        OpaqueCursor::new("cursor.work.timeline.next").unwrap(),
                    )
                    .unwrap()
                } else {
                    WorkGraphTimelineV1::complete(vec![
                        entry(1, from_valid_at, from_valid_at, request.observed_at),
                        entry(2, through_valid_at, through_valid_at, request.observed_at),
                    ])
                    .unwrap()
                },
            },
            WorkGraphReadModeV1::Forensic {
                from_observed_at,
                through_observed_at,
            } => WorkGraphReadV1::Forensic {
                authorized_scope: scope,
                timeline: WorkGraphTimelineV1::complete(vec![
                    entry(
                        1,
                        UtcMicros(from_observed_at.0 - 1),
                        from_observed_at,
                        request.observed_at,
                    ),
                    entry(
                        2,
                        UtcMicros(through_observed_at.0 - 1),
                        through_observed_at,
                        request.observed_at,
                    ),
                ])
                .unwrap(),
            },
        })
    }
}

#[test]
fn graph_read_authorizes_before_calling_the_topology_port() {
    let graph = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let service = WorkProductReadServiceV1::new(&graph, &owner, binding());

    assert_eq!(
        service
            .read_graph(
                &context(false),
                WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(100)),
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::NotAuthorized
    );
    assert_eq!(graph.calls.load(Ordering::Relaxed), 0);
    assert!(owner.selections.lock().unwrap().is_empty());
}

#[test]
fn registered_owner_prevents_profile_spoof_and_rejects_port_scope_leakage() {
    let graph = RecordingGraphPort::default();
    graph.return_wrong_owner.store(true, Ordering::Relaxed);
    let owner = RegisteredOwner::default();
    let service = WorkProductReadServiceV1::new(&graph, &owner, binding());

    assert_eq!(
        service
            .read_graph(
                &context(true),
                WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(100)),
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::GraphAuthorityUnavailable
    );
}

#[test]
fn as_of_accepts_the_latest_authoritative_version_before_the_requested_time() {
    let graph = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let service = WorkProductReadServiceV1::new(&graph, &owner, binding());
    let result = service
        .read_graph(
            &context(true),
            WorkGraphReadRequestV1::as_of(repository_selection(), UtcMicros(10), UtcMicros(100))
                .unwrap(),
        )
        .unwrap();
    assert_eq!(result.entries()[0].valid_at(), UtcMicros(9));
}

#[test]
fn evolution_and_forensic_return_ordered_multi_version_entries_with_projections() {
    let graph = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let service = WorkProductReadServiceV1::new(&graph, &owner, binding());

    let evolution = service
        .read_graph(
            &context(true),
            WorkGraphReadRequestV1::evolution(
                repository_selection(),
                UtcMicros(10),
                UtcMicros(20),
                UtcMicros(100),
            )
            .unwrap(),
        )
        .unwrap();
    let forensic = service
        .read_graph(
            &context(true),
            WorkGraphReadRequestV1::forensic(
                repository_selection(),
                UtcMicros(30),
                UtcMicros(40),
                UtcMicros(101),
            )
            .unwrap(),
        )
        .unwrap();

    for outcome in [&evolution, &forensic] {
        assert_eq!(outcome.entries().len(), 2);
        for entry in outcome.entries() {
            assert_eq!(
                entry.projections().graph_version(),
                entry.verified_version().graph_version()
            );
        }
    }
    assert_eq!(evolution.entries()[0].projected_at(), UtcMicros(100));
    assert_eq!(forensic.entries()[1].projected_at(), UtcMicros(101));
}

#[test]
fn cancellation_and_deadline_fail_before_owner_or_topology_io() {
    let graph = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let service = WorkProductReadServiceV1::new(&graph, &owner, binding());

    assert_eq!(
        service
            .read_graph(
                &context(true),
                WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(600)),
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::TimedOut
    );
    let cancelled = context(true).with_cancellation(
        CancellationContext::cancelled("cancel.work.fixture", UtcMicros(50)).unwrap(),
    );
    assert_eq!(
        service
            .read_graph(
                &cancelled,
                WorkGraphReadRequestV1::current(repository_selection(), UtcMicros(100)),
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::Cancelled
    );
    assert_eq!(graph.calls.load(Ordering::Relaxed), 0);
    assert!(owner.selections.lock().unwrap().is_empty());
}

#[test]
fn timeline_continuation_is_bounded_and_reauthorized_on_every_page() {
    let graph = RecordingGraphPort::default();
    graph.paginate.store(true, Ordering::Relaxed);
    let owner = RegisteredOwner::default();
    let service = WorkProductReadServiceV1::new(&graph, &owner, binding());
    let mut first_request = WorkGraphReadRequestV1::evolution(
        repository_selection(),
        UtcMicros(10),
        UtcMicros(20),
        UtcMicros(100),
    )
    .unwrap();
    let first = service
        .read_graph(&context(true), first_request.clone())
        .unwrap();
    let WorkGraphReadV1::Evolution { timeline, .. } = first else {
        panic!("expected evolution page");
    };
    assert_eq!(timeline.entries().len(), 1);
    first_request.continuation = timeline.continuation().cloned();

    let second = service.read_graph(&context(true), first_request).unwrap();
    assert_eq!(second.entries().len(), 2);
    assert_eq!(owner.selections.lock().unwrap().len(), 2);
    assert_eq!(graph.calls.load(Ordering::Relaxed), 2);
}

#[test]
fn deserialized_empty_relation_scope_and_future_temporal_bounds_are_rejected() {
    let invalid_scope: WorkProductSelectionScopeV1 =
        serde_json::from_str(r#"{"selection":"relations","relation_scopes":[]}"#).unwrap();
    let graph = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let service = WorkProductReadServiceV1::new(&graph, &owner, binding());

    assert_eq!(
        service
            .read_graph(
                &context(true),
                WorkGraphReadRequestV1::current(invalid_scope, UtcMicros(100)),
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::InvalidRequest
    );
    assert_eq!(
        WorkGraphReadRequestV1::forensic(
            repository_selection(),
            UtcMicros(10),
            UtcMicros(101),
            UtcMicros(100),
        )
        .unwrap_err(),
        WorkProductApplicationErrorV1::InvalidRequest
    );
    assert_eq!(graph.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn same_command_replays_with_reordered_canonical_evidence_before_head_read() {
    let context = context(true);
    let selection = WorkProductSelectionScopeV1::ProfileOwnedNoGit;
    let payload = WorkProductEventPayloadV1::Changed {
        change: Box::new(WorkGraphChangeV1::TaskAccepted {
            task_id: id("task.work.fixture"),
            evidence_by_criterion: BTreeMap::new(),
            accepted_at: UtcMicros(100),
        }),
    };
    let evidence = [
        WorkProductEventEvidenceV1 {
            source_store_id: id::<SourceStoreId>("source.work.a"),
            anchor_id: id::<RetrievalAnchorId>("anchor.work.a"),
            evidence_digest: digest('1'),
        },
        WorkProductEventEvidenceV1 {
            source_store_id: id::<SourceStoreId>("source.work.b"),
            anchor_id: id::<RetrievalAnchorId>("anchor.work.b"),
            evidence_digest: digest('2'),
        },
    ];
    let watermark = WorkProductSourceWatermarkV1::new(BTreeMap::from([
        (evidence[0].source_store_id.clone(), 1),
        (evidence[1].source_store_id.clone(), 1),
    ]))
    .unwrap();
    let mut mutation = mutation_identity(WorkProductExpectedAuthorityV1::Verified {
        verified_version: verified_version_with(1, watermark, 'b'),
    });
    mutation.evidence = evidence.iter().rev().cloned().collect();
    let mut replay_mutation = mutation.clone();
    replay_mutation.evidence.reverse();
    let graph_port = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let events = RecordingEventPort::default();
    *events.replay.lock().unwrap() = Some((replay_mutation, payload));
    let publisher = RecordingPublisher::default();
    let service = WorkProductMutationServiceV1::new(&graph_port, &owner, &events, &publisher);

    let receipt = service
        .accept_task(
            &context,
            &binding(),
            AcceptWorkTaskRequestV1 {
                selection,
                task_id: id("task.work.fixture"),
                evidence_by_criterion: BTreeMap::new(),
                mutation,
            },
        )
        .unwrap();

    assert!(receipt.replayed());
    assert_eq!(events.replay_calls.load(Ordering::Relaxed), 1);
    assert_eq!(events.append_calls.load(Ordering::Relaxed), 0);
    assert_eq!(graph_port.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn create_appends_without_requiring_an_existing_head() {
    let context = context(true);
    let selection = WorkProductSelectionScopeV1::ProfileOwnedNoGit;
    let initial_graph = graph(1);
    let mutation = mutation_identity(WorkProductExpectedAuthorityV1::NoPriorGraph);
    let graph_port = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let events = RecordingEventPort::default();
    let publisher = RecordingPublisher::default();
    let service = WorkProductMutationServiceV1::new(&graph_port, &owner, &events, &publisher);

    let receipt = service
        .create(
            &context,
            &binding(),
            CreateWorkProductRequestV1 {
                selection,
                initial_graph,
                mutation,
            },
        )
        .unwrap();

    assert!(!receipt.replayed());
    assert!(matches!(
        receipt.event().payload(),
        WorkProductEventPayloadV1::Created { .. }
    ));
    assert_eq!(receipt.event().expected_graph_version(), None);
    assert_eq!(graph_port.calls.load(Ordering::Relaxed), 0);
    assert_eq!(events.append_calls.load(Ordering::Relaxed), 1);
}

#[test]
fn changed_replay_with_different_payload_is_an_idempotency_conflict() {
    let context = context(true);
    let selection = repository_selection();
    let mutation = mutation_identity(WorkProductExpectedAuthorityV1::Verified {
        verified_version: verified_version(1),
    });
    let replayed_payload = WorkProductEventPayloadV1::Changed {
        change: Box::new(WorkGraphChangeV1::TaskAccepted {
            task_id: id("task.work.different"),
            evidence_by_criterion: BTreeMap::new(),
            accepted_at: UtcMicros(100),
        }),
    };
    let graph_port = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let events = RecordingEventPort::default();
    *events.replay.lock().unwrap() = Some((mutation.clone(), replayed_payload));
    let publisher = RecordingPublisher::default();
    let service = WorkProductMutationServiceV1::new(&graph_port, &owner, &events, &publisher);

    assert_eq!(
        service
            .accept_task(
                &context,
                &binding(),
                AcceptWorkTaskRequestV1 {
                    selection,
                    task_id: id::<TaskId>("task.work.requested"),
                    evidence_by_criterion: BTreeMap::new(),
                    mutation,
                },
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::IdempotencyConflict
    );
    assert_eq!(graph_port.calls.load(Ordering::Relaxed), 0);
    assert_eq!(events.append_calls.load(Ordering::Relaxed), 0);
    assert_eq!(publisher.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn changed_version_generation_or_watermark_fails_before_event_append() {
    let context = context(true);
    let selection = repository_selection();
    let changed_watermark = WorkProductSourceWatermarkV1::new(BTreeMap::from([(
        id::<SourceStoreId>("source.work.changed"),
        1,
    )]))
    .unwrap();
    let expected_versions = [
        verified_version(2),
        verified_version_with(
            1,
            WorkProductSourceWatermarkV1::new(BTreeMap::new()).unwrap(),
            'e',
        ),
        verified_version_with(1, changed_watermark, 'b'),
    ];
    let graph_port = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let events = RecordingEventPort::default();
    let publisher = RecordingPublisher::default();
    let service = WorkProductMutationServiceV1::new(&graph_port, &owner, &events, &publisher);

    for expected_version in expected_versions {
        let mutation = mutation_identity(WorkProductExpectedAuthorityV1::Verified {
            verified_version: expected_version,
        });
        assert_eq!(
            service
                .accept_task(
                    &context,
                    &binding(),
                    AcceptWorkTaskRequestV1 {
                        selection: selection.clone(),
                        task_id: id("task.work.fixture"),
                        evidence_by_criterion: BTreeMap::new(),
                        mutation,
                    },
                )
                .unwrap_err(),
            WorkProductApplicationErrorV1::VersionConflict
        );
    }
    assert_eq!(events.replay_calls.load(Ordering::Relaxed), 3);
    assert_eq!(graph_port.calls.load(Ordering::Relaxed), 3);
}

#[test]
fn durable_append_followed_by_publish_failure_requires_reconciliation() {
    let context = context(true);
    let selection = WorkProductSelectionScopeV1::ProfileOwnedNoGit;
    let initial_graph = graph(1);
    let mutation = mutation_identity(WorkProductExpectedAuthorityV1::NoPriorGraph);
    let graph_port = RecordingGraphPort::default();
    let owner = RegisteredOwner::default();
    let events = RecordingEventPort::default();
    let publisher = RecordingPublisher::default();
    publisher.fail.store(true, Ordering::Relaxed);
    let service = WorkProductMutationServiceV1::new(&graph_port, &owner, &events, &publisher);

    assert_eq!(
        service
            .create(
                &context,
                &binding(),
                CreateWorkProductRequestV1 {
                    selection,
                    initial_graph,
                    mutation,
                },
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::ReconciliationRequired
    );
    assert_eq!(events.append_calls.load(Ordering::Relaxed), 1);
    assert_eq!(publisher.calls.load(Ordering::Relaxed), 1);
}

#[test]
fn evidence_port_cannot_smuggle_invalid_deserialized_coverage() {
    let evidence: WorkTaskEvidenceV1 = serde_json::from_value(serde_json::json!({
        "task_id": "task.work.fixture",
        "graph_version": 1,
        "links": [],
        "coverage": {
            "state": "complete",
            "returned": 0,
            "available": 1
        }
    }))
    .unwrap();
    let port = FixedEvidencePort {
        evidence,
        verified_version: verified_version(1),
    };
    let owner = RegisteredOwner::default();
    let service = WorkProductEvidenceServiceV1::new(&port, &owner);

    assert_eq!(
        service
            .select(
                &context(true),
                &binding(),
                WorkEvidenceSelectRequestV1 {
                    selection: repository_selection(),
                    task_id: id("task.work.fixture"),
                    verified_version: verified_version(1),
                    limit: 1,
                    observed_at: UtcMicros(100),
                },
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable
    );
}

#[test]
fn evidence_port_must_return_the_exact_verified_generation() {
    let port = FixedEvidencePort {
        evidence: WorkTaskEvidenceV1::new(
            id("task.work.fixture"),
            WorkGraphVersionV1::initial(),
            Vec::new(),
            WorkTaskEvidenceCoverageV1::Complete {
                returned: 0,
                available: 0,
            },
        )
        .unwrap(),
        verified_version: verified_version(2),
    };
    let owner = RegisteredOwner::default();
    let service = WorkProductEvidenceServiceV1::new(&port, &owner);

    assert_eq!(
        service
            .select(
                &context(true),
                &binding(),
                WorkEvidenceSelectRequestV1 {
                    selection: repository_selection(),
                    task_id: id("task.work.fixture"),
                    verified_version: verified_version(1),
                    limit: 1,
                    observed_at: UtcMicros(100),
                },
            )
            .unwrap_err(),
        WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable
    );
}

#[test]
fn history_continuation_reauthorizes_each_page() {
    let history = PagingHistoryPort::default();
    let owner = RegisteredOwner::default();
    let service = WorkHistoryServiceV1::new(&history, &owner);
    let mut request = WorkHistoryRequestV1 {
        selection: repository_selection(),
        limit: 10,
        continuation: None,
        observed_at: UtcMicros(100),
    };

    let first = service
        .read(&context(true), &binding(), request.clone())
        .unwrap();
    let WorkHistoryCoverageV1::Partial { continuation, .. } = first.coverage else {
        panic!("expected partial history page");
    };
    request.continuation = Some(continuation);
    let second = service.read(&context(true), &binding(), request).unwrap();

    assert!(matches!(
        second.coverage,
        WorkHistoryCoverageV1::Complete { returned: 0 }
    ));
    assert_eq!(history.calls.load(Ordering::Relaxed), 2);
    assert_eq!(owner.selections.lock().unwrap().len(), 2);
}
