use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use schemars::schema_for;
use tracedecay_application::{
    CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext,
    RequestId, ResolvedScope, TASK_HANDOFF_LIFETIME_MICROS, TaskHandoffAuthorityError,
    TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome, TaskHandoffError, TaskHandoffGrant,
    TaskHandoffIssueRequest, TaskHandoffRedeemRequest, TaskHandoffScope, TaskHandoffService,
    TaskHandoffToken, WorkHandoffFrontierV1, WorkHandoffLineageV1, WorkflowCatalogAdmissionError,
    WorkflowCoordinationError, WorkflowDefinitionAuthorityError, WorkflowDefinitionAuthorityPort,
    WorkflowDefinitionDisposition, WorkflowDefinitionLifecycleCommand,
    WorkflowDefinitionLifecycleState, WorkflowDefinitionService, WorkflowDefinitionTransitionEntry,
    WorkflowDefinitionTransitionOutcome, work_executable_catalog_digest,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, ThreadId, UtcMicros,
    WorkVersion, WorkflowDefinition, WorkflowDefinitionId, WorkflowOperationRef,
    WorkflowOutputName, WorkflowStep, WorkflowStepId, WorktreeId,
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

fn workflow_context(
    actor: ActorId,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
) -> RequestContext {
    let scope = ResolvedScope::new(project_id, repository_id, worktree_id, None).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.workflow.coordination"),
        1,
        digest('d'),
        id("actor.workflow.issuer"),
        UtcMicros(1),
        UtcMicros(1_000_000),
        scope.clone(),
        BTreeSet::from([id::<CapabilityId>("capability.workflow.coordination")]),
        BTreeSet::from([id::<UseCaseId>("use-case.workflow.coordination")]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        actor,
        scope,
        grant,
        id::<RequestId>("request.workflow.coordination"),
        Deadline::new(UtcMicros(900_000)).unwrap(),
        CancellationContext::active("cancellation.workflow.coordination").unwrap(),
    )
    .unwrap()
}

/// A mounted Work operation, so fixtures clear catalog admission.
const MOUNTED_OPERATION: &str = "operation.work.start_attempt";

fn definition(version: u64) -> WorkflowDefinition {
    definition_for_project(
        version,
        id("project.workflow.coordination"),
        MOUNTED_OPERATION,
    )
}

fn definition_with_operation(version: u64, operation: &str) -> WorkflowDefinition {
    definition_for_project(version, id("project.workflow.coordination"), operation)
}

fn definition_for_project(
    version: u64,
    project_id: ProjectId,
    operation: &str,
) -> WorkflowDefinition {
    definition_with_catalog_pin(
        version,
        project_id,
        operation,
        work_executable_catalog_digest().unwrap(),
    )
}

fn definition_with_catalog_pin(
    version: u64,
    project_id: ProjectId,
    operation: &str,
    pinned_catalog_digest: ManifestDigest,
) -> WorkflowDefinition {
    WorkflowDefinition::new(
        id("workflow.definition.coordination"),
        version,
        project_id,
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>(operation),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: None,
        }],
        digest('a'),
        digest('b'),
        pinned_catalog_digest,
    )
    .unwrap()
}

#[derive(Clone, Default)]
struct FakeDefinitionAuthority {
    state: Arc<Mutex<DefinitionState>>,
}

#[derive(Default)]
struct DefinitionState {
    definitions: BTreeMap<(WorkflowDefinitionId, u64), WorkflowDefinition>,
    dispositions: BTreeMap<(WorkflowDefinitionId, u64), WorkflowDefinitionDisposition>,
    transitions: Vec<WorkflowDefinitionTransitionEntry>,
}

impl WorkflowDefinitionAuthorityPort for FakeDefinitionAuthority {
    fn insert(
        &self,
        definition: &WorkflowDefinition,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let key = (
            definition.definition_id().clone(),
            definition.definition_version(),
        );
        let mut state = self.state.lock().unwrap();
        if state.definitions.contains_key(&key) {
            return Err(WorkflowDefinitionAuthorityError::AlreadyExists);
        }
        state.definitions.insert(key.clone(), definition.clone());
        state
            .dispositions
            .entry(key)
            .or_insert_with(|| WorkflowDefinitionDisposition {
                definition_id: definition.definition_id().clone(),
                definition_version: definition.definition_version(),
                state: WorkflowDefinitionLifecycleState::Candidate,
                revision: 1,
                transitioned_at: UtcMicros(0),
            });
        Ok(())
    }

    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .definitions
            .get(&(definition_id.clone(), definition_version))
            .cloned())
    }

    fn list(
        &self,
        definition_id: Option<&WorkflowDefinitionId>,
    ) -> Result<Vec<WorkflowDefinition>, WorkflowDefinitionAuthorityError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .definitions
            .values()
            .filter(|definition| {
                definition_id
                    .is_none_or(|definition_id| definition.definition_id() == definition_id)
            })
            .cloned()
            .collect())
    }

    fn load_disposition(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinitionDisposition>, WorkflowDefinitionAuthorityError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .dispositions
            .get(&(definition_id.clone(), definition_version))
            .cloned())
    }

    fn transition(
        &self,
        command: &WorkflowDefinitionLifecycleCommand,
    ) -> Result<WorkflowDefinitionTransitionOutcome, WorkflowDefinitionAuthorityError> {
        let key = (command.definition_id.clone(), command.definition_version);
        let mut state = self.state.lock().unwrap();
        let Some(current) = state.dispositions.get(&key).cloned() else {
            return Ok(WorkflowDefinitionTransitionOutcome::Missing);
        };
        if current.revision != command.expected_revision {
            let replayed = state.transitions.iter().any(|entry| {
                entry.definition_id == command.definition_id
                    && entry.definition_version == command.definition_version
                    && entry.from_revision == command.expected_revision
                    && entry.operation == command.operation
            });
            return Ok(if replayed {
                WorkflowDefinitionTransitionOutcome::Replayed(current)
            } else {
                WorkflowDefinitionTransitionOutcome::RevisionConflict(current)
            });
        }
        let Some(path) = command.operation.path_from(current.state) else {
            return Ok(WorkflowDefinitionTransitionOutcome::IllegalTransition(
                current,
            ));
        };
        let mut lifecycle = current.state;
        let mut revision = current.revision;
        for next in path {
            state.transitions.push(WorkflowDefinitionTransitionEntry {
                definition_id: command.definition_id.clone(),
                definition_version: command.definition_version,
                operation: command.operation,
                from_state: lifecycle,
                to_state: *next,
                from_revision: revision,
                to_revision: revision + 1,
                transitioned_at: command.transitioned_at,
            });
            lifecycle = *next;
            revision += 1;
        }
        let disposition = WorkflowDefinitionDisposition {
            definition_id: command.definition_id.clone(),
            definition_version: command.definition_version,
            state: lifecycle,
            revision,
            transitioned_at: command.transitioned_at,
        };
        state.dispositions.insert(key, disposition.clone());
        Ok(WorkflowDefinitionTransitionOutcome::Applied(disposition))
    }

    fn transition_history(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Vec<WorkflowDefinitionTransitionEntry>, WorkflowDefinitionAuthorityError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .transitions
            .iter()
            .filter(|entry| {
                entry.definition_id == *definition_id
                    && entry.definition_version == definition_version
            })
            .cloned()
            .collect())
    }
}

#[test]
fn immutable_definition_versions_are_bound_to_the_admitted_project() {
    let authority = FakeDefinitionAuthority::default();
    let service = WorkflowDefinitionService::new(authority.clone());
    let context = workflow_context(
        id("actor.workflow.source"),
        id("project.workflow.coordination"),
        id("repository.workflow.coordination"),
        id("worktree.workflow.coordination"),
    );
    let first = definition(1);
    let second = definition(2);

    assert_eq!(service.register(&context, first.clone()).unwrap(), first);
    assert_eq!(service.register(&context, second.clone()).unwrap(), second);
    assert_eq!(service.register(&context, first.clone()).unwrap(), first);
    assert_eq!(
        service
            .register(
                &context,
                definition_with_operation(1, "operation.graph.workflow_step.v2"),
            )
            .unwrap_err(),
        WorkflowCoordinationError::ImmutableDefinitionConflict
    );

    let foreign = definition_for_project(
        3,
        id("project.workflow.foreign"),
        "operation.graph.workflow_step",
    );
    assert_eq!(
        service.register(&context, foreign).unwrap_err(),
        WorkflowCoordinationError::ScopeMismatch
    );
    assert_eq!(
        authority.state.lock().unwrap().definitions.len(),
        2,
        "a foreign-project definition must never reach storage"
    );
}

#[test]
fn definition_storage_lists_validates_and_diffs_without_rewriting_history() {
    let authority = FakeDefinitionAuthority::default();
    let service = WorkflowDefinitionService::new(authority);
    let context = workflow_context(
        id("actor.workflow.source"),
        id("project.workflow.coordination"),
        id("repository.workflow.coordination"),
        id("worktree.workflow.coordination"),
    );
    let first = definition(1);
    let second = definition(2);
    service.register(&context, first.clone()).unwrap();
    service.register(&context, second.clone()).unwrap();

    assert_eq!(service.validate(first.clone()).unwrap().definition, first);
    assert_eq!(
        service
            .get(first.definition_id(), first.definition_version())
            .unwrap(),
        first
    );
    assert_eq!(
        service
            .history(first.definition_id())
            .unwrap()
            .into_iter()
            .map(|definition| definition.definition_version())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(service.list().unwrap().len(), 2);
    let diff = service.diff(first.definition_id(), 1, 2).unwrap();
    assert_eq!(diff.from_version, 1);
    assert_eq!(diff.to_version, 2);
    assert!(diff.changed_steps.is_empty());

    assert_eq!(
        service.history(first.definition_id()).unwrap(),
        vec![first, second],
        "definition reads must preserve immutable history"
    );
}

#[derive(Clone, Default)]
struct FakeHandoffAuthority {
    grants: Arc<Mutex<BTreeMap<ManifestDigest, (TaskHandoffGrant, bool)>>>,
}

impl TaskHandoffAuthorityPort for FakeHandoffAuthority {
    fn issue(&self, grant: &TaskHandoffGrant) -> Result<(), TaskHandoffAuthorityError> {
        let mut grants = self.grants.lock().unwrap();
        if grants.contains_key(grant.token_digest()) {
            return Err(TaskHandoffAuthorityError::Conflict);
        }
        grants.insert(grant.token_digest().clone(), (grant.clone(), false));
        Ok(())
    }

    fn consume(
        &self,
        token_digest: &ManifestDigest,
        expected_scope: &TaskHandoffScope,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffConsumeOutcome, TaskHandoffAuthorityError> {
        let mut grants = self.grants.lock().unwrap();
        let Some((grant, consumed)) = grants.get_mut(token_digest) else {
            return Ok(TaskHandoffConsumeOutcome::Missing);
        };
        if grant.scope() != expected_scope {
            return Ok(TaskHandoffConsumeOutcome::ScopeMismatch);
        }
        // Half-open: consumed_at >= expires_at is expired.
        if consumed_at >= *grant.expires_at() {
            return Ok(TaskHandoffConsumeOutcome::Expired);
        }
        if *consumed {
            return Ok(TaskHandoffConsumeOutcome::Replay);
        }
        *consumed = true;
        Ok(TaskHandoffConsumeOutcome::Consumed {
            frontier: Box::new(grant.frontier().clone()),
        })
    }
}

fn handoff_scope() -> TaskHandoffScope {
    TaskHandoffScope::new(
        id::<ProjectId>("project.workflow.coordination"),
        id::<RepositoryId>("repository.workflow.coordination"),
        id::<WorktreeId>("worktree.workflow.coordination"),
        id::<WorkflowDefinitionId>("workflow.definition.coordination"),
        1,
        id::<WorkflowStepId>("prepare"),
        id::<TaskId>("task.workflow.coordination.prepare"),
        id::<ThreadId>("thread.workflow.coordination"),
        id::<RunId>("run.workflow.coordination"),
        id::<ActorId>("actor.workflow.source"),
        id::<ActorId>("actor.workflow.target"),
    )
    .unwrap()
}

fn token(value: char) -> TaskHandoffToken {
    TaskHandoffToken::new(value.to_string().repeat(48)).unwrap()
}

fn frontier_for(task: &str, issuer: &str) -> WorkHandoffFrontierV1 {
    WorkHandoffFrontierV1::new(
        id(task),
        WorkVersion::new(3).unwrap(),
        Vec::new(),
        vec!["whether the second attempt's failure is environmental".to_owned()],
        vec!["waiting on the exclusive placement of the shared root".to_owned()],
        vec!["start one recovery attempt against the pinned commit".to_owned()],
        WorkHandoffLineageV1 {
            issued_by: id(issuer),
            issued_at: UtcMicros(9),
            prior_frontier_digest: None,
        },
    )
    .unwrap()
}

fn frontier() -> WorkHandoffFrontierV1 {
    frontier_for(
        "task.workflow.coordination.prepare",
        "actor.workflow.source",
    )
}

#[test]
fn handoff_enforces_authorization_scope_expiry_and_single_use_without_bearer_leakage() {
    assert_eq!(TASK_HANDOFF_LIFETIME_MICROS, UtcMicros(60_000_000));
    let authority = FakeHandoffAuthority::default();
    let service = TaskHandoffService::new(authority);
    let scope = handoff_scope();
    let issue_context = workflow_context(
        scope.from_actor_id().clone(),
        scope.project_id().clone(),
        scope.repository_id().clone(),
        scope.worktree_id().clone(),
    );
    let redeem_context = workflow_context(
        scope.to_actor_id().clone(),
        scope.project_id().clone(),
        scope.repository_id().clone(),
        scope.worktree_id().clone(),
    );
    let handoff = token('s');
    let debug = format!("{handoff:?}");
    assert!(!debug.contains(&"s".repeat(48)));
    assert_eq!(debug, "TaskHandoffToken([REDACTED])");

    assert_eq!(
        TaskHandoffToken::new("short".to_owned()).unwrap_err(),
        TaskHandoffError::InvalidToken
    );
    assert_eq!(
        TaskHandoffToken::new(format!(" {}\n{}", "a".repeat(30), "b".repeat(30))).unwrap_err(),
        TaskHandoffError::InvalidToken
    );
    assert_eq!(
        TaskHandoffToken::new("a".repeat(513)).unwrap_err(),
        TaskHandoffError::InvalidToken
    );
    // Multi-byte UTF-8 must be bounded by bytes, not chars.
    assert!(TaskHandoffToken::new("é".repeat(16)).is_ok());
    assert_eq!(
        TaskHandoffToken::new("é".repeat(257)).unwrap_err(),
        TaskHandoffError::InvalidToken
    );

    assert_eq!(
        TaskHandoffScope::new(
            scope.project_id().clone(),
            scope.repository_id().clone(),
            scope.worktree_id().clone(),
            scope.definition_id().clone(),
            0,
            scope.step_id().clone(),
            scope.task_id().clone(),
            scope.thread_id().clone(),
            scope.run_id().clone(),
            scope.from_actor_id().clone(),
            scope.to_actor_id().clone(),
        )
        .unwrap_err(),
        TaskHandoffError::InvalidScope
    );

    assert_eq!(
        service
            .issue(
                &workflow_context(
                    id("actor.workflow.other"),
                    scope.project_id().clone(),
                    scope.repository_id().clone(),
                    scope.worktree_id().clone(),
                ),
                scope.clone(),
                &handoff,
                UtcMicros(10),
                frontier(),
            )
            .unwrap_err(),
        TaskHandoffError::Unauthorized
    );
    for context in [
        workflow_context(
            scope.from_actor_id().clone(),
            id("project.workflow.other"),
            scope.repository_id().clone(),
            scope.worktree_id().clone(),
        ),
        workflow_context(
            scope.from_actor_id().clone(),
            scope.project_id().clone(),
            id("repository.workflow.other"),
            scope.worktree_id().clone(),
        ),
        workflow_context(
            scope.from_actor_id().clone(),
            scope.project_id().clone(),
            scope.repository_id().clone(),
            id("worktree.workflow.other"),
        ),
    ] {
        assert_eq!(
            service
                .issue(&context, scope.clone(), &handoff, UtcMicros(10), frontier(),)
                .unwrap_err(),
            TaskHandoffError::Unauthorized
        );
    }

    let grant = service
        .issue(
            &issue_context,
            scope.clone(),
            &handoff,
            UtcMicros(10),
            frontier(),
        )
        .unwrap();
    assert_eq!(*grant.issued_at(), UtcMicros(10));
    assert_eq!(*grant.expires_at(), UtcMicros(60_000_010));

    assert_eq!(
        service
            .redeem(
                &workflow_context(
                    id("actor.workflow.other"),
                    scope.project_id().clone(),
                    scope.repository_id().clone(),
                    scope.worktree_id().clone(),
                ),
                &handoff,
                &scope,
                UtcMicros(11),
            )
            .unwrap_err(),
        TaskHandoffError::Unauthorized
    );

    for context in [
        workflow_context(
            scope.to_actor_id().clone(),
            id("project.workflow.other"),
            scope.repository_id().clone(),
            scope.worktree_id().clone(),
        ),
        workflow_context(
            scope.to_actor_id().clone(),
            scope.project_id().clone(),
            id("repository.workflow.other"),
            scope.worktree_id().clone(),
        ),
        workflow_context(
            scope.to_actor_id().clone(),
            scope.project_id().clone(),
            scope.repository_id().clone(),
            id("worktree.workflow.other"),
        ),
    ] {
        assert_eq!(
            service
                .redeem(&context, &handoff, &scope, UtcMicros(11))
                .unwrap_err(),
            TaskHandoffError::Unauthorized
        );
    }

    let wrong_task = TaskHandoffScope::new(
        scope.project_id().clone(),
        scope.repository_id().clone(),
        scope.worktree_id().clone(),
        scope.definition_id().clone(),
        scope.definition_version(),
        scope.step_id().clone(),
        id("task.workflow.coordination.other"),
        scope.thread_id().clone(),
        scope.run_id().clone(),
        scope.from_actor_id().clone(),
        scope.to_actor_id().clone(),
    )
    .unwrap();
    assert_eq!(
        service
            .redeem(&redeem_context, &handoff, &wrong_task, UtcMicros(11))
            .unwrap_err(),
        TaskHandoffError::ScopeMismatch
    );

    let wrong_thread = TaskHandoffScope::new(
        scope.project_id().clone(),
        scope.repository_id().clone(),
        scope.worktree_id().clone(),
        scope.definition_id().clone(),
        scope.definition_version(),
        scope.step_id().clone(),
        scope.task_id().clone(),
        id("thread.workflow.other"),
        scope.run_id().clone(),
        scope.from_actor_id().clone(),
        scope.to_actor_id().clone(),
    )
    .unwrap();
    assert_eq!(
        service
            .redeem(&redeem_context, &handoff, &wrong_thread, UtcMicros(11))
            .unwrap_err(),
        TaskHandoffError::ScopeMismatch
    );

    let wrong_definition = TaskHandoffScope::new(
        scope.project_id().clone(),
        scope.repository_id().clone(),
        scope.worktree_id().clone(),
        scope.definition_id().clone(),
        2,
        scope.step_id().clone(),
        scope.task_id().clone(),
        scope.thread_id().clone(),
        scope.run_id().clone(),
        scope.from_actor_id().clone(),
        scope.to_actor_id().clone(),
    )
    .unwrap();
    assert_eq!(
        service
            .redeem(&redeem_context, &handoff, &wrong_definition, UtcMicros(11),)
            .unwrap_err(),
        TaskHandoffError::ScopeMismatch
    );

    // Half-open expiry: consumed_at == the fixed expiry is Expired.
    assert_eq!(
        service
            .redeem(&redeem_context, &handoff, &scope, UtcMicros(60_000_010))
            .unwrap_err(),
        TaskHandoffError::Expired
    );
    let receipt = service
        .redeem(&redeem_context, &handoff, &scope, UtcMicros(60_000_009))
        .unwrap();
    // The redemption receipt is checkpoint evidence: exactly the recorded
    // frontier, its digest, the scope, and when it was redeemed — no lease,
    // fence, or acceptance authority travels with it.
    assert_eq!(receipt.scope, scope);
    assert_eq!(receipt.frontier, frontier());
    assert_eq!(receipt.frontier_digest, frontier().digest().unwrap());
    assert_eq!(receipt.redeemed_at, UtcMicros(60_000_009));
    assert_eq!(
        service
            .redeem(&redeem_context, &handoff, &scope, UtcMicros(60_000_009))
            .unwrap_err(),
        TaskHandoffError::Replay
    );

    // A frontier cut for another task, or recorded by an actor other than
    // the one handing off, is not this handoff's checkpoint evidence.
    assert_eq!(
        service
            .issue(
                &issue_context,
                scope.clone(),
                &token('w'),
                UtcMicros(10),
                frontier_for("task.workflow.coordination.other", "actor.workflow.source"),
            )
            .unwrap_err(),
        TaskHandoffError::InvalidFrontier
    );
    assert_eq!(
        service
            .issue(
                &issue_context,
                scope.clone(),
                &token('w'),
                UtcMicros(10),
                frontier_for("task.workflow.coordination.prepare", "actor.workflow.other"),
            )
            .unwrap_err(),
        TaskHandoffError::InvalidFrontier
    );

    let expired = token('e');
    service
        .issue(
            &issue_context,
            scope.clone(),
            &expired,
            UtcMicros(10),
            frontier(),
        )
        .unwrap();
    assert_eq!(
        service
            .redeem(&redeem_context, &expired, &scope, UtcMicros(60_000_010),)
            .unwrap_err(),
        TaskHandoffError::Expired
    );

    assert_eq!(
        service
            .issue(
                &issue_context,
                scope.clone(),
                &token('x'),
                UtcMicros(i64::MAX - 59_999_999),
                frontier(),
            )
            .unwrap_err(),
        TaskHandoffError::InvalidExpiry
    );

    let boundary = service
        .issue(
            &issue_context,
            scope.clone(),
            &token('m'),
            UtcMicros(i64::MAX - 60_000_000),
            frontier(),
        )
        .unwrap();
    assert_eq!(*boundary.expires_at(), UtcMicros(i64::MAX));
}

#[test]
fn handoff_wire_requests_reject_caller_supplied_identity_and_time() {
    let scope = serde_json::to_value(handoff_scope()).unwrap();
    let issue = serde_json::json!({
        "scope": scope,
        "secret": "s".repeat(48),
        "frontier": serde_json::to_value(frontier()).unwrap(),
    });
    assert!(serde_json::from_value::<TaskHandoffIssueRequest>(issue.clone()).is_ok());
    let mut caller_issued = issue.clone();
    caller_issued["issuer"] = serde_json::json!("actor.workflow.source");
    assert!(
        serde_json::from_value::<TaskHandoffIssueRequest>(caller_issued).is_err(),
        "issuance actor must come from authenticated context"
    );
    let mut caller_issued_at = issue.clone();
    caller_issued_at["issued_at"] = serde_json::json!(10);
    assert!(
        serde_json::from_value::<TaskHandoffIssueRequest>(caller_issued_at).is_err(),
        "issuance time must come from the daemon clock"
    );
    let mut caller_expires_at = issue;
    caller_expires_at["expires_at"] = serde_json::json!(60_000_010);
    assert!(
        serde_json::from_value::<TaskHandoffIssueRequest>(caller_expires_at).is_err(),
        "expiry must be derived from the fixed authority lifetime"
    );

    let redeem = serde_json::json!({
        "secret": "s".repeat(48),
        "expected_scope": serde_json::to_value(handoff_scope()).unwrap(),
    });
    assert!(serde_json::from_value::<TaskHandoffRedeemRequest>(redeem.clone()).is_ok());
    let mut caller_redeemer = redeem.clone();
    caller_redeemer["redeemer"] = serde_json::json!("actor.workflow.target");
    assert!(
        serde_json::from_value::<TaskHandoffRedeemRequest>(caller_redeemer).is_err(),
        "redeemer must come from authenticated context"
    );
    let mut caller_consumed_at = redeem;
    caller_consumed_at["consumed_at"] = serde_json::json!(11);
    assert!(
        serde_json::from_value::<TaskHandoffRedeemRequest>(caller_consumed_at).is_err(),
        "consumption time must come from the daemon clock"
    );
}

#[test]
fn handoff_grant_deserialization_fails_closed_on_scope_and_expiry() {
    let scope = handoff_scope();
    let grant = TaskHandoffGrant::new(
        scope.clone(),
        digest('f'),
        UtcMicros(10),
        UtcMicros(60_000_010),
        frontier(),
    )
    .unwrap();
    assert_eq!(grant.scope(), &scope);
    assert_eq!(*grant.issued_at(), UtcMicros(10));
    assert_eq!(*grant.expires_at(), UtcMicros(60_000_010));
    assert_eq!(grant.frontier(), &frontier());
    assert_eq!(*grant.frontier_digest(), frontier().digest().unwrap());
    let json = serde_json::to_value(&grant).unwrap();
    assert_eq!(json["scope"]["thread_id"], "thread.workflow.coordination");
    assert_eq!(
        serde_json::from_value::<TaskHandoffGrant>(json.clone()).unwrap(),
        grant
    );

    let mut expired_order = json.clone();
    expired_order["issued_at"] = serde_json::json!(20);
    expired_order["expires_at"] = serde_json::json!(20);
    assert!(serde_json::from_value::<TaskHandoffGrant>(expired_order).is_err());

    let mut inverted = json.clone();
    inverted["issued_at"] = serde_json::json!(60_000_011);
    inverted["expires_at"] = serde_json::json!(60_000_010);
    assert!(serde_json::from_value::<TaskHandoffGrant>(inverted).is_err());

    let mut too_short = json.clone();
    too_short["expires_at"] = serde_json::json!(10 + 59_999_999);
    assert!(serde_json::from_value::<TaskHandoffGrant>(too_short).is_err());

    let mut too_long = json.clone();
    too_long["expires_at"] = serde_json::json!(10 + 60_000_001);
    assert!(serde_json::from_value::<TaskHandoffGrant>(too_long).is_err());

    let mut tampered_frontier = json.clone();
    tampered_frontier["frontier"]["task_id"] = serde_json::json!("task.workflow.tampered");
    assert!(
        serde_json::from_value::<TaskHandoffGrant>(tampered_frontier).is_err(),
        "a frontier rebound to another task must fail closed"
    );

    let mut tampered_digest = json.clone();
    tampered_digest["frontier_digest"] = serde_json::json!(format!("sha256:{}", "0".repeat(64)));
    assert!(
        serde_json::from_value::<TaskHandoffGrant>(tampered_digest).is_err(),
        "a frontier digest that does not match the frontier must fail closed"
    );

    let mut zero_version = json;
    zero_version["scope"]["definition_version"] = serde_json::json!(0);
    assert!(serde_json::from_value::<TaskHandoffGrant>(zero_version).is_err());
    assert!(
        serde_json::from_value::<TaskHandoffScope>(serde_json::json!({
            "project_id": "project.workflow.coordination",
            "repository_id": "repository.workflow.coordination",
            "worktree_id": "worktree.workflow.coordination",
            "definition_id": "workflow.definition.coordination",
            "definition_version": 0,
            "step_id": "prepare",
            "task_id": "task.workflow.coordination.prepare",
            "thread_id": "thread.workflow.coordination",
            "run_id": "run.workflow.coordination",
            "from_actor_id": "actor.workflow.source",
            "to_actor_id": "actor.workflow.target",
        }))
        .is_err()
    );

    let schema = serde_json::to_value(schema_for!(TaskHandoffGrant)).unwrap();
    let scope_schema = serde_json::to_value(schema_for!(TaskHandoffScope)).unwrap();
    assert_eq!(
        scope_schema["properties"]["definition_version"]["minimum"],
        1
    );
    assert!(scope_schema["properties"].get("thread_id").is_some());
    assert!(schema["properties"].get("token").is_none());
    assert!(schema["properties"].get("secret").is_none());
    assert!(schema["properties"].get("token_digest").is_some());
}

fn lifecycle_service() -> (
    FakeDefinitionAuthority,
    WorkflowDefinitionService<FakeDefinitionAuthority>,
    RequestContext,
) {
    let authority = FakeDefinitionAuthority::default();
    let service = WorkflowDefinitionService::new(authority.clone());
    let context = workflow_context(
        id("actor.workflow.source"),
        id("project.workflow.coordination"),
        id("repository.workflow.coordination"),
        id("worktree.workflow.coordination"),
    );
    (authority, service, context)
}

#[test]
fn the_retained_lifecycle_runs_candidate_validated_active_then_retired() {
    let (_authority, service, context) = lifecycle_service();
    let registered = service.register(&context, definition(1)).unwrap();
    let definition_id = registered.definition_id().clone();

    let candidate = service.disposition(&definition_id, 1).unwrap();
    assert_eq!(candidate.state, WorkflowDefinitionLifecycleState::Candidate);
    assert_eq!(candidate.revision, 1);

    // `validate` stays the pure read Plan 32 advertises; activation is what
    // durably clears the definition and records the validated disposition it
    // had to pass through ("... reject before activation").
    assert_eq!(
        service.validate(registered.clone()).unwrap().definition,
        registered
    );

    let active = service
        .activate(&definition_id, 1, candidate.revision, UtcMicros(10))
        .unwrap();
    assert_eq!(active.state, WorkflowDefinitionLifecycleState::Active);
    assert_eq!(active.revision, 3);

    let history = service.lifecycle_history(&definition_id, 1).unwrap();
    assert_eq!(
        history
            .iter()
            .map(|entry| (entry.from_state, entry.to_state))
            .collect::<Vec<_>>(),
        vec![
            (
                WorkflowDefinitionLifecycleState::Candidate,
                WorkflowDefinitionLifecycleState::Validated
            ),
            (
                WorkflowDefinitionLifecycleState::Validated,
                WorkflowDefinitionLifecycleState::Active
            ),
        ]
    );

    let retired = service
        .retire(&definition_id, 1, active.revision, UtcMicros(20))
        .unwrap();
    assert_eq!(retired.state, WorkflowDefinitionLifecycleState::Retired);
    assert_eq!(retired.revision, 4);
    assert!(retired.state.is_terminal());
}

#[test]
fn activation_rejects_a_step_operation_the_catalog_does_not_mount() {
    let (authority, service, context) = lifecycle_service();
    let registered = service
        .register(
            &context,
            definition_with_operation(1, "operation.work.not_a_mounted_operation"),
        )
        .unwrap();
    let definition_id = registered.definition_id().clone();

    // Registration stays lenient; Plan 32 rejects "before activation".
    let denial = service.validate(registered.clone()).unwrap_err();
    let WorkflowCoordinationError::CatalogAdmissionDenied(
        WorkflowCatalogAdmissionError::UnknownOperation { step_id, operation },
    ) = denial
    else {
        panic!("expected an unknown-operation catalog denial, got {denial:?}");
    };
    assert_eq!(step_id.as_str(), "prepare");
    assert_eq!(operation.as_str(), "operation.work.not_a_mounted_operation");

    assert_eq!(
        service
            .activate(&definition_id, 1, 1, UtcMicros(10))
            .unwrap_err(),
        WorkflowCoordinationError::CatalogAdmissionDenied(
            WorkflowCatalogAdmissionError::UnknownOperation {
                step_id: id("prepare"),
                operation: id("operation.work.not_a_mounted_operation"),
            }
        )
    );

    // The denial precedes the lifecycle authority.
    assert_eq!(
        service.disposition(&definition_id, 1).unwrap().state,
        WorkflowDefinitionLifecycleState::Candidate
    );
    assert!(authority.state.lock().unwrap().transitions.is_empty());
}

#[test]
fn activation_rejects_a_definition_pinned_to_a_foreign_catalog() {
    let (authority, service, context) = lifecycle_service();
    let registered = service
        .register(
            &context,
            definition_with_catalog_pin(
                1,
                id("project.workflow.coordination"),
                MOUNTED_OPERATION,
                digest('c'),
            ),
        )
        .unwrap();
    let definition_id = registered.definition_id().clone();

    let denial = service
        .activate(&definition_id, 1, 1, UtcMicros(10))
        .unwrap_err();
    let WorkflowCoordinationError::CatalogAdmissionDenied(
        WorkflowCatalogAdmissionError::CatalogPinMismatch { pinned, current },
    ) = denial
    else {
        panic!("expected a catalog-pin denial, got {denial:?}");
    };
    assert_eq!(pinned, digest('c'));
    assert_eq!(current, work_executable_catalog_digest().unwrap());
    assert!(authority.state.lock().unwrap().transitions.is_empty());
}

#[test]
fn rejection_is_a_terminal_disposition_for_an_unactivated_version() {
    let (_authority, service, context) = lifecycle_service();
    let registered = service.register(&context, definition(1)).unwrap();
    let definition_id = registered.definition_id().clone();

    let rejected = service.reject(&definition_id, 1, 1, UtcMicros(30)).unwrap();
    assert_eq!(rejected.state, WorkflowDefinitionLifecycleState::Rejected);
    assert_eq!(rejected.revision, 2);
    assert!(rejected.state.is_terminal());

    // The rejected version stays readable and immutable; only its disposition
    // is terminal.
    assert_eq!(service.get(&definition_id, 1).unwrap(), registered);
    assert_eq!(
        service
            .activate(&definition_id, 1, rejected.revision, UtcMicros(31))
            .unwrap_err(),
        WorkflowCoordinationError::IllegalLifecycleTransition
    );
}

#[test]
fn illegal_lifecycle_transitions_are_typed_conflicts() {
    let (_authority, service, context) = lifecycle_service();
    let registered = service.register(&context, definition(1)).unwrap();
    let definition_id = registered.definition_id().clone();

    // Retiring a version that never reached `active` has no legal edge.
    assert_eq!(
        service
            .retire(&definition_id, 1, 1, UtcMicros(40))
            .unwrap_err(),
        WorkflowCoordinationError::IllegalLifecycleTransition
    );

    // A stale expected revision is a compare-and-swap conflict, not a silent
    // overwrite.
    assert_eq!(
        service
            .activate(&definition_id, 1, 7, UtcMicros(41))
            .unwrap_err(),
        WorkflowCoordinationError::LifecycleRevisionConflict
    );

    let active = service
        .activate(&definition_id, 1, 1, UtcMicros(42))
        .unwrap();
    let retired = service
        .retire(&definition_id, 1, active.revision, UtcMicros(43))
        .unwrap();

    // Nothing mutates a retired disposition.
    for error in [
        service
            .activate(&definition_id, 1, retired.revision, UtcMicros(44))
            .unwrap_err(),
        service
            .retire(&definition_id, 1, retired.revision, UtcMicros(45))
            .unwrap_err(),
        service
            .reject(&definition_id, 1, retired.revision, UtcMicros(46))
            .unwrap_err(),
    ] {
        assert_eq!(error, WorkflowCoordinationError::IllegalLifecycleTransition);
    }

    // An unregistered version is never found, activated or otherwise.
    assert_eq!(
        service
            .activate(&definition_id, 9, 1, UtcMicros(47))
            .unwrap_err(),
        WorkflowCoordinationError::DefinitionNotFound
    );
}

#[test]
fn every_lifecycle_transition_replays_without_a_second_effect() {
    let (authority, service, context) = lifecycle_service();
    let registered = service.register(&context, definition(1)).unwrap();
    let definition_id = registered.definition_id().clone();

    let activated = service
        .activate(&definition_id, 1, 1, UtcMicros(50))
        .unwrap();
    assert_eq!(
        service
            .activate(&definition_id, 1, 1, UtcMicros(51))
            .unwrap(),
        activated,
        "replayed activation must return the stored disposition unchanged"
    );

    let retired = service
        .retire(&definition_id, 1, activated.revision, UtcMicros(52))
        .unwrap();
    assert_eq!(
        service
            .retire(&definition_id, 1, activated.revision, UtcMicros(53))
            .unwrap(),
        retired,
        "replayed retirement must not advance a terminal disposition"
    );
    assert_eq!(
        authority.state.lock().unwrap().transitions.len(),
        3,
        "replay must not append a second immutable history entry"
    );

    let second = service.register(&context, definition(2)).unwrap();
    let rejected = service
        .reject(second.definition_id(), 2, 1, UtcMicros(54))
        .unwrap();
    assert_eq!(
        service
            .reject(second.definition_id(), 2, 1, UtcMicros(55))
            .unwrap(),
        rejected,
        "replayed rejection must return the stored terminal disposition"
    );
}
