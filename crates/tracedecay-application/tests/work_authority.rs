use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, ApplicationProblemKind,
    CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand, Deadline, DisclosureClass,
    GenerateProposalRequest, ReplanDependenciesCommand, RequestContext, RequestId, ResolvedScope,
    ReviewProposalCommand, WorkAppendOutcome, WorkAppendRequest, WorkReadiness,
    WorkRoutingSnapshotErrorV1, WorkRoutingSnapshotPortV1, WorkRoutingSnapshotV1, WorkService,
    WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, TaskId, UtcMicros, WorkAuthority,
    WorkEvent, WorkProjection, WorkVersion, WorktreeId,
};
use tracedecay_policy::{
    WorkEvidenceFrontierV1, WorkProposalActionV1, WorkProposalDispositionV1, WorkProposalReasonV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

type WorkHistoryKey = (WorkAuthority, TaskId);
type WorkHistories = Arc<Mutex<BTreeMap<WorkHistoryKey, Vec<WorkEvent>>>>;

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

fn context(project: &str, actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>(project),
        id::<RepositoryId>("repository.work.fixture"),
        id::<WorktreeId>("worktree.work.fixture"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.fixture").unwrap();
    let use_case = UseCaseId::new("use-case.work.fixture").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.fixture"),
        1,
        digest('a'),
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

#[derive(Clone, Default)]
struct TestStore {
    histories: WorkHistories,
}

impl WorkStoragePort for TestStore {
    fn load(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<Vec<WorkEvent>, WorkStorageError> {
        self.histories
            .lock()
            .unwrap()
            .get(&(authority.clone(), task_id.clone()))
            .cloned()
            .ok_or(WorkStorageError::NotFoundOrNotAuthorized)
    }

    fn projection(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
    ) -> Result<WorkProjection, WorkStorageError> {
        let history = self.load(authority, task_id)?;
        projection(&history)
    }

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError> {
        let mut histories = self.histories.lock().unwrap();
        let key = (
            request.event.authority().clone(),
            request.event.task_id().clone(),
        );
        let existing = histories.get(&key).cloned().unwrap_or_default();

        if let Some(prior) = existing
            .iter()
            .find(|event| event.command_id() == request.event.command_id())
        {
            return if prior.input_digest() == request.event.input_digest() {
                Ok(WorkAppendOutcome::Replayed(projection(&existing)?))
            } else {
                Err(WorkStorageError::IdempotencyConflict)
            };
        }

        let current = existing.last().map(WorkEvent::version);
        // A caller supplying an expected version asserts the task already exists.
        if current.is_none() && request.expected_version.is_some() {
            return Err(WorkStorageError::NotFoundOrNotAuthorized);
        }
        if current != request.expected_version {
            return Err(WorkStorageError::VersionConflict);
        }
        let history = histories.entry(key).or_default();
        history.push(request.event.clone());
        Ok(WorkAppendOutcome::Appended(projection(history)?))
    }
}

struct EmptyProposalRouting;

impl WorkRoutingSnapshotPortV1 for EmptyProposalRouting {
    fn routing_snapshot(
        &self,
        _context: &RequestContext,
        _task_id: &TaskId,
    ) -> Result<WorkRoutingSnapshotV1, WorkRoutingSnapshotErrorV1> {
        Ok(WorkRoutingSnapshotV1::default())
    }
}

const EMPTY_PROPOSAL_ROUTING: EmptyProposalRouting = EmptyProposalRouting;

fn projection(history: &[WorkEvent]) -> Result<WorkProjection, WorkStorageError> {
    WorkProjection::rebuild(history).map_err(|_| WorkStorageError::Unavailable)
}

fn create(
    service: &WorkService<TestStore>,
    context: &RequestContext,
    task: &str,
    command: &str,
    dependencies: BTreeSet<TaskId>,
) -> WorkProjection {
    service
        .create(
            context,
            CreateWorkCommand {
                task_id: id(task),
                title: format!("Work for {task}"),
                dependencies,
                command_id: id(command),
                occurred_at: UtcMicros(10),
            },
        )
        .unwrap()
}

#[test]
fn create_is_scope_bound_cas_checked_and_idempotent() {
    let service = WorkService::new(TestStore::default());
    let owner = context("project.work.owner", "actor.work.owner");
    let command = CreateWorkCommand {
        task_id: id("task.work.create"),
        title: "Create immutable work".to_owned(),
        dependencies: BTreeSet::new(),
        command_id: id("command.work.create"),
        occurred_at: UtcMicros(10),
    };

    let created = service.create(&owner, command.clone()).unwrap();
    let replayed = service.create(&owner, command.clone()).unwrap();
    assert_eq!(created, replayed);
    assert_eq!(created.version(), WorkVersion::initial());
    assert_eq!(created.history_len(), 1);

    let changed = service
        .create(
            &owner,
            CreateWorkCommand {
                title: "Changed input under the same key".to_owned(),
                ..command
            },
        )
        .unwrap_err();
    assert_eq!(changed.kind(), ApplicationProblemKind::Conflict);

    let concealed = service
        .load(
            &context("project.work.other", "actor.work.owner"),
            &id("task.work.create"),
        )
        .unwrap_err();
    assert_eq!(
        concealed.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[test]
fn readiness_is_derived_and_dependency_replans_reject_cycles() {
    let service = WorkService::new(TestStore::default());
    let context = context("project.work.graph", "actor.work.owner");
    let dependency = id::<TaskId>("task.work.dependency");
    let target = id::<TaskId>("task.work.target");
    create(
        &service,
        &context,
        dependency.as_str(),
        "command.work.dependency.create",
        BTreeSet::new(),
    );
    create(
        &service,
        &context,
        target.as_str(),
        "command.work.target.create",
        BTreeSet::from([dependency.clone()]),
    );

    assert_eq!(
        service.readiness(&context, &target).unwrap(),
        WorkReadiness::Blocked {
            active_dependencies: BTreeSet::from([dependency.clone()])
        }
    );
    service
        .accept_task(
            &context,
            AcceptTaskCommand {
                task_id: dependency.clone(),
                expected_version: WorkVersion::initial(),
                command_id: id("command.work.dependency.accept"),
                occurred_at: UtcMicros(20),
            },
        )
        .unwrap();
    assert_eq!(
        service.readiness(&context, &target).unwrap(),
        WorkReadiness::Ready
    );

    let cycle = service
        .replan_dependencies(
            &context,
            ReplanDependenciesCommand {
                task_id: dependency,
                dependencies: BTreeSet::from([target]),
                expected_version: WorkVersion::new(2).unwrap(),
                command_id: id("command.work.dependency.replan"),
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap_err();
    assert_eq!(cycle.kind(), ApplicationProblemKind::InvalidRequest);
}

#[test]
fn proposal_review_and_execution_admission_are_explicit_mutations() {
    let service = WorkService::new(TestStore::default());
    let context = context("project.work.review", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.review");
    create(
        &service,
        &context,
        task_id.as_str(),
        "command.work.review.create",
        BTreeSet::new(),
    );
    let proposal_request = GenerateProposalRequest {
        task_id: task_id.clone(),
        proposal_id: id("proposal.work.review"),
        live_git_evidence: None,
        occurred_at: UtcMicros(15),
    };
    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &EMPTY_PROPOSAL_ROUTING,
            proposal_request.clone(),
        )
        .unwrap();
    assert_eq!(proposal.based_on_version, WorkVersion::initial());
    assert_eq!(
        proposal.decision.disposition,
        WorkProposalDispositionV1::Allow
    );
    assert_eq!(
        proposal.decision.recommended_action,
        Some(WorkProposalActionV1::ProceedToAcceptance)
    );
    // Proposal generation is read-only and deterministic: nothing is appended
    // and a replay returns the identical explained proposal.
    assert_eq!(
        proposal,
        service
            .generate_proposal(
                &context,
                digest('f'),
                &EMPTY_PROPOSAL_ROUTING,
                proposal_request,
            )
            .unwrap()
    );
    assert_eq!(service.load(&context, &task_id).unwrap().history_len(), 1);

    let accepted = service
        .accept_proposal(
            &context,
            AcceptProposalCommand {
                review: ReviewProposalCommand {
                    task_id: task_id.clone(),
                    proposal_id: proposal.proposal_id,
                    proposal_digest: proposal.proposal_digest,
                    expected_version: WorkVersion::initial(),
                    command_id: id("command.work.proposal.accept"),
                    occurred_at: UtcMicros(20),
                },
            },
        )
        .unwrap();
    assert!(!accepted.is_task_accepted());

    let admitted = service
        .admit_execution(
            &context,
            AdmitExecutionCommand {
                task_id: task_id.clone(),
                expected_version: WorkVersion::new(2).unwrap(),
                command_id: id("command.work.execution.admit"),
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap();
    assert!(admitted.is_execution_admitted());

    let rejected = service
        .reject_proposal(
            &context,
            ReviewProposalCommand {
                task_id: task_id.clone(),
                proposal_id: id::<ProposalId>("proposal.work.rejected"),
                proposal_digest: digest('d'),
                expected_version: WorkVersion::new(3).unwrap(),
                command_id: id("command.work.proposal.reject"),
                occurred_at: UtcMicros(40),
            },
        )
        .unwrap();
    let superseded = service
        .supersede_proposal(
            &context,
            ReviewProposalCommand {
                task_id,
                proposal_id: id("proposal.work.review"),
                proposal_digest: digest('c'),
                expected_version: WorkVersion::new(4).unwrap(),
                command_id: id("command.work.proposal.supersede"),
                occurred_at: UtcMicros(50),
            },
        )
        .unwrap();
    assert_eq!(rejected.history_len(), 4);
    assert_eq!(superseded.history_len(), 5);
}

fn authority(context: &RequestContext) -> WorkAuthority {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .unwrap()
}

#[test]
fn replaying_the_same_mutation_command_is_idempotent_and_input_sensitive() {
    let store = TestStore::default();
    let service = WorkService::new(store.clone());
    let context = context("project.work.idempotent", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.idempotent");
    create(
        &service,
        &context,
        task_id.as_str(),
        "command.work.idempotent.create",
        BTreeSet::new(),
    );

    let command = AcceptTaskCommand {
        task_id: task_id.clone(),
        expected_version: WorkVersion::initial(),
        command_id: id("command.work.idempotent.accept"),
        occurred_at: UtcMicros(20),
    };
    let accepted = service.accept_task(&context, command.clone()).unwrap();
    let replayed = service.accept_task(&context, command.clone()).unwrap();
    assert_eq!(accepted, replayed);
    assert_eq!(store.load(&authority(&context), &task_id).unwrap().len(), 2);

    let conflict = service
        .accept_task(
            &context,
            AcceptTaskCommand {
                occurred_at: UtcMicros(30),
                ..command
            },
        )
        .unwrap_err();
    assert_eq!(conflict.kind(), ApplicationProblemKind::Conflict);
    assert_eq!(
        conflict.diagnostic().unwrap().code,
        "application.work.idempotency-conflict"
    );
    assert_eq!(store.load(&authority(&context), &task_id).unwrap().len(), 2);
}

#[test]
fn an_accepted_task_denies_further_proposals() {
    let service = WorkService::new(TestStore::default());
    let context = context("project.work.denied", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.denied");
    create(
        &service,
        &context,
        task_id.as_str(),
        "command.work.denied.create",
        BTreeSet::new(),
    );
    service
        .accept_task(
            &context,
            AcceptTaskCommand {
                task_id: task_id.clone(),
                expected_version: WorkVersion::initial(),
                command_id: id("command.work.denied.accept"),
                occurred_at: UtcMicros(20),
            },
        )
        .unwrap();

    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &EMPTY_PROPOSAL_ROUTING,
            GenerateProposalRequest {
                task_id,
                proposal_id: id("proposal.work.denied"),
                live_git_evidence: None,
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap();
    assert_eq!(
        proposal.decision.disposition,
        WorkProposalDispositionV1::Deny
    );
    assert_eq!(proposal.decision.recommended_action, None);
    assert!(
        proposal
            .decision
            .ordered_reason_codes
            .contains(&WorkProposalReasonV1::TaskAccepted)
    );
}

#[test]
fn disagreeing_evidence_frontiers_abstain_and_preserve_both_frontiers() {
    let service = WorkService::new(TestStore::default());
    let context = context("project.work.frontier", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.frontier");
    create(
        &service,
        &context,
        task_id.as_str(),
        "command.work.frontier.create",
        BTreeSet::new(),
    );

    let live = WorkEvidenceFrontierV1 {
        watermark: UtcMicros(25),
        digest: digest('9'),
    };
    let proposal = service
        .generate_proposal(
            &context,
            digest('f'),
            &EMPTY_PROPOSAL_ROUTING,
            GenerateProposalRequest {
                task_id,
                proposal_id: id("proposal.work.frontier"),
                live_git_evidence: Some(live.clone()),
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap();
    assert_eq!(
        proposal.decision.disposition,
        WorkProposalDispositionV1::Abstain
    );
    assert_eq!(proposal.decision.recommended_action, None);
    assert_eq!(proposal.decision.live_git_evidence, Some(live));
    assert!(proposal.decision.local_evidence.is_some());
}

#[test]
fn a_cancelled_or_expired_request_never_reaches_the_evaluator() {
    let service = WorkService::new(TestStore::default());
    let admitted = context("project.work.cancel", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.cancel");
    create(
        &service,
        &admitted,
        task_id.as_str(),
        "command.work.cancel.create",
        BTreeSet::new(),
    );

    let cancelled = RequestContext::new(
        admitted.actor().clone(),
        admitted.scope().clone(),
        admitted.grant().clone(),
        RequestId::new("request.work.cancelled").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::cancelled("cancel.work.cancelled", UtcMicros(5)).unwrap(),
    )
    .unwrap();
    let refused = service
        .generate_proposal(
            &cancelled,
            digest('f'),
            &EMPTY_PROPOSAL_ROUTING,
            GenerateProposalRequest {
                task_id: task_id.clone(),
                proposal_id: id("proposal.work.cancelled"),
                live_git_evidence: None,
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap_err();
    assert_eq!(refused.kind(), ApplicationProblemKind::Cancelled);

    let expired = service
        .generate_proposal(
            &admitted,
            digest('f'),
            &EMPTY_PROPOSAL_ROUTING,
            GenerateProposalRequest {
                task_id,
                proposal_id: id("proposal.work.expired"),
                live_git_evidence: None,
                occurred_at: UtcMicros(10_000),
            },
        )
        .unwrap_err();
    assert_eq!(expired.kind(), ApplicationProblemKind::TimedOut);
}

#[test]
fn a_mutation_against_a_task_that_never_existed_is_not_found() {
    let service = WorkService::new(TestStore::default());
    let context = context("project.work.missing", "actor.work.owner");
    let missing = service
        .accept_task(
            &context,
            AcceptTaskCommand {
                task_id: id("task.work.missing"),
                expected_version: WorkVersion::initial(),
                command_id: id("command.work.missing.accept"),
                occurred_at: UtcMicros(20),
            },
        )
        .unwrap_err();
    assert_eq!(
        missing.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}
