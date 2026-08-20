#![allow(dead_code)]

mod work_product_attempt_support;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    ApplicationOperation, AuthorityReceipt, AuthorizationPort, AuthorizationPortOutcome,
    AuthorizationRequest, AuthorizedWorkProductScopeV1, CancellationContext,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, EvidenceCoverage, EvidenceDomain,
    PageState, PolicyDecisionRef, RequestContext, RequestId, ResolvedScope, ResultContractRef,
    RetrievalEvidence, SourceAuthorizationSnapshot, StartWorkAttemptCommand, TemporalState,
    VerifiedWorkGraphVersionV1, WorkAttemptAdmissionKind, WorkAttemptCapacityV1,
    WorkAttemptCapacityVerdictV1, WorkAttemptEvidenceRecordV1, WorkAttemptInsertOutcome,
    WorkAttemptListPageV1, WorkAttemptStorageError, WorkAttemptStoragePort,
    WorkGraphReadPortErrorV1, WorkGraphReadPortV1, WorkGraphReadRequestV1, WorkGraphReadV1,
    WorkGraphVersionEntryV1, WorkProductAttemptAdmissionErrorV1,
    WorkProductAttemptAdmissionOutcomeV1, WorkProductAttemptAdmissionPortV1,
    WorkProductAttemptAdmissionV1, WorkProductBindingV1, WorkProductEventCommitV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductPortContextV1, WorkProductRevisionPinsV1, WorkProductSelectionScopeV1,
    WorkRelationScopeV1, WorkSynthesisAdmissionRecordV1, WorkSynthesisAdmissionStoragePort,
    WorkSynthesisInsertOutcome,
};
use tracedecay_domain::configuration::TopologyConcurrencyPolicyV1;
use tracedecay_domain::{
    ActorId, BrainId, ComponentVersion, ManifestDigest, MilestoneId, ProjectId,
    ProjectionGenerationId, ProposalId, RefId, RepositoryId, TaskId, UserProfileId, UtcMicros,
    WorkAttemptIdentityV1, WorkAttemptStateV1, WorkAttemptV1, WorkAuthority,
    WorkCancellationStateV1, WorkExecutionEnvelopeV1, WorkFenceEpochV1, WorkGraphChangeV1,
    WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1, WorkItemV1,
    WorkLeaseFenceV1, WorkLeaseId, WorkMilestoneV1, WorkPlanId, WorkPlanV1, WorkProductEventId,
    WorkProductEventInputV1, WorkProductEventPayloadV1, WorkProductEventSequenceV1,
    WorkProductEventV1, WorkProductGraphV1, WorkProductProfileScopeV1,
    WorkProductProjectionBundleV1, WorkProductSourceWatermarkV1, WorkProposalV1,
    WorkRecoveryStateV1, WorkRouteDecisionV1, WorkRuntimeProjectionCoverageV1,
    WorkRuntimeProjectionV1, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1, WorktreeId,
};
use tracedecay_policy::authorization::{
    SourceAuthorizationInputV1, SourceAuthorizationTruthTableV1,
};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, SortContractId, UseCaseId};

use work_product_attempt_support::{
    append_seed_change, append_seed_event, attempt_capacity, attempt_key, digest_char,
    graph_with_task, insert_attempt, insert_synthesis_attempt, load_attempt, product_commit_for,
    work_item,
};

pub const SHA256_A: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const SHA256_B: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SOURCE_AUTHORIZATION_TRUTH_TABLES: &str =
    include_str!("../../../tracedecay-policy/tests/fixtures/source_authorization/core.json");

pub fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity is canonical")
}

pub fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).expect("fixture digest is canonical")
}

/// Platform-absolute fixture root: the work and registered-root contracts
/// require `Path::is_absolute`, which a bare `/...` literal fails on Windows.
pub fn fixture_abs_root(posix: &str) -> String {
    if cfg!(windows) {
        format!("C:{}", posix.replace('/', "\\"))
    } else {
        posix.to_owned()
    }
}

/// Canonical digest fixture for the Work-attempt product journey.
pub fn work_digest(value: char) -> ManifestDigest {
    digest_char(value)
}

pub fn result_contract() -> ResultContractRef {
    ResultContractRef::new(
        SchemaId::new("schema.application.fixture.result").unwrap(),
        1,
    )
    .unwrap()
}

pub fn operation() -> ApplicationOperation {
    ApplicationOperation::new(
        CapabilityId::new("capability.application.symbol-search").unwrap(),
        UseCaseId::new("use-case.application.symbol-search").unwrap(),
        result_contract(),
        true,
    )
}

pub fn scope() -> ResolvedScope {
    ResolvedScope::new(
        id::<ProjectId>("project.fixture"),
        id::<RepositoryId>("repository.fixture"),
        id::<WorktreeId>("worktree.fixture"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap()
}

pub fn context(operation: &ApplicationOperation) -> RequestContext {
    let scope = scope();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.fixture"),
        1,
        digest(SHA256_A),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.requester"),
        scope,
        grant,
        RequestId::new("request.fixture").unwrap(),
        Deadline::new(UtcMicros(500)).unwrap(),
        CancellationContext::active("cancel.fixture").unwrap(),
    )
    .unwrap()
}

/// Request authority used by the Work-attempt product fixture.
pub fn work_attempt_context(project: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.attempt.fixture"),
        id::<WorktreeId>("worktree.attempt.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.fixture").unwrap();
    let use_case = UseCaseId::new("use-case.work.fixture").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.fixture"),
        1,
        work_digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>(actor),
        scope,
        grant,
        RequestId::new(format!("request.{project}.{actor}")).unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active(format!("cancel.{project}.{actor}")).unwrap(),
    )
    .unwrap()
}

pub fn authority(context: &RequestContext) -> AuthorityReceipt {
    AuthorityReceipt::from_context(
        context,
        PolicyDecisionRef::new(
            "policy.fixture",
            1,
            digest(SHA256_B),
            ComponentVersion::new("policy.evaluator.v1").unwrap(),
        )
        .unwrap(),
        UtcMicros(2),
    )
    .unwrap()
}

pub fn source_authorization_input(name: &str) -> SourceAuthorizationInputV1 {
    serde_json::from_str::<Vec<SourceAuthorizationTruthTableV1>>(SOURCE_AUTHORIZATION_TRUTH_TABLES)
        .expect("checked-in source authorization truth tables deserialize")
        .into_iter()
        .find(|row| row.name == name)
        .unwrap_or_else(|| panic!("source authorization fixture {name} exists"))
        .input
}

pub fn authorized_source_input() -> SourceAuthorizationInputV1 {
    source_authorization_input("project_authorized_live")
}

pub fn source_snapshot(input: SourceAuthorizationInputV1) -> SourceAuthorizationSnapshot {
    SourceAuthorizationSnapshot::new(input, true)
}

pub struct StaticAuthorizationPort {
    outcome: AuthorizationPortOutcome,
}

impl StaticAuthorizationPort {
    pub fn authorized() -> Self {
        Self::new(AuthorizationPortOutcome::Snapshot(Box::new(
            source_snapshot(authorized_source_input()),
        )))
    }

    pub fn new(outcome: AuthorizationPortOutcome) -> Self {
        Self { outcome }
    }
}

impl AuthorizationPort for StaticAuthorizationPort {
    fn source_authorization_snapshot(
        &self,
        _request: &AuthorizationRequest<'_>,
    ) -> AuthorizationPortOutcome {
        self.outcome.clone()
    }
}

pub struct SequencedAuthorizationPort {
    outcomes: RefCell<VecDeque<AuthorizationPortOutcome>>,
}

impl SequencedAuthorizationPort {
    pub fn snapshots(snapshots: impl IntoIterator<Item = SourceAuthorizationSnapshot>) -> Self {
        Self {
            outcomes: RefCell::new(
                snapshots
                    .into_iter()
                    .map(|snapshot| AuthorizationPortOutcome::Snapshot(Box::new(snapshot)))
                    .collect(),
            ),
        }
    }
}

impl AuthorizationPort for SequencedAuthorizationPort {
    fn source_authorization_snapshot(
        &self,
        _request: &AuthorizationRequest<'_>,
    ) -> AuthorizationPortOutcome {
        self.outcomes
            .borrow_mut()
            .pop_front()
            .expect("authorization snapshot sequence is not exhausted")
    }
}

pub fn evidence<T>(payload: T) -> RetrievalEvidence<T> {
    RetrievalEvidence {
        payload: Some(payload),
        temporal: TemporalState::current(UtcMicros(2)),
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 1, 1, 1).unwrap(),
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.symbol.fixture.v1").unwrap(),
            1,
            Some(1),
            1,
        )
        .unwrap(),
        finished_at: UtcMicros(3),
        budget: Default::default(),
        cancellation: None,
    }
}

/// Canonical repository relation selected by Work-product attempt admission.
pub fn work_product_selection(context: &RequestContext) -> WorkProductSelectionScopeV1 {
    WorkProductSelectionScopeV1::relations(BTreeSet::from([WorkRelationScopeV1::Repository {
        project_id: context.scope().project_id.clone(),
        repository_id: context.scope().repository_id.clone(),
    }]))
    .expect("request scope produces a canonical Work product selection")
}

pub fn work_product_binding() -> WorkProductBindingV1 {
    WorkProductBindingV1::new(
        CapabilityId::new("capability.work.fixture").expect("fixture capability is canonical"),
        UseCaseId::new("use-case.work.fixture").expect("fixture use case is canonical"),
    )
}

pub fn work_product_revisions(context: &RequestContext) -> WorkProductRevisionPinsV1 {
    WorkProductRevisionPinsV1 {
        // Keep the shared fixture's policy revision stable across its seeded
        // product graph and attempt services.
        policy_revision_id: id(context.grant().digest.as_str()),
        configuration_revision_id: id("configuration.work-product.fixture"),
        catalog_generation_id: id("catalog.work-product.fixture"),
    }
}

pub fn work_authority(context: &RequestContext) -> WorkAuthority {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .expect("request context produces a canonical Work authority")
}

type AttemptKey = (WorkAuthority, String);

struct StoredProductEvent {
    commit: WorkProductEventCommitV1,
}

#[derive(Default)]
struct WorkProductAttemptRows {
    fences: BTreeMap<WorkAuthority, u64>,
    attempts: BTreeMap<AttemptKey, String>,
    evidence: BTreeMap<AttemptKey, String>,
    syntheses: BTreeMap<AttemptKey, WorkSynthesisAdmissionRecordV1>,
    graph: Option<WorkProductGraphV1>,
    events: Vec<StoredProductEvent>,
}

/// A single in-memory authority that retains canonical Work-product events,
/// the verified graph they publish, and fenced provider-attempt rows together.
/// Its combined admission implementation mutates the event journal and row in
/// one mutex transaction, matching the atomic production port boundary.
#[derive(Clone, Default)]
pub struct WorkProductAttemptStore {
    inner: Arc<Mutex<WorkProductAttemptRows>>,
}

impl WorkProductAttemptStore {
    /// Seeds a real Work-product graph through its immutable event journal.
    /// This replaces legacy Work command/projection setup in integration tests.
    pub fn seed_task(&self, context: &RequestContext, task_id: TaskId, execution_admitted: bool) {
        let mut rows = self.inner.lock().expect("fixture store lock is available");
        if rows.graph.is_none() {
            let graph = graph_with_task(task_id.clone());
            append_seed_event(
                &mut rows,
                context,
                WorkProductEventPayloadV1::Created { graph },
                format!("command.work-product.{task_id}.create"),
                UtcMicros(10),
            );
        } else {
            append_seed_change(
                &mut rows,
                context,
                WorkGraphChangeV1::TaskAdded {
                    item: Box::new(work_item(task_id.clone())),
                },
                format!("command.work-product.{task_id}.add"),
                UtcMicros(10),
            );
        }
        let graph_version = rows
            .graph
            .as_ref()
            .expect("seeded graph is retained")
            .version();
        let proposal = WorkProposalV1::new(
            id::<ProposalId>(&format!("proposal.work-product.{task_id}")),
            task_id.clone(),
            graph_version,
            WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1)
                .expect("fixture shape is valid"),
            WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "complete fixture")
                .expect("fixture sizing is valid"),
            Vec::new(),
            WorkRouteDecisionV1::abstain("fixture route").expect("fixture route is valid"),
            format!("Proposal for {task_id}"),
            digest_char('b'),
        )
        .expect("fixture proposal is valid");
        append_seed_change(
            &mut rows,
            context,
            WorkGraphChangeV1::ProposalAccepted {
                proposal,
                accepted_at: UtcMicros(20),
            },
            format!("command.work-product.{task_id}.accept"),
            UtcMicros(20),
        );
        if execution_admitted {
            let based_on_version = rows
                .graph
                .as_ref()
                .expect("accepted graph is retained")
                .version();
            append_seed_change(
                &mut rows,
                context,
                WorkGraphChangeV1::ExecutionAdmitted {
                    task_id: task_id.clone(),
                    based_on_version,
                    admitted_at: UtcMicros(30),
                },
                format!("command.work-product.{task_id}.admit"),
                UtcMicros(30),
            );
        }
    }

    /// Persists a leased row without invoking Start. Lifecycle tests use this
    /// to exercise only fenced transitions over a durable production-shaped
    /// row, rather than borrowing the retired projection authority.
    pub fn persist_leased_attempt(
        &self,
        context: &RequestContext,
        command: &StartWorkAttemptCommand,
    ) -> WorkAttemptV1 {
        let authority = work_authority(context);
        let (binding, requested_route) = {
            let rows = self.inner.lock().expect("fixture store lock is available");
            let graph = rows.graph.as_ref().expect("lifecycle Work graph is seeded");
            let item = graph
                .item(&command.task_id)
                .expect("lifecycle task is retained in the graph");
            let proposal = item
                .accepted_proposal()
                .expect("lifecycle task has an accepted proposal")
                .clone();
            assert!(
                item.is_execution_admitted(),
                "lifecycle attempt rows must be rooted in admitted Work"
            );
            let verified = rows
                .events
                .last()
                .expect("seeded graph has a verified event")
                .commit
                .verified_graph_version();
            (
                tracedecay_domain::WorkAttemptProjectionBindingV1::new(
                    verified.graph_version(),
                    verified.event_sequence(),
                    verified.source_watermark().clone(),
                    verified.recovered_graph_digest().clone(),
                    proposal,
                )
                .expect("seeded graph supplies a valid attempt binding"),
                command.execution_snapshot.route().clone(),
            )
        };
        let identity = WorkAttemptIdentityV1::new(
            command.task_id.clone(),
            command.run_id.clone(),
            command.attempt_id.clone(),
        )
        .expect("fixture attempt identity is valid");
        let lease = WorkLeaseFenceV1::new(
            WorkLeaseId::new(format!("fixture-lease-{}", command.attempt_id.as_str()))
                .expect("fixture lease id is valid"),
            WorkFenceEpochV1::new(
                self.next_fence_epoch(&authority)
                    .expect("fixture fence epoch is available"),
            )
            .expect("fixture fence epoch is valid"),
        )
        .expect("fixture lease fence is valid");
        let envelope = WorkExecutionEnvelopeV1::new(
            identity.clone(),
            binding.clone(),
            command.operation.clone(),
            command.execution_snapshot.clone(),
            context.scope().project_id.clone(),
            context.scope().repository_id.clone(),
            context.scope().worktree_id.clone(),
            command.worktree_root.clone(),
            command.reference.clone(),
            command.commit.clone(),
            command.instructions.clone(),
            1,
            command.effect_state,
        )
        .expect("fixture execution envelope is valid");
        let attempt = WorkAttemptV1::new(
            identity,
            binding,
            envelope,
            lease,
            WorkAttemptStateV1::Leased,
            None,
            Vec::new(),
            WorkCancellationStateV1::None,
            WorkRecoveryStateV1::Fresh,
            requested_route,
            None,
            None,
        )
        .expect("fixture leased attempt is valid");
        match self
            .insert(&authority, &attempt)
            .expect("fixture lifecycle row persists")
        {
            WorkAttemptInsertOutcome::Inserted | WorkAttemptInsertOutcome::Replayed(_) => attempt,
        }
    }

    pub fn graph_version(&self) -> Option<WorkGraphVersionV1> {
        self.inner
            .lock()
            .expect("fixture store lock is available")
            .graph
            .as_ref()
            .map(WorkProductGraphV1::version)
    }

    pub fn attempt_evidence(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Option<WorkAttemptEvidenceRecordV1> {
        self.inner
            .lock()
            .expect("fixture store lock is available")
            .evidence
            .get(&attempt_key(authority, identity))
            .and_then(|payload| serde_json::from_str(payload).ok())
    }
}

impl WorkProductOwnerAuthorizationPortV1 for WorkProductAttemptStore {
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
        AuthorizedWorkProductScopeV1::new(
            id("brain.work-product.fixture"),
            id("profile.work-product.fixture"),
            selection.clone(),
        )
        .map_err(|_| WorkProductOwnerAuthorizationErrorV1::Unavailable)
    }
}

impl WorkGraphReadPortV1 for WorkProductAttemptStore {
    fn read_graph(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkGraphReadRequestV1,
    ) -> Result<WorkGraphReadV1, WorkGraphReadPortErrorV1> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        let graph = rows
            .graph
            .as_ref()
            .ok_or(WorkGraphReadPortErrorV1::NotFoundOrNotAuthorized)?;
        let verified = rows
            .events
            .last()
            .ok_or(WorkGraphReadPortErrorV1::Unavailable)?
            .commit
            .verified_graph_version()
            .clone();
        let runtime_coverage = if graph
            .items()
            .iter()
            .any(|item| !item.accepted_attempts().is_empty())
        {
            WorkRuntimeProjectionCoverageV1::Unavailable
        } else {
            WorkRuntimeProjectionCoverageV1::Complete
        };
        let runtime = WorkRuntimeProjectionV1::new(
            graph.version(),
            ProjectionGenerationId::new("generation.work-product.fixture")
                .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?,
            tracedecay_domain::WorkProjectionSequenceV1::new(graph.version().get()),
            request.observed_at,
            Vec::new(),
            runtime_coverage,
        )
        .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        let projections =
            WorkProductProjectionBundleV1::from_graph(graph, &runtime, request.observed_at)
                .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        let entry = WorkGraphVersionEntryV1::new(
            request.observed_at,
            request.observed_at,
            request.observed_at,
            verified,
            graph.clone(),
            runtime,
            projections,
        )
        .map_err(|_| WorkGraphReadPortErrorV1::Unavailable)?;
        match request.mode {
            tracedecay_application::WorkGraphReadModeV1::Current => Ok(WorkGraphReadV1::Current {
                authorized_scope: context.authorized_scope().clone(),
                selection_coverage:
                    tracedecay_application::WorkGraphSelectionCoverageV1::Complete {
                        covered_events: 1,
                    },
                snapshot: entry,
            }),
            _ => Err(WorkGraphReadPortErrorV1::Unavailable),
        }
    }
}

impl WorkProductAttemptAdmissionPortV1 for WorkProductAttemptStore {
    fn admit_attempt(
        &self,
        admission: &WorkProductAttemptAdmissionV1,
    ) -> Result<WorkProductAttemptAdmissionOutcomeV1, WorkProductAttemptAdmissionErrorV1> {
        admission.validate()?;
        let payload = serde_json::to_string(&admission.attempt)
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?;
        let key = attempt_key(&admission.authority, admission.attempt.identity());
        let mut rows = self
            .inner
            .lock()
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?;
        let product = rows
            .events
            .iter()
            .find(|event| event.commit.event().command_id() == &admission.product_draft.command_id)
            .map(|event| event.commit.clone());
        let stored_attempt = rows.attempts.get(&key).cloned();
        match (product, stored_attempt) {
            (Some(product), Some(existing)) => {
                if product.event().canonical_input_digest()
                    != &admission.product_draft.canonical_input_digest
                {
                    return Err(WorkProductAttemptAdmissionErrorV1::IdempotencyConflict);
                }
                if existing != payload {
                    return Err(WorkProductAttemptAdmissionErrorV1::IdentityConflict);
                }
                return Ok(WorkProductAttemptAdmissionOutcomeV1::Replayed {
                    product,
                    attempt: admission.attempt.clone(),
                });
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(WorkProductAttemptAdmissionErrorV1::IdentityConflict);
            }
            (None, None) => {}
        }
        let (next_graph, product) = product_commit_for(&rows, &admission.product_draft)?;
        admission
            .attempt
            .validate_graph_admission(&next_graph)
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::InvalidAdmission)?;
        if matches!(
            attempt_capacity(
                &rows,
                &admission.authority,
                admission.attempt.identity().task_id(),
                &admission.concurrency,
            )
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?
            .verdict(),
            WorkAttemptCapacityVerdictV1::Exhausted(_)
        ) {
            return Err(WorkProductAttemptAdmissionErrorV1::CapacityExceeded);
        }
        rows.graph = Some(next_graph);
        rows.events.push(StoredProductEvent {
            commit: product.clone(),
        });
        rows.attempts.insert(key, payload);
        Ok(WorkProductAttemptAdmissionOutcomeV1::Inserted {
            product,
            attempt: admission.attempt.clone(),
        })
    }

    fn admit_retry(
        &self,
        _admission: &tracedecay_application::WorkProductRetryAdmissionV1,
    ) -> Result<
        (
            WorkProductEventCommitV1,
            tracedecay_application::WorkRetryAttemptOutcomeV1,
        ),
        WorkProductAttemptAdmissionErrorV1,
    > {
        Err(WorkProductAttemptAdmissionErrorV1::Unavailable)
    }

    fn admit_synthesis(
        &self,
        admission: &tracedecay_application::WorkProductSynthesisAdmissionV1,
    ) -> Result<
        (
            WorkProductEventCommitV1,
            tracedecay_application::WorkSynthesisInsertOutcome,
        ),
        WorkProductAttemptAdmissionErrorV1,
    > {
        admission.validate()?;
        let attempt = &admission.admission.attempt;
        let payload = serde_json::to_string(attempt)
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?;
        let key = attempt_key(&admission.admission.authority, attempt.identity());
        let mut rows = self
            .inner
            .lock()
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?;
        let product = rows
            .events
            .iter()
            .find(|event| {
                event.commit.event().command_id() == &admission.admission.product_draft.command_id
            })
            .map(|event| event.commit.clone());
        let synthesis = rows.syntheses.get(&key).cloned();
        match (product, synthesis, rows.attempts.get(&key)) {
            (Some(product), Some(existing), Some(existing_attempt))
                if existing.request_digest == admission.synthesis.request_digest
                    && existing.result == admission.synthesis.result
                    && existing_attempt == &payload =>
            {
                return Ok((
                    product,
                    WorkSynthesisInsertOutcome::Replayed(Box::new(existing.result)),
                ));
            }
            (Some(_), Some(_), Some(_)) => {
                return Err(WorkProductAttemptAdmissionErrorV1::IdentityConflict);
            }
            (None, None, None) => {}
            _ => return Err(WorkProductAttemptAdmissionErrorV1::IdentityConflict),
        }
        let (next_graph, product) = product_commit_for(&rows, &admission.admission.product_draft)?;
        attempt
            .validate_graph_admission(&next_graph)
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::InvalidAdmission)?;
        if matches!(
            attempt_capacity(
                &rows,
                &admission.admission.authority,
                attempt.identity().task_id(),
                &admission.admission.concurrency,
            )
            .map_err(|_| WorkProductAttemptAdmissionErrorV1::Unavailable)?
            .verdict(),
            WorkAttemptCapacityVerdictV1::Exhausted(_)
        ) {
            return Err(WorkProductAttemptAdmissionErrorV1::CapacityExceeded);
        }
        rows.graph = Some(next_graph);
        rows.events.push(StoredProductEvent {
            commit: product.clone(),
        });
        rows.attempts.insert(key.clone(), payload);
        rows.syntheses.insert(key, admission.synthesis.clone());
        Ok((product, WorkSynthesisInsertOutcome::Inserted))
    }
}

impl WorkAttemptStoragePort for WorkProductAttemptStore {
    fn next_fence_epoch(&self, authority: &WorkAuthority) -> Result<u64, WorkAttemptStorageError> {
        let mut rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let epoch = rows.fences.entry(authority.clone()).or_insert(0);
        *epoch += 1;
        Ok(*epoch)
    }

    fn insert(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        insert_attempt(&self.inner, authority, attempt, None)
    }

    fn insert_bounded(
        &self,
        authority: &WorkAuthority,
        attempt: &WorkAttemptV1,
        concurrency: &TopologyConcurrencyPolicyV1,
    ) -> Result<WorkAttemptInsertOutcome, WorkAttemptStorageError> {
        insert_attempt(&self.inner, authority, attempt, Some(concurrency))
    }

    fn admission_capacities(
        &self,
        authority: &WorkAuthority,
        task_ids: &[TaskId],
        concurrency: &TopologyConcurrencyPolicyV1,
    ) -> Result<BTreeMap<TaskId, WorkAttemptCapacityV1>, WorkAttemptStorageError> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        task_ids
            .iter()
            .map(|task_id| {
                attempt_capacity(&rows, authority, task_id, concurrency)
                    .map(|capacity| (task_id.clone(), capacity))
            })
            .collect()
    }

    fn load(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptV1, WorkAttemptStorageError> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        load_attempt(&rows, authority, identity)
    }

    fn load_admission_kind(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptAdmissionKind, WorkAttemptStorageError> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        load_attempt(&rows, authority, identity)?;
        Ok(
            if rows
                .syntheses
                .contains_key(&attempt_key(authority, identity))
            {
                WorkAttemptAdmissionKind::Synthesis
            } else {
                WorkAttemptAdmissionKind::Ordinary
            },
        )
    }

    fn update(
        &self,
        authority: &WorkAuthority,
        expected_fence: &WorkLeaseFenceV1,
        expected_state: WorkAttemptStateV1,
        next: &WorkAttemptV1,
        evidence: Option<&WorkAttemptEvidenceRecordV1>,
    ) -> Result<(), WorkAttemptStorageError> {
        let payload =
            serde_json::to_string(next).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let mut rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let key = attempt_key(authority, next.identity());
        let current = load_attempt(&rows, authority, next.identity())?;
        if current.lease() != expected_fence || current.state() != expected_state {
            return Err(WorkAttemptStorageError::FenceConflict);
        }
        if let Some(evidence) = evidence {
            rows.evidence.insert(
                key.clone(),
                serde_json::to_string(evidence)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)?,
            );
        }
        rows.attempts.insert(key, payload);
        Ok(())
    }

    fn open_attempts(
        &self,
        authority: &WorkAuthority,
    ) -> Result<Vec<WorkAttemptV1>, WorkAttemptStorageError> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        rows.attempts
            .iter()
            .filter(|((stored_authority, _), _)| stored_authority == authority)
            .map(|(_, payload)| {
                serde_json::from_str::<WorkAttemptV1>(payload)
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            })
            .filter(|attempt| {
                attempt
                    .as_ref()
                    .map(|attempt| !attempt.is_terminal())
                    .unwrap_or(true)
            })
            .collect()
    }

    fn has_open_attempts_in_exact_scope(
        &self,
        project_id: &ProjectId,
        repository_id: &RepositoryId,
        worktree_id: &WorktreeId,
    ) -> Result<bool, WorkAttemptStorageError> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        rows.attempts
            .iter()
            .try_fold(false, |found, ((authority, _), payload)| {
                if found
                    || authority.project_id() != project_id
                    || authority.repository_id() != repository_id
                    || authority.worktree_id() != worktree_id
                {
                    return Ok(found);
                }
                serde_json::from_str::<WorkAttemptV1>(payload)
                    .map(|attempt| !attempt.is_terminal())
                    .map_err(|_| WorkAttemptStorageError::Unavailable)
            })
    }

    fn list(
        &self,
        authority: &WorkAuthority,
        start_after: Option<&WorkAttemptIdentityV1>,
        limit: u32,
    ) -> Result<WorkAttemptListPageV1, WorkAttemptStorageError> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        let start = start_after.map(|identity| attempt_key(authority, identity).1);
        let mut attempts = Vec::new();
        for ((stored_authority, ordinal), payload) in &rows.attempts {
            if stored_authority != authority || start.as_ref().is_some_and(|start| ordinal <= start)
            {
                continue;
            }
            attempts.push(
                serde_json::from_str(payload).map_err(|_| WorkAttemptStorageError::Unavailable)?,
            );
        }
        let remaining =
            u32::try_from(attempts.len()).map_err(|_| WorkAttemptStorageError::Unavailable)?;
        attempts.truncate(limit as usize);
        Ok(WorkAttemptListPageV1 {
            attempts,
            remaining,
        })
    }
}

impl WorkSynthesisAdmissionStoragePort for WorkProductAttemptStore {
    fn insert_synthesis(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError> {
        insert_synthesis_attempt(&self.inner, authority, record, None)
    }

    fn insert_synthesis_bounded(
        &self,
        authority: &WorkAuthority,
        record: &WorkSynthesisAdmissionRecordV1,
        concurrency: &TopologyConcurrencyPolicyV1,
    ) -> Result<WorkSynthesisInsertOutcome, WorkAttemptStorageError> {
        insert_synthesis_attempt(&self.inner, authority, record, Some(concurrency))
    }

    fn load_synthesis(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkSynthesisAdmissionRecordV1, WorkAttemptStorageError> {
        let rows = self
            .inner
            .lock()
            .map_err(|_| WorkAttemptStorageError::Unavailable)?;
        load_attempt(&rows, authority, identity)?;
        rows.syntheses
            .get(&attempt_key(authority, identity))
            .cloned()
            .ok_or(WorkAttemptStorageError::AttemptConflict)
    }
}
