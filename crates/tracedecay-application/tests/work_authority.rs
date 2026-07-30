use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, ApplicationProblemKind,
    AttachRuntimeEvidenceCommand, CancellationContext, CapabilityGrantSnapshot, CreateWorkCommand,
    Deadline, DisclosureClass, GenerateProposalRequest, ReplanDependenciesCommand, RequestContext,
    RequestId, ResolvedScope, ReviewProposalCommand, WorkAppendOutcome, WorkAppendRequest,
    WorkReadiness, WorkService, WorkStorageError, WorkStoragePort,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProposalId, RepositoryId, RuntimeEvidenceRef, TaskId,
    UtcMicros, WorkAuthority, WorkCommandId, WorkEvent, WorkProjection, WorkVersion, WorktreeId,
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

#[derive(Default)]
struct TestStore {
    histories: Mutex<BTreeMap<(WorkAuthority, TaskId), Vec<WorkEvent>>>,
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

    fn append(&self, request: &WorkAppendRequest) -> Result<WorkAppendOutcome, WorkStorageError> {
        let mut histories = self.histories.lock().unwrap();
        let key = (
            request.event.authority().clone(),
            request.event.task_id().clone(),
        );
        let history = histories.entry(key).or_default();

        if let Some(prior) = history
            .iter()
            .find(|event| event.command_id() == request.event.command_id())
        {
            return if prior.input_digest() == request.event.input_digest() {
                Ok(WorkAppendOutcome::Replayed(projection(history)?))
            } else {
                Err(WorkStorageError::IdempotencyConflict)
            };
        }

        let current = history.last().map(WorkEvent::version);
        if current != request.expected_version {
            return Err(WorkStorageError::VersionConflict);
        }
        history.push(request.event.clone());
        Ok(WorkAppendOutcome::Appended(projection(history)?))
    }
}

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
    let proposal = service
        .generate_proposal(
            &context,
            GenerateProposalRequest {
                task_id: task_id.clone(),
                proposal_id: id("proposal.work.review"),
                proposal_digest: digest('c'),
            },
        )
        .unwrap();
    assert_eq!(proposal.based_on_version, WorkVersion::initial());
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

#[test]
fn terminal_runtime_evidence_never_auto_accepts_the_task() {
    let service = WorkService::new(TestStore::default());
    let context = context("project.work.runtime", "actor.work.owner");
    let task_id = id::<TaskId>("task.work.runtime");
    create(
        &service,
        &context,
        task_id.as_str(),
        "command.work.runtime.create",
        BTreeSet::new(),
    );

    let with_evidence = service
        .attach_runtime_evidence(
            &context,
            AttachRuntimeEvidenceCommand {
                task_id: task_id.clone(),
                evidence: RuntimeEvidenceRef::new(id("runtime.work.fixture"), digest('e'), true)
                    .unwrap(),
                expected_version: WorkVersion::initial(),
                command_id: id::<WorkCommandId>("command.work.runtime.attach"),
                occurred_at: UtcMicros(20),
            },
        )
        .unwrap();
    assert_eq!(with_evidence.runtime_evidence().len(), 1);
    assert!(!with_evidence.is_task_accepted());

    let accepted = service
        .accept_task(
            &context,
            AcceptTaskCommand {
                task_id,
                expected_version: WorkVersion::new(2).unwrap(),
                command_id: id("command.work.runtime.accept"),
                occurred_at: UtcMicros(30),
            },
        )
        .unwrap();
    assert!(accepted.is_task_accepted());
}
