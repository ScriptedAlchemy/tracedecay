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
    ResumeWorkRunCommand, WorkRunAdmissionV1, WorkRunControlReadingV1, WorkRunControlRequestV1,
    WorkRunControlService, WorkRunControlStorageError, WorkRunControlStoragePort,
};
use tracedecay_domain::{
    ActorId, AttemptId, ManifestDigest, ProjectId, RepositoryId, RunId, TaskId, UtcMicros,
    WorkAuthority, WorkRunControlAuthorityV1, WorkRunControlReasonV1, WorkRunControlStateV1,
    WorkRunControlV1, WorktreeId,
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
    controls: Arc<Mutex<BTreeMap<RunKey, WorkRunControlV1>>>,
}

impl TestStore {
    fn admit(&self, authority: &WorkAuthority, live_attempts: Vec<AttemptId>) {
        self.admissions.lock().unwrap().insert(
            (authority.clone(), task(), run()),
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

    fn publish_run_control(
        &self,
        authority: &WorkAuthority,
        expected: Option<WorkRunControlAuthorityV1>,
        next: &WorkRunControlV1,
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
        Ok(())
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
