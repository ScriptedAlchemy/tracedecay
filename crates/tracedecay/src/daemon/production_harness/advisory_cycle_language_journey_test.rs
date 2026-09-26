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
#[hotpath::skip]
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

#[tokio::test(flavor = "multi_thread")]
#[hotpath::skip]
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
