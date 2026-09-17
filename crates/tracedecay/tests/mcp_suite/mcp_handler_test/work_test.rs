#![cfg(all(feature = "test-transport", unix))]

use crate::support::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
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

async fn call_envelope(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, tool, arguments).await;
    serde_json::from_str(extract_real_server_text(&result))
        .unwrap_or_else(|error| panic!("{tool} returned invalid JSON ({error}): {result}"))
}

async fn call(server: &tracedecay::mcp::McpServer, tool: &str, arguments: Value) -> Value {
    let decoded = call_envelope(server, tool, arguments).await;
    decoded
        .pointer("/value/outcome/value/payload")
        .cloned()
        .unwrap_or(decoded)
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

async fn configure_attempt_provider(production: &ProductionCompositionFixture) {
    let executable_bytes = b"#!/bin/sh\nexit 0\n";
    let isolation_root = production
        .project_root
        .parent()
        .expect("production fixture isolation root");
    let executable_path = isolation_root.join("work-attempt-provider");
    std::fs::write(&executable_path, executable_bytes).expect("write Work provider executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&executable_path)
            .expect("Work provider executable metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable_path, permissions)
            .expect("Work provider executable permissions");
    }
    let executable_path = executable_path
        .canonicalize()
        .expect("canonical Work provider executable");
    let mut hasher = ManifestDigestHasher::new();
    hasher.update(executable_bytes);
    let executable = WorkExecutableReference::new(
        "executable.work.mcp-attempt-provider".to_owned(),
        hasher.finalize().expect("Work provider executable digest"),
    )
    .expect("Work provider executable reference");
    let route = WorkRouteCandidateV1 {
        route_id: "route.work.mcp-attempt-codex.v1".to_owned(),
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
                .expect("Work provider execution limits"),
            maximum_duration_micros: 60_000_000,
            fallback: WorkFallbackTopology::Disabled,
        },
    };
    let binding = WorkExecutableBindingV1::new(
        executable,
        executable_path,
        vec![WorkExecutableCapabilityV1::CodexCliExecJson],
        vec![route],
    )
    .expect("configured Work provider binding");
    let server = production
        .harness
        .server(&production.project_root)
        .expect("production MCP server");
    let expected_revision = production
        .harness
        .configuration_revision(&production.project_root)
        .await
        .expect("fixture configuration revision");
    let configured = call_envelope(
        &server,
        "tracedecay_configuration_set",
        json!({
            "layer": {
                "kind": "project",
                "project_id": production
                    .harness
                    .project_id(&production.project_root)
                    .await
                    .expect("registered fixture project")
            },
            "key": WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
            "value": serde_json::to_value(ConfigurationValueV1::WorkExecutableBindings(vec![
                binding,
            ]))
            .expect("serialize Work provider binding"),
            "expected_revision": expected_revision,
            "idempotency_key": "configuration.idempotency.mcp-attempt-provider",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(
        configured.pointer("/outcome/outcome"),
        Some(&json!("effect")),
        "Work provider configuration must commit: {configured}"
    );
    drop(server);
}

/// A fresh provider attempt has no session association yet. The public Work
/// reads must still project it from the authority that committed the attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn work_attempt_consumers_read_the_public_start_attempt_effect() {
    let production = production_composition_fixture().await;
    let project_root = production.project_root.clone();
    let isolation_root = project_root
        .parent()
        .expect("production fixture isolation root")
        .to_path_buf();
    configure_attempt_provider(&production).await;
    production.harness.shutdown().await;
    let harness = tracedecay::daemon::ProductionProjectCompositionHarnessV1::open(
        &isolation_root,
        [project_root.clone()],
    )
    .await
    .expect("reopen production composition with Work provider");
    let server = harness
        .server(&production.project_root)
        .expect("production MCP server");
    let occurred_at = now_micros();
    let selection = json!({ "selection": "profile_owned_no_git" });

    let prepared_create = call(
        &server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": {
                "change": "create_task",
                "initiative": {
                    "id": "initiative.mcp-attempt-read",
                    "title": "MCP attempt read initiative",
                    "created_at": occurred_at
                },
                "plan": {
                    "id": "plan.mcp-attempt-read",
                    "initiative_id": "initiative.mcp-attempt-read",
                    "title": "MCP attempt read plan",
                    "created_at": occurred_at
                },
                "milestone": {
                    "id": "milestone.mcp-attempt-read",
                    "plan_id": "plan.mcp-attempt-read",
                    "title": "MCP attempt read milestone",
                    "created_at": occurred_at
                },
                "item": {
                    "input": {
                        "task_id": "task.mcp-attempt-read",
                        "hierarchy": {
                            "initiative_id": "initiative.mcp-attempt-read",
                            "plan_id": "plan.mcp-attempt-read",
                            "milestone_id": "milestone.mcp-attempt-read"
                        },
                        "title": "Read a freshly started attempt",
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
        &server,
        "tracedecay_work_create",
        prepared_create["request"].clone(),
    )
    .await;
    assert_eq!(created["replayed"], false, "{created}");

    let generated = call(
        &server,
        "tracedecay_work_generate_proposal",
        json!({
            "selection": selection,
            "task_id": "task.mcp-attempt-read",
            "proposal_id": "proposal.mcp-attempt-read",
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert!(generated["proposal"].is_object(), "{generated}");
    let initial_version = generated["verified_graph_version"].clone();
    let prepared_accept = call(
        &server,
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
    assert_eq!(
        prepared_accept["mutation"], "decide_proposal",
        "{prepared_accept}"
    );
    let accepted = call(
        &server,
        "tracedecay_work_accept_proposal",
        prepared_accept["request"].clone(),
    )
    .await;
    assert_eq!(accepted["replayed"], false, "{accepted}");
    let prepared_admit = call(
        &server,
        "tracedecay_work_prepare_graph_mutation",
        json!({
            "selection": selection,
            "change": {
                "change": "admit_execution",
                "task_id": "task.mcp-attempt-read"
            },
            "evidence": []
        }),
    )
    .await;
    let admitted = call(
        &server,
        "tracedecay_work_admit_execution",
        prepared_admit["request"].clone(),
    )
    .await;
    let admitted_version = admitted["mutation"]["verified_graph_version"].clone();
    assert_eq!(admitted["mutation"]["replayed"], false, "{admitted}");

    let placement_request = json!({
        "task_id": "task.mcp-attempt-read",
        "run_id": "run.mcp-attempt-read",
        "target": {
            "kind": "clean_in_place",
            "root": null,
            "network_free": true,
            "in_place_acknowledged": true
        },
        "occurred_at": now_micros()
    });
    let preflight = call(
        &server,
        "tracedecay_work_placement_preflight",
        placement_request.clone(),
    )
    .await;
    assert_eq!(preflight["blockers"], json!([]), "{preflight}");
    let placed = call(
        &server,
        "tracedecay_work_admit_placement",
        placement_request,
    )
    .await;
    assert_eq!(
        placed["identity"]["run_id"], "run.mcp-attempt-read",
        "{placed}"
    );

    let commit = Command::new(crate::common::git_program())
        .args(["rev-parse", "HEAD"])
        .current_dir(&project_root)
        .output()
        .expect("read fixture commit");
    assert!(commit.status.success(), "git rev-parse must succeed");
    let commit = String::from_utf8(commit.stdout)
        .expect("commit is UTF-8")
        .trim()
        .to_owned();
    let attempt_at = now_micros();
    let start_request = json!({
        "task_id": "task.mcp-attempt-read",
        "run_id": "run.mcp-attempt-read",
        "attempt_id": "attempt.mcp-attempt-read",
        "operation": "operation.work.start_attempt",
        "worktree_root": project_root,
        "commit": commit,
        "instructions": "Observe the fixture only.",
        "effect_state": "observational",
        "occurred_at": attempt_at,
        "execution_snapshot": admitted["execution_snapshot"].clone()
    });
    let _: tracedecay_contracts::StartWorkAttemptCommand =
        serde_json::from_value(start_request.clone()).expect("valid start-attempt request");
    let mut second_start_request = start_request.clone();
    second_start_request["attempt_id"] = json!("attempt.mcp-attempt-read.second");
    let started = call(&server, "tracedecay_work_start_attempt", start_request).await;
    assert_eq!(
        started["identity"]["attempt_id"], "attempt.mcp-attempt-read",
        "{started}"
    );
    let settled = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status = call(
                &server,
                "tracedecay_work_attempt_status",
                json!({
                    "task_id": "task.mcp-attempt-read",
                    "run_id": "run.mcp-attempt-read",
                    "attempt_id": "attempt.mcp-attempt-read"
                }),
            )
            .await;
            match status["state"].as_str() {
                Some("succeeded") => break status,
                Some("leased" | "running") => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                _ => panic!("Work provider must settle successfully: {status}"),
            }
        }
    })
    .await
    .expect("Work provider did not settle within the fixture budget");
    assert_eq!(settled["terminal"]["outcome"], "succeeded", "{settled}");
    let second_started = call(
        &server,
        "tracedecay_work_start_attempt",
        second_start_request,
    )
    .await;
    assert_eq!(
        second_started["identity"]["attempt_id"], "attempt.mcp-attempt-read.second",
        "{second_started}"
    );

    let attempts = call(
        &server,
        "tracedecay_work_list_attempts",
        json!({ "page_size": 50 }),
    )
    .await;
    assert_eq!(attempts["state"], "listed", "{attempts}");
    let listed_attempt = attempts["attempts"]
        .as_array()
        .and_then(|attempts| {
            attempts
                .iter()
                .find(|attempt| attempt["identity"] == started["identity"])
        })
        .expect("started attempt must be listed");
    assert_eq!(listed_attempt["state"], "succeeded", "{attempts}");
    assert_eq!(
        listed_attempt["terminal"]["outcome"], "succeeded",
        "{attempts}"
    );

    let history = call(
        &server,
        "tracedecay_work_execution_history",
        json!({ "page_size": 50 }),
    )
    .await;
    assert_eq!(history["state"], "listed", "{history}");
    assert!(
        history["observed_order"]
            .as_array()
            .is_some_and(|events| events
                .iter()
                .any(|event| event["identity"] == started["identity"])),
        "{history}"
    );

    let topology = call(
        &server,
        "tracedecay_work_topology",
        json!({ "page_size": 50 }),
    )
    .await;
    assert_eq!(topology["state"], "view", "{topology}");
    assert!(
        topology["execution_placement"]["lanes"]
            .as_array()
            .is_some_and(|lanes| lanes.iter().any(|lane| {
                lane["task_id"] == "task.mcp-attempt-read"
                    && lane["run_id"] == "run.mcp-attempt-read"
            })),
        "{topology}"
    );

    let compared = call(
        &server,
        "tracedecay_work_compare_proposal",
        json!({
            "selection": selection,
            "task_id": "task.mcp-attempt-read",
            "old_version": initial_version,
            "new_version": admitted_version,
            "observed_at": now_micros()
        }),
    )
    .await;
    assert_eq!(compared["task_id"], "task.mcp-attempt-read", "{compared}");

    let duplicate = call(
        &server,
        "tracedecay_work_prepare_duplicate_adjudication",
        json!({
            "first_attempt": started["identity"],
            "second_attempt": second_started["identity"],
            "verdict": "not_duplicate",
            "reason": "distinct fixture attempts",
            "quantities": {
                "wall_micros": null,
                "token_count": null,
                "cost_micros": null,
                "test_count": null,
                "effect_count": null,
                "evidence": "owner_receipt",
                "effect_outcome": "not_applicable",
                "coverage": "known"
            }
        }),
    )
    .await;
    assert_eq!(
        duplicate["first_attempt"], started["identity"],
        "{duplicate}"
    );
    assert_eq!(
        duplicate["second_attempt"], second_started["identity"],
        "{duplicate}"
    );
    assert!(
        duplicate["evidence"]["work_generation"].is_string(),
        "{duplicate}"
    );
    assert!(
        duplicate["evidence"]["topology_generation"].is_string(),
        "{duplicate}"
    );
    let adjudicated = call(
        &server,
        "tracedecay_work_adjudicate_duplicate",
        duplicate.clone(),
    )
    .await;
    assert_eq!(
        adjudicated["receipt"]["command"], duplicate,
        "{adjudicated}"
    );

    let experience = call(
        &server,
        "tracedecay_work_experience",
        json!({
            "selection": selection,
            "task_id": "task.mcp-attempt-read",
            "verified_version": admitted_version,
            "expertise_categories": ["testing"],
            "evidence_not_before": occurred_at,
            "observed_at": now_micros(),
            "limit": 10
        }),
    )
    .await;
    assert_ne!(
        experience.pointer("/value/problem/code"),
        Some(&json!("not_found_or_not_authorized")),
        "experience must not require a task-session association: {experience}"
    );

    let placement = call(
        &server,
        "tracedecay_work_placement_status",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read"
        }),
    )
    .await;
    assert_ne!(placement["state"], "absent", "{placement}");

    let cancelled = call(
        &server,
        "tracedecay_work_cancel_attempt",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read",
            "attempt_id": "attempt.mcp-attempt-read",
            "request_id": "cancel.mcp-attempt-read",
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert_ne!(
        cancelled.pointer("/value/problem/code"),
        Some(&json!("not_found_or_not_authorized")),
        "the attempt consumer must find the fresh attempt even if its provider already settled: {cancelled}"
    );

    let paused = call(
        &server,
        "tracedecay_work_pause_run",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read",
            "reason": "operator_request",
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert_eq!(paused["state"], "paused", "{paused}");
    let resumed = call(
        &server,
        "tracedecay_work_resume_run",
        json!({
            "task_id": "task.mcp-attempt-read",
            "run_id": "run.mcp-attempt-read",
            "reason": "operator_request",
            "expected_authority_version": paused["authority"],
            "occurred_at": now_micros()
        }),
    )
    .await;
    assert_eq!(resumed["state"], "running", "{resumed}");
}
