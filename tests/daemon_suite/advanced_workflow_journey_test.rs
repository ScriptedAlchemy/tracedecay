//! Mounted advanced-workflow journey across fan-out, crash recovery,
//! cancellation, synthesis, and a single-use host handoff.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_application::configuration::{
    ConfigurationGetRequestV1, ConfigurationObservedStateRequestV1, ConfigurationSetRequestV1,
};
use tracedecay_application::{
    AdmitWorkSynthesisCommand, PrepareWorkProductMutationRequestV1, TaskHandoffIssueRequest,
    TaskHandoffRedeemRequest, TaskHandoffScope, WorkAttemptStatusRequestV1,
    WorkEvidenceRetrieveRequestV1, WorkEvidenceSourceV1, WorkGraphReadRequestV1,
    WorkHandoffFrontierV1, WorkHandoffLineageV1, WorkProductChangeDraftV1,
    WorkProductMutationRequestV1, WorkProductSelectionScopeV1, WorkRelationScopeV1,
    WorkSynthesisAttemptV1, WorkflowDefinitionActivateRequest, WorkflowDefinitionRegisterRequest,
    WorkflowExecutionFence, WorkflowFailurePolicy, WorkflowFanOutInput, WorkflowFanOutStartV1,
    WorkflowProviderRegistration, WorkflowRunCancelRequest, WorkflowRunGetRequest,
    WorkflowRunStartRequest,
};
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationValueV1, SettingKey,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1, WorkExecutableCapabilityV1,
    safe_work_topology_policy_v1,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, InitiativeId, ManifestDigest,
    MilestoneId, ObservationSourceIdentityV1, ProjectId, ProposalId, ProviderId, RefId,
    RepositoryId, RunId, SessionId, TaskId, TemporalModeV1, ThreadId, UtcMicros,
    WorkApprovalPolicy, WorkAttemptIdentityV1, WorkAttemptStateV1, WorkCommandId,
    WorkEffectStateV1, WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits,
    WorkExecutionSnapshot, WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1,
    WorkFilesystemPolicy, WorkGraphVersionV1, WorkHierarchyV1, WorkInitiativeV1, WorkItemInputV1,
    WorkItemV1, WorkLeaseFenceV1, WorkLeaseId, WorkMilestoneV1, WorkPlanId, WorkPlanV1,
    WorkProposalDispositionV1, WorkProposalV1, WorkProviderBackendV1, WorkProviderProtocol,
    WorkProviderRouteId, WorkProviderRouteV1, WorkRouteDecisionV1, WorkSandboxPolicy,
    WorkScoreKindV1, WorkShapeAssessmentV1, WorkSizingV1, WorkTerminalEvidenceV1, WorkVersion,
    WorkflowDefinition, WorkflowDefinitionId, WorkflowFanOut, WorkflowOperationRef,
    WorkflowOutputName, WorkflowRunStatus, WorkflowStep, WorkflowStepId, WorktreeId,
    canonical_sha256,
};
use tracedecay_sdk::client::{Client, ClientError};
use tracedecay_sdk::operations::{
    ApplicationConfigurationGet, ApplicationConfigurationObservedState,
    ApplicationConfigurationSet, WorkAttemptStatus, WorkMutateGraph, WorkPrepareGraphMutation,
    WorkRetrieveEvidence, WorkSynthesize, WorkViews, WorkflowActivateDefinition, WorkflowCancelRun,
    WorkflowGetRun, WorkflowHandoffIssue, WorkflowHandoffRedeem, WorkflowRegisterDefinition,
    WorkflowStartRun,
};

use super::common;

#[path = "advanced_workflow_journey/daemon_fixture.rs"]
mod daemon_fixture;
#[path = "advanced_workflow_journey/task_session.rs"]
mod task_session;

use daemon_fixture::{
    sdk_client, spawn_project_daemon, wait_for_application_mount, wait_for_work_mount,
    workflow_tempdir,
};

const DAEMON_ACTOR: &str = "actor.tracedecay-daemon.project-open";
const PROVIDER_SESSION_ID: &str = "session.advanced-workflow-provider";
const PROVIDER_TRANSCRIPT_USER_MESSAGE_ID: &str = "advanced-workflow-provider-user";
const PROVIDER_TRANSCRIPT_ASSISTANT_MESSAGE_ID: &str = "message.advanced-workflow-provider";
const PROVIDER_TRANSCRIPT_REFRESH_MESSAGE_ID: &str =
    "message.advanced-workflow-provider-participant-refresh";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("advanced workflow identity")
}

fn run(command: &mut Command, operation: &str) -> Vec<u8> {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{operation} failed to start: {error}"));
    assert!(
        output.status.success(),
        "{operation} failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn now() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock")
                .as_micros(),
        )
        .expect("test clock fits"),
    )
}

fn sha256(bytes: &[u8]) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
        .expect("sha256 digest")
}

fn fan_out_input(identity: &str, graph_version: u64) -> WorkflowFanOutInput {
    let input_digest = sha256(identity.as_bytes());
    let task_identity = match identity {
        "fast" => "task.advanced-workflow.01-fast",
        "crash" => "task.advanced-workflow.02-crash",
        "cancel" => "task.advanced-workflow.03-cancel",
        "synthesis" => "task.advanced-workflow-synthesis",
        other => panic!("unknown advanced workflow child {other}"),
    };
    let task_id = id::<TaskId>(task_identity);
    let initiative_id = id::<InitiativeId>(&format!("initiative.advanced-workflow.{identity}"));
    let plan_id = id::<WorkPlanId>(&format!("plan.advanced-workflow.{identity}"));
    let milestone_id = id::<MilestoneId>(&format!("milestone.advanced-workflow.{identity}"));
    let created_at = now();
    let initiative = WorkInitiativeV1::new(
        initiative_id.clone(),
        format!("Advanced workflow initiative {identity}"),
        created_at,
    )
    .expect("fan-out initiative");
    let plan = WorkPlanV1::new(
        plan_id.clone(),
        initiative_id.clone(),
        format!("Advanced workflow plan {identity}"),
        created_at,
    )
    .expect("fan-out plan");
    let milestone = WorkMilestoneV1::new(
        milestone_id.clone(),
        plan_id.clone(),
        format!("Advanced workflow milestone {identity}"),
        created_at,
    )
    .expect("fan-out milestone");
    let item = WorkItemV1::new(WorkItemInputV1 {
        task_id: task_id.clone(),
        hierarchy: WorkHierarchyV1::new(initiative_id, plan_id, milestone_id),
        title: format!("Advanced workflow child {identity}"),
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
    .expect("fan-out Work item");
    let proposal = WorkProposalV1::new(
        id::<ProposalId>(&format!("proposal.advanced-workflow.{identity}")),
        task_id,
        WorkGraphVersionV1::new(graph_version).expect("fan-out graph version"),
        WorkShapeAssessmentV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, 1)
            .expect("fan-out proposal shape"),
        WorkSizingV1::new(WorkScoreKindV1::Ordinal, 1, 1, 1, "complete workflow child")
            .expect("fan-out proposal sizing"),
        Vec::new(),
        WorkRouteDecisionV1::abstain("workflow provider is pinned by admission")
            .expect("fan-out proposal route"),
        format!("Execute fan-out child {identity}"),
        input_digest.clone(),
    )
    .expect("fan-out proposal");
    WorkflowFanOutInput {
        instructions: identity.to_owned(),
        input_digest,
        initiative,
        plan,
        milestone,
        item,
        proposal,
    }
}

fn sha256_path(path: &Path) -> String {
    hex::encode(Sha256::digest(path.to_string_lossy().as_bytes()))
}

fn wait_until<T>(label: &str, mut observe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(value) = observe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
fn write_provider_fixture(
    root: &Path,
    first_started: &Path,
    cancellation_started: &Path,
    first_hold: &Path,
    cancellation_hold: &Path,
) -> (PathBuf, Vec<u8>) {
    use std::os::unix::fs::PermissionsExt;

    let script = format!(
        "#!/bin/sh\ninput=$(/bin/cat)\ncase \"$input\" in\n  crash)\n    : > '{first_started}'\n    while [ -e '{first_hold}' ]; do /bin/sleep 1; done\n    exit 1\n    ;;\n  cancel)\n    : > '{cancellation_started}'\n    while [ -e '{cancellation_hold}' ]; do /bin/sleep 1; done\n    ;;\n  *)\n    printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{provider_session}\"}}'\n    printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"fan-out evidence\"}}]}}}}'\n    printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}}'\n    ;;\nesac\n",
        first_started = first_started.display(),
        cancellation_started = cancellation_started.display(),
        first_hold = first_hold.display(),
        cancellation_hold = cancellation_hold.display(),
        provider_session = PROVIDER_SESSION_ID,
    )
    .into_bytes();
    let path = root.join("workflow-provider");
    std::fs::write(&path, &script).expect("provider script");
    let mut permissions = std::fs::metadata(&path)
        .expect("provider metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&path, permissions).expect("provider executable mode");
    (path.canonicalize().expect("canonical provider"), script)
}

#[cfg(windows)]
fn write_provider_fixture(
    root: &Path,
    first_started: &Path,
    cancellation_started: &Path,
    first_hold: &Path,
    cancellation_hold: &Path,
) -> (PathBuf, Vec<u8>) {
    let script = format!(
        "@echo off\r\nset \"input=\"\r\nset /p \"input=\"\r\nif /I \"%input%\"==\"crash\" goto crash\r\nif /I \"%input%\"==\"cancel\" goto cancel\r\necho {{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{provider_session}\"}}\r\necho {{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"fan-out evidence\"}}]}}}}\r\necho {{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}}\r\nexit /b 0\r\n:crash\r\ntype nul > \"{first_started}\"\r\n:wait_first\r\nif exist \"{first_hold}\" (timeout /t 1 /nobreak >nul & goto wait_first)\r\nexit /b 1\r\n:cancel\r\ntype nul > \"{cancellation_started}\"\r\n:wait_cancel\r\nif exist \"{cancellation_hold}\" (timeout /t 1 /nobreak >nul & goto wait_cancel)\r\nexit /b 0\r\n",
        first_started = first_started.display(),
        cancellation_started = cancellation_started.display(),
        first_hold = first_hold.display(),
        cancellation_hold = cancellation_hold.display(),
        provider_session = PROVIDER_SESSION_ID,
    )
    .into_bytes();
    let path = root.join("workflow-provider.cmd");
    std::fs::write(&path, &script).expect("provider script");
    (path.canonicalize().expect("canonical provider"), script)
}

fn attempt_status(
    client: &Client,
    identity: &WorkAttemptIdentityV1,
) -> Option<tracedecay_domain::WorkAttemptV1> {
    match client.execute::<WorkAttemptStatus>(&WorkAttemptStatusRequestV1 {
        task_id: identity.task_id().clone(),
        run_id: identity.run_id().clone(),
        attempt_id: identity.attempt_id().clone(),
    }) {
        Ok(response) => Some(response.result),
        Err(ClientError::Problem(problem)) if problem.kind == "not_found_or_not_authorized" => None,
        Err(error) => panic!("mounted Work attempt status failed: {error}"),
    }
}

fn provider_transcript_path(home: &Path) -> PathBuf {
    home.join(".claude/projects/advanced-workflow-provider")
        .join(format!("{PROVIDER_SESSION_ID}.jsonl"))
}

fn provider_transcript_query(identity: &WorkAttemptIdentityV1) -> String {
    format!(
        "{PROVIDER_TRANSCRIPT_USER_MESSAGE_ID}: {} {}:{} claude {}",
        identity.task_id().as_str(),
        identity.run_id().as_str(),
        identity.attempt_id().as_str(),
        PROVIDER_SESSION_ID,
    )
}

fn provider_transcript_assistant_text(identity: &WorkAttemptIdentityV1) -> String {
    format!(
        "{PROVIDER_TRANSCRIPT_ASSISTANT_MESSAGE_ID}: {} completed through the typed SDK provider session",
        provider_transcript_query(identity),
    )
}

pub(super) fn seeded_provider_transcript_contents(
    identity: &WorkAttemptIdentityV1,
) -> [Vec<u8>; 2] {
    let assistant = serde_json::to_string(&serde_json::json!([{
        "type": "text",
        "text": provider_transcript_assistant_text(identity),
    }]))
    .expect("serialize seeded assistant transcript content");
    [
        provider_transcript_query(identity).into_bytes(),
        assistant.into_bytes(),
    ]
}

fn write_provider_transcript(home: &Path, project: &Path, identity: &WorkAttemptIdentityV1) {
    let directory = home.join(".claude/projects/advanced-workflow-provider");
    std::fs::create_dir_all(&directory).expect("provider transcript directory");
    let query = provider_transcript_query(identity);
    let cwd = project.to_string_lossy();
    let records = [
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": PROVIDER_SESSION_ID,
            "uuid": PROVIDER_TRANSCRIPT_USER_MESSAGE_ID,
            "timestamp": "2026-08-09T12:00:00.000Z",
            "message": {"role": "user", "content": query},
        }),
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": PROVIDER_SESSION_ID,
            "uuid": "advanced-workflow-provider-assistant",
            "timestamp": "2026-08-09T12:00:01.000Z",
            "message": {
                "id": PROVIDER_TRANSCRIPT_ASSISTANT_MESSAGE_ID,
                "role": "assistant",
                "model": "fixture-model",
                "content": [{"type": "text", "text": provider_transcript_assistant_text(identity)}],
            },
        }),
    ];
    let contents = records
        .into_iter()
        .map(|record| record.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(provider_transcript_path(home), format!("{contents}\n"))
        .expect("provider transcript");
}

/// Advances the real imported participant generation through the public
/// transcript-ingest command. The previous TaskSession continuation must then
/// be rejected against the manifest newly frozen from this source.
pub(super) fn advance_provider_transcript_participant_generation(
    home: &Path,
    project: &Path,
    identity: &WorkAttemptIdentityV1,
) {
    let record = serde_json::json!({
        "type": "assistant",
        "cwd": project.to_string_lossy(),
        "sessionId": PROVIDER_SESSION_ID,
        "uuid": "advanced-workflow-provider-participant-refresh",
        "timestamp": "2026-08-09T12:00:02.000Z",
        "message": {
            "id": PROVIDER_TRANSCRIPT_REFRESH_MESSAGE_ID,
            "role": "assistant",
            "model": "fixture-model",
            "content": [{
                "type": "text",
                "text": format!(
                    "{PROVIDER_TRANSCRIPT_REFRESH_MESSAGE_ID}: {} refreshed through the public sessions import authority",
                    provider_transcript_query(identity),
                ),
            }],
        },
    });
    let mut transcript = OpenOptions::new()
        .append(true)
        .open(provider_transcript_path(home))
        .expect("open provider transcript for participant refresh");
    writeln!(transcript, "{record}").expect("append provider transcript participant refresh");
    drop(transcript);
    run(
        common::tracedecay_command_with_home(home)
            .args(["sessions", "import", "--project-path"])
            .arg(project)
            .current_dir(project),
        "tracedecay sessions import participant refresh",
    );
}

fn initialize_project(home: &Path, project: &Path) -> (String, CommitId) {
    std::fs::create_dir_all(home).expect("home directory");
    std::fs::create_dir_all(project).expect("project directory");
    task_session::seed_semantic_source(project);
    std::fs::write(project.join("README.md"), "advanced workflow journey\n")
        .expect("fixture source");
    run(
        Command::new(common::git_program())
            .args(["init", "--quiet"])
            .current_dir(project),
        "git init",
    );
    run(
        Command::new(common::git_program())
            .args([
                "-c",
                "user.email=workflow@tracedecay.invalid",
                "-c",
                "user.name=Workflow Journey",
                "add",
                ".",
            ])
            .current_dir(project),
        "git add",
    );
    run(
        Command::new(common::git_program())
            .args([
                "-c",
                "user.email=workflow@tracedecay.invalid",
                "-c",
                "user.name=Workflow Journey",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ])
            .current_dir(project),
        "git commit",
    );
    let commit = String::from_utf8(run(
        Command::new(common::git_program())
            .args(["rev-parse", "HEAD"])
            .current_dir(project),
        "git rev-parse",
    ))
    .expect("commit UTF-8")
    .trim()
    .to_owned();
    (commit.clone(), id(&commit))
}

#[test]
fn mounted_fan_out_recovers_then_synthesizes_and_hands_off() {
    let scratch = workflow_tempdir();
    let home = scratch.path().join("home");
    let project = scratch.path().join("project");
    let (_commit_text, commit) = initialize_project(&home, &project);
    let project = project.canonicalize().expect("canonical project root");
    let semantic_fixture = task_session::install_semantic_fixture(&home);

    let mut daemon = spawn_project_daemon(&home, &project);
    run(
        common::tracedecay_command_with_home(&home)
            .arg("init")
            .current_dir(&project),
        "tracedecay init",
    );
    let context: Value = serde_json::from_slice(&run(
        common::tracedecay_command_with_home(&home)
            .args(["projects", "context"])
            .arg(&project)
            .arg("--json")
            .current_dir(&project),
        "tracedecay projects context",
    ))
    .expect("project context JSON");
    let project_id: ProjectId = id(context["project"]["project_id"]
        .as_str()
        .expect("registered project id"));
    let client = sdk_client(&home, project_id.as_str());
    let _ = wait_for_application_mount(&client);
    wait_for_work_mount(&client);

    let first_started = scratch.path().join("first-started");
    let cancellation_started = scratch.path().join("cancellation-started");
    let first_hold = scratch.path().join("first-hold");
    let cancellation_hold = scratch.path().join("cancellation-hold");
    std::fs::write(&first_hold, b"hold").expect("first hold");
    std::fs::write(&cancellation_hold, b"hold").expect("cancellation hold");
    let (executable_path, script) = write_provider_fixture(
        scratch.path(),
        &first_started,
        &cancellation_started,
        &first_hold,
        &cancellation_hold,
    );
    let executable = WorkExecutableReference::new(
        "executable.advanced-workflow-journey".to_owned(),
        sha256(&script),
    )
    .expect("executable reference");

    let observed = client
        .execute::<ApplicationConfigurationObservedState>(&ConfigurationObservedStateRequestV1 {})
        .expect("configuration observed state")
        .result;
    let expected_revision = observed
        .first()
        .expect("configuration component")
        .desired_revision_id
        .clone();
    client
        .execute::<ApplicationConfigurationSet>(&ConfigurationSetRequestV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            key: SettingKey::new(WORK_EXECUTABLE_BINDINGS_SETTING_KEY).expect("binding key"),
            value: ConfigurationValueV1::WorkExecutableBindings(vec![
                WorkExecutableBindingV1::new(
                    executable.clone(),
                    executable_path,
                    vec![WorkExecutableCapabilityV1::ClaudeCodeStreamJson],
                )
                .expect("provider binding"),
            ]),
            expected_revision,
            idempotency_key: ConfigurationIdempotencyKey::new(
                "configuration.advanced-workflow-provider".to_owned(),
            )
            .expect("configuration idempotency"),
        })
        .expect("configure mounted workflow provider");

    daemon
        .kill_and_wait()
        .expect("restart after provider configuration");
    daemon = spawn_project_daemon(&home, &project);
    let client = sdk_client(&home, project_id.as_str());
    let observed = wait_for_application_mount(&client);
    wait_for_work_mount(&client);
    let configuration_revision_id: ConfigurationRevisionId = observed
        .first()
        .expect("configuration component")
        .desired_revision_id
        .clone();
    let resolved = client
        .execute::<ApplicationConfigurationGet>(&ConfigurationGetRequestV1 {
            key: SettingKey::new(WORK_EXECUTABLE_BINDINGS_SETTING_KEY).expect("binding key"),
        })
        .expect("pinned executable configuration")
        .result;

    let common_dir = tracedecay::worktree::git_common_dir(&project).expect("Git common dir");
    let repository_id: RepositoryId =
        id(&format!("repository.daemon.{}", sha256_path(&common_dir)));
    let worktree_id: WorktreeId = id(&format!("worktree.daemon.{}", sha256_path(&project)));
    let product_selection =
        WorkProductSelectionScopeV1::relations(BTreeSet::from([WorkRelationScopeV1::Repository {
            project_id: project_id.clone(),
            repository_id: repository_id.clone(),
        }]))
        .expect("repository Work selection");
    let reference = tracedecay::branch::current_branch(&project)
        .map(|branch| id::<RefId>(&format!("refs/heads/{branch}")));
    let scope = tracedecay_application::ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        worktree_id.clone(),
        reference,
    )
    .expect("resolved project scope");
    let policy_digest = canonical_sha256(&(
        "tracedecay.daemon.configuration-policy.v1",
        &scope.scope_digest,
        &resolved.effective_behavior_digest,
        &resolved.resolution_provenance_digest,
    ))
    .expect("project-open policy digest");
    let catalog_digest =
        tracedecay_application::work_executable_catalog_digest().expect("Work catalog digest");
    let definition_id: WorkflowDefinitionId = id("workflow.advanced-production-journey");
    let step_id: WorkflowStepId = id("fan-out");
    let downstream_step_id: WorkflowStepId = id("collect-results");
    let definition = WorkflowDefinition::new(
        definition_id.clone(),
        1,
        project_id.clone(),
        vec![
            // Keep the dependent step first so production admission must use
            // the verified workflow topology, not caller-controlled Vec order,
            // to select the runnable entry step.
            WorkflowStep {
                step_id: downstream_step_id.clone(),
                operation: id::<WorkflowOperationRef>("operation.work.start_attempt"),
                predecessors: BTreeSet::from([step_id.clone()]),
                inputs: Vec::new(),
                outputs: Vec::new(),
                // This blocked step is deliberately fan-out-capable too: an
                // order scan for the first fan-out step would select it, so
                // only the verified ready-set can choose the runnable root.
                fan_out: Some(WorkflowFanOut { max_width: 3 }),
            },
            WorkflowStep {
                step_id: step_id.clone(),
                operation: id::<WorkflowOperationRef>("operation.work.start_attempt"),
                predecessors: BTreeSet::new(),
                inputs: Vec::new(),
                outputs: vec![id::<WorkflowOutputName>("finding")],
                fan_out: Some(WorkflowFanOut { max_width: 3 }),
            },
        ],
        policy_digest,
        resolved.effective_behavior_digest.clone(),
        catalog_digest,
    )
    .expect("workflow definition");
    client
        .execute::<WorkflowRegisterDefinition>(&WorkflowDefinitionRegisterRequest {
            definition: definition.clone(),
        })
        .expect("mounted workflow definition registration");

    // Catalog admission refuses an uncataloged step operation before the
    // lifecycle transition is journaled (Plan 32).
    let uncataloged_definition_id: WorkflowDefinitionId =
        id("workflow.advanced-production-journey.uncataloged");
    let uncataloged = WorkflowDefinition::new(
        uncataloged_definition_id.clone(),
        1,
        project_id.clone(),
        vec![WorkflowStep {
            step_id: id("fan-out"),
            operation: id::<WorkflowOperationRef>("operation.work.not_a_mounted_operation"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            fan_out: Some(WorkflowFanOut { max_width: 3 }),
        }],
        definition.pinned_policy_digest().clone(),
        definition.pinned_configuration_digest().clone(),
        definition.pinned_catalog_digest().clone(),
    )
    .expect("uncataloged workflow definition");
    client
        .execute::<WorkflowRegisterDefinition>(&WorkflowDefinitionRegisterRequest {
            definition: uncataloged,
        })
        .expect("candidate registration stays lenient before activation");
    let admission_denial = client
        .execute::<WorkflowActivateDefinition>(&WorkflowDefinitionActivateRequest {
            definition_id: uncataloged_definition_id,
            definition_version: 1,
            expected_revision: 1,
        })
        .expect_err("activation must refuse an operation the catalog does not mount");
    assert!(
        matches!(
            admission_denial,
            ClientError::Problem(ref problem) if problem.kind == "invalid_request"
        ),
        "catalog admission denial must be a typed refusal: {admission_denial}"
    );

    client
        .execute::<WorkflowActivateDefinition>(&WorkflowDefinitionActivateRequest {
            definition_id: definition_id.clone(),
            definition_version: 1,
            expected_revision: 1,
        })
        .expect("mounted workflow definition activation");

    let route = WorkProviderRouteV1::new(
        id::<ProviderId>("provider.work.claude-code-cli"),
        id::<WorkProviderRouteId>("route.advanced-workflow-journey"),
    )
    .expect("provider route");
    let execution_snapshot = WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id,
        configuration_snapshot_id: resolved.snapshot_id,
        effective_behavior_digest: resolved.effective_behavior_digest,
        resolution_provenance_digest: resolved.resolution_provenance_digest,
        route: route.clone(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "fixture-model".to_owned(),
        executable,
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 2)
            .expect("execution limits"),
        deadline: UtcMicros(now().0 + 300_000_000),
        fallback: WorkFallbackTopology::Disabled,
        topology: safe_work_topology_policy_v1(),
    })
    .expect("execution snapshot");
    let run_id: RunId = id("run.advanced-production-journey");
    let start_request = WorkflowRunStartRequest {
        run_id: run_id.clone(),
        definition_id: definition_id.clone(),
        definition_version: 1,
        provider: WorkflowProviderRegistration::new(
            route,
            WorkProviderBackendV1::ClaudeCodeCli,
            "fixture-model".to_owned(),
            1,
        )
        .expect("provider registration"),
        fan_out: Some(WorkflowFanOutStartV1 {
            fence: WorkflowExecutionFence {
                attempt_id: id::<AttemptId>("attempt.workflow-controller"),
                lease: WorkLeaseFenceV1::new(
                    id::<WorkLeaseId>("lease.workflow-controller"),
                    WorkFenceEpochV1::new(1).expect("controller fence"),
                )
                .expect("controller lease"),
            },
            max_parallel: 1,
            failure_policy: WorkflowFailurePolicy::Collect,
            execution_snapshot: execution_snapshot.clone(),
            reference: None,
            commit: commit.clone(),
            effect_state: WorkEffectStateV1::Observational,
            // Each released child advances the graph four times: create,
            // accept its proposal, admit execution, then link the accepted
            // attempt before the next child is released.
            // The proposal fence names the exact head before its child
            // begins rather than relying on a fabricated workflow state.
            inputs: vec![
                fan_out_input("fast", 1),
                fan_out_input("crash", 5),
                fan_out_input("cancel", 9),
            ],
        }),
        command_id: id::<WorkCommandId>("command.workflow.start"),
    };
    let started_run =
        wait_until("idempotent mounted workflow fan-out start", || match client
            .execute::<WorkflowStartRun>(&start_request)
        {
            Ok(response) => Some(response.result),
            Err(ClientError::Problem(problem)) if problem.kind == "unavailable" => {
                std::thread::sleep(Duration::from_millis(250));
                None
            }
            Err(error) => panic!("mounted workflow fan-out start failed: {error}"),
        });
    let fan_out_identities = started_run
        .fan_out_plans()
        .values()
        .flat_map(|plan| &plan.children)
        .map(|child| child.attempt_identity.clone())
        .collect::<Vec<_>>();
    assert!(
        started_run.fan_out_plans().contains_key(&step_id),
        "the mounted verified workflow topology must select the ready fan-out root"
    );
    assert!(
        !started_run
            .fan_out_plans()
            .contains_key(&downstream_step_id),
        "caller-controlled definition order must not select a blocked step"
    );
    assert_eq!(fan_out_identities.len(), 3, "three fan-out children");

    let first_generation_deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let attempts = fan_out_identities
            .iter()
            .filter_map(|identity| attempt_status(&client, identity))
            .collect::<Vec<_>>();
        if first_started.exists()
            && attempts
                .iter()
                .any(|attempt| attempt.state() == WorkAttemptStateV1::Succeeded)
        {
            break;
        }
        assert!(
            Instant::now() < first_generation_deadline,
            "timed out waiting for crash-bound provider generation and successful sibling: \
             crash_started={}, attempts={attempts:?}",
            first_started.exists()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    daemon
        .kill_and_wait()
        .expect("force daemon crash during fan-out");
    std::fs::remove_file(&first_hold).expect("release orphaned first-generation provider");

    let mut restarted = spawn_project_daemon(&home, &project);
    let client = sdk_client(&home, project_id.as_str());
    let _ = wait_for_application_mount(&client);
    wait_for_work_mount(&client);
    wait_until("post-recovery cancellation child", || {
        cancellation_started.exists().then_some(())
    });
    let running = client
        .execute::<WorkflowGetRun>(&WorkflowRunGetRequest {
            run_id: run_id.clone(),
        })
        .expect("durably recovered workflow run")
        .result;
    assert_eq!(running.status(), WorkflowRunStatus::Running);
    client
        .execute::<WorkflowCancelRun>(&WorkflowRunCancelRequest {
            run_id: run_id.clone(),
            expected_sequence: running.sequence(),
            command_id: id::<WorkCommandId>("command.workflow.cancel-after-restart"),
        })
        .expect("mounted workflow cancellation");
    let cancellation_deadline = Instant::now() + Duration::from_secs(20);
    let sources: Vec<WorkAttemptIdentityV1> = loop {
        let attempts = fan_out_identities
            .iter()
            .filter_map(|identity| attempt_status(&client, identity))
            .collect::<Vec<_>>();
        let success = attempts
            .iter()
            .find(|attempt| attempt.state() == WorkAttemptStateV1::Succeeded);
        let failed = attempts
            .iter()
            .find(|attempt| attempt.state() == WorkAttemptStateV1::Failed);
        let cancelled = attempts
            .iter()
            .find(|attempt| attempt.state() == WorkAttemptStateV1::Cancelled);
        if let (Some(success), Some(failed), Some(cancelled)) = (success, failed, cancelled) {
            break vec![
                success.identity().clone(),
                failed.identity().clone(),
                cancelled.identity().clone(),
            ];
        }
        assert!(
            Instant::now() < cancellation_deadline,
            "timed out waiting for typed fan-out cancellation: attempts={attempts:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let cancelled_run = wait_until("cancelled workflow run", || {
        let projection = client
            .execute::<WorkflowGetRun>(&WorkflowRunGetRequest {
                run_id: run_id.clone(),
            })
            .ok()?
            .result;
        (projection.status() == WorkflowRunStatus::Cancelled).then_some(projection)
    });

    let synthesis_task: TaskId = id("task.advanced-workflow-synthesis");
    let synthesis_seed = fan_out_input("synthesis", 1);
    let prepared_synthesis_create = client
        .execute::<WorkPrepareGraphMutation>(&PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::CreateTask {
                initiative: synthesis_seed.initiative,
                plan: synthesis_seed.plan,
                milestone: synthesis_seed.milestone,
                item: Box::new(synthesis_seed.item),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
        .expect("prepare synthesis product task")
        .result;
    let created_synthesis = client
        .execute::<WorkMutateGraph>(&prepared_synthesis_create)
        .expect("create synthesis product task")
        .result;
    let synthesis_input = fan_out_input(
        "synthesis",
        created_synthesis
            .verified_graph_version()
            .graph_version()
            .get(),
    );
    let prepared_proposal_acceptance = client
        .execute::<WorkPrepareGraphMutation>(&PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::DecideProposal {
                proposal: synthesis_input.proposal,
                disposition: WorkProposalDispositionV1::Accepted,
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
        .expect("prepare synthesis proposal acceptance")
        .result;
    let accepted_synthesis = client
        .execute::<WorkMutateGraph>(&prepared_proposal_acceptance)
        .expect("accept synthesis proposal")
        .result;
    let prepared_execution_admission = client
        .execute::<WorkPrepareGraphMutation>(&PrepareWorkProductMutationRequestV1 {
            selection: product_selection.clone(),
            change: WorkProductChangeDraftV1::AdmitExecution {
                task_id: synthesis_task.clone(),
            },
            causation_event_id: None,
            evidence: Vec::new(),
        })
        .expect("prepare synthesis execution admission")
        .result;
    let WorkProductMutationRequestV1::AdmitExecution(admission) = prepared_execution_admission
    else {
        panic!("synthesis admission preparation must produce the canonical request");
    };
    assert_eq!(
        admission.based_on_version,
        accepted_synthesis.verified_graph_version().graph_version(),
        "synthesis admission must use the exact graph version that accepted its proposal"
    );
    client
        .execute::<WorkMutateGraph>(&WorkProductMutationRequestV1::AdmitExecution(admission))
        .expect("admit synthesis execution through the canonical product request");
    let synthesis_attempt_id: AttemptId = id("attempt.advanced-workflow-synthesis");
    let synthesis_identity = WorkAttemptIdentityV1::new(
        synthesis_task.clone(),
        run_id.clone(),
        synthesis_attempt_id.clone(),
    )
    .expect("synthesis attempt identity");
    let synthesis = client
        .execute::<WorkSynthesize>(&AdmitWorkSynthesisCommand {
            start: tracedecay_application::StartWorkAttemptCommand {
                task_id: synthesis_task.clone(),
                run_id: run_id.clone(),
                attempt_id: synthesis_attempt_id.clone(),
                operation: id("operation.work.synthesize"),
                execution_snapshot,
                worktree_root: project.to_string_lossy().into_owned(),
                reference: None,
                commit,
                instructions: "synthesize".to_owned(),
                effect_state: WorkEffectStateV1::Observational,
                occurred_at: now(),
            },
            output_name: id("finding"),
            sources: sources.clone(),
        })
        .expect("mounted synthesis admission")
        .result;
    let WorkSynthesisAttemptV1::Admitted(admission) = synthesis else {
        panic!("one successful source must admit synthesis: {synthesis:?}");
    };
    assert_eq!(admission.source_set.sources.len(), 3);
    assert_eq!(admission.uncited, sources[1..].to_vec());
    assert_eq!(admission.draft.cited_source_digests.len(), 1);
    let completed_synthesis = wait_until("synthesis provider completion", || {
        attempt_status(&client, &synthesis_identity)
            .filter(|attempt| attempt.state() == WorkAttemptStateV1::Succeeded)
    });
    write_provider_transcript(&home, &project, completed_synthesis.identity());
    run(
        common::tracedecay_command_with_home(&home)
            .args(["sessions", "import", "--project-path"])
            .arg(&project)
            .current_dir(&project),
        "tracedecay sessions import",
    );
    let graph = client
        .execute::<WorkViews>(&WorkGraphReadRequestV1::current(
            product_selection.clone(),
            now(),
        ))
        .expect("read synthesis product graph")
        .result;
    let verified_version = graph
        .entries()
        .last()
        .expect("current synthesis graph version")
        .verified_version()
        .clone();
    let sealed_receipt = client
        .execute::<WorkRetrieveEvidence>(&WorkEvidenceRetrieveRequestV1 {
            selection: product_selection.clone(),
            task_id: synthesis_task.clone(),
            verified_version,
            temporal: TemporalModeV1::Current,
            page_size: 100,
            expansion: None,
            continuation: None,
            observed_at: now(),
        })
        .expect("retrieve sealed synthesis attempt evidence")
        .result
        .sources
        .into_iter()
        .find_map(|source| match source {
            WorkEvidenceSourceV1::AttemptReceipt { receipt }
                if receipt.identity == completed_synthesis.identity().clone() =>
            {
                Some(receipt)
            }
            _ => None,
        })
        .expect("synthesis accepted attempt receipt");
    let terminal_digest = match completed_synthesis.terminal() {
        Some(WorkTerminalEvidenceV1::Succeeded {
            evidence_digest, ..
        }) => evidence_digest.clone(),
        terminal => panic!("synthesis must have succeeded with terminal evidence: {terminal:?}"),
    };
    let sealed_evidence = sealed_receipt
        .evidence
        .as_ref()
        .expect("sealed synthesis receipt contains evidence");
    assert_eq!(
        sealed_evidence.provider_session,
        Some(
            ObservationSourceIdentityV1::for_provider(
                id::<ProviderId>("claude"),
                id::<SessionId>(PROVIDER_SESSION_ID),
            )
            .expect("provider-qualified synthesis session"),
        ),
        "the CLI session-start frame must survive the typed SDK receipt",
    );
    assert_eq!(
        sealed_evidence
            .digest()
            .expect("sealed synthesis evidence digest"),
        terminal_digest,
        "the sealed receipt evidence must match the terminal attempt evidence"
    );

    restarted
        .kill_and_wait()
        .expect("physically restart daemon after accepted synthesis settlement");
    let mut restored_daemon = spawn_project_daemon(&home, &project);
    let client = sdk_client(&home, project_id.as_str());
    let _ = wait_for_application_mount(&client);
    wait_for_work_mount(&client);
    let restored_graph = client
        .execute::<WorkViews>(&WorkGraphReadRequestV1::current(
            product_selection.clone(),
            now(),
        ))
        .expect("read product graph after physical restart")
        .result;
    let restored_entry = restored_graph
        .entries()
        .last()
        .expect("restored synthesis graph version");
    let restored_item = restored_entry
        .graph()
        .items()
        .iter()
        .find(|item| item.task_id() == &synthesis_task)
        .expect("restored synthesis task");
    assert!(
        restored_item
            .accepted_attempts()
            .contains(completed_synthesis.identity()),
        "the accepted-attempt relation must survive physical daemon restart"
    );
    restored_daemon = task_session::configure_restart_and_activate_semantic_profile(
        &home,
        &project,
        &client,
        &project_id,
        restored_daemon,
        &product_selection,
        &synthesis_task,
        restored_entry.verified_version(),
        completed_synthesis.identity(),
        &sealed_receipt,
        &semantic_fixture,
    );
    restored_daemon
        .kill_and_wait()
        .expect("physically restart daemon after evaluated semantic activation");
    let _activated_daemon = spawn_project_daemon(&home, &project);
    let client = sdk_client(&home, project_id.as_str());
    let _ = wait_for_application_mount(&client);
    wait_for_work_mount(&client);
    task_session::wait_for_evaluated_semantic_profile_current(&home, &project, &client);
    let dashboard = task_session::DashboardProcess::start(&home, &project);
    let _task_session = task_session::assert_available_over_sdk_mcp_and_dashboard(
        &home,
        &project,
        &client,
        &dashboard,
        task_session::TaskSessionEvidenceScope {
            selection: &product_selection,
            task_id: &synthesis_task,
            verified_version: restored_entry.verified_version(),
            identity: completed_synthesis.identity(),
        },
    );

    let handoff_scope = TaskHandoffScope::new(
        project_id,
        repository_id,
        worktree_id,
        definition_id,
        1,
        step_id,
        synthesis_task.clone(),
        id::<ThreadId>("thread.advanced-workflow-handoff"),
        run_id,
        id::<ActorId>(DAEMON_ACTOR),
        id::<ActorId>(DAEMON_ACTOR),
    )
    .expect("host handoff scope");
    let frontier = WorkHandoffFrontierV1::new(
        synthesis_task,
        WorkVersion::new(restored_entry.verified_version().graph_version().get())
            .expect("synthesis product graph version"),
        Vec::new(),
        vec![format!(
            "fan-out recovered and cancelled at workflow sequence {}",
            cancelled_run.sequence()
        )],
        vec!["cancelled sibling preserved as uncited synthesis evidence".to_owned()],
        vec!["continue in the receiving host from the synthesis receipt".to_owned()],
        WorkHandoffLineageV1 {
            issued_by: id(DAEMON_ACTOR),
            issued_at: now(),
            prior_frontier_digest: None,
        },
    )
    .expect("handoff frontier");
    let secret = "h".repeat(48);
    client
        .execute::<WorkflowHandoffIssue>(&TaskHandoffIssueRequest {
            scope: handoff_scope.clone(),
            secret: secret.clone(),
            frontier: frontier.clone(),
        })
        .expect("mounted host handoff issue");
    let redeem = TaskHandoffRedeemRequest {
        secret,
        expected_scope: handoff_scope,
    };
    let redeemed = client
        .execute::<WorkflowHandoffRedeem>(&redeem)
        .expect("mounted host handoff redemption")
        .result;
    assert_eq!(redeemed.frontier, frontier);
    let replay = client
        .execute::<WorkflowHandoffRedeem>(&redeem)
        .expect_err("host handoff must be single-use");
    assert!(
        matches!(replay, ClientError::Problem(ref problem) if problem.kind == "invalid_request"),
        "handoff replay must be a typed refusal: {replay}"
    );
}
