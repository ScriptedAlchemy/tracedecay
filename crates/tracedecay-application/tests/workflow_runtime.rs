use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

use tracedecay_application::{
    WorkflowChildExecutionBatchV1, WorkflowChildExecutionOutcomeV1, WorkflowChildExecutionPort,
    WorkflowChildExecutionResultV1, WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError,
    WorkflowExecutionAuthorityPort, WorkflowExecutionFenceV1, WorkflowExecutionIdentityV1,
    WorkflowExecutionTruthV1, WorkflowFanOutCheckpointV1, WorkflowFanOutInputV1,
    WorkflowFanOutRequestV1, WorkflowFanOutRuntimeError, WorkflowFanOutRuntimeService,
    WorkflowRecoveryDirectiveV1, WorkflowSynthesisError, WorkflowSynthesisPort,
    WorkflowSynthesisRequestV1, WorkflowSynthesisTruthV1,
};
use tracedecay_domain::{
    AttemptId, ManifestDigest, ProjectId, RunId, TaskId, WorkFenceEpochV1, WorkLeaseFenceV1,
    WorkLeaseId, WorkflowDefinitionV1, WorkflowFanOutV1, WorkflowOperationRef, WorkflowOutputName,
    WorkflowStepId, WorkflowStepV1,
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

fn definition(max_parallel: u32) -> WorkflowDefinitionV1 {
    WorkflowDefinitionV1::new(
        id("workflow.definition.runtime"),
        1,
        id::<ProjectId>("project.workflow.runtime"),
        vec![WorkflowStepV1 {
            step_id: id::<WorkflowStepId>("fan-out"),
            operation: id::<WorkflowOperationRef>("operation.review.v1"),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("finding")],
            fan_out: Some(WorkflowFanOutV1 { max_parallel }),
        }],
        digest('a'),
        digest('b'),
        digest('c'),
    )
    .unwrap()
}

fn fence(epoch: u64) -> WorkflowExecutionFenceV1 {
    WorkflowExecutionFenceV1 {
        attempt_id: id::<AttemptId>("attempt.workflow.runtime"),
        lease: WorkLeaseFenceV1::new(
            id::<WorkLeaseId>("lease.workflow.runtime"),
            WorkFenceEpochV1::new(epoch).unwrap(),
        )
        .unwrap(),
    }
}

fn input(identity: &str, byte: char) -> WorkflowFanOutInputV1 {
    WorkflowFanOutInputV1 {
        identity: identity.to_owned(),
        input_digest: digest(byte),
    }
}

fn request(inputs: Vec<WorkflowFanOutInputV1>, epoch: u64) -> WorkflowFanOutRequestV1 {
    WorkflowFanOutRequestV1 {
        definition: definition(4),
        run_id: id::<RunId>("run.workflow.runtime"),
        step_id: id::<WorkflowStepId>("fan-out"),
        fence: fence(epoch),
        inputs,
    }
}

#[derive(Default)]
struct AuthorityState {
    active_fence: Option<WorkflowExecutionFenceV1>,
    checkpoint: Option<WorkflowFanOutCheckpointV1>,
    terminal: Option<WorkflowExecutionTruthV1>,
    begins: usize,
    recovery: WorkflowRecoveryDirectiveV1,
    executing: bool,
}

#[derive(Clone)]
struct FakeAuthority {
    state: Arc<Mutex<AuthorityState>>,
    changed: Arc<Condvar>,
}

impl Default for FakeAuthority {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(AuthorityState::default())),
            changed: Arc::new(Condvar::new()),
        }
    }
}

impl WorkflowExecutionAuthorityPort for FakeAuthority {
    fn begin(
        &self,
        _identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        _plan_digest: &ManifestDigest,
    ) -> Result<WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError> {
        let mut state = self.state.lock().unwrap();
        while state.executing && state.terminal.is_none() {
            state = self.changed.wait(state).unwrap();
        }
        state.begins += 1;
        if let Some(terminal) = &state.terminal {
            return Ok(WorkflowExecutionAdmissionV1::Replay(terminal.clone()));
        }
        if let Some(active) = &state.active_fence {
            if active.lease.lease_id() != fence.lease.lease_id()
                || active.lease.epoch() >= fence.lease.epoch()
            {
                return Ok(WorkflowExecutionAdmissionV1::StaleFence);
            }
        }
        state.active_fence = Some(fence.clone());
        state.executing = true;
        match state.checkpoint.clone() {
            Some(checkpoint) => Ok(WorkflowExecutionAdmissionV1::Recover {
                checkpoint,
                directive: state.recovery,
            }),
            None => Ok(WorkflowExecutionAdmissionV1::Execute),
        }
    }

    fn checkpoint(
        &self,
        _identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        checkpoint: &WorkflowFanOutCheckpointV1,
    ) -> Result<(), WorkflowExecutionAuthorityError> {
        let mut state = self.state.lock().unwrap();
        if state.active_fence.as_ref() != Some(fence) {
            return Err(WorkflowExecutionAuthorityError::Conflict);
        }
        state.checkpoint = Some(checkpoint.clone());
        if checkpoint.children.iter().any(|child| {
            matches!(
                child.outcome,
                WorkflowChildExecutionOutcomeV1::Interrupted { .. }
            )
        }) {
            state.executing = false;
            self.changed.notify_all();
        }
        Ok(())
    }

    fn complete(
        &self,
        _identity: &WorkflowExecutionIdentityV1,
        fence: &WorkflowExecutionFenceV1,
        truth: &WorkflowExecutionTruthV1,
    ) -> Result<(), WorkflowExecutionAuthorityError> {
        let mut state = self.state.lock().unwrap();
        if state.active_fence.as_ref() != Some(fence) {
            return Err(WorkflowExecutionAuthorityError::Conflict);
        }
        state.terminal = Some(truth.clone());
        state.executing = false;
        self.changed.notify_all();
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FakeChildren {
    calls: Arc<Mutex<Vec<TaskId>>>,
    batches: Arc<Mutex<Vec<(u32, usize)>>>,
    outcomes: Arc<Mutex<BTreeMap<String, WorkflowChildExecutionOutcomeV1>>>,
}

impl WorkflowChildExecutionPort for FakeChildren {
    fn execute_bounded(
        &self,
        batch: &WorkflowChildExecutionBatchV1,
    ) -> Result<Vec<WorkflowChildExecutionResultV1>, WorkflowFanOutRuntimeError> {
        self.batches
            .lock()
            .unwrap()
            .push((batch.max_parallel, batch.children.len()));
        let outcomes = self.outcomes.lock().unwrap();
        Ok(batch
            .children
            .iter()
            .map(|request| {
                self.calls.lock().unwrap().push(request.task_id.clone());
                WorkflowChildExecutionResultV1 {
                    task_id: request.task_id.clone(),
                    outcome: outcomes
                        .get(&request.input.identity)
                        .cloned()
                        .unwrap_or_else(|| WorkflowChildExecutionOutcomeV1::Succeeded {
                            output_digest: request.input.input_digest.clone(),
                        }),
                }
            })
            .collect())
    }
}

#[derive(Clone, Default)]
struct FakeSynthesis {
    joins: Arc<Mutex<Vec<Vec<TaskId>>>>,
    fail: Arc<Mutex<bool>>,
}

impl WorkflowSynthesisPort for FakeSynthesis {
    fn synthesize(
        &self,
        request: &WorkflowSynthesisRequestV1,
    ) -> Result<ManifestDigest, WorkflowSynthesisError> {
        self.joins.lock().unwrap().push(
            request
                .children
                .iter()
                .map(|child| child.task_id.clone())
                .collect(),
        );
        if *self.fail.lock().unwrap() {
            Err(WorkflowSynthesisError::Failed("fixture".to_owned()))
        } else {
            Ok(digest('f'))
        }
    }
}

#[test]
fn execution_rejects_empty_over_limit_and_duplicate_fan_out_before_authority() {
    let authority = FakeAuthority::default();
    let service = WorkflowFanOutRuntimeService::new(
        authority.clone(),
        FakeChildren::default(),
        FakeSynthesis::default(),
    );

    assert_eq!(
        service.execute(request(Vec::new(), 1)).unwrap_err(),
        WorkflowFanOutRuntimeError::EmptyFanOut
    );
    assert_eq!(
        service
            .execute(request(
                vec![
                    input("a", '1'),
                    input("b", '2'),
                    input("c", '3'),
                    input("d", '4'),
                    input("e", '5'),
                ],
                1,
            ))
            .unwrap_err(),
        WorkflowFanOutRuntimeError::FanOutLimitExceeded {
            limit: 4,
            actual: 5
        }
    );
    assert_eq!(
        service
            .execute(request(vec![input("same", '1'), input("same", '2')], 1))
            .unwrap_err(),
        WorkflowFanOutRuntimeError::DuplicateChildIdentity("same".to_owned())
    );
    assert_eq!(authority.state.lock().unwrap().begins, 0);
}

#[test]
fn deterministic_join_and_replay_do_not_repeat_work_or_synthesis() {
    let authority = FakeAuthority::default();
    let children = FakeChildren::default();
    let synthesis = FakeSynthesis::default();
    let service = Arc::new(WorkflowFanOutRuntimeService::new(
        authority,
        children.clone(),
        synthesis.clone(),
    ));
    let request = request(vec![input("zeta", '1'), input("alpha", '2')], 1);

    let first_service = Arc::clone(&service);
    let first_request = request.clone();
    let first = std::thread::spawn(move || first_service.execute(first_request).unwrap());
    let replay_service = Arc::clone(&service);
    let replay = std::thread::spawn(move || replay_service.execute(request).unwrap());
    let first = first.join().unwrap();
    let replay = replay.join().unwrap();
    assert_eq!(first, replay);
    assert_eq!(children.calls.lock().unwrap().len(), 2);
    assert_eq!(&*children.batches.lock().unwrap(), &[(4, 2)]);
    assert_eq!(synthesis.joins.lock().unwrap().len(), 1);
    let joined = &synthesis.joins.lock().unwrap()[0];
    assert!(joined.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(matches!(
        first,
        WorkflowExecutionTruthV1::Synthesized(WorkflowSynthesisTruthV1::Complete { .. })
    ));
}

#[test]
fn partial_child_failure_and_synthesis_failure_remain_typed_truth() {
    let authority = FakeAuthority::default();
    let children = FakeChildren::default();
    children.outcomes.lock().unwrap().insert(
        "bad".to_owned(),
        WorkflowChildExecutionOutcomeV1::Failed {
            evidence_digest: digest('9'),
        },
    );
    let synthesis = FakeSynthesis::default();
    let service = WorkflowFanOutRuntimeService::new(authority, children, synthesis.clone());

    let partial = service
        .execute(request(vec![input("good", '1'), input("bad", '2')], 1))
        .unwrap();
    assert!(matches!(
        partial,
        WorkflowExecutionTruthV1::Synthesized(WorkflowSynthesisTruthV1::Partial {
            ref failed_children,
            ..
        }) if failed_children.len() == 1
    ));

    let authority = FakeAuthority::default();
    *synthesis.fail.lock().unwrap() = true;
    let failed = WorkflowFanOutRuntimeService::new(authority, FakeChildren::default(), synthesis)
        .execute(request(vec![input("only", '3')], 1))
        .unwrap();
    assert!(matches!(
        failed,
        WorkflowExecutionTruthV1::Synthesized(WorkflowSynthesisTruthV1::Failed {
            synthesis_error: Some(_),
            ..
        })
    ));
}

#[test]
fn stale_fence_is_rejected_and_interrupted_children_resume_without_replay() {
    let authority = FakeAuthority::default();
    authority.state.lock().unwrap().active_fence = Some(fence(2));
    let service = WorkflowFanOutRuntimeService::new(
        authority.clone(),
        FakeChildren::default(),
        FakeSynthesis::default(),
    );
    assert_eq!(
        service
            .execute(request(vec![input("a", '1')], 1))
            .unwrap_err(),
        WorkflowFanOutRuntimeError::StaleFence
    );

    authority.state.lock().unwrap().active_fence = None;
    let children = FakeChildren::default();
    children.outcomes.lock().unwrap().insert(
        "b".to_owned(),
        WorkflowChildExecutionOutcomeV1::Interrupted {
            checkpoint_digest: Some(digest('8')),
        },
    );
    let service = WorkflowFanOutRuntimeService::new(
        authority.clone(),
        children.clone(),
        FakeSynthesis::default(),
    );
    let interrupted = service
        .execute(request(vec![input("a", '1'), input("b", '2')], 1))
        .unwrap();
    assert!(matches!(
        interrupted,
        WorkflowExecutionTruthV1::Interrupted {
            directive: WorkflowRecoveryDirectiveV1::ResumeIncomplete,
            ..
        }
    ));

    children.outcomes.lock().unwrap().remove("b");
    authority.state.lock().unwrap().recovery = WorkflowRecoveryDirectiveV1::ResumeIncomplete;
    let mut restarted_request = request(vec![input("a", '1'), input("b", '2')], 2);
    restarted_request.fence.attempt_id = id("attempt.workflow.runtime.restart");
    let completed = service.execute(restarted_request).unwrap();
    assert!(matches!(
        completed,
        WorkflowExecutionTruthV1::Synthesized(WorkflowSynthesisTruthV1::Complete { .. })
    ));
    let calls = children.calls.lock().unwrap();
    assert_eq!(
        calls.len(),
        3,
        "completed child must not rerun after restart"
    );
    assert_eq!(&*children.batches.lock().unwrap(), &[(4, 2), (4, 1)]);
}

#[test]
fn restart_all_reexecutes_completed_children_under_a_new_fence() {
    let authority = FakeAuthority::default();
    let children = FakeChildren::default();
    children.outcomes.lock().unwrap().insert(
        "b".to_owned(),
        WorkflowChildExecutionOutcomeV1::Interrupted {
            checkpoint_digest: Some(digest('8')),
        },
    );
    let service = WorkflowFanOutRuntimeService::new(
        authority.clone(),
        children.clone(),
        FakeSynthesis::default(),
    );
    service
        .execute(request(vec![input("a", '1'), input("b", '2')], 1))
        .unwrap();

    children.outcomes.lock().unwrap().remove("b");
    authority.state.lock().unwrap().recovery = WorkflowRecoveryDirectiveV1::RestartAll;
    let mut restarted_request = request(vec![input("a", '1'), input("b", '2')], 2);
    restarted_request.fence.attempt_id = id("attempt.workflow.runtime.restart-all");
    let completed = service.execute(restarted_request).unwrap();

    assert!(matches!(
        completed,
        WorkflowExecutionTruthV1::Synthesized(WorkflowSynthesisTruthV1::Complete { .. })
    ));
    assert_eq!(children.calls.lock().unwrap().len(), 4);
    assert_eq!(&*children.batches.lock().unwrap(), &[(4, 2), (4, 2)]);
}
