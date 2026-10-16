use tracedecay_application::work::RegisteredWorkApplicationServicesV1;
use tracedecay_contracts::{
    AdjudicateWorkLeakCommandV1, VerifiedWorkLeakEvidenceV1, WorkAttemptStoragePort,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkDuplicateAdjudicationPortV1,
    WorkDuplicateAdjudicationStorageErrorV1, WorkLeakAdjudicationOutcomeV1,
    WorkLeakAdjudicationStorageErrorV1, WorkLeakEvidenceErrorV1, WorkLeakEvidencePortV1,
};
use tracedecay_domain::test_fixtures::id;
use tracedecay_domain::{
    CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1, LeakOwnerClassV1, ProjectId,
    ProjectionGenerationId, QuantityEvidenceClassV1, RunId, UtcMicros, WorkAttemptIdentityV1,
    WorkAuthority, WorkCommandId, WorkDuplicateAdjudicationCommandV1,
    WorkDuplicateAdjudicationEvidenceV1, WorkDuplicateAdjudicationQuantitiesV1,
    WorkExecutionLeakKindV1, WorkExecutionLeakRecoveryV1, WorkTopologyGenerationRefV1,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_rusqlite_runtime::exact_sql::native_thread_reader_waits;
use tracedecay_rusqlite_runtime::native_runtime::{
    NativeTask, NativeTaskOutcome, native_thread_writer_waits,
};

use crate::work_run_control_jobs::{authority, context, leased_attempt_of};

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
async fn on_native_worker<T: Clone + Send + Sync + 'static>(
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
