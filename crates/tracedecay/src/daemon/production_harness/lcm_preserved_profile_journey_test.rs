//! Preserved-profile LCM discovery journey (#843).
//!
//! Seeds a large multi-provider corpus from the checked-in native fixtures,
//! opens an isolated production composition, lets ordinary background
//! convergence run, and asserts the umbrella acceptance: nonzero current
//! summary and git-correlation generations, a known worktree returns its
//! session, a direct-user 12-hour search finishes under five seconds, and
//! lexical / graph / ordinary retrieval stay admitted while that work runs.
//! Query deadlines stay at the product 5s / 30s budgets.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::journey_test_support::{git, tool_answer};
use super::*;

const PROBE_SYMBOL: &str = "lcm_preserved_profile_probe";
const KNOWN_WORKTREE_SESSION: &str = "lcm-preserved-cursor-000";
const SESSION_REPLAYS: usize = 12;
const SEARCH_BUDGET: Duration = Duration::from_secs(5);
const ADMISSION_BUDGET: Duration = Duration::from_secs(30);
const CONVERGENCE_WAIT: Duration = Duration::from_mins(2);
const DIRECT_USER_QUERY: &str = "compact-summary pair extraction";

const CLAUDE_BOUNDARY: &str = include_str!(
    "../../../../../tests/fixtures/provider_normalization/claude/compact_summary_pair.boundary.input.json"
);
const CLAUDE_USER: &str = include_str!(
    "../../../../../tests/fixtures/provider_normalization/claude/compact_summary_pair.summary.input.json"
);
const CLAUDE_ASSISTANT: &str = include_str!(
    "../../../../../tests/fixtures/provider_normalization/claude/assistant_tool_use.input.json"
);
const CODEX_META: &str = include_str!(
    "../../../../../tests/fixtures/provider_normalization/codex/session_meta.input.json"
);
const CODEX_ASSISTANT: &str = include_str!(
    "../../../../../tests/fixtures/provider_normalization/codex/agent_message.input.json"
);
const CURSOR_ASSISTANT: &str =
    include_str!("../../../../../tests/fixtures/provider_normalization/cursor/tool_use.input.json");

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
    let (refused, payload) = tool_answer(&response);
    if refused {
        return payload;
    }
    payload
}

fn retained_payload(_tool: &str, envelope: &Value) -> Value {
    if envelope["outcome"]["outcome"] == json!("evidence") {
        return envelope["outcome"]["value"]["payload"].clone();
    }
    envelope.clone()
}

fn timed_call<'a>(
    harness: &'a ProductionProjectCompositionHarnessV1,
    project: &'a Path,
    tool: &'a str,
    arguments: Value,
) -> impl std::future::Future<Output = (Duration, Value)> + 'a {
    async move {
        let started = Instant::now();
        let payload = called(harness, project, tool, arguments).await;
        (started.elapsed(), payload)
    }
}

fn utc_rfc3339(unix_secs: i64) -> String {
    const DAYS_BEFORE_UNIX: i64 = 719_468;
    let z = unix_secs.div_euclid(86_400) + DAYS_BEFORE_UNIX;
    let tod = unix_secs.rem_euclid(86_400) as u32;
    let era = z.div_euclid(146_097);
    let doe = u32::try_from(z.rem_euclid(146_097)).expect("day of era");
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let second = tod % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

fn now_unix() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs(),
    )
    .expect("unix seconds fit i64")
}

fn cursor_slug(project_root: &Path) -> String {
    project_root
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_fixture(text: &str) -> Value {
    serde_json::from_str(text).expect("checked-in native fixture is JSON")
}

fn write_jsonl(path: &Path, records: &[Value]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("transcript directory");
    }
    let body = records
        .iter()
        .map(|record| serde_json::to_string(record).expect("serialize fixture record"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(path, format!("{body}\n")).expect("write native transcript");
}

fn seed_preserved_profile_corpus(isolation_root: &Path, project: &Path) {
    let home = ProductionProjectCompositionHarnessV1::transcript_source_home(isolation_root)
        .expect("composed transcript source home");
    let cwd = std::fs::canonicalize(project)
        .expect("canonical journey project")
        .to_string_lossy()
        .into_owned();
    let cursor_dir = cursor_slug(Path::new(&cwd));
    let origin = now_unix() - 1_800;

    for index in 0..SESSION_REPLAYS {
        let stamp = |offset: i64| utc_rfc3339(origin + (index as i64) * 8 + offset);

        let claude_session = format!("lcm-preserved-claude-{index:03}");
        let boundary_uuid = format!("ffffffff-0000-4000-8000-{index:012}");
        let summary_uuid = format!("aaaaaaaa-0000-4000-8000-{index:012}");
        let mut claude_prior = parse_fixture(CLAUDE_ASSISTANT);
        claude_prior["sessionId"] = json!(claude_session);
        claude_prior["cwd"] = json!(cwd);
        claude_prior["timestamp"] = json!(stamp(0));
        claude_prior["uuid"] = json!(format!("{claude_session}-prior"));
        let mut claude_boundary = parse_fixture(CLAUDE_BOUNDARY);
        claude_boundary["sessionId"] = json!(claude_session);
        claude_boundary["cwd"] = json!(cwd);
        claude_boundary["timestamp"] = json!(stamp(1));
        claude_boundary["uuid"] = json!(boundary_uuid);
        claude_boundary["compactMetadata"]["preservedSegment"]["anchorUuid"] = json!(summary_uuid);
        let mut claude_user = parse_fixture(CLAUDE_USER);
        claude_user["sessionId"] = json!(claude_session);
        claude_user["cwd"] = json!(cwd);
        claude_user["timestamp"] = json!(stamp(2));
        claude_user["uuid"] = json!(summary_uuid);
        claude_user["parentUuid"] = json!(boundary_uuid);
        let mut claude_assistant = parse_fixture(CLAUDE_ASSISTANT);
        claude_assistant["sessionId"] = json!(claude_session);
        claude_assistant["cwd"] = json!(cwd);
        claude_assistant["timestamp"] = json!(stamp(3));
        claude_assistant["uuid"] = json!(format!("{claude_session}-assistant"));
        write_jsonl(
            &home
                .join(".claude/projects/lcm-preserved-profile")
                .join(format!("{claude_session}.jsonl")),
            &[claude_prior, claude_boundary, claude_user, claude_assistant],
        );

        let codex_session = format!("lcm-preserved-codex-{index:03}");
        let mut codex_meta = parse_fixture(CODEX_META);
        codex_meta["timestamp"] = json!(stamp(2));
        codex_meta["payload"]["id"] = json!(codex_session);
        codex_meta["payload"]["cwd"] = json!(cwd);
        let mut codex_assistant = parse_fixture(CODEX_ASSISTANT);
        codex_assistant["timestamp"] = json!(stamp(3));
        write_jsonl(
            &home
                .join(".codex/sessions/2026/09/11")
                .join(format!("rollout-{codex_session}.jsonl")),
            &[codex_meta, codex_assistant],
        );

        let cursor_session = format!("lcm-preserved-cursor-{index:03}");
        let mut cursor_assistant = parse_fixture(CURSOR_ASSISTANT);
        cursor_assistant["id"] = json!(cursor_session);
        cursor_assistant["timestamp"] = json!(stamp(4));
        write_jsonl(
            &home
                .join(".cursor/projects")
                .join(&cursor_dir)
                .join("agent-transcripts")
                .join(format!("{cursor_session}.jsonl")),
            &[cursor_assistant],
        );
    }
}

fn seed_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    git(project, &["init", "--quiet", "-b", "main"]);
    std::fs::write(
        project.join("src/lib.rs"),
        format!("pub fn {PROBE_SYMBOL}() -> &'static str {{ \"preserved\" }}\n"),
    )
    .expect("journey source");
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "test: seed lcm preserved profile journey",
        ],
    );
}

fn assert_under_budget(label: &str, elapsed: Duration, budget: Duration) {
    assert!(
        elapsed <= budget,
        "{label} took {elapsed:?}, over the unchanged {budget:?} budget"
    );
}

fn lcm_status_body(status: &Value) -> &Value {
    if status.get("lcm").is_some_and(Value::is_object) {
        &status["lcm"]
    } else {
        status
    }
}

fn summary_generation_nonzero(status: &Value) -> bool {
    let status = lcm_status_body(status);
    if status["summary_node_count"]
        .as_i64()
        .is_some_and(|count| count > 0)
    {
        return true;
    }
    status["summary_convergence"]["current_session_count"]
        .as_i64()
        .is_some_and(|count| count > 0)
        || status["dag"]["total_nodes"]
            .as_i64()
            .is_some_and(|count| count > 0)
}

fn git_generation_nonzero(payload: &Value) -> bool {
    payload["index"]["generation"]
        .as_str()
        .is_some_and(|generation| !generation.is_empty())
        && payload["index"]["projection_available"] == json!(true)
}

fn session_ids(payload: &Value) -> Vec<String> {
    payload["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            hit["session_id"]
                .as_str()
                .or_else(|| hit["sessionId"].as_str())
                .map(str::to_owned)
        })
        .collect()
}

async fn wait_for_preserved_discovery(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    worktree: &str,
) -> (Value, Value, Duration) {
    let started = Instant::now();
    let mut last_status = json!(null);
    let mut last_sessions = json!(null);
    tokio::time::timeout(CONVERGENCE_WAIT, async {
        loop {
            let status = retained_payload(
                "tracedecay_lcm_status",
                &called(
                    harness,
                    project,
                    "tracedecay_lcm_status",
                    json!({"format": "json"}),
                )
                .await,
            );
            let sessions = retained_payload(
                "tracedecay_sessions_for",
                &called(
                    harness,
                    project,
                    "tracedecay_sessions_for",
                    json!({
                        "git_ref": "worktree",
                        "value": worktree,
                        "format": "json",
                    }),
                )
                .await,
            );
            last_status = status.clone();
            last_sessions = sessions.clone();
            if summary_generation_nonzero(&status)
                && git_generation_nonzero(&sessions)
                && session_ids(&sessions)
                    .iter()
                    .any(|id| id == KNOWN_WORKTREE_SESSION)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "ordinary background convergence never published summary and git-correlation generations; \
             status={last_status}; sessions_for={last_sessions}"
        )
    });
    (last_status, last_sessions, started.elapsed())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preserved_profile_lcm_discovery_converges_without_blocking_retrieval() {
    let _profile = crate::config::PinnedUserDataDir::new();
    let isolation = tempfile::TempDir::new().expect("isolated home/profile");
    let project = isolation.path().join("project");
    seed_project(&project);
    seed_preserved_profile_corpus(isolation.path(), &project);
    let worktree = std::fs::canonicalize(&project)
        .expect("canonical worktree")
        .to_string_lossy()
        .into_owned();

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");

    let (lexical_elapsed, lexical) = timed_call(
        &harness,
        &project,
        "tracedecay_grep",
        json!({"pattern": PROBE_SYMBOL, "format": "json"}),
    )
    .await;
    assert_under_budget("lexical admission", lexical_elapsed, ADMISSION_BUDGET);
    assert!(
        lexical.get("results").is_some() || lexical.get("matches").is_some(),
        "lexical retrieval must stay admitted while LCM converges: {lexical}"
    );

    let (graph_elapsed, graph) = timed_call(
        &harness,
        &project,
        "tracedecay_body",
        json!({"symbol": PROBE_SYMBOL, "format": "json"}),
    )
    .await;
    assert_under_budget("graph admission", graph_elapsed, ADMISSION_BUDGET);
    assert!(
        graph.get("matches").is_some()
            || graph.get("body").is_some()
            || graph.get("nodes").is_some()
            || graph["outcome"]["outcome"] == json!("evidence"),
        "graph retrieval must stay admitted while LCM converges: {graph}"
    );

    let (session_elapsed, session) = timed_call(
        &harness,
        &project,
        "tracedecay_message_search",
        json!({"query": "billing pipeline", "limit": 5, "format": "json"}),
    )
    .await;
    assert_under_budget(
        "ordinary session admission",
        session_elapsed,
        ADMISSION_BUDGET,
    );
    assert!(
        session["outcome"]["outcome"] == json!("evidence")
            || session.get("results").is_some()
            || session["status"] == json!("unavailable"),
        "ordinary session retrieval must answer or report a typed unavailable state: {session}"
    );

    let (status, sessions_for, convergence_elapsed) =
        wait_for_preserved_discovery(&harness, &project, &worktree).await;
    assert!(
        summary_generation_nonzero(&status),
        "LCM status must report a nonzero current summary generation: {status}"
    );
    assert!(
        git_generation_nonzero(&sessions_for),
        "git-correlation generation must be published after ordinary convergence: {sessions_for}"
    );
    assert!(
        session_ids(&sessions_for)
            .iter()
            .any(|id| id == KNOWN_WORKTREE_SESSION),
        "known TraceDecay worktree must return its correlated session: {sessions_for}"
    );

    let since = now_unix() - 12 * 3_600;
    let (search_elapsed, search) = timed_call(
        &harness,
        &project,
        "tracedecay_message_search",
        json!({
            "query": DIRECT_USER_QUERY,
            "message_type": "direct_user",
            "since": since,
            "limit": 20,
            "format": "json",
        }),
    )
    .await;
    assert_under_budget(
        "direct-user 12-hour message_search",
        search_elapsed,
        SEARCH_BUDGET,
    );
    let search_payload = retained_payload("tracedecay_message_search", &search);
    assert_ne!(
        search_payload["status"],
        json!("error"),
        "direct-user search must stay typed, not a transport failure: {search_payload}"
    );

    let (grep_elapsed, grep) = timed_call(
        &harness,
        &project,
        "tracedecay_lcm_grep",
        json!({
            "query": DIRECT_USER_QUERY,
            "message_type": "direct_user",
            "since": since,
            "limit": 20,
            "format": "json",
        }),
    )
    .await;
    assert_under_budget("direct-user 12-hour lcm_grep", grep_elapsed, SEARCH_BUDGET);
    let grep_payload = retained_payload("tracedecay_lcm_grep", &grep);
    assert!(
        grep_payload.get("hits").is_some()
            || grep_payload.get("results").is_some()
            || grep_payload["status"] == json!("unavailable"),
        "lcm_grep must answer with hits or a typed unavailable state: {grep_payload}"
    );
    if let Some(hits) = grep_payload["hits"].as_array() {
        for hit in hits {
            let snippet = hit["snippet"].as_str().unwrap_or_default();
            assert!(
                snippet.chars().count() <= 8_192,
                "canonical redaction/content authority leaked an unbounded snippet: {hit}"
            );
        }
    }

    assert!(
        lcm_status_body(&status)["redaction"].is_object()
            || lcm_status_body(&status).get("redaction").is_some(),
        "LCM status must preserve the redaction authority block: {status}"
    );

    let (strict_elapsed, strict) = timed_call(
        &harness,
        &project,
        "tracedecay_search",
        json!({
            "query": PROBE_SYMBOL,
            "semantic_mode": "strict_semantic",
            "limit": 5,
            "format": "json",
        }),
    )
    .await;
    assert_under_budget(
        "typed unavailable semantic search",
        strict_elapsed,
        ADMISSION_BUDGET,
    );
    let unavailable = strict["semantic"]["status"] == json!("unavailable")
        || strict["status"] == json!("unavailable")
        || strict["outcome"]["outcome"] == json!("problem");
    assert!(
        unavailable,
        "strict semantic search must stay a typed unavailable state, not a transport error: {strict}"
    );

    eprintln!(
        "lcm-preserved-profile timings: \
         lexical_admission={:?} graph_admission={:?} session_admission={:?} \
         convergence_wait={:?} message_search={:?} lcm_grep={:?} \
         summary_nodes={} git_generation={} raw_messages={}",
        lexical_elapsed,
        graph_elapsed,
        session_elapsed,
        convergence_elapsed,
        search_elapsed,
        grep_elapsed,
        lcm_status_body(&status)["summary_node_count"],
        sessions_for["index"]["generation"],
        lcm_status_body(&status)["raw_message_count"]
    );

    harness.shutdown().await;
}
