use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use schemars::schema_for;
use tracedecay_application::{
    MAX_CALIBRATED_SCORE_MICROS, MAX_TASK_HANDOFF_LIFETIME_MICROS, TaskHandoffAuthorityError,
    TaskHandoffAuthorityPort, TaskHandoffConsumeOutcome, TaskHandoffError, TaskHandoffGrant,
    TaskHandoffScope, TaskHandoffService, TaskHandoffToken, WORKFLOW_CANONICAL_WORK_OPERATION,
    WorkflowActivation, WorkflowCoordinationError, WorkflowDefinitionAuthorityError,
    WorkflowDefinitionAuthorityPort, WorkflowDefinitionService, WorkflowPlacementCandidate,
    WorkflowPlacementError, WorkflowPlacementPort, WorkflowPlacementRequest,
    WorkflowPlacementService, work_executable_catalog_digest,
};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, ProviderId, RepositoryId, RunId, TaskId, ThreadId,
    UtcMicros, WorkProviderRouteId, WorkProviderRouteV1, WorkflowDefinitionId,
    WorkflowDefinition, WorkflowOperationRef, WorkflowOutputName, WorkflowStepId, WorkflowStep,
    WorktreeId,
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

fn definition(version: u64) -> WorkflowDefinition {
    definition_with_operation(version, WORKFLOW_CANONICAL_WORK_OPERATION)
}

fn definition_with_operation(version: u64, operation: &str) -> WorkflowDefinition {
    definition_with_operation_and_catalog(
        version,
        operation,
        work_executable_catalog_digest().unwrap(),
    )
}

fn definition_with_operation_and_catalog(
    version: u64,
    operation: &str,
    catalog_digest: ManifestDigest,
) -> WorkflowDefinition {
    WorkflowDefinition::new(
        id("workflow.definition.coordination"),
        version,
        id::<ProjectId>("project.workflow.coordination"),
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
    definitions: BTreeMap<(WorkflowDefinitionId, u64), WorkflowDefinition>,
    active: BTreeMap<WorkflowDefinitionId, u64>,
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
        state.definitions.insert(key, definition.clone());
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
        WorkflowActivation {
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

#[test]
fn activation_rejects_a_definition_pinned_to_another_catalog() {
    let authority = FakeDefinitionAuthority::default();
    let service = WorkflowDefinitionService::new(authority);
    let definition =
        definition_with_operation_and_catalog(1, WORKFLOW_CANONICAL_WORK_OPERATION, digest('c'));
    service.register(definition.clone()).unwrap();

    assert_eq!(
        service
            .activate(
                definition.definition_id(),
                None,
                definition.definition_version(),
            )
            .unwrap_err(),
        WorkflowCoordinationError::CatalogDigestMismatch
    );
}

#[derive(Clone)]
struct FakePlacement {
    candidates: Vec<WorkflowPlacementCandidate>,
}

impl WorkflowPlacementPort for FakePlacement {
    fn candidates(
        &self,
        _request: &WorkflowPlacementRequest,
    ) -> Result<Vec<WorkflowPlacementCandidate>, WorkflowPlacementError> {
        Ok(self.candidates.clone())
    }
}

fn route(provider: &str, route: &str) -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(id::<ProviderId>(provider), id::<WorkProviderRouteId>(route)).unwrap()
}

fn placement_request() -> WorkflowPlacementRequest {
    WorkflowPlacementRequest {
        definition_id: id("workflow.definition.coordination"),
        definition_version: 1,
        run_id: id::<RunId>("run.workflow.coordination"),
        step_id: id::<WorkflowStepId>("prepare"),
        task_id: id::<TaskId>("task.workflow.coordination.prepare"),
        required_expertise_digest: digest('e'),
        calibration_profile_digest: digest('c'),
        minimum_calibrated_score_micros: 500_000,
    }
}

fn matching_candidate(
    provider: &str,
    route_id: &str,
    priority: u32,
    score: u32,
) -> WorkflowPlacementCandidate {
    WorkflowPlacementCandidate {
        route: route(provider, route_id),
        priority,
        expertise_digest: digest('e'),
        calibration_profile_digest: digest('c'),
        calibrated_score_micros: score,
    }
}

#[test]
fn placement_is_deterministic_and_unavailability_is_typed() {
    assert_eq!(MAX_CALIBRATED_SCORE_MICROS, 1_000_000u32);
    let later = matching_candidate("provider.z", "route.z", 7, 900_000);
    let chosen = matching_candidate("provider.a", "route.a", 7, 900_000);
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

#[test]
fn placement_filters_invalid_evidence_and_rejects_invalid_requests() {
    let mismatched_expertise = WorkflowPlacementCandidate {
        route: route("provider.a", "route.mismatch"),
        priority: 1,
        expertise_digest: digest('d'),
        calibration_profile_digest: digest('c'),
        calibrated_score_micros: 999_999,
    };
    let below_threshold = matching_candidate("provider.b", "route.low", 1, 499_999);
    let out_of_range_score = matching_candidate("provider.c", "route.over", 1, 1_000_001);
    let eligible_later = matching_candidate("provider.z", "route.z", 3, 500_000);
    let eligible_chosen = matching_candidate("provider.a", "route.a", 3, 750_000);

    let service = WorkflowPlacementService::new(FakePlacement {
        candidates: vec![
            mismatched_expertise,
            below_threshold,
            out_of_range_score,
            eligible_later,
            eligible_chosen.clone(),
        ],
    });
    assert_eq!(
        service.place(&placement_request()).unwrap(),
        eligible_chosen.route
    );

    let only_invalid = WorkflowPlacementService::new(FakePlacement {
        candidates: vec![matching_candidate("provider.a", "route.a", 1, 100)],
    })
    .place(&placement_request())
    .unwrap_err();
    assert_eq!(
        only_invalid,
        WorkflowPlacementError::Unavailable {
            step_id: id("prepare"),
        }
    );

    let mut zero_version = placement_request();
    zero_version.definition_version = 0;
    assert_eq!(
        WorkflowPlacementService::new(FakePlacement {
            candidates: vec![eligible_chosen.clone()],
        })
        .place(&zero_version)
        .unwrap_err(),
        WorkflowPlacementError::InvalidRequest
    );

    let mut invalid_minimum = placement_request();
    invalid_minimum.minimum_calibrated_score_micros = 1_000_001;
    assert_eq!(
        WorkflowPlacementService::new(FakePlacement {
            candidates: vec![eligible_chosen],
        })
        .place(&invalid_minimum)
        .unwrap_err(),
        WorkflowPlacementError::InvalidRequest
    );

    let schema = serde_json::to_value(schema_for!(WorkflowPlacementRequest)).unwrap();
    assert_eq!(schema["properties"]["definition_version"]["minimum"], 1);
    assert_eq!(
        schema["properties"]["minimum_calibrated_score_micros"]["maximum"],
        1_000_000
    );
    assert_eq!(
        schema["properties"]["minimum_calibrated_score_micros"]["type"],
        "integer"
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
        Ok(TaskHandoffConsumeOutcome::Consumed)
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

#[test]
fn handoff_enforces_authorization_scope_expiry_and_single_use_without_bearer_leakage() {
    assert_eq!(MAX_TASK_HANDOFF_LIFETIME_MICROS, UtcMicros(60_000_000));
    let authority = FakeHandoffAuthority::default();
    let service = TaskHandoffService::new(authority);
    let scope = handoff_scope();
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

    service
        .issue(
            scope.from_actor_id(),
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
            .redeem(&handoff, &wrong_task, scope.to_actor_id(), UtcMicros(11),)
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
            .redeem(&handoff, &wrong_thread, scope.to_actor_id(), UtcMicros(11),)
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
            .redeem(
                &handoff,
                &wrong_definition,
                scope.to_actor_id(),
                UtcMicros(11),
            )
            .unwrap_err(),
        TaskHandoffError::ScopeMismatch
    );

    // Half-open expiry: consumed_at == expires_at is Expired.
    assert_eq!(
        service
            .redeem(&handoff, &scope, scope.to_actor_id(), UtcMicros(20))
            .unwrap_err(),
        TaskHandoffError::Expired
    );
    service
        .redeem(&handoff, &scope, scope.to_actor_id(), UtcMicros(19))
        .unwrap();
    assert_eq!(
        service
            .redeem(&handoff, &scope, scope.to_actor_id(), UtcMicros(19))
            .unwrap_err(),
        TaskHandoffError::Replay
    );

    let expired = token('e');
    service
        .issue(
            scope.from_actor_id(),
            scope.clone(),
            &expired,
            UtcMicros(20),
            UtcMicros(10),
        )
        .unwrap();
    assert_eq!(
        service
            .redeem(&expired, &scope, scope.to_actor_id(), UtcMicros(21))
            .unwrap_err(),
        TaskHandoffError::Expired
    );

    assert_eq!(
        service
            .issue(
                scope.from_actor_id(),
                scope.clone(),
                &token('x'),
                UtcMicros(10),
                UtcMicros(10),
            )
            .unwrap_err(),
        TaskHandoffError::InvalidExpiry
    );

    // Maximum lifetime is 60 seconds (60_000_000 micros).
    assert_eq!(
        service
            .issue(
                scope.from_actor_id(),
                scope.clone(),
                &token('l'),
                UtcMicros(10 + 60_000_001),
                UtcMicros(10),
            )
            .unwrap_err(),
        TaskHandoffError::InvalidExpiry
    );
    service
        .issue(
            scope.from_actor_id(),
            scope.clone(),
            &token('m'),
            UtcMicros(10 + 60_000_000),
            UtcMicros(10),
        )
        .unwrap();
}

#[test]
fn handoff_grant_deserialization_fails_closed_on_scope_and_expiry() {
    let scope = handoff_scope();
    let grant =
        TaskHandoffGrant::new(scope.clone(), digest('f'), UtcMicros(10), UtcMicros(20)).unwrap();
    assert_eq!(grant.scope(), &scope);
    assert_eq!(*grant.issued_at(), UtcMicros(10));
    assert_eq!(*grant.expires_at(), UtcMicros(20));
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
    inverted["issued_at"] = serde_json::json!(21);
    inverted["expires_at"] = serde_json::json!(20);
    assert!(serde_json::from_value::<TaskHandoffGrant>(inverted).is_err());

    let mut too_long = json.clone();
    too_long["issued_at"] = serde_json::json!(10);
    too_long["expires_at"] = serde_json::json!(10 + 60_000_001);
    assert!(serde_json::from_value::<TaskHandoffGrant>(too_long).is_err());

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
