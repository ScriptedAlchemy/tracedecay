//! Live journey: every non-semantic retrieval mode answers before a semantic
//! profile is ever activated, a real `FastEmbed` accepted-profile evaluation
//! runs, activation completes, and the other modes answer identically after.
//!
//! The contract under test is progressive degradation, not semantic search:
//! exact, lexical, graph, and ordinary session retrieval must never be blocked
//! by a semantic runtime that is pending, unevaluated, or unavailable, and a
//! strict-semantic request must report typed unavailability instead of failing
//! the request or poisoning the surrounding lanes.

#![cfg(feature = "semantic-fastembed")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::journey_test_support::{git, tool_payload};
use super::semantic_activation_journey_test::{
    assert_semantic_probe_contribution, evaluate_native_profile, installed_selection_material,
    seed_distribution_fixture, selection, semantic_candidate, set_semantic_profile,
    wait_for_semantic_generation,
};
use super::*;

const PROBE_SYMBOL: &str = "semantic_availability_probe";
const SESSION_ID: &str = "semantic-availability-journey-session";

/// One tool call, decoded to the payload the transport actually carried.
async fn called(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> Value {
    let response = harness
        .call_tool(project, tool, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool} was blocked instead of answering: {error}"));
    assert!(
        response.error.is_none(),
        "{tool} must answer with a typed payload, not a transport error: {response:?}"
    );
    tool_payload(&response)
}

/// A retrieval mode "answered" when the transport carried a typed payload the
/// caller can act on. A JSON-RPC error is the one outcome that means the mode
/// was blocked rather than degraded.
///
/// A payload larger than the MCP response cap is carried as a preview plus a
/// local response handle. That is reversible transport framing, not a
/// degraded lane, so resolve it through the public retrieve tool before
/// comparing a response.
pub(super) async fn answered(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> Value {
    let payload = called(harness, project, tool, arguments).await;
    if payload["truncated"] != json!(true) {
        return payload;
    }
    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} truncated its answer without a handle: {payload}"))
        .to_owned();
    let retrieved = called(
        harness,
        project,
        "tracedecay_retrieve",
        json!({"handle": handle, "format": "json"}),
    )
    .await;
    let content = retrieved["content"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} response handle carried no content: {retrieved}"));
    serde_json::from_str(content).unwrap_or_else(|error| {
        panic!("{tool} response handle content is not JSON: {error}; content={content}")
    })
}

async fn search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    strict: bool,
) -> Value {
    let mut arguments = json!({
        "query": PROBE_SYMBOL,
        "limit": 10,
        "format": "json"
    });
    if strict {
        arguments["semantic_mode"] = json!("strict_semantic");
    }
    answered(harness, project, "tracedecay_search", arguments).await
}

/// The tool-owned result inside a retained evidence envelope.
///
/// Ordinary session retrieval is a retained application tool, so it returns a
/// versioned retained envelope rather than its bare payload. A problem
/// envelope is a refusal and must not become a synthetic empty result.
fn retained_payload(tool: &str, envelope: &Value) -> Value {
    assert_eq!(
        envelope["outcome"]["outcome"],
        json!("evidence"),
        "{tool} must answer with retained evidence, not a refusal: {envelope}"
    );
    envelope["outcome"]["value"]["payload"].clone()
}

/// The three generation-bound non-semantic lanes plus ordinary session
/// retrieval, captured as one comparable value. Volatile freshness metadata is
/// excluded: the claim is that the *answers* are identical across activation,
/// not that wall-clock bookkeeping froze.
async fn non_semantic_answers(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    let lexical = answered(
        harness,
        project,
        "tracedecay_grep",
        json!({"pattern": PROBE_SYMBOL, "format": "json"}),
    )
    .await;
    let graph = answered(
        harness,
        project,
        "tracedecay_body",
        json!({"symbol": PROBE_SYMBOL, "format": "json"}),
    )
    .await;
    let session = retained_payload(
        "tracedecay_message_search",
        &answered(
            harness,
            project,
            "tracedecay_message_search",
            json!({"query": PROBE_SYMBOL, "limit": 5, "format": "json"}),
        )
        .await,
    );
    json!({
        "lexical": {
            "results": lexical["results"],
            "match_count": lexical["match_count"],
        },
        "graph": {
            "matches": graph["matches"],
            "match_count": graph["match_count"],
        },
        "session": {
            "results": session["results"],
            "count": session["count"],
        },
    })
}

/// Wait until the mount-woken historical transcript pass makes the seeded
/// Claude transcript visible to ordinary session retrieval. The baseline is
/// captured only after the non-semantic lane has actually answered.
async fn wait_for_session_lane(harness: &ProductionProjectCompositionHarnessV1, project: &Path) {
    tokio::time::timeout(Duration::from_mins(2), async {
        loop {
            let answers = non_semantic_answers(harness, project).await;
            if answers["session"]["count"]
                .as_u64()
                .is_some_and(|count| count > 0)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the seeded session transcript never reached ordinary session retrieval");
}

/// The runtime tool renders markdown unless JSON is requested explicitly.
async fn semantic_runtime_state(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> Value {
    answered(
        harness,
        project,
        "tracedecay_runtime",
        json!({"format": "json"}),
    )
    .await["semantic_runtime"]
        .clone()
}

/// Writes one real Claude transcript into the harness-owned source home so
/// ordinary session retrieval has a genuine message to find. The recorded
/// cwd uses canonical project identity, which transcript ingestion matches.
fn seed_session_transcript(isolation_root: &Path, project: &Path) {
    let home = ProductionProjectCompositionHarnessV1::transcript_source_home(isolation_root)
        .expect("composed transcript source home");
    let directory = home.join(".claude/projects/semantic-availability-journey");
    std::fs::create_dir_all(&directory).expect("session transcript directory");
    let canonical_project = std::fs::canonicalize(project).expect("canonical journey project");
    let cwd = canonical_project.to_string_lossy();
    let records = [
        json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": SESSION_ID,
            "uuid": "semantic-availability-journey-user",
            "timestamp": "2026-08-14T12:00:00.000Z",
            "message": {
                "role": "user",
                "content": format!("where is {PROBE_SYMBOL} defined"),
            },
        }),
        json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": SESSION_ID,
            "uuid": "semantic-availability-journey-assistant",
            "timestamp": "2026-08-14T12:00:01.000Z",
            "message": {
                "id": "message.semantic-availability-journey",
                "role": "assistant",
                "model": "claude-test",
                "content": [{
                    "type": "text",
                    "text": format!("{PROBE_SYMBOL} lives in src/lib.rs"),
                }],
            },
        }),
    ];
    let contents = records
        .iter()
        .map(|record| serde_json::to_string(record).expect("transcript record"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        directory.join(format!("{SESSION_ID}.jsonl")),
        format!("{contents}\n"),
    )
    .expect("session transcript");
}

pub(super) fn assert_lane_complete(coverage: &Value, lane: &str) {
    assert_eq!(
        coverage[lane],
        json!("complete"),
        "the {lane} lane must serve the current generation while semantic activation is pending; \
         coverage={coverage}"
    );
}

/// The whole point of the pending state: semantic is typed-unavailable with a
/// machine-readable reason, and that verdict is confined to the semantic lane.
pub(super) fn assert_semantic_pending(payload: &Value) {
    assert_eq!(
        payload["semantic"]["status"],
        json!("unavailable"),
        "semantic must report typed unavailability before activation: {payload}"
    );
    assert!(
        payload["semantic"]["reason"].is_string(),
        "typed semantic unavailability must carry a machine-readable reason: {payload}"
    );
    let coverage = &payload["coverage"];
    assert_eq!(
        coverage["semantic"]["status"],
        json!("unavailable"),
        "the semantic lane must be reported unavailable: {coverage}"
    );
    assert_eq!(
        coverage["recall"],
        json!("partial"),
        "a pending semantic lane must be disclosed as partial recall: {coverage}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrieval_answers_before_activation_and_is_unchanged_by_live_semantic_activation() {
    // The journey needs the byte-pinned FastEmbed package from distribution
    // acceptance; it cannot be synthesized, and a default `cargo test --lib`
    // has no reason to have it. Skip explicitly rather than fail the lane.
    let Some(fixture_root) = std::env::var_os("TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    else {
        eprintln!(
            "skipping the live semantic availability journey; prepare the \
             distribution-acceptance package and set \
             TRACEDECAY_DISTRIBUTION_FASTEMBED_FIXTURE"
        );
        return;
    };
    let _profile = crate::config::PinnedUserDataDir::new();
    let lifecycle_root =
        crate::semantic_code::default_lifecycle_root().expect("isolated lifecycle root");
    let lifecycle =
        crate::semantic_code::shared_lifecycle_owner().expect("production lifecycle owner");
    seed_distribution_fixture(&lifecycle_root, &fixture_root, &lifecycle);
    lifecycle
        .select_model(Some(crate::semantic_code::DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select production semantic model");
    lifecycle
        .acquire_blocking_for_tests()
        .expect("install verified distribution fixture");
    let (artifact_digest, artifact_path) = installed_selection_material(&lifecycle);

    let isolation = tempfile::TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    git(&project, &["init", "--quiet"]);
    std::fs::write(
        project.join("src/lib.rs"),
        format!(
            "pub fn {PROBE_SYMBOL}() -> &'static str {{ \"availability\" }}\n\
             pub fn {PROBE_SYMBOL}_caller() -> &'static str {{ {PROBE_SYMBOL}() }}\n"
        ),
    )
    .expect("journey source");
    git(&project, &["add", "."]);
    git(
        &project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: seed semantic availability journey",
        ],
    );
    seed_session_transcript(isolation.path(), &project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let resources = harness.resources.as_ref().expect("live harness");
    let code_id = resources
        .invocation
        .code_index_schedulers
        .latest_generation_id(&project)
        .await
        .expect("published code generation");
    let (code, vector) = wait_for_semantic_generation(&harness, &project, &code_id).await;

    // ---- Phase 1: nothing is activated yet. -------------------------------
    let pending_state = semantic_runtime_state(&harness, &project).await;
    assert_ne!(
        pending_state["state"],
        json!("ready"),
        "no semantic profile has been activated yet: {pending_state}"
    );

    let core_before = search(&harness, &project, false).await;
    assert_semantic_pending(&core_before);
    assert!(
        core_before["results"]
            .as_array()
            .is_some_and(|results| !results.is_empty()),
        "exact/lexical/graph fusion must still return results with semantic pending: {core_before}"
    );
    assert_eq!(
        core_before["code_generation"],
        json!(code.manifest().generation_id),
        "the served answer must be bound to the current code generation"
    );
    let coverage_before = core_before["coverage"].clone();
    for lane in ["exact", "lexical", "graph"] {
        assert_lane_complete(&coverage_before, lane);
    }
    assert!(
        core_before["status"].is_null(),
        "a pending semantic lane must not make the whole search unavailable: {core_before}"
    );
    let fallback_digest_before = core_before["query_fallback_digest"].clone();
    assert!(
        fallback_digest_before.is_string(),
        "the canonical core query bytes must be published while semantic is pending"
    );

    let strict_before = search(&harness, &project, true).await;
    assert_eq!(
        strict_before["status"],
        json!("unavailable"),
        "strict semantic must be typed-unavailable before activation: {strict_before}"
    );
    assert_ne!(strict_before["semantic"]["status"], json!("complete"));

    // A strict-semantic refusal must not poison the lanes around it: the very
    // next ordinary request has to produce the same answer as before it.
    let core_after_strict = search(&harness, &project, false).await;
    assert_eq!(
        core_after_strict["results"], core_before["results"],
        "a typed strict-semantic refusal must not block or alter the other lanes"
    );
    assert_eq!(
        core_after_strict["query_fallback_digest"], fallback_digest_before,
        "a typed strict-semantic refusal must preserve the canonical core query bytes"
    );

    wait_for_session_lane(&harness, &project).await;
    let answers_before = non_semantic_answers(&harness, &project).await;
    assert!(
        answers_before["lexical"]["match_count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "lexical retrieval must answer non-vacuously before activation: {answers_before}"
    );
    assert!(
        answers_before["graph"]["match_count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "graph retrieval must answer non-vacuously before activation: {answers_before}"
    );
    assert!(
        answers_before["session"]["count"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "ordinary session retrieval must answer non-vacuously before activation: {answers_before}"
    );

    // ---- Phase 2: the real accepted-profile evaluation. -------------------
    let accepted_profile =
        evaluate_native_profile(&harness, &project, semantic_candidate(&code, &vector)).await;
    assert_eq!(
        non_semantic_answers(&harness, &project).await,
        answers_before,
        "a live FastEmbed evaluation must not disturb the non-semantic retrieval modes"
    );
    let core_after_evaluation = search(&harness, &project, false).await;
    assert_semantic_pending(&core_after_evaluation);
    assert_eq!(
        core_after_evaluation["query_fallback_digest"], fallback_digest_before,
        "evaluation alone must not change the canonical core query bytes"
    );

    // ---- Phase 3: activation. --------------------------------------------
    set_semantic_profile(
        &harness,
        &project,
        selection(accepted_profile, &artifact_digest, &artifact_path),
        None,
    )
    .await;
    let (activated, activated_state) = tokio::time::timeout(Duration::from_mins(1), async {
        loop {
            let strict = search(&harness, &project, true).await;
            let state = semantic_runtime_state(&harness, &project).await;
            if strict["semantic"]["status"] == json!("complete") && state["state"] == json!("ready")
            {
                return (strict, state);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("activated semantic retrieval did not begin answering");
    assert_eq!(activated["semantic"]["status"], json!("complete"));
    assert_semantic_probe_contribution(
        &activated,
        PROBE_SYMBOL,
        "semantic availability activation",
    );
    assert_eq!(activated_state["state"], json!("ready"));
    assert_eq!(
        activated_state["receipt"]["activated_generation"],
        json!(vector.generation_id()),
        "activation must bind the exact evaluated vector generation"
    );

    let core_after = search(&harness, &project, false).await;
    assert_eq!(
        core_after["semantic"]["status"],
        json!("complete"),
        "semantic retrieval must answer once activation completes: {core_after}"
    );
    assert_semantic_probe_contribution(
        &core_after,
        PROBE_SYMBOL,
        "semantic availability fallback search after activation",
    );
    assert_eq!(
        core_after["coverage"]["recall"],
        json!("full"),
        "every lane serves the current generation after activation: {core_after}"
    );
    for lane in ["exact", "lexical", "graph"] {
        assert_lane_complete(&core_after["coverage"], lane);
    }
    assert_eq!(
        core_after["query_fallback_digest"], fallback_digest_before,
        "activation must preserve the canonical core query bytes"
    );
    assert_eq!(
        non_semantic_answers(&harness, &project).await,
        answers_before,
        "exact, lexical, graph, and session retrieval must be identical after activation"
    );
    assert_eq!(
        resources
            .invocation
            .code_index_schedulers
            .latest_generation_id(&project)
            .await,
        Some(code_id),
        "semantic activation must not publish a code-index generation"
    );
    harness.shutdown().await;
}
