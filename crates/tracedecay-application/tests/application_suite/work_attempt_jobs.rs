use std::collections::BTreeSet;
use std::time::Duration;

use tracedecay_application::work::{
    RegisteredWorkApplicationServicesV1, RegisteredWorkProductServicesV1,
};
use tracedecay_contracts::{
    AdmitWorkSynthesisCommand, ApplicationProblem, PauseWorkRunCommand,
    PrepareWorkProductMutationRequestV1, RequestContext, ResumeWorkAttemptsCommand,
    RetryWorkAttemptCommandV1, StartWorkAttemptCommand, WorkAttemptEvidenceRecordV1,
    WorkAttemptProviderOutcomeV1, WorkAttemptStoragePort, WorkProductAuthorizedRelationScopeV1,
    WorkProductBindingV1, WorkProductChangeDraftV1, WorkProductMutationReceiptV1,
    WorkProductRevisionPinsV1, WorkProductSelectionScopeV1, WorkRetryAttemptOutcomeV1,
    WorkRetryCauseV1, WorkRetryFailureSelectorV1, WorkRetrySourceV1, WorkSynthesisAttemptV1,
    WorkflowArtifactPayload, WorkflowArtifactStorePort, workflow_artifact_payload_digest,
};
use tracedecay_domain::test_fixtures::{digest, fixture_abs_root, id};
use tracedecay_domain::{
    AttemptId, CatalogGenerationId, CommitId, ConfigurationRevisionId, InitiativeId, MilestoneId,
    PolicyRevisionId, ProjectId, ProposalId, RepositoryId, RunId, UtcMicros, WorkArtifactId,
    WorkArtifactRefV1, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkCommandId, WorkEffectStateV1,
    WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1, WorkItemV1, WorkMilestoneV1, WorkPlanId,
    WorkPlanV1, WorkProposalDispositionV1, WorkProposalV1, WorkRouteDecisionV1,
    WorkRunControlReasonV1, WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1,
    WorkflowOperationRef, WorkflowOutputName,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_rusqlite_runtime::native_runtime::{NativeTask, NativeTaskOutcome};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::work_leaf_jobs::on_native_worker;
use crate::work_run_control_jobs::{
    authority, context, context_for, execution_snapshot, route, task,
};

const PROJECT: &str = "project.run-control-jobs";
const REPOSITORY: &str = "repository.run-control-jobs";

async fn project_runtime(
    profile: &std::path::Path,
    project: &std::path::Path,
) -> RegisteredGlobalDbTestRuntime {
    tracedecay_global_db::register_registered_schema_installer();
    RegisteredGlobalDbTestRuntime::project(profile, project, id::<ProjectId>(PROJECT))
        .await
        .expect("registered project store opens")
}

fn product_context() -> RequestContext {
    context_for("start_attempt")
}

fn binding() -> WorkProductBindingV1 {
    WorkProductBindingV1::new(
        CapabilityId::new("capability.work.start_attempt").unwrap(),
        UseCaseId::new("use-case.work.start_attempt").unwrap(),
    )
}

fn revisions() -> WorkProductRevisionPinsV1 {
    WorkProductRevisionPinsV1 {
        policy_revision_id: id::<PolicyRevisionId>("policy-revision.attempt-jobs"),
        configuration_revision_id: id::<ConfigurationRevisionId>(
            "configuration-revision.attempt-jobs",
        ),
        catalog_generation_id: id::<CatalogGenerationId>("catalog-generation.attempt-jobs"),
    }
}

fn selection() -> WorkProductSelectionScopeV1 {
    WorkProductSelectionScopeV1::relations(BTreeSet::from([
        WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: id::<ProjectId>(PROJECT),
            repository_id: id::<RepositoryId>(REPOSITORY),
        },
    ]))
    .unwrap()
}

/// Prepares one mutation from the current head, commits it as a writer job
/// on a native worker, and adds the native waits the commit parked to
/// `waits`.
async fn mutate(
    database: &RegisteredGlobalDb,
    change: WorkProductChangeDraftV1,
    command_id: &str,
    occurred_at: UtcMicros,
    waits: &mut (u64, u64),
) -> WorkProductMutationReceiptV1 {
    let product =
        RegisteredWorkProductServicesV1::attach(database, binding()).expect("product services");
    let request = product
        .mutations()
        .prepare_mutation(
            &product_context(),
            &binding(),
            PrepareWorkProductMutationRequestV1 {
                selection: selection(),
                change,
                causation_event_id: None,
                evidence: Vec::new(),
            },
            id::<WorkCommandId>(command_id),
            occurred_at,
            revisions(),
        )
        .expect("the mutation prepares");
    let (receipt, writer_waits, reader_waits) = on_native_worker(async move {
        product
            .mutate(product_context(), binding(), request, revisions())
            .await
    })
    .await;
    waits.0 += writer_waits;
    waits.1 += reader_waits;
    receipt.expect("the mutation commits")
}

/// Publishes `task()` with an accepted proposal and admitted execution, the
/// product graph every attempt admission is bound to, and returns the native
/// writer and reader waits its mutation jobs parked.
async fn admit_task(database: &RegisteredGlobalDb) -> (u64, u64) {
    let mut waits = (0, 0);
    let created_at = UtcMicros(10);
    let initiative_id = id::<InitiativeId>("initiative.attempt-jobs");
    let plan_id = id::<WorkPlanId>("plan.attempt-jobs");
    let milestone_id = id::<MilestoneId>("milestone.attempt-jobs");
    let item = WorkItemV1::new(WorkItemInputV1 {
        task_id: task(),
        hierarchy: WorkHierarchyV1::new(
            initiative_id.clone(),
            plan_id.clone(),
            milestone_id.clone(),
        ),
        title: "Run the admitted provider step".to_owned(),
        dependencies: BTreeSet::new(),
        informational_relations: BTreeSet::new(),
        causal_candidates: BTreeSet::new(),
        acceptance_criteria: Vec::new(),
        effort: 1,
        scheduled_at: None,
        deadline: None,
        created_at,
        updated_at: created_at,
    })
    .unwrap();
    let created = mutate(
        database,
        WorkProductChangeDraftV1::CreateTask {
            initiative: WorkInitiativeV1::new(
                initiative_id.clone(),
                "Attempt jobs".to_owned(),
                created_at,
            )
            .unwrap(),
            plan: WorkPlanV1::new(
                plan_id.clone(),
                initiative_id,
                "Attempt jobs plan".to_owned(),
                created_at,
            )
            .unwrap(),
            milestone: WorkMilestoneV1::new(
                milestone_id,
                plan_id,
                "Attempt jobs milestone".to_owned(),
                created_at,
            )
            .unwrap(),
            item: Box::new(item),
        },
        "command.attempt-jobs.create",
        UtcMicros(10_000),
        &mut waits,
    )
    .await;
    let proposal = WorkProposalV1::new(
        id::<ProposalId>("proposal.attempt-jobs"),
        task(),
        created.verified_graph_version().graph_version(),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1).unwrap(),
        WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "attempt jobs").unwrap(),
        Vec::new(),
        WorkRouteDecisionV1::selected(route(), Vec::new(), BTreeSet::new(), "none".to_owned())
            .unwrap(),
        "Run the admitted provider step.".to_owned(),
        digest('4'),
        digest('5'),
    )
    .unwrap();
    mutate(
        database,
        WorkProductChangeDraftV1::DecideProposal {
            proposal,
            disposition: WorkProposalDispositionV1::Accepted,
        },
        "command.attempt-jobs.accept",
        UtcMicros(11_000),
        &mut waits,
    )
    .await;
    mutate(
        database,
        WorkProductChangeDraftV1::AdmitExecution { task_id: task() },
        "command.attempt-jobs.admit",
        UtcMicros(12_000),
        &mut waits,
    )
    .await;
    waits
}

fn start_command(
    run_id: &str,
    attempt_id: &str,
    occurred_at: UtcMicros,
) -> StartWorkAttemptCommand {
    StartWorkAttemptCommand {
        task_id: task(),
        run_id: id::<RunId>(run_id),
        attempt_id: id::<AttemptId>(attempt_id),
        operation: id::<WorkflowOperationRef>("operation.attempt-jobs.execute-provider"),
        execution_snapshot: execution_snapshot(),
        worktree_root: fixture_abs_root("/tmp/attempt-jobs"),
        reference: None,
        commit: id::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        instructions: "Execute the admitted provider step.".to_owned(),
        effect_state: WorkEffectStateV1::Observational,
        occurred_at,
    }
}

async fn start(
    product: &RegisteredWorkProductServicesV1,
    command: StartWorkAttemptCommand,
) -> Result<tracedecay_domain::WorkAttemptV1, ApplicationProblem> {
    product
        .start_attempt(
            product_context(),
            binding(),
            revisions(),
            execution_snapshot().topology().clone(),
            command,
        )
        .await
}

fn identity(run_id: &str, attempt_id: &str) -> WorkAttemptIdentityV1 {
    WorkAttemptIdentityV1::new(task(), id::<RunId>(run_id), id::<AttemptId>(attempt_id)).unwrap()
}

#[tokio::test]
async fn attempt_admissions_awaited_on_a_native_worker_park_no_native_thread() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    admit_task(database).await;
    let product =
        RegisteredWorkProductServicesV1::attach(database, binding()).expect("product services");
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let original = identity("run.attempt-jobs.native", "attempt.attempt-jobs.native");
    let retry_command = RetryWorkAttemptCommandV1 {
        original_attempt: original.clone(),
        new_attempt_id: id::<AttemptId>("attempt.attempt-jobs.native.retry"),
        failure: WorkRetryFailureSelectorV1 {
            source: WorkRetrySourceV1::Runtime,
            cause: WorkRetryCauseV1::RestartRecoveryRequired,
            evidence_ref: "recovery-required".to_owned(),
        },
        command_id: id::<WorkCommandId>("command.attempt-jobs.native.retry"),
    };
    let (outcome, writer_waits, reader_waits) = on_native_worker(async move {
        let started = start(
            &product,
            start_command(
                "run.attempt-jobs.native",
                "attempt.attempt-jobs.native",
                UtcMicros(20_000),
            ),
        )
        .await;
        let replayed = start(
            &product,
            start_command(
                "run.attempt-jobs.native",
                "attempt.attempt-jobs.native",
                UtcMicros(20_000),
            ),
        )
        .await;
        let resumed = work
            .resume_attempts(
                product_context(),
                ResumeWorkAttemptsCommand {
                    occurred_at: UtcMicros(30_000),
                },
            )
            .await;
        let retried = product
            .retry_attempt(
                product_context(),
                binding(),
                revisions(),
                execution_snapshot().topology().clone(),
                retry_command,
                UtcMicros(40_000),
                None,
            )
            .await;
        (started, replayed, resumed, retried)
    })
    .await;
    assert_eq!(
        writer_waits, 0,
        "the attempt admissions parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "the attempt admissions parked no native worker on a reader"
    );
    let (started, replayed, resumed, retried) = outcome;
    let started = started.expect("the start commits");
    assert_eq!(started.state(), WorkAttemptStateV1::Leased);
    assert_eq!(
        replayed.expect("the same start replays"),
        started,
        "the replayed start returns the committed attempt"
    );
    let resumed = resumed.expect("the resume commits");
    assert_eq!(
        resumed
            .recovery_required
            .iter()
            .map(|attempt| attempt.identity().clone())
            .collect::<Vec<_>>(),
        vec![original.clone()],
        "the resume fenced the started attempt"
    );
    let WorkRetryAttemptOutcomeV1::Created { attempt, .. } = retried.expect("the retry commits")
    else {
        panic!("the retry creates a replacement attempt");
    };
    let storage = database.work_storage().expect("Work storage");
    assert_eq!(
        storage
            .load(&authority(), &original)
            .expect("the original reads")
            .state(),
        WorkAttemptStateV1::RecoveryRequired
    );
    assert_eq!(
        storage
            .load(&authority(), attempt.identity())
            .expect("the replacement reads"),
        attempt,
        "the retry job committed its replacement attempt"
    );
}

#[tokio::test]
async fn synthesis_awaited_on_a_native_worker_hydrates_sources_in_its_job() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    admit_task(database).await;
    let product =
        RegisteredWorkProductServicesV1::attach(database, binding()).expect("product services");
    let source = identity("run.attempt-jobs.source", "attempt.attempt-jobs.source");
    let source_product =
        RegisteredWorkProductServicesV1::attach(database, binding()).expect("product services");
    let (started, _, _) = on_native_worker(async move {
        start(
            &source_product,
            start_command(
                "run.attempt-jobs.source",
                "attempt.attempt-jobs.source",
                UtcMicros(20_000),
            ),
        )
        .await
    })
    .await;
    started.expect("the source attempt starts");
    let bytes = b"source answer".to_vec();
    let artifact = WorkArtifactRefV1::new(
        id::<WorkArtifactId>("artifact.provider.stdout"),
        workflow_artifact_payload_digest(&bytes).unwrap(),
        bytes.len() as u64,
    )
    .unwrap();
    let payload = WorkflowArtifactPayload::new(artifact.clone(), bytes).unwrap();
    database
        .workflow_storage()
        .await
        .expect("workflow storage")
        .transact(move |storage| storage.persist(&payload))
        .await
        .expect("the writer runs the job")
        .expect("the source payload persists");
    let attempts = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    attempts
        .attempts()
        .mark_running(&product_context(), &source, route())
        .expect("the source runs");
    attempts
        .attempts()
        .settle_with_artifacts(
            &product_context(),
            &source,
            &WorkAttemptEvidenceRecordV1 {
                identity: source.clone(),
                requested_route: route(),
                actual_route: Some(route()),
                outcome: WorkAttemptProviderOutcomeV1::Exited { code: 0 },
                stdout: None,
                stderr: None,
                provider_session: None,
                provider_fallback: None,
                observed_at: UtcMicros(21_000),
            },
            vec![artifact.clone()],
        )
        .expect("the source succeeds");
    let command = AdmitWorkSynthesisCommand {
        start: start_command(
            "run.attempt-jobs.synthesis",
            "attempt.attempt-jobs.synthesis",
            UtcMicros(30_000),
        ),
        output_name: id::<WorkflowOutputName>("output.attempt-jobs"),
        sources: vec![source],
    };
    let (outcome, writer_waits, reader_waits) = on_native_worker(async move {
        product
            .synthesize(
                product_context(),
                binding(),
                revisions(),
                execution_snapshot().topology().clone(),
                command,
                move |storage, start, sources| {
                    let payload =
                        WorkflowArtifactStorePort::load(storage, &sources[0].artifacts()[0])
                            .map_err(|_| {
                                ApplicationProblem::from(
                                    tracedecay_contracts::WorkAttemptStorageError::Unavailable,
                                )
                            })?;
                    Ok(format!(
                        "{}\n{}",
                        start.instructions,
                        String::from_utf8_lossy(payload.bytes())
                    ))
                },
            )
            .await
    })
    .await;
    assert_eq!(
        writer_waits, 0,
        "synthesis parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "synthesis parked no native worker on a reader"
    );
    let WorkSynthesisAttemptV1::Admitted(admission) = outcome.expect("the synthesis commits")
    else {
        panic!("the cited source admits a synthesis");
    };
    assert_eq!(
        admission.attempt.execution().instructions(),
        "Execute the admitted provider step.\nsource answer",
        "the job hydrated the source payload into the provider instructions"
    );
    assert_eq!(
        database
            .work_storage()
            .expect("Work storage")
            .load(&authority(), admission.attempt.identity())
            .expect("the synthesis attempt reads"),
        admission.attempt,
        "the synthesis job committed its attempt"
    );
}

/// A pause racing a start lands either before the start's job, which then
/// refuses the reservation as paused, or after it. It can no longer land
/// between the reservation fence and the attempt insert.
#[tokio::test]
async fn a_pause_racing_a_start_never_lands_between_its_fence_and_its_reservation() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    admit_task(database).await;
    let run = "run.run-control-jobs";
    let storage = database.work_storage().expect("Work storage");
    let seed = crate::work_run_control_jobs::leased_attempt_of(id::<RunId>(run), "seed");
    storage
        .insert(&authority(), &seed)
        .expect("the run holds a seed attempt");
    let attempts = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    attempts
        .attempts()
        .mark_running(&product_context(), seed.identity(), route())
        .expect("the seed runs");
    attempts
        .attempts()
        .settle(
            &product_context(),
            seed.identity(),
            &WorkAttemptEvidenceRecordV1 {
                identity: seed.identity().clone(),
                requested_route: route(),
                actual_route: Some(route()),
                outcome: WorkAttemptProviderOutcomeV1::Exited { code: 1 },
                stdout: None,
                stderr: None,
                provider_session: None,
                provider_fallback: None,
                observed_at: UtcMicros(15_000),
            },
        )
        .expect("the seed settles, so the run frees its capacity");
    let product =
        RegisteredWorkProductServicesV1::attach(database, binding()).expect("product services");
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    // Hold the writer so the start's job and the pause's job queue behind it
    // in the order they were submitted.
    let blocker = NativeTask::spawn(async move {
        storage
            .transact(|_| {
                std::thread::sleep(Duration::from_millis(400));
                Ok::<_, ()>(())
            })
            .await
            .is_ok()
    })
    .expect("the blocker spawns");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let starter = NativeTask::spawn(async move {
        start(
            &product,
            start_command(run, "attempt.attempt-jobs.raced", UtcMicros(20_000)),
        )
        .await
    })
    .expect("the start spawns");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let pauser = NativeTask::spawn(async move {
        work.pause_run(
            context(),
            PauseWorkRunCommand {
                task_id: task(),
                run_id: id::<RunId>(run),
                reason: WorkRunControlReasonV1::OperatorRequest,
                expected_authority_version: None,
                occurred_at: UtcMicros(25_000),
            },
        )
        .await
        .map(|transition| transition.control.state())
    })
    .expect("the pause spawns");
    let NativeTaskOutcome::Completed(blocked) = blocker.wait().await else {
        panic!("the blocker completes");
    };
    assert!(blocked, "the blocker held the writer");
    let NativeTaskOutcome::Completed(started) = starter.wait().await else {
        panic!("the start completes");
    };
    let NativeTaskOutcome::Completed(paused) = pauser.wait().await else {
        panic!("the pause completes");
    };
    paused.expect("the pause commits");
    let raced = identity(run, "attempt.attempt-jobs.raced");
    let stored = database
        .work_storage()
        .expect("Work storage")
        .load(&authority(), &raced);
    match started {
        Ok(attempt) => assert_eq!(
            stored.expect("a committed start stored its attempt"),
            attempt,
            "the start committed before the pause"
        ),
        Err(problem) => {
            assert_eq!(
                problem
                    .diagnostic()
                    .map(|diagnostic| diagnostic.code.as_str()),
                Some("application.work-run-control.paused"),
                "a start the pause reached first is refused by the reservation fence, \
                 never by a pause committed between the fence and the insert: {problem:?}"
            );
            assert!(stored.is_err(), "a refused start stored no attempt");
        }
    }
}

fn provider_evidence(
    attempt: &WorkAttemptIdentityV1,
    outcome: WorkAttemptProviderOutcomeV1,
    observed_at: UtcMicros,
) -> WorkAttemptEvidenceRecordV1 {
    WorkAttemptEvidenceRecordV1 {
        identity: attempt.clone(),
        requested_route: route(),
        actual_route: Some(route()),
        outcome,
        stdout: None,
        stderr: None,
        provider_session: None,
        provider_fallback: None,
        observed_at,
    }
}

#[tokio::test]
async fn provider_driver_steps_awaited_on_a_native_worker_park_no_native_thread() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let storage = database.work_storage().expect("Work storage");
    let run = id::<RunId>("run.attempt-jobs.driver");
    let [succeeded, denied, cancelled] = ["succeeded", "denied", "cancelled"].map(|suffix| {
        let attempt = crate::work_run_control_jobs::leased_attempt_of(
            run.clone(),
            &format!("attempt-jobs.driver.{suffix}"),
        );
        storage
            .insert(&authority(), &attempt)
            .expect("the attempt is stored");
        attempt
    });
    let (effect_state, deadline) = (
        succeeded.execution().effect_state(),
        succeeded.execution().deadline(),
    );
    let [succeeded, denied, cancelled] =
        [succeeded, denied, cancelled].map(|attempt| attempt.identity().clone());
    let setup = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    setup
        .attempts()
        .mark_running(&product_context(), &cancelled, route())
        .expect("the cancelled attempt runs");
    setup
        .attempts()
        .request_cancellation(
            &product_context(),
            tracedecay_contracts::CancelWorkAttemptCommand {
                task_id: task(),
                run_id: run.clone(),
                attempt_id: cancelled.attempt_id().clone(),
                request_id: id("cancellation.attempt-jobs.driver"),
                occurred_at: UtcMicros(20_000),
            },
        )
        .expect("the cancellation is requested");
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let identities = (succeeded.clone(), denied.clone(), cancelled.clone());
    let (outcome, writer_waits, reader_waits) = on_native_worker(async move {
        let (succeeded, denied, cancelled) = identities;
        let context = product_context();
        let dispatched = work
            .record_effect_dispatch(
                context.clone(),
                succeeded.clone(),
                effect_state,
                UtcMicros(21_000),
                deadline,
            )
            .await
            .map(|_| ());
        let running = work
            .mark_running(context.clone(), succeeded.clone(), route())
            .await
            .map(|attempt| attempt.state());
        let effect = work
            .settle_effect_dispatch(
                context.clone(),
                succeeded.clone(),
                tracedecay_contracts::WorkAttemptEffectResolutionV1::NoEffect,
                UtcMicros(22_000),
            )
            .await
            .map(|holder| holder.resolution());
        let settled = work
            .settle_with_artifacts(
                context.clone(),
                succeeded.clone(),
                provider_evidence(
                    &succeeded,
                    WorkAttemptProviderOutcomeV1::Exited { code: 0 },
                    UtcMicros(23_000),
                ),
                Vec::new(),
            )
            .await
            .map(|attempt| attempt.state());
        let fenced = work
            .mark_provider_unavailable(context.clone(), denied.clone(), UtcMicros(24_000))
            .await
            .map(|attempt| attempt.state());
        let failed = work
            .fail_recovery(
                context.clone(),
                denied.clone(),
                WorkAttemptEvidenceRecordV1 {
                    actual_route: None,
                    ..provider_evidence(
                        &denied,
                        WorkAttemptProviderOutcomeV1::LaunchFailed,
                        UtcMicros(25_000),
                    )
                },
            )
            .await
            .map(|attempt| attempt.state());
        let acknowledged = work
            .acknowledge_cancellation(context.clone(), cancelled.clone(), UtcMicros(26_000))
            .await
            .map(|attempt| attempt.state());
        let escalated = work
            .escalate_cancellation(context.clone(), cancelled.clone(), UtcMicros(27_000))
            .await
            .map(|attempt| attempt.state());
        let sealed = work
            .settle_with_artifacts(
                context,
                cancelled.clone(),
                provider_evidence(
                    &cancelled,
                    WorkAttemptProviderOutcomeV1::Cancelled,
                    UtcMicros(28_000),
                ),
                Vec::new(),
            )
            .await
            .map(|attempt| attempt.state());
        vec![
            dispatched.map(|()| None),
            running.map(Some),
            effect.map(|_| None),
            settled.map(Some),
            fenced.map(Some),
            failed.map(Some),
            acknowledged.map(Some),
            escalated.map(Some),
            sealed.map(Some),
        ]
    })
    .await;
    assert_eq!(
        writer_waits, 0,
        "the provider driver's steps parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "the provider driver's steps parked no native worker on a reader"
    );
    assert_eq!(
        outcome,
        vec![
            Ok(None),
            Ok(Some(WorkAttemptStateV1::Running)),
            Ok(None),
            Ok(Some(WorkAttemptStateV1::Succeeded)),
            Ok(Some(WorkAttemptStateV1::RecoveryRequired)),
            Ok(Some(WorkAttemptStateV1::Failed)),
            Ok(Some(WorkAttemptStateV1::CancellationAcknowledged)),
            Ok(Some(WorkAttemptStateV1::CancellationEscalated)),
            Ok(Some(WorkAttemptStateV1::Cancelled)),
        ],
        "every driver step committed its transition"
    );
    for (identity, state) in [
        (&succeeded, WorkAttemptStateV1::Succeeded),
        (&denied, WorkAttemptStateV1::Failed),
        (&cancelled, WorkAttemptStateV1::Cancelled),
    ] {
        assert_eq!(
            storage
                .load(&authority(), identity)
                .expect("the attempt reads")
                .state(),
            state,
            "the driver's jobs committed the terminal state"
        );
    }
}

#[tokio::test]
async fn cancellation_awaited_on_a_native_worker_parks_no_native_thread() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let storage = database.work_storage().expect("Work storage");
    let attempt = crate::work_run_control_jobs::leased_attempt_of(
        id::<RunId>("run.attempt-jobs.cancel"),
        "attempt-jobs.cancel",
    );
    storage
        .insert(&authority(), &attempt)
        .expect("the attempt is stored");
    let identity = attempt.identity().clone();
    let command = |request_id: &str| tracedecay_contracts::CancelWorkAttemptCommand {
        task_id: task(),
        run_id: identity.run_id().clone(),
        attempt_id: identity.attempt_id().clone(),
        request_id: id(request_id),
        occurred_at: UtcMicros(20_000),
    };
    let (first, replay, other) = (
        command("cancellation.attempt-jobs.1"),
        command("cancellation.attempt-jobs.1"),
        command("cancellation.attempt-jobs.2"),
    );
    let work = RegisteredWorkApplicationServicesV1::attach(database).expect("Work services");
    let (outcome, writer_waits, reader_waits) = on_native_worker(async move {
        (
            work.cancel_attempt(product_context(), first).await,
            work.cancel_attempt(product_context(), replay).await,
            work.cancel_attempt(product_context(), other).await,
        )
    })
    .await;
    assert_eq!(
        writer_waits, 0,
        "the cancellation parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "the cancellation parked no native worker on a reader"
    );
    let (requested, replayed, conflicting) = outcome;
    let requested = requested.expect("the cancellation commits");
    assert_eq!(requested.state(), WorkAttemptStateV1::CancellationRequested);
    assert_eq!(
        replayed.expect("the same request replays"),
        requested,
        "the same request replays the committed attempt"
    );
    assert_eq!(
        conflicting
            .expect_err("a different request conflicts")
            .diagnostic()
            .map(|diagnostic| diagnostic.code.clone()),
        Some("application.work-attempt.cancellation-conflict".to_owned())
    );
    assert_eq!(
        storage
            .load(&authority(), &identity)
            .expect("the attempt reads"),
        requested,
        "the job committed the cancellation request"
    );
}

#[tokio::test]
async fn product_mutations_awaited_on_a_native_worker_park_no_native_thread() {
    let profile = tempfile::tempdir().expect("profile");
    let project = tempfile::tempdir().expect("project");
    let runtime = project_runtime(profile.path(), project.path()).await;
    let database = runtime.project_database().expect("project database");
    let (writer_waits, reader_waits) = admit_task(database).await;
    assert_eq!(
        writer_waits, 0,
        "the create, proposal and admission jobs parked no native worker on the writer"
    );
    assert_eq!(
        reader_waits, 0,
        "the create, proposal and admission jobs parked no native worker on a reader"
    );
    let product =
        RegisteredWorkProductServicesV1::attach(database, binding()).expect("product services");
    let read = product
        .reads()
        .read_graph(
            &product_context(),
            tracedecay_contracts::WorkGraphReadRequestV1::current(selection(), UtcMicros(13_000)),
        )
        .expect("the graph reads")
        .into_current_snapshot()
        .expect("the graph has a head");
    let item = read.graph().item(&task()).expect("the task is published");
    assert!(
        item.is_execution_admitted(),
        "the admission job committed the execution admission"
    );
    assert_eq!(
        item.accepted_proposal(),
        Some(&id::<ProposalId>("proposal.attempt-jobs")),
        "the proposal job committed the acceptance"
    );
}
