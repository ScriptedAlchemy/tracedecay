use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    TaskHandoffAuthorityError, TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome,
    TaskHandoffError, TaskHandoffGrantV1, TaskHandoffScopeV1, TaskHandoffService, TaskHandoffToken,
    WorkflowActivationV1, WorkflowCoordinationError, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort, WorkflowDefinitionService, WorkflowPlacementCandidateV1,
    WorkflowPlacementError, WorkflowPlacementPort, WorkflowPlacementRequestV1,
    WorkflowPlacementService,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProviderId, RepositoryId, RunId, TaskId, UtcMicros,
    WorkProviderRouteId, WorkProviderRouteV1, WorkflowDefinitionId, WorkflowDefinitionV1,
    WorkflowOperationRef, WorkflowOutputName, WorkflowStepId, WorkflowStepV1, WorktreeId,
};

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

fn definition(version: u64) -> WorkflowDefinitionV1 {
    definition_with_operation(version, "operation.prepare.v1")
}

fn definition_with_operation(version: u64, operation: &str) -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id("workflow.definition.coordination"),
        version,
        id::<ProjectId>("project.workflow.coordination"),
        vec![WorkflowStepV1 {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>(operation),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: None,
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap()
}

#[derive(Clone, Default)]
struct FakeDefinitionAuthority {
    state: Arc<Mutex<DefinitionState>>,
}

#[derive(Default)]
struct DefinitionState {
    definitions: BTreeMap<(WorkflowDefinitionId, u64), WorkflowDefinitionV1>,
    active: BTreeMap<WorkflowDefinitionId, u64>,
}

impl WorkflowDefinitionAuthorityPort for FakeDefinitionAuthority {
    fn insert(
        &self,
        definition: &WorkflowDefinitionV1,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let key = (
            definition.definition_id().clone(),
            definition.definition_version(),
        );
        let mut state = self.state.lock().unwrap();
        if state.definitions.contains_key(&key) {
            return Err(WorkflowDefinitionAuthorityError::AlreadyExists);
        }
        state.definitions.insert(key, definition.clone());
        Ok(())
    }

    fn load(
        &self,
        definition_id: &WorkflowDefinitionId,
        definition_version: u64,
    ) -> Result<Option<WorkflowDefinitionV1>, WorkflowDefinitionAuthorityError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .definitions
            .get(&(definition_id.clone(), definition_version))
            .cloned())
    }

    fn active_version(
        &self,
        definition_id: &WorkflowDefinitionId,
    ) -> Result<Option<u64>, WorkflowDefinitionAuthorityError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .active
            .get(definition_id)
            .copied())
    }

    fn compare_and_swap_activation(
        &self,
        definition_id: &WorkflowDefinitionId,
        expected_version: Option<u64>,
        replacement_version: u64,
    ) -> Result<(), WorkflowDefinitionAuthorityError> {
        let mut state = self.state.lock().unwrap();
        if state.active.get(definition_id).copied() != expected_version {
            return Err(WorkflowDefinitionAuthorityError::Conflict);
        }
        state
            .active
            .insert(definition_id.clone(), replacement_version);
        Ok(())
    }
}

#[test]
fn immutable_definition_versions_and_activation_use_compare_and_swap() {
    let authority = FakeDefinitionAuthority::default();
    let service = WorkflowDefinitionService::new(authority);
    let first = definition(1);
    let second = definition(2);

    assert_eq!(service.register(first.clone()).unwrap(), first);
    assert_eq!(service.register(second.clone()).unwrap(), second);
    assert_eq!(service.register(first.clone()).unwrap(), first);
    assert_eq!(
        service
            .register(definition_with_operation(1, "operation.prepare.v2"))
            .unwrap_err(),
        WorkflowCoordinationError::ImmutableDefinitionConflict
    );

    let activated = service
        .activate(first.definition_id(), None, first.definition_version())
        .unwrap();
    assert_eq!(
        activated,
        WorkflowActivationV1 {
            definition_id: first.definition_id().clone(),
            active_version: 1,
        }
    );

    assert_eq!(
        service
            .activate(first.definition_id(), None, second.definition_version())
            .unwrap_err(),
        WorkflowCoordinationError::StaleActivation
    );
    assert_eq!(
        service
            .activate(
                first.definition_id(),
                Some(first.definition_version()),
                second.definition_version(),
            )
            .unwrap()
            .active_version,
        2
    );
}

#[derive(Clone)]
struct FakePlacement {
    candidates: Vec<WorkflowPlacementCandidateV1>,
}

impl WorkflowPlacementPort for FakePlacement {
    fn candidates(
        &self,
        _request: &WorkflowPlacementRequestV1,
    ) -> Result<Vec<WorkflowPlacementCandidateV1>, WorkflowPlacementError> {
        Ok(self.candidates.clone())
    }
}

fn route(provider: &str, route: &str) -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(id::<ProviderId>(provider), id::<WorkProviderRouteId>(route)).unwrap()
}

fn placement_request() -> WorkflowPlacementRequestV1 {
    WorkflowPlacementRequestV1 {
        definition_id: id("workflow.definition.coordination"),
        definition_version: 1,
        run_id: id::<RunId>("run.workflow.coordination"),
        step_id: id::<WorkflowStepId>("prepare"),
        task_id: id::<TaskId>("task.workflow.coordination.prepare"),
    }
}

#[test]
fn placement_is_deterministic_and_unavailability_is_typed() {
    let later = WorkflowPlacementCandidateV1 {
        route: route("provider.z", "route.z"),
        priority: 7,
    };
    let chosen = WorkflowPlacementCandidateV1 {
        route: route("provider.a", "route.a"),
        priority: 7,
    };
    let service = WorkflowPlacementService::new(FakePlacement {
        candidates: vec![later, chosen.clone()],
    });

    assert_eq!(service.place(&placement_request()).unwrap(), chosen.route);

    let unavailable = WorkflowPlacementService::new(FakePlacement {
        candidates: Vec::new(),
    })
    .place(&placement_request())
    .unwrap_err();
    assert_eq!(
        unavailable,
        WorkflowPlacementError::Unavailable {
            step_id: id("prepare"),
        }
    );
}

#[derive(Clone, Default)]
struct FakeHandoffAuthority {
    grants: Arc<Mutex<BTreeMap<ManifestDigest, (TaskHandoffGrantV1, bool)>>>,
}

impl TaskHandoffAuthorityPort for FakeHandoffAuthority {
    fn issue(&self, grant: &TaskHandoffGrantV1) -> Result<(), TaskHandoffAuthorityError> {
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
        expected_scope: &TaskHandoffScopeV1,
        consumed_at: UtcMicros,
    ) -> Result<TaskHandoffConsumeOutcome, TaskHandoffAuthorityError> {
        let mut grants = self.grants.lock().unwrap();
        let Some((grant, consumed)) = grants.get_mut(token_digest) else {
            return Ok(TaskHandoffConsumeOutcome::Missing);
        };
        if &grant.scope != expected_scope {
            return Ok(TaskHandoffConsumeOutcome::ScopeMismatch);
        }
        if consumed_at > grant.expires_at {
            return Ok(TaskHandoffConsumeOutcome::Expired);
        }
        if *consumed {
            return Ok(TaskHandoffConsumeOutcome::Replay);
        }
        *consumed = true;
        Ok(TaskHandoffConsumeOutcome::Consumed)
    }
}

fn handoff_scope() -> TaskHandoffScopeV1 {
    TaskHandoffScopeV1 {
        project_id: id::<ProjectId>("project.workflow.coordination"),
        repository_id: id::<RepositoryId>("repository.workflow.coordination"),
        worktree_id: id::<WorktreeId>("worktree.workflow.coordination"),
        task_id: id::<TaskId>("task.workflow.coordination.prepare"),
        run_id: id::<RunId>("run.workflow.coordination"),
        from_actor_id: id::<ActorId>("actor.workflow.source"),
        to_actor_id: id::<ActorId>("actor.workflow.target"),
    }
}

fn token(value: char) -> TaskHandoffToken {
    TaskHandoffToken::new(value.to_string().repeat(48)).unwrap()
}

#[test]
fn handoff_enforces_authorization_scope_expiry_and_single_use_without_bearer_leakage() {
    let authority = FakeHandoffAuthority::default();
    let service = TaskHandoffService::new(authority);
    let scope = handoff_scope();
    let handoff = token('s');
    let debug = format!("{handoff:?}");
    assert!(!debug.contains(&"s".repeat(48)));

    service
        .issue(
            &scope.from_actor_id,
            scope.clone(),
            &handoff,
            UtcMicros(20),
            UtcMicros(10),
        )
        .unwrap();

    assert_eq!(
        service
            .redeem(
                &handoff,
                &scope,
                &id::<ActorId>("actor.workflow.other"),
                UtcMicros(11),
            )
            .unwrap_err(),
        TaskHandoffError::Unauthorized
    );

    let mut wrong_scope = scope.clone();
    wrong_scope.task_id = id("task.workflow.coordination.other");
    assert_eq!(
        service
            .redeem(&handoff, &wrong_scope, &scope.to_actor_id, UtcMicros(11),)
            .unwrap_err(),
        TaskHandoffError::ScopeMismatch
    );

    service
        .redeem(&handoff, &scope, &scope.to_actor_id, UtcMicros(11))
        .unwrap();
    assert_eq!(
        service
            .redeem(&handoff, &scope, &scope.to_actor_id, UtcMicros(12))
            .unwrap_err(),
        TaskHandoffError::Replay
    );

    let expired = token('e');
    service
        .issue(
            &scope.from_actor_id,
            scope.clone(),
            &expired,
            UtcMicros(20),
            UtcMicros(10),
        )
        .unwrap();
    assert_eq!(
        service
            .redeem(&expired, &scope, &scope.to_actor_id, UtcMicros(21))
            .unwrap_err(),
        TaskHandoffError::Expired
    );
}
