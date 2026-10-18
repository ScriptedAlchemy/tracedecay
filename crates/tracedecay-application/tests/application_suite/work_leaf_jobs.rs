use std::sync::{Arc, Mutex};
use tracedecay_application::work::RegisteredWorkApplicationServicesV1;

use tracedecay_contracts::{
    AdjudicateWorkLeakCommandV1, AdmitWorkPlacementCommand, ApplicationProblem,
    ReleaseWorkPlacementCommand, VerifiedWorkLeakEvidenceV1, WorkAttemptStoragePort,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkDuplicateAdjudicationPortV1,
    WorkDuplicateAdjudicationStorageErrorV1, WorkLeakAdjudicationOutcomeV1,
    WorkLeakAdjudicationStorageErrorV1, WorkLeakEvidenceErrorV1, WorkLeakEvidencePortV1,
    WorkPlacementStorageError, WorkPlacementStoragePort,
};
use tracedecay_domain::test_fixtures::{fixture_abs_root, id};
use tracedecay_domain::{
    CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1, LeakOwnerClassV1, ProjectId,
    ProjectionGenerationId, QuantityEvidenceClassV1, RunId, UtcMicros, WorkAttemptIdentityV1,
    WorkAuthority, WorkCommandId, WorkDuplicateAdjudicationCommandV1,
    WorkDuplicateAdjudicationEvidenceV1, WorkDuplicateAdjudicationQuantitiesV1,
    WorkExecutionLeakKindV1, WorkExecutionLeakRecoveryV1, WorkPlacementIdentityV1,
    WorkPlacementKindV1, WorkPlacementObservationV1, WorkPlacementStateV1, WorkPlacementTargetV1,
    WorkTopologyGenerationRefV1,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_rusqlite_runtime::exact_sql::native_thread_reader_waits;
use tracedecay_rusqlite_runtime::native_runtime::{
    NativeTask, NativeTaskOutcome, native_thread_writer_waits,
};

use crate::work_run_control_jobs::{
    authority, context, context_for_actor, leased_attempt_of, task,
};

const PROJECT: &str = "project.run-control-jobs";

async fn project_runtime(
    profile: &std::path::Path,
    project: &std::path::Path,
) -> RegisteredGlobalDbTestRuntime {
    tracedecay_global_db::register_registered_schema_installer();
    RegisteredGlobalDbTestRuntime::project(profile, project, id::<ProjectId>(PROJECT))
        .await
        .expect("registered project store opens")
}

/// Runs `job` on a native runtime worker and returns its output with the
/// writer and reader waits it parked that worker on.
pub(crate) async fn on_native_worker<T: Clone + Send + Sync + 'static>(
    job: impl Future<Output = T> + Send + 'static,
) -> (T, u64, u64) {
    let writer_waits = native_thread_writer_waits();
    let reader_waits = native_thread_reader_waits();
    let native = NativeTask::spawn(job).expect("native task spawns");
    let NativeTaskOutcome::Completed(output) = native.wait().await else {
        panic!("the native task completes");
    };
    (
        output,
        native_thread_writer_waits() - writer_waits,
        native_thread_reader_waits() - reader_waits,
    )
}

fn evidence() -> WorkDuplicateAdjudicationEvidenceV1 {
    WorkDuplicateAdjudicationEvidenceV1 {
        work_generation: id::<ProjectionGenerationId>("generation.work.leaf-jobs"),
        topology_generation: WorkTopologyGenerationRefV1::new(format!("sha256:{}", "1".repeat(64)))
            .unwrap(),
    }
}

fn duplicate_command(
    command_id: &str,
    pair: (&WorkAttemptIdentityV1, &WorkAttemptIdentityV1),
) -> WorkDuplicateAdjudicationCommandV1 {
    WorkDuplicateAdjudicationCommandV1 {
        expected_revision: None,
        first_attempt: pair.0.clone(),
        second_attempt: pair.1.clone(),
        evidence: evidence(),
        verdict: DuplicateEffortKindV1::ExactDuplicate,
        quantities: WorkDuplicateAdjudicationQuantitiesV1 {
            wall_micros: Some(10),
            token_count: None,
            cost_micros: None,
            test_count: None,
            effect_count: None,
            evidence: QuantityEvidenceClassV1::OwnerReceipt,
            effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
            coverage: CoverageStateV1::Known,
        },
        reason: "independent review".to_owned(),
        command_id: id::<WorkCommandId>(command_id),
        occurred_at: UtcMicros(50_000),
    }
    .canonicalized()
}

#[tokio::test]
async fn duplicate_adjudication_awaited_on_a_native_worker_parks_no_native_thread() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let storage = database.work_storage().expect("Work storage");
    let first = leased_attempt_of(id::<RunId>("run.leaf-jobs.1"), "leaf-jobs.1");
    let second = leased_attempt_of(id::<RunId>("run.leaf-jobs.2"), "leaf-jobs.2");
    for attempt in [&first, &second] {
        storage
            .insert(&authority(), attempt)
            .expect("the attempt is stored");
    }
    let pair = (first.identity().clone(), second.identity().clone());
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let (outcome, writer_waits, reader_waits) = on_native_worker(async move {
        let appended = work
            .adjudicate_duplicate(
                context(),
                duplicate_command("command.leaf-jobs.first", (&pair.0, &pair.1)),
                evidence(),
            )
            .await;
        let conflicting = work
            .adjudicate_duplicate(
                context(),
                duplicate_command("command.leaf-jobs.second", (&pair.0, &pair.1)),
                evidence(),
            )
            .await;
        (appended, conflicting)
    })
    .await;
    assert_eq!(
        writer_waits, 0,
        "the adjudication parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "the adjudication parked no native worker on a reader"
    );
    let (appended, conflicting) = outcome;
    let WorkDuplicateAdjudicationAppendOutcomeV1::Appended(receipt) =
        appended.expect("the first adjudication commits")
    else {
        panic!("the first adjudication appends");
    };
    assert_eq!(receipt.revision().get(), 1);
    assert_eq!(
        conflicting,
        Err(WorkDuplicateAdjudicationStorageErrorV1::RevisionConflict.into()),
        "a second command for the same pair at no revision is refused by the revision compare"
    );
    let (first_attempt, second_attempt) = (
        &receipt.command().first_attempt,
        &receipt.command().second_attempt,
    );
    assert_eq!(
        storage
            .latest_duplicate_adjudication_for_pair(&authority(), first_attempt, second_attempt)
            .expect("the pair reads"),
        Some(receipt),
        "the job committed exactly the first receipt"
    );
}

/// A completed scan that found the attempt running without a live owner.
#[derive(Clone)]
struct OwnerlessAttemptScan;

impl WorkLeakEvidencePortV1 for OwnerlessAttemptScan {
    fn inspect(
        &self,
        _authority: &WorkAuthority,
        command: &AdjudicateWorkLeakCommandV1,
        scan_started_at: UtcMicros,
        _scan_deadline: UtcMicros,
    ) -> Result<VerifiedWorkLeakEvidenceV1, WorkLeakEvidenceErrorV1> {
        Ok(VerifiedWorkLeakEvidenceV1 {
            attempt: command.attempt.clone(),
            kind: WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
            recovery: WorkExecutionLeakRecoveryV1::Pending,
            owner_class: LeakOwnerClassV1::Work,
            coverage: CoverageStateV1::Known,
            detection_horizon_micros: command.detection_horizon_micros,
            scan_started_at,
            scan_completed_at: UtcMicros(scan_started_at.0 + 1),
            evidence_refs: vec!["work-leak:attempt-without-live-owner:fixture".to_owned()],
        })
    }
}

#[tokio::test]
async fn leak_adjudication_awaited_on_a_native_worker_parks_no_native_thread() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let attempt = leased_attempt_of(id::<RunId>("run.leaf-jobs.leak"), "leaf-jobs.leak");
    database
        .work_storage()
        .expect("Work storage")
        .insert(&authority(), &attempt)
        .expect("the attempt is stored");
    let command = |command_id: &str| AdjudicateWorkLeakCommandV1 {
        adjudication_id: "adjudication.leaf-jobs.leak".to_owned(),
        expected_revision: None,
        attempt: attempt.identity().clone(),
        detection_horizon_micros: 1_000,
        command_id: id::<WorkCommandId>(command_id),
    };
    let (first, second) = (
        command("command.leaf-jobs.leak"),
        command("command.leaf-jobs.leak.2"),
    );
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let (outcomes, writer_waits, reader_waits) = on_native_worker(async move {
        let adjudicate = |command: AdjudicateWorkLeakCommandV1| {
            work.adjudicate_leak(
                context(),
                command,
                OwnerlessAttemptScan,
                UtcMicros(50_000),
                UtcMicros(60_000),
            )
        };
        (
            adjudicate(first.clone()).await,
            adjudicate(first).await,
            adjudicate(second).await,
        )
    })
    .await;
    assert_eq!(
        writer_waits, 0,
        "the adjudication parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "the adjudication parked no native worker on a reader"
    );
    let (appended, replayed, conflicting) = outcomes;
    let WorkLeakAdjudicationOutcomeV1::Appended(receipt) =
        appended.expect("the first adjudication commits")
    else {
        panic!("the first adjudication appends");
    };
    assert_eq!(receipt.revision, 1);
    assert_eq!(
        replayed,
        Ok(WorkLeakAdjudicationOutcomeV1::Replayed(receipt)),
        "the same command replays the committed receipt"
    );
    assert_eq!(
        conflicting,
        Err(WorkLeakAdjudicationStorageErrorV1::RevisionConflict.into()),
        "a second command at no revision is refused by the revision compare"
    );
}

fn placement_target() -> WorkPlacementTargetV1 {
    WorkPlacementTargetV1::new(
        WorkPlacementKindV1::LinkedWorktree,
        Some(fixture_abs_root("/workspace/leaf-jobs-placement")),
        false,
        true,
    )
    .unwrap()
}

/// A Git observer stand-in that reports a clean target and records the name
/// of every thread it ran on.
#[derive(Clone, Default)]
struct RecordingObserver {
    threads: Arc<Mutex<Vec<Option<String>>>>,
}

impl RecordingObserver {
    fn observe(
        &self,
        _target: &WorkPlacementTargetV1,
    ) -> Result<WorkPlacementObservationV1, ApplicationProblem> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().name().map(str::to_owned));
        Ok(WorkPlacementObservationV1 {
            dirty_tracked_paths: 0,
            untracked_paths: 0,
            unique_commits: Some(0),
            readable: true,
            active_holder: false,
            network_required: false,
            observed_at: UtcMicros(50_000),
        })
    }

    fn observer(
        &self,
    ) -> impl Fn(&WorkPlacementTargetV1) -> Result<WorkPlacementObservationV1, ApplicationProblem>
    + Send
    + Sync
    + 'static {
        let recorder = self.clone();
        move |target| recorder.observe(target)
    }

    fn threads(&self) -> Vec<Option<String>> {
        self.threads.lock().unwrap().clone()
    }
}

fn admit_command(run: &str) -> AdmitWorkPlacementCommand {
    AdmitWorkPlacementCommand {
        task_id: task(),
        run_id: id::<RunId>(run),
        target: placement_target(),
        retention_eligible_at: None,
        occurred_at: UtcMicros(50_000),
    }
}

fn release_command(run: &str, expected_authority_version: u64) -> ReleaseWorkPlacementCommand {
    ReleaseWorkPlacementCommand {
        task_id: task(),
        run_id: id::<RunId>(run),
        expected_authority_version,
        occurred_at: UtcMicros(60_000),
    }
}

#[tokio::test]
async fn placement_admit_and_release_awaited_on_a_native_worker_observe_off_the_writer() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let observer = RecordingObserver::default();
    let (admit_observer, release_observer) = (observer.observer(), observer.observer());
    let (outcome, writer_waits, reader_waits) = on_native_worker(async move {
        let admitted = work
            .admit_placement(
                context(),
                admit_command("run.leaf-jobs.placement"),
                admit_observer,
            )
            .await?;
        let released = work
            .release_placement(
                context(),
                release_command("run.leaf-jobs.placement", admitted.authority_version()),
                release_observer,
            )
            .await?;
        Ok::<_, ApplicationProblem>((admitted.state(), released.state()))
    })
    .await;
    assert_eq!(
        writer_waits, 0,
        "the placement parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "the placement parked no native worker on a reader"
    );
    assert_eq!(
        outcome,
        Ok((
            WorkPlacementStateV1::Admitted,
            WorkPlacementStateV1::Released
        ))
    );
    let threads = observer.threads();
    assert_eq!(
        threads.len(),
        2,
        "admit and release each observed the target once"
    );
    assert!(
        threads.iter().all(|name| name
            .as_deref()
            .is_some_and(|name| name.contains("-blocking-"))),
        "the Git observation ran on the blocking pool, never the writer or the native worker: \
         {threads:?}"
    );
    assert_eq!(
        database
            .work_storage()
            .expect("Work storage")
            .load_placement(
                &authority(),
                &WorkPlacementIdentityV1::new(task(), id::<RunId>("run.leaf-jobs.placement")),
            )
            .expect("the placement reads")
            .map(|placement| placement.state()),
        Some(WorkPlacementStateV1::Released),
        "the release committed"
    );
}

#[tokio::test]
async fn placement_admission_refuses_a_root_another_actor_holds_before_any_effect() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let observer = RecordingObserver::default();
    let peer = || context_for_actor("pause_run", "actor.leaf-jobs.peer");

    let holder = work
        .admit_placement(
            context(),
            admit_command("run.leaf-jobs.exclusive"),
            observer.observer(),
        )
        .await
        .expect("the first actor admits the root");
    let refused = work
        .admit_placement(
            peer(),
            admit_command("run.leaf-jobs.exclusive"),
            observer.observer(),
        )
        .await
        .expect_err("a second actor cannot admit the held root");
    let refusal = format!("{refused:?}");
    assert!(refusal.contains("active_holder"), "{refusal}");
    assert!(
        !refusal.contains("actor.run-control-jobs"),
        "the refusal must not disclose the holder: {refusal}"
    );
    let storage = database.work_storage().expect("Work storage");
    let peer_authority = tracedecay_domain::WorkAuthority::new(
        authority().project_id().clone(),
        authority().repository_id().clone(),
        authority().worktree_id().clone(),
        id("actor.leaf-jobs.peer"),
        authority().policy_digest().clone(),
    )
    .unwrap();
    assert_eq!(
        storage
            .load_placement(
                &peer_authority,
                &WorkPlacementIdentityV1::new(task(), id::<RunId>("run.leaf-jobs.exclusive")),
            )
            .expect("the peer's placement reads"),
        None,
        "the refused actor holds no placement"
    );

    work.release_placement(
        context(),
        release_command("run.leaf-jobs.exclusive", holder.authority_version()),
        observer.observer(),
    )
    .await
    .expect("the holder releases");
    work.admit_placement(
        peer(),
        admit_command("run.leaf-jobs.exclusive"),
        observer.observer(),
    )
    .await
    .expect("the root is free after release");
}

#[tokio::test]
async fn placement_release_refuses_a_placement_that_changed_after_its_observation() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let interloper = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let clean = RecordingObserver::default();
    let observer = clean.observer();
    let outcome = NativeTask::spawn(async move {
        let admitted = work
            .admit_placement(context(), admit_command("run.leaf-jobs.raced"), observer)
            .await
            .map_err(|problem| format!("admit: {problem:?}"))?;
        let version = admitted.authority_version();
        work.release_placement(
            context(),
            release_command("run.leaf-jobs.raced", version),
            move |target| {
                // Another release of the same placement commits while this
                // one is observing its target.
                let target = target.clone();
                std::thread::spawn(move || {
                    let observer = RecordingObserver::default();
                    tracedecay_rusqlite_runtime::native_runtime::block_on(
                        interloper.release_placement(
                            context(),
                            release_command("run.leaf-jobs.raced", version),
                            move |_| observer.observe(&target),
                        ),
                    )
                })
                .join()
                .expect("the interloper joins")
                .expect("native runtime")
                .expect("the interloping release commits");
                clean.observe(&placement_target())
            },
        )
        .await
        .map_err(|problem| format!("release: {problem:?}"))
    })
    .expect("native task spawns");
    let NativeTaskOutcome::Completed(outcome) = outcome.wait().await else {
        panic!("the native task completes");
    };
    assert_eq!(
        outcome,
        Err(format!(
            "release: {:?}",
            ApplicationProblem::from(WorkPlacementStorageError::AuthorityConflict)
        )),
        "the job refuses with the authority conflict instead of publishing over the newer release"
    );
}
