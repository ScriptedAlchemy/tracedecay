//! Run-control authority contract: version-checked pause/resume, the
//! reservation fence, a preserved deadline balance, and typed absence.
//!
//! Plan 32 (`docs/plans/tracedecay-v2/32-dynamic-workflow-runtime-and-sdk.md`,
//! "One runtime, run control, and effect budget") requires that "pause and
//! cancellation fence new reservations and reconcile active effects before
//! publishing a stable state", and that "remaining time never increases after
//! pause, human wait, retry, reconnect, failover, clock rollback, or daemon
//! restart". "Application operations and surfaces" lists pause/resume as
//! retained callable operations.
//!
//! The fake storage below is deliberately dumb: it holds rows and enforces the
//! compare-and-swap, so every decision the assertions grade belongs to the
//! service rather than to a clever fixture.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    ApplicationProblemKind, CancellationContext, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, PauseWorkRunCommand, RequestContext, RequestId, ResolvedScope,
    ResumeWorkRunCommand, WorkRunAdmissionV1, WorkRunControlFrontierV1, WorkRunControlReadingV1,
    WorkRunControlRequestV1, WorkRunControlService, WorkRunControlStorageError,
    WorkRunControlStoragePort, WorkRunLiveAttemptV1,
};
use tracedecay_domain::{
    ActorId, AttemptId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, UtcMicros,
    WorkAuthority, WorkBlockedIntervalReceiptV1, WorkRunControlAuthorityV1, WorkRunControlReasonV1,
    WorkRunControlStateV1, WorkRunControlV1, WorkflowStepId, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

const ADMITTED_DEADLINE: UtcMicros = UtcMicros(10_000);

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

fn task() -> TaskId {
    id::<TaskId>("task.run-control")
}

fn run() -> RunId {
    id::<RunId>("run.run-control")
}

fn context(actor: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.run-control"),
        id::<RepositoryId>("repository.run-control"),
        id::<WorktreeId>("worktree.run-control"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.work.pause_run").unwrap();
    let use_case = UseCaseId::new("use-case.work.pause_run").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.work.run-control"),
        1,
        digest('a'),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(100_000),
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
        RequestId::new(format!("request.run-control.{actor}")).unwrap(),
        Deadline::new(UtcMicros(90_000)).unwrap(),
        CancellationContext::active(format!("cancel.run-control.{actor}")).unwrap(),
    )
    .unwrap()
}

type RunKey = (WorkAuthority, TaskId, RunId);

#[derive(Clone, Default)]
struct TestStore {
    admissions: Arc<Mutex<BTreeMap<RunKey, WorkRunAdmissionV1>>>,
    workflow_bindings: Arc<Mutex<BTreeMap<RunKey, bool>>>,
    controls: Arc<Mutex<BTreeMap<RunKey, WorkRunControlV1>>>,
    intervals: Arc<Mutex<BTreeMap<RunKey, Vec<WorkBlockedIntervalReceiptV1>>>>,
    settle_frontier_during_binding_read: Arc<Mutex<bool>>,
}

impl TestStore {
    fn admit(&self, authority: &WorkAuthority, live_attempts: Vec<AttemptId>) {
        self.admit_with_workflow_binding(authority, live_attempts, true);
    }

    fn admit_ordinary(&self, authority: &WorkAuthority, live_attempts: Vec<AttemptId>) {
        self.admit_with_workflow_binding(authority, live_attempts, false);
    }

    fn admit_with_workflow_binding(
        &self,
        authority: &WorkAuthority,
        live_attempts: Vec<AttemptId>,
        workflow_bound: bool,
    ) {
        let key = (authority.clone(), task(), run());
        self.workflow_bindings
            .lock()
            .unwrap()
            .insert(key.clone(), workflow_bound);
        self.admissions.lock().unwrap().insert(
            key,
            WorkRunAdmissionV1 {
                deadline: ADMITTED_DEADLINE,
                total_attempts: u32::try_from(live_attempts.len()).unwrap(),
                live_attempts,
            },
        );
    }

    fn stored(&self, authority: &WorkAuthority) -> Option<WorkRunControlV1> {
        self.controls
            .lock()
            .unwrap()
            .get(&(authority.clone(), task(), run()))
            .cloned()
    }
}

impl WorkRunControlStoragePort for TestStore {
    fn run_control_frontier(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunControlFrontierV1>, WorkRunControlStorageError> {
        let key = (authority.clone(), task_id.clone(), run_id.clone());
        let admissions = self.admissions.lock().unwrap();
        let controls = self.controls.lock().unwrap();
        let intervals = self.intervals.lock().unwrap();
        Ok(admissions
            .get(&key)
            .cloned()
            .map(|admission| WorkRunControlFrontierV1 {
                admission,
                control: controls.get(&key).cloned(),
                open_blocked_intervals: intervals
                    .get(&key)
                    .into_iter()
                    .flatten()
                    .filter(|receipt| !receipt.is_settled())
                    .cloned()
                    .collect(),
            }))
    }

    fn run_admission(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunAdmissionV1>, WorkRunControlStorageError> {
        Ok(self
            .admissions
            .lock()
            .unwrap()
            .get(&(authority.clone(), task_id.clone(), run_id.clone()))
            .cloned())
    }

    fn load_run_control(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Option<WorkRunControlV1>, WorkRunControlStorageError> {
        Ok(self
            .controls
            .lock()
            .unwrap()
            .get(&(authority.clone(), task_id.clone(), run_id.clone()))
            .cloned())
    }

    fn workflow_bound_live_attempts(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Vec<WorkRunLiveAttemptV1>, WorkRunControlStorageError> {
        let key = (authority.clone(), task_id.clone(), run_id.clone());
        let workflow_bound = self
            .workflow_bindings
            .lock()
            .unwrap()
            .get(&key)
            .copied()
            .unwrap_or(false);
        let attempts = self
            .admissions
            .lock()
            .unwrap()
            .get(&key)
            .map(|admission| {
                admission
                    .live_attempts
                    .iter()
                    .cloned()
                    .map(|attempt_id| WorkRunLiveAttemptV1 {
                        attempt_id,
                        step_id: workflow_bound.then(|| id::<WorkflowStepId>("step.run-control")),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if std::mem::take(&mut *self.settle_frontier_during_binding_read.lock().unwrap())
            && let Some(admission) = self.admissions.lock().unwrap().get_mut(&key)
        {
            admission.live_attempts.clear();
        }
        Ok(attempts)
    }

    fn publish_run_control(
        &self,
        authority: &WorkAuthority,
        expected: Option<WorkRunControlAuthorityV1>,
        next: &WorkRunControlV1,
        blocked_intervals: &[WorkBlockedIntervalReceiptV1],
    ) -> Result<(), WorkRunControlStorageError> {
        let mut controls = self.controls.lock().unwrap();
        let key = (
            authority.clone(),
            next.task_id().clone(),
            next.run_id().clone(),
        );
        let current = controls.get(&key).map(WorkRunControlV1::authority);
        if current != expected {
            return Err(WorkRunControlStorageError::AuthorityConflict);
        }
        controls.insert(key, next.clone());
        let mut intervals = self.intervals.lock().unwrap();
        for receipt in blocked_intervals {
            let key = (
                authority.clone(),
                receipt.identity().task_id().clone(),
                receipt.identity().run_id().clone(),
            );
            let rows = intervals.entry(key).or_default();
            if receipt.is_settled() {
                let Some(existing) = rows.iter_mut().find(|existing| {
                    existing.identity() == receipt.identity() && !existing.is_settled()
                }) else {
                    return Err(WorkRunControlStorageError::AuthorityConflict);
                };
                *existing = receipt.clone();
            } else {
                rows.push(receipt.clone());
            }
        }
        Ok(())
    }

    fn publish_run_control_at_frontier(
        &self,
        authority: &WorkAuthority,
        expected: &WorkRunControlFrontierV1,
        next: &WorkRunControlV1,
        blocked_intervals: &[WorkBlockedIntervalReceiptV1],
    ) -> Result<(), WorkRunControlStorageError> {
        let current = self
            .run_control_frontier(authority, next.task_id(), next.run_id())?
            .ok_or(WorkRunControlStorageError::AuthorityConflict)?;
        if &current != expected {
            return Err(WorkRunControlStorageError::AuthorityConflict);
        }
        self.publish_run_control(
            authority,
            expected.control.as_ref().map(WorkRunControlV1::authority),
            next,
            blocked_intervals,
        )
    }

    fn open_blocked_intervals(
        &self,
        authority: &WorkAuthority,
        task_id: &TaskId,
        run_id: &RunId,
    ) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError> {
        Ok(self
            .intervals
            .lock()
            .unwrap()
            .get(&(authority.clone(), task_id.clone(), run_id.clone()))
            .into_iter()
            .flatten()
            .filter(|receipt| !receipt.is_settled())
            .cloned()
            .collect())
    }

    fn next_settled_blocked_intervals_for_observation(
        &self,
        authority: &WorkAuthority,
        limit: u32,
    ) -> Result<Vec<WorkBlockedIntervalReceiptV1>, WorkRunControlStorageError> {
        Ok(self
            .intervals
            .lock()
            .unwrap()
            .iter()
            .filter(|((stored, _, _), _)| stored == authority)
            .flat_map(|(_, receipts)| receipts)
            .filter(|receipt| receipt.is_settled())
            .take(usize::try_from(limit).map_err(|_| WorkRunControlStorageError::Unavailable)?)
            .cloned()
            .collect())
    }

    fn mark_settled_blocked_interval_durable(
        &self,
        _authority: &WorkAuthority,
        receipt: &WorkBlockedIntervalReceiptV1,
    ) -> Result<(), WorkRunControlStorageError> {
        receipt
            .is_settled()
            .then_some(())
            .ok_or(WorkRunControlStorageError::AuthorityConflict)
    }
}

fn service(store: TestStore) -> WorkRunControlService<TestStore> {
    WorkRunControlService::new(store)
}

fn authority_of(context: &RequestContext) -> WorkAuthority {
    WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .unwrap()
}

fn pause_command(expected: Option<u64>, at: i64) -> PauseWorkRunCommand {
    PauseWorkRunCommand {
        task_id: task(),
        run_id: run(),
        reason: WorkRunControlReasonV1::OperatorRequest,
        expected_authority_version: expected,
        occurred_at: UtcMicros(at),
    }
}

#[test]
fn pausing_a_run_nobody_leased_an_attempt_for_is_concealed_absence() {
    let store = TestStore::default();
    let service = service(store.clone());
    let context = context("actor.run-control.absent");

    let problem = service
        .pause(&context, pause_command(None, 100))
        .expect_err("an unadmitted run cannot be paused");
    assert_eq!(
        problem.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
    // Nothing was published for a run the authority does not hold.
    assert!(store.stored(&authority_of(&context)).is_none());

    let read = service
        .read(
            &context,
            &WorkRunControlRequestV1 {
                task_id: task(),
                run_id: run(),
            },
        )
        .expect_err("an unadmitted run has no control reading");
    assert_eq!(read.kind(), ApplicationProblemKind::NotFoundOrNotAuthorized);
}

#[test]
fn an_admitted_but_uncontrolled_run_reads_as_uncontrolled_and_admits_reservations() {
    let store = TestStore::default();
    let context = context("actor.run-control.uncontrolled");
    store.admit(&authority_of(&context), vec![id::<AttemptId>("attempt.1")]);
    let service = service(store);

    let reading = service
        .read(
            &context,
            &WorkRunControlRequestV1 {
                task_id: task(),
                run_id: run(),
            },
        )
        .expect("uncontrolled reading");
    // "Never controlled" is a distinct answer from "controlled and running".
    assert!(matches!(
        reading,
        WorkRunControlReadingV1::Uncontrolled { deadline, .. } if deadline == ADMITTED_DEADLINE
    ));
    assert!(reading.admits_reservation());
    service
        .admit_reservation(&context, &task(), &run())
        .expect("an uncontrolled run admits reservations");
}

#[test]
fn pausing_fences_new_reservations_and_records_the_live_frontier() {
    let store = TestStore::default();
    let context = context("actor.run-control.pause");
    store.admit(
        &authority_of(&context),
        vec![id::<AttemptId>("attempt.1"), id::<AttemptId>("attempt.2")],
    );
    let service = service(store.clone());

    let paused = service
        .pause(&context, pause_command(None, 4_000))
        .expect("pause");
    assert_eq!(paused.state(), WorkRunControlStateV1::Paused);
    assert_eq!(paused.fenced_attempts().len(), 2);
    assert_eq!(paused.deadline().remaining_micros, 6_000);

    let fenced = service
        .admit_reservation(&context, &task(), &run())
        .expect_err("a paused run fences new reservations");
    assert_eq!(fenced.kind(), ApplicationProblemKind::Conflict);

    let reading = service
        .read(
            &context,
            &WorkRunControlRequestV1 {
                task_id: task(),
                run_id: run(),
            },
        )
        .expect("controlled reading");
    assert!(!reading.admits_reservation());
    assert_eq!(store.stored(&authority_of(&context)), Some(paused));
}

#[test]
fn pause_refuses_a_frontier_that_settled_after_its_snapshot() {
    let store = TestStore::default();
    let context = context("actor.run-control.frontier-race");
    store.admit(
        &authority_of(&context),
        vec![id::<AttemptId>("attempt.frontier-race")],
    );
    *store.settle_frontier_during_binding_read.lock().unwrap() = true;
    let service = service(store.clone());

    let problem = service
        .pause(&context, pause_command(None, 4_000))
        .expect_err("a settled attempt invalidates the prepared pause frontier");

    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
    assert!(store.stored(&authority_of(&context)).is_none());
    assert!(store.intervals.lock().unwrap().values().all(Vec::is_empty));
}

#[test]
fn workflow_bound_pause_and_resume_commit_one_revisioned_interval() {
    let store = TestStore::default();
    let context = context("actor.run-control.interval");
    store.admit(
        &authority_of(&context),
        vec![id::<AttemptId>("attempt.interval")],
    );
    let service = service(store.clone());

    let paused = service
        .pause_with_receipt(&context, pause_command(None, 4_000))
        .expect("workflow pause");
    assert_eq!(paused.blocked_intervals.len(), 1);
    let opened = &paused.blocked_intervals[0];
    assert!(!opened.is_settled());
    assert_eq!(opened.interval_revision(), 1);
    assert_eq!(opened.started_at(), UtcMicros(4_000));

    let resumed = service
        .resume_with_receipt(
            &context,
            ResumeWorkRunCommand {
                task_id: task(),
                run_id: run(),
                reason: WorkRunControlReasonV1::HumanWait,
                expected_authority_version: paused.control.authority().get(),
                occurred_at: UtcMicros(7_000),
            },
        )
        .expect("workflow resume");
    assert_eq!(resumed.blocked_intervals.len(), 1);
    let settled = &resumed.blocked_intervals[0];
    assert!(settled.is_settled());
    assert_eq!(settled.interval_revision(), 2);
    assert_eq!(settled.identity(), opened.identity());
    assert_eq!(settled.cause(), opened.cause());
    assert_eq!(settled.ended_at(), Some(UtcMicros(7_000)));

    let recovery_page = service
        .next_settled_blocked_intervals_for_observation(&context, 8)
        .expect("settled receipt recovery page");
    assert_eq!(recovery_page, vec![settled.clone()]);
}

#[test]
fn ordinary_pause_and_resume_stay_controllable_without_workflow_metric_receipts() {
    let store = TestStore::default();
    let context = context("actor.run-control.ordinary");
    store.admit_ordinary(
        &authority_of(&context),
        vec![id::<AttemptId>("attempt.ordinary")],
    );
    let service = service(store);

    let paused = service
        .pause_with_receipt(&context, pause_command(None, 4_000))
        .expect("ordinary pause");
    assert_eq!(paused.control.state(), WorkRunControlStateV1::Paused);
    assert!(paused.blocked_intervals.is_empty());

    let resumed = service
        .resume_with_receipt(
            &context,
            ResumeWorkRunCommand {
                task_id: task(),
                run_id: run(),
                reason: WorkRunControlReasonV1::OperatorRequest,
                expected_authority_version: paused.control.authority().get(),
                occurred_at: UtcMicros(7_000),
            },
        )
        .expect("ordinary resume");
    assert_eq!(resumed.control.state(), WorkRunControlStateV1::Running);
    assert!(resumed.blocked_intervals.is_empty());
}

#[test]
fn resume_restores_the_exact_remaining_balance_and_readmits_reservations() {
    let store = TestStore::default();
    let context = context("actor.run-control.resume");
    store.admit(&authority_of(&context), Vec::new());
    let service = service(store);

    let paused = service
        .pause(&context, pause_command(None, 4_000))
        .expect("pause");
    let resumed = service
        .resume(
            &context,
            ResumeWorkRunCommand {
                task_id: task(),
                run_id: run(),
                reason: WorkRunControlReasonV1::OperatorRequest,
                expected_authority_version: paused.authority().get(),
                // A long human wait: far past the original deadline.
                occurred_at: UtcMicros(50_000),
            },
        )
        .expect("resume");
    assert_eq!(resumed.state(), WorkRunControlStateV1::Running);
    // The wait neither spent nor bought budget.
    assert_eq!(resumed.deadline().remaining_micros, 6_000);
    assert_eq!(resumed.deadline().deadline, UtcMicros(56_000));
    assert_eq!(resumed.authority().get(), paused.authority().get() + 1);
    service
        .admit_reservation(&context, &task(), &run())
        .expect("a resumed run readmits reservations");
}

#[test]
fn a_stale_authority_version_conflicts_instead_of_overwriting() {
    let store = TestStore::default();
    let context = context("actor.run-control.stale");
    store.admit(&authority_of(&context), Vec::new());
    let service = service(store.clone());

    let paused = service
        .pause(&context, pause_command(None, 4_000))
        .expect("pause");
    // A caller that still believes nothing is published is refused.
    let problem = service
        .pause(&context, pause_command(None, 5_000))
        .expect_err("stale pause");
    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
    // So is a resume naming a version that is not current.
    let problem = service
        .resume(
            &context,
            ResumeWorkRunCommand {
                task_id: task(),
                run_id: run(),
                reason: WorkRunControlReasonV1::OperatorRequest,
                expected_authority_version: paused.authority().get() + 7,
                occurred_at: UtcMicros(5_000),
            },
        )
        .expect_err("stale resume");
    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
    // Neither refusal moved the published state.
    assert_eq!(store.stored(&authority_of(&context)), Some(paused));
}

#[test]
fn resuming_a_run_that_was_never_paused_is_refused_rather_than_receipted() {
    let store = TestStore::default();
    let context = context("actor.run-control.never-paused");
    store.admit(&authority_of(&context), Vec::new());
    let service = service(store);

    let problem = service
        .resume(
            &context,
            ResumeWorkRunCommand {
                task_id: task(),
                run_id: run(),
                reason: WorkRunControlReasonV1::OperatorRequest,
                expected_authority_version: 1,
                occurred_at: UtcMicros(1_000),
            },
        )
        .expect_err("resume with no published control");
    assert_eq!(problem.kind(), ApplicationProblemKind::Conflict);
}

#[test]
fn one_actors_pause_does_not_fence_another_actors_run() {
    let store = TestStore::default();
    let mine = context("actor.run-control.mine");
    let peer = context("actor.run-control.peer");
    store.admit(&authority_of(&mine), Vec::new());
    store.admit(&authority_of(&peer), Vec::new());
    let service = service(store);

    service
        .pause(&mine, pause_command(None, 4_000))
        .expect("pause mine");
    // The peer authority is a separate aggregate, not a shared switch.
    service
        .admit_reservation(&peer, &task(), &run())
        .expect("the peer run still admits reservations");
}
