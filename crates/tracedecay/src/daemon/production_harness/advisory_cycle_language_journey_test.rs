//! The advisory cycle that admits pull requests mounts on whatever languages
//! a checkout indexes. A TypeScript-only repository has a sealed generation
//! like any other, so the explicit cycle must run over it instead of staying
//! "not mounted yet" for the daemon's whole life.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::journey_test_support::tool_answer;
use super::*;
use crate::test_support::git::git;

const WARMING_CODE: &str = "feedback.advisory-cycle.unavailable";

/// Calls the explicit advisory cycle until it answers with anything other
/// than the retryable pre-mount state, or the bound expires.
async fn settled_advisory_cycle(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    document: &str,
) -> (bool, Value) {
    let document_uri = url::Url::from_file_path(project.join(document))
        .expect("document uri")
        .to_string();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let response = harness
            .call_tool(
                project,
                "tracedecay_feedback_advisory_cycle",
                json!({"document_uri": document_uri, "format": "json"}),
            )
            .await
            .expect("advisory cycle call");
        let (refused, answer) = tool_answer(&response);
        if !refused || answer["problem"]["code"] != json!(WARMING_CODE) {
            return (refused, answer);
        }
        assert!(
            Instant::now() < deadline,
            "the advisory cycle stayed in its pre-mount state: {answer}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn typescript_only_checkout_runs_the_pull_request_advisory_cycle() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source dir");
    std::fs::write(
        project.join("src/index.ts"),
        "export function admitPullRequest(number: number): string {\n  return `pr-${number}`;\n}\n",
    )
    .expect("typescript source");
    git(&project, &["init", "--quiet", "-b", "feature/ts-admission"]);
    git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "https://git.example.invalid/tracedecay-fixture/typescript-admission.git",
        ],
    );
    git(&project, &["add", "."]);
    git(
        &project,
        &["commit", "--quiet", "-m", "seed typescript admission"],
    );

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition opens a TypeScript-only checkout");

    let (refused, payload) = settled_advisory_cycle(&harness, &project, "src/index.ts").await;
    assert!(
        !refused,
        "the cycle must run over the TypeScript generation: {payload}"
    );
    assert_eq!(
        payload["outcome"]["outcome"],
        json!("evidence"),
        "{payload}"
    );
    let cycle = &payload["outcome"]["value"]["payload"]["cycle"];
    let producers: Vec<&Value> = cycle["advisory_provider_states"]
        .as_array()
        .unwrap_or_else(|| panic!("advisory provider states: {payload}"))
        .iter()
        .map(|state| &state["producer"])
        .collect();
    assert_eq!(
        producers,
        [
            &json!("git_hub_review"),
            &json!("ci_localization"),
            &json!("proximity")
        ],
        "{payload}"
    );
    harness.shutdown().await;
}

/// A reopened project already holds a ready sealed generation, so the first
/// advisory-cycle call after the reopen answers instead of the retryable
/// pre-mount state a second call would get past.
#[tokio::test(flavor = "multi_thread")]
async fn first_advisory_cycle_after_a_reopen_answers_without_a_retry() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source dir");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn reopened_cycle(value: u32) -> u32 {\n    value + 1\n}\n",
    )
    .expect("rust source");
    git(&project, &["init", "--quiet", "-b", "feature/reopen"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &["commit", "--quiet", "-m", "seed reopened cycle"],
    );

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("first production composition");
    let (refused, settled) = settled_advisory_cycle(&harness, &project, "src/lib.rs").await;
    assert!(!refused, "the first open must mount the cycle: {settled}");
    harness.shutdown().await;

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("reopened production composition");
    let document_uri = url::Url::from_file_path(project.join("src/lib.rs"))
        .expect("document uri")
        .to_string();
    let response = harness
        .call_tool(
            &project,
            "tracedecay_feedback_advisory_cycle",
            json!({"document_uri": document_uri, "format": "json"}),
        )
        .await
        .expect("advisory cycle call");
    let (refused, answer) = tool_answer(&response);
    assert!(
        !refused,
        "the first call after a reopen was refused: {answer}"
    );
    assert_eq!(answer["outcome"]["outcome"], json!("evidence"), "{answer}");
    harness.shutdown().await;
}

/// A reopened checkout whose deferred advisory mount fails terminally names
/// that failure as soon as the mount gives up, instead of holding the request
/// to its deadline and then answering the retryable pre-mount state.
#[tokio::test(flavor = "multi_thread")]
async fn reopened_checkout_whose_advisory_mount_failed_names_the_failure() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source dir");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn failed_mount(value: u32) -> u32 {\n    value * 2\n}\n",
    )
    .expect("rust source");
    git(&project, &["init", "--quiet", "-b", "feature/failed-mount"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "--quiet", "-m", "seed failed mount"]);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("first production composition");
    let (refused, settled) = settled_advisory_cycle(&harness, &project, "src/lib.rs").await;
    assert!(!refused, "the first open must mount the cycle: {settled}");
    let project_id = tracedecay_domain::ProjectId::new(
        harness
            .project_id(&project)
            .await
            .expect("registered project id"),
    )
    .expect("project id");
    harness.shutdown().await;

    // A foreign live hook-notice queue for this checkout refuses the reopened
    // advisory owner its registration on every attempt.
    let canonical_project = std::fs::canonicalize(&project).expect("canonical project");
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(&canonical_project, &project_id)
            .expect("resolved scope");
    let (hook_project, hook_worktree) = tracedecay_agent_hosts::hooks::hook_scope_locators(&scope);
    let foreign = tracedecay_application::advisory::AdvisoryHookNoticeQueueV1::new(
        tracedecay_application::feedback::resolve_project_feedback_scope_v1(
            &canonical_project,
            &scope,
        )
        .expect("feedback scope"),
    );
    assert!(
        tracedecay_application::advisory::register_advisory_hook_notice_queue(
            hook_project,
            hook_worktree,
            &foreign,
        )
    );

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("reopened production composition");
    let document_uri = url::Url::from_file_path(project.join("src/lib.rs"))
        .expect("document uri")
        .to_string();
    let request_deadline = tracedecay_mcp::tools::binding::canonical_tool_dispatch_ceiling(
        "tracedecay_feedback_advisory_cycle",
    )
    .expect("advisory cycle dispatch deadline");
    let started = Instant::now();
    let response = harness
        .call_tool(
            &project,
            "tracedecay_feedback_advisory_cycle",
            json!({"document_uri": document_uri, "format": "json"}),
        )
        .await
        .expect("advisory cycle call");
    let elapsed = started.elapsed();
    let (refused, answer) = tool_answer(&response);
    assert!(refused, "a failed mount cannot run the cycle: {answer}");
    let problem = &answer["problem"];
    assert_eq!(
        problem["code"],
        json!("feedback.advisory-cycle.mount-failed"),
        "{answer}"
    );
    assert_eq!(problem["kind"], json!("unavailable"), "{answer}");
    assert_eq!(
        problem["legal_actions"],
        json!(["contact_administrator"]),
        "{answer}"
    );
    assert!(
        elapsed < request_deadline / 2,
        "the failure was held for {elapsed:?} of a {request_deadline:?} deadline: {answer}"
    );
    harness.shutdown().await;
    assert!(
        tracedecay_application::advisory::unregister_advisory_hook_notice_queue(
            hook_project,
            hook_worktree,
            &foreign,
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn checkout_without_indexable_source_names_why_the_advisory_cycle_cannot_run() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(&project).expect("project root");
    std::fs::write(project.join("LICENSE"), "MIT License\n").expect("license");
    git(&project, &["init", "--quiet", "-b", "main"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "--quiet", "-m", "seed license only"]);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition opens a checkout without indexable source");

    let (refused, answer) = settled_advisory_cycle(&harness, &project, "LICENSE").await;
    assert!(
        refused,
        "no generation exists to run a cycle over: {answer}"
    );
    let problem = &answer["problem"];
    assert_eq!(problem["kind"], json!("unsupported"), "{answer}");
    assert_eq!(
        problem["code"],
        json!("feedback.advisory-cycle.no-indexable-source"),
        "{answer}"
    );
    assert_eq!(problem["legal_actions"], json!(["reconcile"]), "{answer}");
    harness.shutdown().await;
}

/// A checkout with one Rust file and one file no analyzer supports, opened
/// and settled until the advisory owner has mounted over its generation.
async fn settled_checkout_with_language_less_file(
    isolation: &Path,
) -> (ProductionProjectCompositionHarnessV1, std::path::PathBuf) {
    let project = isolation.join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source dir");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn analyzable(value: u32) -> u32 {\n    value + 3\n}\n",
    )
    .expect("rust source");
    std::fs::write(project.join("deploy.conf"), "listen = 8080\n").expect("config file");
    git(&project, &["init", "--quiet", "-b", "main"]);
    git(&project, &["add", "."]);
    git(
        &project,
        &["commit", "--quiet", "-m", "seed mixed checkout"],
    );
    let harness = ProductionProjectCompositionHarnessV1::open(isolation, [project.clone()])
        .await
        .expect("production composition");
    let (refused, settled) = settled_advisory_cycle(&harness, &project, "src/lib.rs").await;
    assert!(
        !refused,
        "the Rust document must mount the cycle: {settled}"
    );
    (harness, project)
}

#[tokio::test(flavor = "multi_thread")]
async fn language_less_document_is_a_terminal_advisory_request() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let (harness, project) = settled_checkout_with_language_less_file(isolation.path()).await;

    let (refused, answer) = settled_advisory_cycle(&harness, &project, "deploy.conf").await;
    assert!(refused, "no analyzer can run over deploy.conf: {answer}");
    let problem = &answer["problem"];
    assert_eq!(problem["kind"], json!("invalid_request"), "{answer}");
    assert_eq!(
        problem["code"],
        json!("feedback.advisory-cycle.invalid-request"),
        "{answer}"
    );
    assert_eq!(problem["retryable"], json!(false), "{answer}");
    harness.shutdown().await;
}

/// Admits one native hook boundary for `project` through the production hook
/// orchestrator and reports whether its producer work reached a terminal state.
/// The completion is what clears the durable pending-work entry; work that
/// never completes is redriven by the replay sweep until it expires.
async fn hook_work_completes(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    saved_document: Option<&str>,
) -> bool {
    let project_id = tracedecay_domain::ProjectId::new(
        harness
            .project_id(project)
            .await
            .expect("registered project id"),
    )
    .expect("project id");
    let canonical_project = std::fs::canonicalize(project).expect("canonical project");
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(&canonical_project, &project_id)
            .expect("resolved scope");
    let (hook_project, hook_worktree) = tracedecay_agent_hosts::hooks::hook_scope_locators(&scope);
    let host = tracedecay_domain::NativeHostIdentityV1::Codex;
    let binding = tracedecay_hooks::HookScopeBindingV1 {
        host,
        project_id: hook_project,
        repository_id: [4; 16],
        worktree_id: hook_worktree,
        worktree_epoch: 1,
        binding_token: [6; 32],
        capabilities: [
            tracedecay_hooks::HookEventFamily::SessionBoundary,
            tracedecay_hooks::HookEventFamily::SavedEdit,
        ]
        .into_iter()
        .map(|family| tracedecay_hooks::HookCapabilityV1 {
            family,
            support: tracedecay_hooks::stock_event_support(host, family),
        })
        .collect(),
    };
    let mut envelope = tracedecay_hooks::HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [1; 16],
        producer: host,
        protected_session_id: [2; 32],
        project_id: hook_project,
        repository_id: [4; 16],
        worktree_id: hook_worktree,
        worktree_epoch: 1,
        binding_token: [6; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: tracedecay_contracts::now_micros(),
        event: tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
        },
    };
    if let Some(document) = saved_document {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(document.as_bytes());
        let mut native_file_id = [0_u8; 16];
        native_file_id.copy_from_slice(&digest[..16]);
        envelope.event = tracedecay_hooks::HookEventV2::SavedEdit {
            file_id: tracedecay_mcp::handlers::hook_runtime::daemon_mint_hook_v2_file_id(
                &envelope,
                native_file_id,
            ),
            changed_range_count: 1,
        };
    }
    let (completed, completion) = tokio::sync::oneshot::channel();
    let completed = std::sync::Mutex::new(Some(completed));
    assert_eq!(
        tracedecay_daemon_service::admit_registered_hook_orchestration(
            envelope,
            binding,
            None,
            1,
            false,
            Some(std::sync::Arc::new(move || {
                if let Some(completed) = completed.lock().expect("completion").take() {
                    let _ = completed.send(());
                }
            })),
        ),
        tracedecay_daemon_service::HookOrchestrationAdmissionV1::Enqueued,
    );
    tokio::time::timeout(Duration::from_secs(60), completion)
        .await
        .is_ok_and(|completed| completed.is_ok())
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_edit_of_a_language_less_file_completes_its_hook_work() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let (harness, project) = settled_checkout_with_language_less_file(isolation.path()).await;

    assert!(
        hook_work_completes(&harness, &project, Some("deploy.conf")).await,
        "a deterministic language-unavailable cycle must settle its hook work instead of \
         leaving it owed for redrive"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_context_scout_ends_the_hook_cycle_after_its_advisory_half() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let (harness, project) = settled_checkout_with_language_less_file(isolation.path()).await;

    assert!(
        hook_work_completes(&harness, &project, Some("src/lib.rs")).await,
        "with Context Scout disabled the cycle owes no Scout work, so a saved edit \
         without a Scout lifecycle must still complete"
    );
    harness.shutdown().await;
}

/// Moves the project's Context Scout to `state` through the production
/// configuration surface.
async fn set_context_scout_state(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    state: &str,
) {
    let key = tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY;
    let response = harness
        .call_tool(
            project,
            "tracedecay_configuration_get",
            json!({"key": key, "format": "json"}),
        )
        .await
        .expect("configuration get");
    let (refused, current) = tool_answer(&response);
    assert!(!refused, "configuration get refused: {current}");
    let setting = &current["outcome"]["value"]["payload"];
    let mut value = setting["effective_value"].clone();
    value["value"]["state"] = json!(state);
    let response = harness
        .call_tool(
            project,
            "tracedecay_configuration_set",
            json!({
                "layer": {"kind": "project", "project_id": current["scope"]["project_id"]},
                "key": key,
                "value": value,
                "expected_revision": setting["revision_id"],
                "idempotency_key": format!("context-scout.state.{state}"),
                "format": "json",
            }),
        )
        .await
        .expect("configuration set");
    let (refused, written) = tool_answer(&response);
    assert!(!refused, "Context Scout {state} was refused: {written}");
}

/// Admits the same saved-edit boundary the way the replay sweep redrives
/// owed producer work, until a run completes it. Returns the redrives that
/// ran before completion, or `None` if it stayed owed past `max_redrives`.
async fn redrives_until_completed(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    max_redrives: usize,
) -> Option<usize> {
    for redrives in 0..=max_redrives {
        if hook_work_completes(harness, project, Some("src/lib.rs")).await {
            return Some(redrives);
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread")]
async fn hook_boundary_without_a_scout_session_is_not_redriven() {
    let isolation = tempfile::TempDir::new().expect("production harness isolation");
    let (harness, project) = settled_checkout_with_language_less_file(isolation.path()).await;

    for state in ["paused", "active"] {
        set_context_scout_state(&harness, &project, state).await;
        assert_eq!(
            redrives_until_completed(&harness, &project, 2).await,
            Some(0),
            "with Context Scout {state}, a boundary without a Scout session owes no Scout \
             work; redriving it cannot produce a session"
        );
    }
    harness.shutdown().await;
}
