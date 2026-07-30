use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

use tracedecay_application::{
    CancellationContext, WorkflowChildExecutionBatchV1, WorkflowChildExecutionOutcomeV1,
    WorkflowChildExecutionPort, WorkflowChildExecutionResultV1, WorkflowChildRecordV1,
    WorkflowExecutionAdmissionV1, WorkflowExecutionAuthorityError, WorkflowExecutionAuthorityPort,
    WorkflowExecutionFenceV1, WorkflowExecutionIdentityV1, WorkflowExecutionTruthV1,
    WorkflowFanOutCheckpointV1, WorkflowFanOutInputV1, WorkflowFanOutRequestV1,
    WorkflowFanOutRuntimeError, WorkflowFanOutRuntimeService, WorkflowRecoveryDirectiveV1,
    WorkflowSynthesisError, WorkflowSynthesisPort, WorkflowSynthesisRequestV1,
    WorkflowSynthesisTruthV1,
};
use tracedecay_domain::{
    AttemptId, ManifestDigest, ProjectId, RunId, TaskId, UtcMicros, WorkCommandId, WorkFenceEpochV1,
    WorkLeaseFenceV1, WorkLeaseId, WorkflowDefinitionV1, WorkflowFanOutV1, WorkflowOperationRef,
    WorkflowOutputName, WorkflowStepId, WorkflowStepV1,
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

fn active_cancellation() -> CancellationContext {
    CancellationContext::active("cancel.workflow.runtime").unwrap()
}

fn request(inputs: Vec<WorkflowFanOutInputV1>, epoch: u64) -> WorkflowFanOutRequestV1 {
    WorkflowFanOutRequestV1 {
        definition: definition(4),
        run_id: id::<RunId>("run.workflow.runtime"),
        step_id: id::<WorkflowStepId>("fan-out"),
        fence: fence(epoch),
        cancellation: active_cancellation(),
        inputs,
    }
}

#[derive(Default)]
struct AuthorityState {
    active_fence: Option<WorkflowExecutionFenceV1>,
    checkpoint: Option<WorkflowFanOutCheckpointV1>,
    terminal: Option<WorkflowExecutionTruthV1>,
    begins: usize,
    completes: usize,
    recovery: WorkflowRecoveryDirectiveV1,
    executing: bool,
    fail_complete_once: bool,
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
        if state.fail_complete_once {
            state.fail_complete_once = false;
            state.executing = false;
            self.changed.notify_all();
            return Err(WorkflowExecutionAuthorityError::Unavailable(
                "complete uncertain".to_owned(),
            ));
        }
        state.completes += 1;
        state.terminal = Some(truth.clone());
        state.executing = false;
        self.changed.notify_all();
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FakeChildren {
    calls: Arc<Mutex<Vec<TaskId>>>,
    batches: Arc<Mutex<Vec<(u32, usize, CancellationContext)>>>,
    outcomes: Arc<Mutex<BTreeMap<String, WorkflowChildExecutionOutcomeV1>>>,
    spoof_extra: Arc<Mutex<Option<TaskId>>>,
}

impl WorkflowChildExecutionPort for FakeChildren {
    fn execute_bounded(
        &self,
        batch: &WorkflowChildExecutionBatchV1,
    ) -> Result<Vec<WorkflowChildExecutionResultV1>, WorkflowFanOutRuntimeError> {
        self.batches.lock().unwrap().push((
            batch.max_parallel,
            batch.children.len(),
            batch.cancellation.clone(),
        ));
        let outcomes = self.outcomes.lock().unwrap();
        let mut results = batch
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
            .collect::<Vec<_>>();
        if let Some(extra) = self.spoof_extra.lock().unwrap().clone() {
            // Replace the pending result with a non-pending planned task id so
            // length matches while violating the pending-only contract.
            if let Some(first) = results.first_mut() {
                first.task_id = extra;
                first.outcome = WorkflowChildExecutionOutcomeV1::Succeeded {
                    output_digest: digest('e'),
                };
            }
        }
        Ok(results)
    }
}

#[derive(Clone, Default)]
struct FakeSynthesis {
    joins: Arc<Mutex<Vec<Vec<TaskId>>>>,
    command_ids: Arc<Mutex<Vec<WorkCommandId>>>,
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
        self.command_ids
            .lock()
            .unwrap()
            .push(request.synthesis_command_id.clone());
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
    assert_eq!(children.batches.lock().unwrap().len(), 1);
    assert_eq!(children.batches.lock().unwrap()[0].0, 4);
    assert_eq!(children.batches.lock().unwrap()[0].1, 2);
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
    assert_eq!(
        children
            .batches
            .lock()
            .unwrap()
            .iter()
            .map(|batch| (batch.0, batch.1))
            .collect::<Vec<_>>(),
        vec![(4, 2), (4, 1)]
    );
    let checkpoint = authority.state.lock().unwrap().checkpoint.clone().unwrap();
    assert!(
        checkpoint
            .children
            .iter()
            .all(|child| !child.input.identity.is_empty() && child.input.input_digest.validate().is_ok()),
        "checkpoints must retain input digests; raw event rows are not backfill"
    );
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
    assert_eq!(
        children
            .batches
            .lock()
            .unwrap()
            .iter()
            .map(|batch| (batch.0, batch.1))
            .collect::<Vec<_>>(),
        vec![(4, 2), (4, 2)]
    );
}

#[test]
fn pre_dispatch_cancellation_completes_terminal_without_children_or_synthesis() {
    let authority = FakeAuthority::default();
    let children = FakeChildren::default();
    let synthesis = FakeSynthesis::default();
    let service = WorkflowFanOutRuntimeService::new(
        authority.clone(),
        children.clone(),
        synthesis.clone(),
    );
    let mut cancelled = request(vec![input("a", '1'), input("b", '2')], 1);
    cancelled.cancellation =
        CancellationContext::cancelled("cancel.workflow.runtime", UtcMicros(11)).unwrap();

    let truth = service.execute(cancelled).unwrap();
    assert!(matches!(
        truth,
        WorkflowExecutionTruthV1::Cancelled { ref cancellation }
            if cancellation.is_cancelled()
    ));
    assert!(children.calls.lock().unwrap().is_empty());
    assert!(synthesis.joins.lock().unwrap().is_empty());
    assert_eq!(authority.state.lock().unwrap().completes, 1);
    assert_eq!(authority.state.lock().unwrap().terminal.as_ref(), Some(&truth));

    // A fresh admission under a new fence (not a replay of cancelled terminal)
    // must proceed with active cancellation and the delegated max_parallel bound.
    {
        let mut state = authority.state.lock().unwrap();
        state.terminal = None;
        state.checkpoint = None;
        state.active_fence = None;
        state.executing = false;
    }
    let mut restarted = request(vec![input("a", '1'), input("b", '2')], 2);
    restarted.fence.attempt_id = id("attempt.workflow.runtime.after-cancel");
    let completed = service.execute(restarted).unwrap();
    assert!(matches!(
        completed,
        WorkflowExecutionTruthV1::Synthesized(WorkflowSynthesisTruthV1::Complete { .. })
    ));
    assert_eq!(children.calls.lock().unwrap().len(), 2);
    assert_eq!(children.batches.lock().unwrap()[0].0, 4);
}

#[test]
fn forged_checkpoint_records_are_rejected_against_current_plan() {
    let authority = FakeAuthority::default();
    let children = FakeChildren::default();
    let service = WorkflowFanOutRuntimeService::new(
        authority.clone(),
        children.clone(),
        FakeSynthesis::default(),
    );
    let first = request(vec![input("a", '1'), input("b", '2')], 1);
    service.execute(first.clone()).unwrap();

    let plan_digest = authority
        .state
        .lock()
        .unwrap()
        .checkpoint
        .as_ref()
        .unwrap()
        .plan_digest
        .clone();
    let forged_task = children.calls.lock().unwrap()[0].clone();
    authority.state.lock().unwrap().terminal = None;
    authority.state.lock().unwrap().executing = false;
    authority.state.lock().unwrap().recovery = WorkflowRecoveryDirectiveV1::ResumeIncomplete;
    authority.state.lock().unwrap().checkpoint = Some(WorkflowFanOutCheckpointV1 {
        plan_digest,
        children: vec![WorkflowChildRecordV1 {
            task_id: forged_task,
            input: input("a", '9'),
            outcome: WorkflowChildExecutionOutcomeV1::Succeeded {
                output_digest: digest('f'),
            },
        }],
    });

    let mut resume = first;
    resume.fence = fence(2);
    resume.fence.attempt_id = id("attempt.workflow.runtime.forged");
    assert_eq!(
        service.execute(resume).unwrap_err(),
        WorkflowFanOutRuntimeError::InvalidPlan
    );
    assert_eq!(children.calls.lock().unwrap().len(), 2);
}

#[test]
fn child_results_accepted_only_for_pending_task_ids_exactly_once() {
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

    let completed_task = children.calls.lock().unwrap()[0].clone();
    children.outcomes.lock().unwrap().remove("b");
    *children.spoof_extra.lock().unwrap() = Some(completed_task);
    authority.state.lock().unwrap().recovery = WorkflowRecoveryDirectiveV1::ResumeIncomplete;
    let mut resume = request(vec![input("a", '1'), input("b", '2')], 2);
    resume.fence.attempt_id = id("attempt.workflow.runtime.spoof");
    assert_eq!(
        service.execute(resume).unwrap_err(),
        WorkflowFanOutRuntimeError::InvalidChildResults
    );
}

#[test]
fn synthesis_command_identity_is_stable_across_uncertain_complete_retry() {
    let authority = FakeAuthority::default();
    authority.state.lock().unwrap().fail_complete_once = true;
    let synthesis = FakeSynthesis::default();
    let service = WorkflowFanOutRuntimeService::new(
        authority.clone(),
        FakeChildren::default(),
        synthesis.clone(),
    );
    let request = request(vec![input("only", '1')], 1);

    assert!(matches!(
        service.execute(request.clone()).unwrap_err(),
        WorkflowFanOutRuntimeError::AuthorityUnavailable(_)
    ));
    let first_id = synthesis.command_ids.lock().unwrap()[0].clone();

    let mut retry = request;
    retry.fence = fence(2);
    retry.fence.attempt_id = id("attempt.workflow.runtime.synthesis-retry");
    let completed = service.execute(retry).unwrap();
    assert!(matches!(
        completed,
        WorkflowExecutionTruthV1::Synthesized(WorkflowSynthesisTruthV1::Complete { .. })
    ));
    let ids = synthesis.command_ids.lock().unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], first_id);
    assert_eq!(ids[1], first_id);
}

#[test]
fn max_parallel_bound_propagates_with_deterministic_child_identities() {
    let children = FakeChildren::default();
    let service = WorkflowFanOutRuntimeService::new(
        FakeAuthority::default(),
        children.clone(),
        FakeSynthesis::default(),
    );
    let mut req = request(vec![input("b", '2'), input("a", '1')], 1);
    req.definition = definition(2);
    service.execute(req.clone()).unwrap();
    assert_eq!(children.batches.lock().unwrap()[0].0, 2);
    assert_eq!(children.batches.lock().unwrap()[0].1, 2);
    assert_eq!(
        children.batches.lock().unwrap()[0].2,
        active_cancellation()
    );

    let first_ids = children.calls.lock().unwrap().clone();
    let children = FakeChildren::default();
    let service = WorkflowFanOutRuntimeService::new(
        FakeAuthority::default(),
        children.clone(),
        FakeSynthesis::default(),
    );
    service.execute(req).unwrap();
    assert_eq!(&*children.calls.lock().unwrap(), &first_ids);
}

#[test]
fn multi_step_journey_reports_missing_canonical_work_adapter_hook() {
    // A pure multi-step application journey cannot be composed here without a
    // second fake authority: production must wire these ports through the
    // canonical Work runtime owner.
    assert_eq!(
        tracedecay_application::workflow_runtime::MISSING_CANONICAL_WORK_ADAPTER_HOOK,
        "canonical Work runtime must implement WorkflowExecutionAuthorityPort, WorkflowChildExecutionPort, and WorkflowSynthesisPort for WorkflowFanOutRuntimeService"
    );
}
