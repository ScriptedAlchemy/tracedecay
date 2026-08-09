//! Mounted advanced-workflow journey across fan-out, crash recovery,
//! cancellation, synthesis, and a single-use host handoff.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_application::configuration::{
    ConfigurationGetRequestV1, ConfigurationObservedStateRequestV1, ConfigurationSetRequestV1,
};
use tracedecay_application::{
    AcceptProposalCommand, AdmitExecutionCommand, AdmitWorkSynthesisCommand, CreateWorkCommand,
    ReviewProposalCommand, TaskHandoffIssueRequest, TaskHandoffRedeemRequest, TaskHandoffScope,
    WorkAttemptListRequestV1, WorkHandoffFrontierV1, WorkHandoffLineageV1, WorkSynthesisAttemptV1,
    WorkflowDefinitionActivateRequest, WorkflowDefinitionRegisterRequest, WorkflowExecutionFence,
    WorkflowFailurePolicy, WorkflowFanOutInput, WorkflowFanOutStartV1,
    WorkflowProviderRegistration, WorkflowRunCancelRequest, WorkflowRunGetRequest,
    WorkflowRunStartRequest,
};
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationValueV1, SettingKey,
    WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1, WorkExecutableCapabilityV1,
    safe_work_topology_policy_v1,
};
use tracedecay_domain::{
    ActorId, AttemptId, CommitId, ConfigurationRevisionId, ManifestDigest, ProjectId, ProposalId,
    ProviderId, RepositoryId, RunId, TaskId, ThreadId, UtcMicros, WorkApprovalPolicy,
    WorkAttemptIdentityV1, WorkAttemptStateV1, WorkCommandId, WorkEffectStateV1, WorkEgressPolicy,
    WorkExecutableReference, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFenceEpochV1, WorkFilesystemPolicy,
    WorkLeaseFenceV1, WorkLeaseId, WorkProviderBackendV1, WorkProviderProtocol,
    WorkProviderRouteId, WorkProviderRouteV1, WorkSandboxPolicy, WorkVersion, WorkflowDefinition,
    WorkflowDefinitionId, WorkflowFanOut, WorkflowOperationRef, WorkflowOutputName,
    WorkflowRunStatus, WorkflowStep, WorkflowStepId, WorktreeId, canonical_sha256,
};
use tracedecay_sdk::client::{Client, ClientError, ConnectionMode};
use tracedecay_sdk::operations::{
    ApplicationConfigurationGet, ApplicationConfigurationObservedState,
    ApplicationConfigurationSet, WorkAcceptProposal, WorkAdmitExecution, WorkCreate,
    WorkListAttempts, WorkSynthesize, WorkflowActivateDefinition, WorkflowCancelRun,
    WorkflowGetRun, WorkflowHandoffIssue, WorkflowHandoffRedeem, WorkflowRegisterDefinition,
    WorkflowStartRun,
};

use super::common;

const DAEMON_ACTOR: &str = "actor.tracedecay-daemon.project-open";

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

fn sha256_path(path: &Path) -> String {
    hex::encode(Sha256::digest(path.to_string_lossy().as_bytes()))
}

fn read_daemon_authority(home: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(common::daemon_authority_path(&home.join(".tracedecay")))
            .expect("daemon authority record"),
    )
    .expect("daemon authority JSON")
}

fn sdk_client(home: &Path, project_id: &str) -> Client {
    let authority = read_daemon_authority(home);
    let endpoint = authority["http_application_endpoint"]
        .as_str()
        .expect("HTTP application endpoint");
    let token = authority["auth_token"].as_str().expect("daemon token");
    let base = format!("http://{endpoint}");
    Client::builder(ConnectionMode::local(&base, project_id, token))
        .origin(&base)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("canonical SDK client")
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
        "#!/bin/sh\ninput=$(/bin/cat)\ncase \"$input\" in\n  crash)\n    : > '{first_started}'\n    while [ -e '{first_hold}' ]; do /bin/sleep 1; done\n    ;;\n  cancel)\n    : > '{cancellation_started}'\n    while [ -e '{cancellation_hold}' ]; do /bin/sleep 1; done\n    ;;\n  *)\n    printf 'fan-out evidence: %s\\n' \"$input\"\n    ;;\nesac\n",
        first_started = first_started.display(),
        cancellation_started = cancellation_started.display(),
        first_hold = first_hold.display(),
        cancellation_hold = cancellation_hold.display(),
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
        "@echo off\r\nset \"input=\"\r\nset /p \"input=\"\r\nif /I \"%input%\"==\"crash\" goto crash\r\nif /I \"%input%\"==\"cancel\" goto cancel\r\necho fan-out evidence: %input%\r\nexit /b 0\r\n:crash\r\ntype nul > \"{first_started}\"\r\n:wait_first\r\nif exist \"{first_hold}\" (timeout /t 1 /nobreak >nul & goto wait_first)\r\nexit /b 0\r\n:cancel\r\ntype nul > \"{cancellation_started}\"\r\n:wait_cancel\r\nif exist \"{cancellation_hold}\" (timeout /t 1 /nobreak >nul & goto wait_cancel)\r\nexit /b 0\r\n",
        first_started = first_started.display(),
        cancellation_started = cancellation_started.display(),
        first_hold = first_hold.display(),
        cancellation_hold = cancellation_hold.display(),
    )
    .into_bytes();
    let path = root.join("workflow-provider.cmd");
    std::fs::write(&path, &script).expect("provider script");
    (path.canonicalize().expect("canonical provider"), script)
}

fn listed_attempts(client: &Client) -> Vec<tracedecay_domain::WorkAttemptV1> {
    match client
        .execute::<WorkListAttempts>(&WorkAttemptListRequestV1 {
            page_size: 100,
            cursor: None,
        })
        .expect("mounted Work attempt list")
        .result
    {
        tracedecay_application::WorkAttemptListV1::Listed { attempts, .. } => attempts,
        tracedecay_application::WorkAttemptListV1::Absent => Vec::new(),
    }
}

fn initialize_project(home: &Path, project: &Path) -> (String, CommitId) {
    std::fs::create_dir_all(home).expect("home directory");
    std::fs::create_dir_all(project).expect("project directory");
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
    run(
        common::tracedecay_command_with_home(home)
            .arg("init")
            .current_dir(project),
        "tracedecay init",
    );
    (commit.clone(), id(&commit))
}

#[test]
fn mounted_fan_out_recovers_then_synthesizes_and_hands_off() {
    let scratch = TempDir::new().expect("advanced workflow isolation");
    let home = scratch.path().join("home");
    let project = scratch.path().join("project");
    let (_commit_text, commit) = initialize_project(&home, &project);
    let project = project.canonicalize().expect("canonical project root");

    let mut daemon = common::spawn_tracedecay_daemon(&home);
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
    daemon = common::spawn_tracedecay_daemon(&home);
    let client = sdk_client(&home, project_id.as_str());
    let observed = client
        .execute::<ApplicationConfigurationObservedState>(&ConfigurationObservedStateRequestV1 {})
        .expect("restarted configuration observed state")
        .result;
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
    let scope = tracedecay_application::ResolvedScope::new(
        project_id.clone(),
        repository_id.clone(),
        worktree_id.clone(),
        None,
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
    let definition = WorkflowDefinition::new(
        definition_id.clone(),
        1,
        project_id.clone(),
        vec![WorkflowStep {
            step_id: step_id.clone(),
            operation: id::<WorkflowOperationRef>("operation.work.attempt_start"),
            predecessors: BTreeSet::new(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("finding")],
            fan_out: Some(WorkflowFanOut { max_width: 3 }),
        }],
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
    client
        .execute::<WorkflowStartRun>(&WorkflowRunStartRequest {
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
                inputs: vec![
                    WorkflowFanOutInput {
                        identity: "fast".to_owned(),
                        input_digest: sha256(b"fast"),
                    },
                    WorkflowFanOutInput {
                        identity: "crash".to_owned(),
                        input_digest: sha256(b"crash"),
                    },
                    WorkflowFanOutInput {
                        identity: "cancel".to_owned(),
                        input_digest: sha256(b"cancel"),
                    },
                ],
            }),
            command_id: id::<WorkCommandId>("command.workflow.start"),
        })
        .expect("mounted workflow fan-out start");

    wait_until(
        "crash-bound provider generation and successful sibling",
        || {
            let attempts = listed_attempts(&client);
            (first_started.exists()
                && attempts
                    .iter()
                    .any(|attempt| attempt.state() == WorkAttemptStateV1::Succeeded))
            .then_some(())
        },
    );
    daemon
        .kill_and_wait()
        .expect("force daemon crash during fan-out");
    std::fs::remove_file(&first_hold).expect("release orphaned first-generation provider");

    let _restarted = common::spawn_tracedecay_daemon(&home);
    let client = sdk_client(&home, project_id.as_str());
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
    let sources: Vec<WorkAttemptIdentityV1> = wait_until("typed fan-out cancellation", || {
        let attempts = listed_attempts(&client);
        let success = attempts
            .iter()
            .find(|attempt| attempt.state() == WorkAttemptStateV1::Succeeded)?;
        let failed = attempts
            .iter()
            .find(|attempt| attempt.state() == WorkAttemptStateV1::Failed)?;
        let cancelled = attempts
            .iter()
            .find(|attempt| attempt.state() == WorkAttemptStateV1::Cancelled)?;
        Some(vec![
            success.identity().clone(),
            failed.identity().clone(),
            cancelled.identity().clone(),
        ])
    });
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
    client
        .execute::<WorkCreate>(&CreateWorkCommand {
            task_id: synthesis_task.clone(),
            title: "Synthesize recovered fan-out evidence".to_owned(),
            dependencies: BTreeSet::new(),
            command_id: id("command.synthesis.create"),
            occurred_at: now(),
        })
        .expect("create synthesis Work");
    let proposal_id: ProposalId = id("proposal.advanced-workflow-synthesis");
    client
        .execute::<WorkAcceptProposal>(&AcceptProposalCommand {
            review: ReviewProposalCommand {
                task_id: synthesis_task.clone(),
                proposal_id: proposal_id.clone(),
                proposal_digest: sha256(b"advanced workflow synthesis proposal"),
                expected_version: WorkVersion::initial(),
                command_id: id("command.synthesis.accept"),
                occurred_at: now(),
            },
        })
        .expect("accept synthesis proposal");
    client
        .execute::<WorkAdmitExecution>(&AdmitExecutionCommand {
            task_id: synthesis_task.clone(),
            expected_version: WorkVersion::new(2).expect("accepted Work version"),
            command_id: id("command.synthesis.admit"),
            occurred_at: now(),
        })
        .expect("admit synthesis Work");
    let synthesis_attempt_id: AttemptId = id("attempt.advanced-workflow-synthesis");
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
    wait_until("synthesis provider completion", || {
        listed_attempts(&client)
            .into_iter()
            .find(|attempt| attempt.identity().attempt_id() == &synthesis_attempt_id)
            .filter(|attempt| attempt.state() == WorkAttemptStateV1::Succeeded)
    });

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
        WorkVersion::new(3).expect("synthesis admission version"),
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
        matches!(replay, ClientError::Problem(problem) if problem.kind == "invalid_request"),
        "handoff replay must be a typed refusal: {replay}"
    );
}
