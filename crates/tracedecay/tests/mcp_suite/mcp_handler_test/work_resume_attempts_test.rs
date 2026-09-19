#![cfg(all(feature = "test-transport", unix))]

//! `tracedecay_work_resume_attempts` through the production MCP server.
//!
//! Restart recovery fences durable open attempts only after this daemon's
//! process is gone. A live provider holder is a conflict, and a settled
//! attempt is left sealed. Shutdown would run the cancellation ladder and
//! settle the child recovery is supposed to find still open, so the live
//! daemon is a separate process stopped with SIGKILL.

use crate::fixture;
use crate::support::{extract_real_server_text, handle_real_server_tool_call, test_temp_dir};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay::mcp::McpServer;
use tracedecay_domain::configuration::{
    ConfigurationValueV1, WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1,
    WorkExecutableCapabilityV1,
};
use tracedecay_domain::{
    ManifestDigestHasher, WorkApprovalPolicy, WorkContentLocationClassV1, WorkEffortClassV1,
    WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits, WorkFallbackTopology,
    WorkFilesystemPolicy, WorkOrdinalBandV1, WorkProviderBackendV1, WorkRouteCandidateV1,
    WorkRouteExecutionProfileV1, WorkSandboxPolicy,
};

const TASK_ID: &str = "task.resume-attempts";
const RUN_ID: &str = "run.resume-attempts";
const SETTLED_ATTEMPT_ID: &str = "attempt.resume-attempts.settled";
const HELD_ATTEMPT_ID: &str = "attempt.resume-attempts.held";
const LIVE_HOLDER_CODE: &str = "application.work-attempt.live-holder";
const LIVE_HOLDER_MESSAGE: &str =
    "Work attempt recovery requires the current worktree to have no live provider holder.";
const CRASH_CHILD_ENV: &str = "TRACEDECAY_WORK_RESUME_ATTEMPTS_CHILD";
const CRASH_ROOT_ENV: &str = "TRACEDECAY_WORK_RESUME_ATTEMPTS_ROOT";
const CRASH_READY_FILE: &str = "resume-attempts-ready";
const CRASH_EVIDENCE_FILE: &str = "resume-attempts-evidence.json";
const CRASH_LOG_FILE: &str = "resume-attempts-child.log";

#[derive(Debug, Serialize, Deserialize)]
struct LiveDaemonEvidence {
    empty_report: Value,
    missing_field: Value,
    unknown_field: Value,
    live_holder: Value,
    held_status: Value,
    settled_status: Value,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_attempts_refuses_a_live_holder_and_reports_the_lost_attempt() {
    if std::env::var_os(CRASH_CHILD_ENV).is_some() {
        hold_provider_until_killed().await;
        return;
    }

    let isolation = test_temp_dir();
    let project_root = isolation.path().join("project");
    seed_project(&project_root);
    let evidence = kill_live_daemon(isolation.path()).await;

    assert_eq!(
        evidence.empty_report,
        json!({
            "recovery_required": [],
            "cancelled": []
        }),
        "an authority with no open attempts returns an empty recovery report"
    );
    assert_invalid_request(&evidence.missing_field);
    assert_invalid_request(&evidence.unknown_field);
    let refused = problem(&evidence.live_holder);
    assert_eq!(refused["kind"], "conflict");
    assert_eq!(refused["code"], LIVE_HOLDER_CODE);
    assert_eq!(refused["message"], LIVE_HOLDER_MESSAGE);
    assert_eq!(refused["owning_layer"], "application");
    assert_eq!(refused["terminality"], "pre_admission");
    assert_eq!(refused["retryable"], true);
    assert_eq!(refused["retry"], "after_revalidate");
    assert_eq!(refused["retry_scope"], "fresh_request");
    assert_eq!(refused["legal_actions"], json!(["refresh"]));
    assert_eq!(refused["committed_receipt"], Value::Null);
    assert_eq!(refused["details"], json!([]));
    assert_eq!(
        refused["diagnostic"],
        json!({
            "code": LIVE_HOLDER_CODE,
            "message": LIVE_HOLDER_MESSAGE
        })
    );
    assert_eq!(evidence.held_status["state"], "running");
    assert_eq!(
        evidence.held_status["identity"],
        json!({
            "task_id": TASK_ID,
            "run_id": RUN_ID,
            "attempt_id": HELD_ATTEMPT_ID
        })
    );
    assert_eq!(evidence.held_status["terminal"], Value::Null);
    assert_eq!(evidence.settled_status["state"], "succeeded");
    assert_eq!(
        evidence.settled_status["identity"]["attempt_id"],
        SETTLED_ATTEMPT_ID
    );
    assert_eq!(evidence.settled_status["terminal"]["outcome"], "succeeded");

    let harness =
        ProductionProjectCompositionHarnessV1::open(isolation.path(), [project_root.clone()])
            .await
            .expect("restart production composition after the provider holder was lost");
    let server = harness
        .server(&project_root)
        .expect("restarted production MCP server");
    let before = wait_for_lost_attempt_state(&server).await;
    let held_epoch = evidence.held_status["lease"]["epoch"]
        .as_u64()
        .expect("pre-crash lease epoch");
    assert_eq!(
        before["lease"]["lease_id"], evidence.held_status["lease"]["lease_id"],
        "restart must keep the lost attempt's lease identity: {before}"
    );

    let occurred_at = now_micros();
    let report = call(
        &server,
        "tracedecay_work_resume_attempts",
        json!({ "occurred_at": occurred_at }),
    )
    .await;
    assert_eq!(report["cancelled"], json!([]), "{report}");
    let fenced = only_recovery(&report);
    assert_eq!(
        fenced["identity"],
        json!({
            "task_id": TASK_ID,
            "run_id": RUN_ID,
            "attempt_id": HELD_ATTEMPT_ID
        }),
        "{report}"
    );
    assert_eq!(fenced["state"], "recovery_required", "{report}");
    assert_eq!(fenced["terminal"], Value::Null, "{report}");
    assert_eq!(fenced["progress"], Value::Null, "{report}");
    assert_eq!(fenced["artifacts"], json!([]), "{report}");
    assert_eq!(
        fenced["cancellation"],
        json!({ "state": "none" }),
        "{report}"
    );
    assert_eq!(
        fenced["actual_route"]["route_id"], "route.work.resume-attempts-codex.v1",
        "{report}"
    );
    assert_eq!(
        fenced["requested_route"]["route_id"], "route.work.resume-attempts-codex.v1",
        "{report}"
    );
    assert_eq!(fenced["lease"]["lease_id"], before["lease"]["lease_id"]);
    assert_eq!(fenced["lease"]["epoch"], held_epoch + 1, "{report}");
    let observed_at = if before["state"] == "running" {
        occurred_at
    } else {
        assert_eq!(before["state"], "recovery_required", "{before}");
        before["recovery"]["observed_at"]
            .as_i64()
            .expect("startup fence observation")
    };
    assert_eq!(
        fenced["recovery"],
        json!({
            "state": "recovery_required",
            "source_attempt_id": null,
            "reason": "process_lost",
            "observed_at": observed_at
        }),
        "{report}"
    );

    let later = now_micros();
    assert_ne!(later, occurred_at);
    let again = call(
        &server,
        "tracedecay_work_resume_attempts",
        json!({ "occurred_at": later }),
    )
    .await;
    assert_eq!(again["cancelled"], json!([]), "{again}");
    let repeated = only_recovery(&again);
    assert_eq!(repeated["lease"], fenced["lease"], "{again}");
    assert_eq!(repeated["recovery"], fenced["recovery"], "{again}");
    assert_eq!(repeated["state"], "recovery_required", "{again}");

    let held_after = call(
        &server,
        "tracedecay_work_attempt_status",
        json!({
            "task_id": TASK_ID,
            "run_id": RUN_ID,
            "attempt_id": HELD_ATTEMPT_ID
        }),
    )
    .await;
    assert_eq!(held_after["state"], "recovery_required", "{held_after}");
    assert_eq!(held_after["lease"], fenced["lease"], "{held_after}");
    assert_eq!(held_after["recovery"], fenced["recovery"], "{held_after}");
    assert_eq!(held_after["terminal"], Value::Null, "{held_after}");

    let settled_after = call(
        &server,
        "tracedecay_work_attempt_status",
        json!({
            "task_id": TASK_ID,
            "run_id": RUN_ID,
            "attempt_id": SETTLED_ATTEMPT_ID
        }),
    )
    .await;
    assert_eq!(settled_after["state"], "succeeded", "{settled_after}");
    assert_eq!(
        settled_after["terminal"], evidence.settled_status["terminal"],
        "resume must not rewrite a sealed receipt: {settled_after}"
    );

    drop(server);
    harness.shutdown().await;
}

fn assert_invalid_request(envelope: &Value) {
    let refused = problem(envelope);
    assert_eq!(refused["kind"], "invalid_request", "{envelope}");
    assert_eq!(refused["code"], "work.invalid_request", "{envelope}");
    assert_eq!(
        refused["message"], "The Work application request is invalid",
        "{envelope}"
    );
    assert_eq!(refused["owning_layer"], "adapter", "{envelope}");
    assert_eq!(refused["retry"], "never", "{envelope}");
    assert_eq!(refused["retryable"], false, "{envelope}");
    assert_eq!(refused["legal_actions"], json!([]), "{envelope}");
    assert_eq!(
        refused["diagnostic"],
        json!({
            "code": "work.invalid_request",
            "message": "The Work application request is invalid"
        }),
        "{envelope}"
    );
}

fn problem(envelope: &Value) -> &Value {
    assert_eq!(envelope["kind"], "problem", "{envelope}");
    envelope
        .pointer("/value/problem")
        .unwrap_or_else(|| panic!("problem envelope missing its record: {envelope}"))
}

fn only_recovery(report: &Value) -> &Value {
    let required = report["recovery_required"]
        .as_array()
        .unwrap_or_else(|| panic!("recovery report missing recovery_required: {report}"));
    assert_eq!(required.len(), 1, "{report}");
    &required[0]
}

async fn kill_live_daemon(isolation: &Path) -> LiveDaemonEvidence {
    let log_path = isolation.join(CRASH_LOG_FILE);
    let log_file = std::fs::File::create(&log_path).expect("child log");
    let filter = format!(
        "{}::resume_attempts_refuses_a_live_holder_and_reports_the_lost_attempt",
        module_path!()
            .strip_prefix("mcp_suite::")
            .unwrap_or(module_path!())
    );
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .arg(&filter)
        .arg("--exact")
        .arg("--nocapture")
        .env(CRASH_CHILD_ENV, "1")
        .env(CRASH_ROOT_ENV, isolation)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone().expect("clone child log")))
        .stderr(Stdio::from(log_file))
        .process_group(0)
        .spawn()
        .expect("spawn the daemon that will lose its provider holder");
    let ready = isolation.join(CRASH_READY_FILE);
    let started = Instant::now();
    while !ready.is_file() {
        if let Some(status) = child.try_wait().expect("poll crash child") {
            panic!(
                "crash child exited before the provider was held ({status}) filter={filter}: {}",
                child_log(&log_path)
            );
        }
        if started.elapsed() > Duration::from_secs(90) {
            let _ = kill_process_group(child.id());
            let _ = child.wait();
            panic!(
                "crash child did not hold the provider within 90s: {}",
                child_log(&log_path)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        kill_process_group(child.id()),
        0,
        "SIGKILL must reach the live daemon before recovery"
    );
    let status = child.wait().expect("wait for the killed daemon");
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the live daemon must die by SIGKILL so shutdown cannot settle the open attempt: {status}; {}",
        child_log(&log_path)
    );
    let bytes = std::fs::read(isolation.join(CRASH_EVIDENCE_FILE)).unwrap_or_else(|error| {
        panic!(
            "lost the live daemon's MCP evidence ({error}): {}",
            child_log(&log_path)
        )
    });
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "live daemon MCP evidence was not JSON ({error}): {}",
            child_log(&log_path)
        )
    })
}

async fn hold_provider_until_killed() {
    let isolation = PathBuf::from(std::env::var_os(CRASH_ROOT_ENV).unwrap_or_else(|| {
        panic!("{CRASH_ROOT_ENV} must name the isolation root the parent will reopen")
    }));
    let project_root = isolation.join("project");
    let (harness, evidence) = drive_live_daemon(isolation.clone(), project_root).await;
    let evidence_path = isolation.join(CRASH_EVIDENCE_FILE);
    let mut evidence_file = std::fs::File::create(&evidence_path).expect("evidence file");
    evidence_file
        .write_all(&serde_json::to_vec(&evidence).expect("serialize MCP evidence"))
        .expect("write MCP evidence");
    evidence_file.sync_all().expect("sync MCP evidence");
    drop(evidence_file);
    std::fs::write(isolation.join(CRASH_READY_FILE), b"held").expect("ready file");
    // The parent SIGKILLs this process. Dropping the harness here would shut
    // the daemon down and settle the attempt recovery has to find still open.
    std::mem::forget(harness);
    std::future::pending::<()>().await;
}

fn kill_process_group(pid: u32) -> i32 {
    let pgid = i32::try_from(pid).expect("process id fits SIGKILL");
    // The child is the leader of its own group, including the provider it
    // spawned. A negative id is the process-group form of kill(2).
    let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if result == 0 {
        return 0;
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return 0;
    }
    panic!("could not SIGKILL process group {pgid}: {error}");
}

fn child_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| format!("child log unreadable: {error}"))
}

async fn drive_live_daemon(
    isolation: PathBuf,
    project_root: PathBuf,
) -> (ProductionProjectCompositionHarnessV1, LiveDaemonEvidence) {
    let harness =
        ProductionProjectCompositionHarnessV1::open(isolation.clone(), [project_root.clone()])
            .await
            .expect("production composition");
    configure_hold_provider(&harness, &project_root, &isolation).await;
    harness.shutdown().await;
    let harness = ProductionProjectCompositionHarnessV1::open(isolation, [project_root.clone()])
        .await
        .expect("reopen production composition with the hold provider");
    let server = harness
        .server(&project_root)
        .expect("production MCP server");

    let missing_field = call_raw(&server, "tracedecay_work_resume_attempts", json!({})).await;
    let unknown_field = call_raw(
        &server,
        "tracedecay_work_resume_attempts",
        json!({ "occurred_at": now_micros(), "replay": true }),
    )
    .await;
    let empty_report = call(
        &server,
        "tracedecay_work_resume_attempts",
        json!({ "occurred_at": now_micros() }),
    )
    .await;

    let snapshot = admit_placed_run(&server, &project_root).await;
    let settled = start_attempt(
        &server,
        &project_root,
        &snapshot,
        SETTLED_ATTEMPT_ID,
        "Observe the fixture only.",
    )
    .await;
    let settled_status = wait_until(&server, SETTLED_ATTEMPT_ID, "succeeded").await;
    assert_eq!(
        settled_status["identity"]["attempt_id"],
        settled["identity"]["attempt_id"]
    );
    let held = start_attempt(
        &server,
        &project_root,
        &snapshot,
        HELD_ATTEMPT_ID,
        "resume-hold the provider until the daemon is lost.",
    )
    .await;
    let held_status = wait_until(&server, HELD_ATTEMPT_ID, "running").await;
    assert_eq!(held_status["identity"], held["identity"]);

    let live_holder = call_raw(
        &server,
        "tracedecay_work_resume_attempts",
        json!({ "occurred_at": now_micros() }),
    )
    .await;
    let held_after_refusal = call(
        &server,
        "tracedecay_work_attempt_status",
        attempt_status_args(HELD_ATTEMPT_ID),
    )
    .await;
    assert_eq!(
        held_after_refusal["state"], "running",
        "{held_after_refusal}"
    );
    assert_eq!(held_after_refusal["lease"], held_status["lease"]);

    drop(server);
    (
        harness,
        LiveDaemonEvidence {
            empty_report,
            missing_field,
            unknown_field,
            live_holder,
            held_status: held_after_refusal,
            settled_status,
        },
    )
}

async fn wait_for_lost_attempt_state(server: &McpServer) -> Value {
    let mut last = Value::Null;
    for _ in 0..40 {
        last = call(
            server,
            "tracedecay_work_attempt_status",
            attempt_status_args(HELD_ATTEMPT_ID),
        )
        .await;
        match last["state"].as_str() {
            Some("recovery_required" | "running") => return last,
            Some("leased") => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            _ => break,
        }
    }
    panic!("lost attempt was not left running or fenced: {last}");
}

fn attempt_status_args(attempt_id: &str) -> Value {
    json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "attempt_id": attempt_id
    })
}

async fn wait_until(server: &McpServer, attempt_id: &str, expected: &str) -> Value {
    let status = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let status = call(
                server,
                "tracedecay_work_attempt_status",
                attempt_status_args(attempt_id),
            )
            .await;
            match status["state"].as_str() {
                Some(state) if state == expected => break status,
                Some("leased" | "running") => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                _ => panic!("{attempt_id} reached an unexpected state: {status}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{attempt_id} did not reach {expected}"));
    status
}

async fn admit_placed_run(server: &McpServer, project_root: &Path) -> Value {
    let occurred_at = now_micros();
    let selection = json!({ "selection": "profile_owned_no_git" });
    let prepared_create = call(
        server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": {
                "change": "create_task",
                "initiative": {
                    "id": "initiative.resume-attempts",
                    "title": "Resume attempts",
                    "created_at": occurred_at
                },
                "plan": {
                    "id": "plan.resume-attempts",
                    "initiative_id": "initiative.resume-attempts",
                    "title": "Resume attempts",
                    "created_at": occurred_at
                },
                "milestone": {
                    "id": "milestone.resume-attempts",
                    "plan_id": "plan.resume-attempts",
                    "title": "Resume attempts",
                    "created_at": occurred_at
                },
                "item": {
                    "input": {
                        "task_id": TASK_ID,
                        "hierarchy": {
                            "initiative_id": "initiative.resume-attempts",
                            "plan_id": "plan.resume-attempts",
                            "milestone_id": "milestone.resume-attempts"
                        },
                        "title": "Resume lost attempts",
                        "dependencies": [],
                        "informational_relations": [],
                        "causal_candidates": [],
                        "acceptance_criteria": [],
                        "effort": 1,
                        "scheduled_at": null,
                        "deadline": null,
                        "created_at": occurred_at,
                        "updated_at": occurred_at
                    },
                    "accepted_proposal": null,
                    "accepted_route": null,
                    "execution_admitted_at": null,
                    "accepted_attempts": [],
                    "accepted_criteria": {},
                    "accepted_at": null,
                    "archived_at": null,
                    "evidence_links": [],
                    "handoffs": []
                }
            },
            "evidence": []
        }),
    )
    .await;
    let created = call(
        server,
        "tracedecay_work_create",
        prepared_create["request"].clone(),
    )
    .await;
    assert_eq!(created["replayed"], false, "{created}");

    let generated = call(
        server,
        "tracedecay_work_generate_proposal",
        json!({
            "selection": selection,
            "task_id": TASK_ID,
            "proposal_id": "proposal.resume-attempts",
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert!(generated["proposal"].is_object(), "{generated}");
    let prepared_accept = call(
        server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": {
                "change": "decide_proposal",
                "proposal": generated["proposal"].clone(),
                "disposition": "accepted"
            },
            "evidence": []
        }),
    )
    .await;
    let accepted = call(
        server,
        "tracedecay_work_accept_proposal",
        prepared_accept["request"].clone(),
    )
    .await;
    assert_eq!(accepted["replayed"], false, "{accepted}");

    let prepared_admit = call(
        server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": { "change": "admit_execution", "task_id": TASK_ID },
            "evidence": []
        }),
    )
    .await;
    let admitted = call(
        server,
        "tracedecay_work_admit_execution",
        prepared_admit["request"].clone(),
    )
    .await;
    assert_eq!(admitted["mutation"]["replayed"], false, "{admitted}");

    let placement_request = json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "target": {
            "kind": "clean_in_place",
            "root": null,
            "network_free": true,
            "in_place_acknowledged": true
        },
        "occurred_at": now_micros()
    });
    let preflight = call(
        server,
        "tracedecay_work_placement_preflight",
        placement_request.clone(),
    )
    .await;
    assert_eq!(preflight["blockers"], json!([]), "{preflight}");
    let placed = call(server, "tracedecay_work_admit_placement", placement_request).await;
    assert_eq!(placed["identity"]["run_id"], RUN_ID, "{placed}");

    let _commit = fixture_commit(project_root);
    admitted["execution_snapshot"].clone()
}

async fn start_attempt(
    server: &McpServer,
    project_root: &Path,
    snapshot: &Value,
    attempt_id: &str,
    instructions: &str,
) -> Value {
    call(
        server,
        "tracedecay_work_start_attempt",
        json!({
            "task_id": TASK_ID,
            "run_id": RUN_ID,
            "attempt_id": attempt_id,
            "operation": "operation.work.start_attempt",
            "worktree_root": project_root,
            "commit": fixture_commit(project_root),
            "instructions": instructions,
            "effect_state": "observational",
            "occurred_at": now_micros(),
            "execution_snapshot": snapshot
        }),
    )
    .await
}

fn fixture_commit(project_root: &Path) -> String {
    let commit = Command::new(crate::common::git_program())
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .expect("read fixture commit");
    assert!(commit.status.success(), "git rev-parse must succeed");
    String::from_utf8(commit.stdout)
        .expect("commit is UTF-8")
        .trim()
        .to_owned()
}

async fn configure_hold_provider(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    isolation_root: &Path,
) {
    let script = b"#!/bin/sh\ninput=$(cat)\ncase \"$input\" in\n  *resume-hold*)\n    while :; do sleep 30; done;;\nesac\nprintf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\"}'\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}'\nexit 0\n";
    let executable_path = isolation_root.join("work-resume-provider");
    std::fs::write(&executable_path, script).expect("write hold provider");
    let mut permissions = std::fs::metadata(&executable_path)
        .expect("provider metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable_path, permissions).expect("provider mode");
    let executable_path = executable_path
        .canonicalize()
        .expect("canonical hold provider");
    let mut hasher = ManifestDigestHasher::new();
    hasher.update(script);
    let executable = WorkExecutableReference::new(
        "executable.work.resume-attempts-provider".to_owned(),
        hasher.finalize().expect("provider digest"),
    )
    .expect("provider reference");
    let route = WorkRouteCandidateV1 {
        route_id: "route.work.resume-attempts-codex.v1".to_owned(),
        provider_capability_id: WorkProviderBackendV1::CodexCli
            .provider_id()
            .as_str()
            .to_owned(),
        model_id: "gpt-5.6-sol".to_owned(),
        effort: WorkEffortClassV1::Standard,
        declared_budget_ceiling: 1,
        content_location: WorkContentLocationClassV1::Local,
        correctness: WorkOrdinalBandV1::High,
        sensitive_data_fitness: WorkOrdinalBandV1::High,
        latency: WorkOrdinalBandV1::Moderate,
        cost: WorkOrdinalBandV1::Moderate,
        autonomy: WorkOrdinalBandV1::High,
        evidence_quality: WorkOrdinalBandV1::High,
        execution: WorkRouteExecutionProfileV1 {
            sandbox: WorkSandboxPolicy::Required,
            approval: WorkApprovalPolicy::Never,
            filesystem: WorkFilesystemPolicy::WorkspaceWrite,
            egress: WorkEgressPolicy::Deny,
            environment_allowlist: BTreeSet::new(),
            credential_references: BTreeSet::new(),
            limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1)
                .expect("provider limits"),
            maximum_duration_micros: 300_000_000,
            fallback: WorkFallbackTopology::Disabled,
        },
    };
    let binding = WorkExecutableBindingV1::new(
        executable,
        executable_path,
        vec![WorkExecutableCapabilityV1::CodexCliExecJson],
        vec![route],
    )
    .expect("provider binding");
    let server = harness.server(project_root).expect("configuration server");
    let expected_revision = harness
        .configuration_revision(project_root)
        .await
        .expect("configuration revision");
    let configured = call_raw(
        &server,
        "tracedecay_configuration_set",
        json!({
            "layer": {
                "kind": "project",
                "project_id": harness.project_id(project_root).await.expect("project id")
            },
            "key": WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
            "value": serde_json::to_value(ConfigurationValueV1::WorkExecutableBindings(vec![binding]))
                .expect("serialize provider binding"),
            "expected_revision": expected_revision,
            "idempotency_key": "configuration.idempotency.resume-attempts",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        configured.pointer("/outcome/outcome"),
        Some(&json!("effect")),
        "hold provider configuration must commit: {configured}"
    );
}

fn seed_project(project_root: &Path) {
    std::fs::create_dir_all(project_root).expect("project root");
    fixture::write_indexed_fixture_sources(project_root);
    let git = crate::common::git_program();
    assert!(
        Command::new(&git)
            .args(["init", "-q"])
            .current_dir(project_root)
            .status()
            .expect("git init")
            .success()
    );
    assert!(
        Command::new(&git)
            .args(["add", "."])
            .current_dir(project_root)
            .status()
            .expect("git add")
            .success()
    );
    assert!(
        Command::new(git)
            .args([
                "-c",
                "user.name=TraceDecay Test",
                "-c",
                "user.email=tracedecay@example.invalid",
                "commit",
                "-qm",
                "resume attempts fixture",
            ])
            .current_dir(project_root)
            .status()
            .expect("git commit")
            .success()
    );
}

fn now_micros() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_micros(),
    )
    .expect("current time fits UtcMicros")
}

async fn call(server: &McpServer, tool: &str, arguments: Value) -> Value {
    let decoded = call_raw(server, tool, arguments).await;
    decoded
        .pointer("/value/outcome/value/payload")
        .cloned()
        .unwrap_or(decoded)
}

async fn call_raw(server: &McpServer, tool: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"))
}
