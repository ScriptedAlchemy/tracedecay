//! Preserved-profile LCM discovery journey (#843).
//!
//! Seeds a large multi-provider corpus from the checked-in native fixtures,
//! opens an isolated production composition, lets ordinary background
//! convergence run, and asserts the umbrella acceptance: nonzero current
//! summary and git-correlation generations, a known worktree returns its
//! session, a 12-hour direct-user search stays under the product 5s budget
//! on this corpus (the >30s filter-pushdown regression lives on
//! `codex/lcm-search-filter-pushdown`), and lexical / graph / ordinary
//! retrieval stay admitted while convergence is still in progress.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracedecay_mcp::JsonRpcResponse;

use super::journey_test_support::{git, tool_answer};
use super::*;

const PROBE_SYMBOL: &str = "lcm_preserved_profile_probe";
const OLDEST_CLAUDE_SESSION: &str = "lcm-preserved-claude-000";
const NATIVE_BOUNDARY_UUID: &str = "ffffffff-0000-1111-2222-333333333333";
const NATIVE_SUMMARY_UUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
// 40 sessions × (4 Claude + 2 Codex + 1 Cursor) = 280 native records.
// Claude ingest stores the compact pair plus prior/assistant; Cursor adds
// one raw row. Target is ≥ 200 raw rows after ordinary ingest.
const SESSION_REPLAYS: usize = 40;
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
    assert!(!refused, "{tool} refused instead of answering: {payload}");
    payload
}

fn retained_payload(envelope: &Value) -> Value {
    if envelope["outcome"]["outcome"] == json!("evidence") {
        return envelope["outcome"]["value"]["payload"].clone();
    }
    if envelope["truncated"] == json!(true)
        && let Some(preview) = envelope["preview"].as_str()
        && let Ok(inner) = serde_json::from_str::<Value>(preview)
    {
        return retained_payload(&inner);
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

fn timed_raw<'a>(
    harness: &'a ProductionProjectCompositionHarnessV1,
    project: &'a Path,
    tool: &'a str,
    arguments: Value,
) -> impl std::future::Future<Output = (Duration, JsonRpcResponse)> + 'a {
    async move {
        let started = Instant::now();
        let response = harness
            .call_tool(project, tool, arguments)
            .await
            .unwrap_or_else(|error| panic!("{tool} was blocked instead of answering: {error}"));
        (started.elapsed(), response)
    }
}

fn assert_code_graph_unavailable(tool: &str, response: &JsonRpcResponse) {
    let error = response.error.as_ref().unwrap_or_else(|| {
        panic!("{tool} must stay a typed code-graph unavailable, not a payload: {response:?}")
    });
    let data = error
        .data
        .as_ref()
        .unwrap_or_else(|| panic!("{tool} unavailable error must carry data: {response:?}"));
    assert_eq!(
        data["reason_code"],
        json!("code-graph-unavailable"),
        "{tool} must name the skipped code-index wait: {response:?}"
    );
    assert_eq!(
        data["retryable"],
        json!(true),
        "{tool} unavailable state must stay retryable: {response:?}"
    );
}

fn assert_admitted_without_code_index(tool: &str, response: &JsonRpcResponse, payload_key: &str) {
    if response.error.is_some() {
        assert_code_graph_unavailable(tool, response);
        return;
    }
    let (refused, payload) = tool_answer(response);
    assert!(!refused, "{tool} refused instead of answering: {payload}");
    let payload = retained_payload(&payload);
    assert!(
        payload[payload_key].is_array(),
        "{tool} must stay admitted as a {payload_key} array: {payload}"
    );
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
    let origin = now_unix() - 13 * 3_600;

    for index in 0..SESSION_REPLAYS {
        let session_offset = (index as i64) * 20 * 60;
        let stamp = |offset: i64| utc_rfc3339(origin + session_offset + offset);

        let claude_session = format!("lcm-preserved-claude-{index:03}");
        let mut claude_prior = parse_fixture(CLAUDE_ASSISTANT);
        claude_prior["sessionId"] = json!(claude_session);
        claude_prior["cwd"] = json!(cwd);
        claude_prior["timestamp"] = json!(stamp(0));
        claude_prior["uuid"] = json!(format!("{claude_session}-prior"));
        let mut claude_boundary = parse_fixture(CLAUDE_BOUNDARY);
        claude_boundary["sessionId"] = json!(claude_session);
        claude_boundary["cwd"] = json!(cwd);
        claude_boundary["timestamp"] = json!(stamp(1));
        let mut claude_user = parse_fixture(CLAUDE_USER);
        claude_user["sessionId"] = json!(claude_session);
        claude_user["cwd"] = json!(cwd);
        claude_user["timestamp"] = json!(stamp(2));
        if index == 0 {
            assert_eq!(
                claude_boundary["uuid"],
                json!(NATIVE_BOUNDARY_UUID),
                "first replay must keep the fixture compact-pair boundary id"
            );
            assert_eq!(
                claude_user["uuid"],
                json!(NATIVE_SUMMARY_UUID),
                "first replay must keep the fixture compact-pair summary id"
            );
            assert_eq!(
                claude_boundary["compactMetadata"]["preservedSegment"]["anchorUuid"],
                json!(NATIVE_SUMMARY_UUID),
                "first replay must keep the fixture pairing evidence"
            );
        } else {
            let boundary_uuid = format!("ffffffff-0000-4000-8000-{index:012}");
            let summary_uuid = format!("aaaaaaaa-0000-4000-8000-{index:012}");
            claude_boundary["uuid"] = json!(boundary_uuid);
            claude_user["uuid"] = json!(summary_uuid);
            claude_user["parentUuid"] = json!(boundary_uuid);
            claude_boundary["compactMetadata"]["preservedSegment"]["anchorUuid"] =
                json!(summary_uuid);
        }
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

fn evidence_blob(envelope: &Value) -> String {
    envelope["preview"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| retained_payload(envelope).to_string())
}

fn blob_has_session(blob: &str, session_id: &str) -> bool {
    blob.contains(&format!("\"{session_id}\""))
}

fn blob_has_in_window_claude(blob: &str) -> bool {
    (1..SESSION_REPLAYS)
        .any(|index| blob_has_session(blob, &format!("lcm-preserved-claude-{index:03}")))
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

fn grep_hits(payload: &Value) -> &[Value] {
    payload["hits"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

async fn wait_for_preserved_discovery(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    worktree: &str,
    since: i64,
) -> (Value, Value, Duration) {
    let started = Instant::now();
    let mut last_status = json!(null);
    let mut last_sessions = json!(null);
    let mut last_grep = json!(null);
    tokio::time::timeout(CONVERGENCE_WAIT, async {
        loop {
            let status = retained_payload(&called(
                harness,
                project,
                "tracedecay_lcm_status",
                json!({"format": "json"}),
            )
            .await);
            let sessions = retained_payload(&called(
                harness,
                project,
                "tracedecay_sessions_for",
                json!({
                    "git_ref": "worktree",
                    "value": worktree,
                    "limit": 100,
                    "format": "json",
                }),
            )
            .await);
            let grep = retained_payload(&called(
                harness,
                project,
                "tracedecay_lcm_grep",
                json!({
                    "query": DIRECT_USER_QUERY,
                    "message_type": "direct_user",
                    "since": since,
                    "limit": 5,
                    "format": "json",
                }),
            )
            .await);
            last_status = status.clone();
            last_sessions = sessions.clone();
            last_grep = grep.clone();
            if summary_generation_nonzero(&status)
                && git_generation_nonzero(&sessions)
                && session_ids(&sessions)
                    .iter()
                    .any(|id| id.starts_with("lcm-preserved-codex-"))
                && !grep_hits(&grep).is_empty()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "ordinary background convergence never published summary, git-correlation, and 12-hour hits; \
             status={last_status}; sessions_for={last_sessions}; lcm_grep={last_grep}"
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

    let harness = ProductionProjectCompositionHarnessV1::open_for_session_retrieval(
        isolation.path(),
        [project.clone()],
    )
    .await
    .expect("production composition");

    let since = now_unix() - 12 * 3_600;
    let discovery = wait_for_preserved_discovery(&harness, &project, &worktree, since);
    let admissions = async {
        let status_at_admission = retained_payload(
            &called(
                &harness,
                &project,
                "tracedecay_lcm_status",
                json!({"format": "json"}),
            )
            .await,
        );
        let lexical = timed_raw(
            &harness,
            &project,
            "tracedecay_grep",
            json!({"pattern": PROBE_SYMBOL, "format": "json"}),
        );
        let graph = timed_raw(
            &harness,
            &project,
            "tracedecay_body",
            json!({"symbol": PROBE_SYMBOL, "format": "json"}),
        );
        let session = timed_call(
            &harness,
            &project,
            "tracedecay_message_search",
            json!({"query": "billing pipeline", "limit": 5, "format": "json"}),
        );
        let (lexical, graph, session) = tokio::join!(lexical, graph, session);
        (status_at_admission, lexical, graph, session)
    };
    let (
        (status, sessions_for, convergence_elapsed),
        (
            status_at_admission,
            (lexical_elapsed, lexical),
            (graph_elapsed, graph),
            (session_elapsed, session),
        ),
    ) = tokio::join!(discovery, admissions);
    assert!(
        !summary_generation_nonzero(&status_at_admission),
        "admission must observe LCM still converging: {status_at_admission}"
    );
    assert_under_budget("lexical admission", lexical_elapsed, ADMISSION_BUDGET);
    assert_admitted_without_code_index("tracedecay_grep", &lexical, "results");
    assert_under_budget("graph admission", graph_elapsed, ADMISSION_BUDGET);
    assert_admitted_without_code_index("tracedecay_body", &graph, "matches");
    assert_under_budget(
        "ordinary session admission",
        session_elapsed,
        ADMISSION_BUDGET,
    );
    assert_eq!(
        session["outcome"]["outcome"],
        json!("evidence"),
        "ordinary session retrieval must stay admitted as evidence: {session}"
    );
    assert_under_budget(
        "background discovery wait",
        convergence_elapsed,
        CONVERGENCE_WAIT,
    );
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
            .any(|id| id.starts_with("lcm-preserved-codex-")),
        "known TraceDecay worktree must return its correlated session: {sessions_for}"
    );

    let (search_elapsed, search) = timed_call(
        &harness,
        &project,
        "tracedecay_message_search",
        json!({
            "query": DIRECT_USER_QUERY,
            "message_type": "direct_user",
            "since": since,
            "limit": 5,
            "format": "json",
        }),
    )
    .await;
    assert_under_budget(
        "direct-user 12-hour message_search",
        search_elapsed,
        SEARCH_BUDGET,
    );
    let search_payload = retained_payload(&search);
    assert_ne!(
        search_payload["status"],
        json!("error"),
        "direct-user search must stay typed, not a transport failure: {search_payload}"
    );
    let search_blob = evidence_blob(&search);
    assert!(
        !blob_has_session(&search_blob, OLDEST_CLAUDE_SESSION),
        "12-hour search must exclude the oldest rows: {search}"
    );
    assert!(
        blob_has_in_window_claude(&search_blob),
        "12-hour search must keep rows inside the window: {search}"
    );

    let (grep_elapsed, grep) = timed_call(
        &harness,
        &project,
        "tracedecay_lcm_grep",
        json!({
            "query": DIRECT_USER_QUERY,
            "message_type": "direct_user",
            "since": since,
            "limit": 5,
            "format": "json",
        }),
    )
    .await;
    assert_under_budget("direct-user 12-hour lcm_grep", grep_elapsed, SEARCH_BUDGET);
    let grep_payload = retained_payload(&grep);
    let grep_blob = evidence_blob(&grep);
    if let Some(hits) = grep_payload["hits"].as_array() {
        assert!(
            !hits.is_empty(),
            "12-hour lcm_grep must return hits: {grep_payload}"
        );
        for hit in hits {
            let snippet = hit["snippet"].as_str().unwrap_or_default();
            assert!(
                snippet.chars().count() <= 8_192,
                "canonical redaction/content authority leaked an unbounded snippet: {hit}"
            );
        }
    } else {
        assert_eq!(
            grep["truncated"],
            json!(true),
            "lcm_grep without hits must be a truncated evidence page: {grep}"
        );
    }
    assert!(
        !blob_has_session(&grep_blob, OLDEST_CLAUDE_SESSION),
        "12-hour grep must exclude the oldest rows: {grep}"
    );
    assert!(
        blob_has_in_window_claude(&grep_blob),
        "12-hour grep must keep rows inside the window: {grep}"
    );

    assert!(
        lcm_status_body(&status)["redaction"].is_object(),
        "LCM status must preserve the redaction authority block: {status}"
    );

    let (strict_elapsed, strict_response) = timed_raw(
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
    let (refused, strict) = tool_answer(&strict_response);
    assert!(
        refused,
        "strict semantic search must refuse as a typed unavailable payload: {strict}"
    );
    assert_eq!(
        strict["semantic"]["reason"],
        json!("calibration_unavailable"),
        "strict semantic search must abstain with calibration_unavailable: {strict}"
    );

    harness.shutdown().await;
}
