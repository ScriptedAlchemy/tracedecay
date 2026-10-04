//! The advisory cycle that admits pull requests mounts on whatever languages
//! a checkout indexes. A TypeScript-only repository has a sealed generation
//! like any other, so the explicit cycle must run over it instead of staying
//! "not mounted yet" for the daemon's whole life.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::journey_test_support::{git, tool_answer};
use super::*;

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
