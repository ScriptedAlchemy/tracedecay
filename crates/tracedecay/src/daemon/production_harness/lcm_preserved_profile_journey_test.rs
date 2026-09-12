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
//!
//! The issue's acceptance names graph retrieval, so the composition opens
//! through the ordinary [`ProductionProjectCompositionHarnessV1::open`] and
//! every admission claim is asserted as a populated payload. A typed
//! `code-graph-unavailable` is not accepted as admission here: it would make
//! "graph retrieval stays admitted" satisfiable with no code graph at all.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracedecay_mcp::JsonRpcResponse;

use super::journey_test_support::{git, resolved, tool_answer};
use super::*;

const PROBE_SYMBOL: &str = "lcm_preserved_profile_probe";
const NATIVE_BOUNDARY_UUID: &str = "ffffffff-0000-1111-2222-333333333333";
const NATIVE_SUMMARY_UUID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SESSION_REPLAYS: usize = 40;
/// Corpus and window arithmetic, shared by the seed and every window
/// assertion so the two cannot drift: replay `index` is stamped at
/// `origin + index * SESSION_STRIDE_SECS` with
/// `origin = now - CORPUS_SPAN_SECS`, and the searches ask for the trailing
/// `SEARCH_WINDOW_SECS`.
const CORPUS_SPAN_SECS: i64 = 13 * 3_600;
const SESSION_STRIDE_SECS: i64 = 20 * 60;
const SEARCH_WINDOW_SECS: i64 = 12 * 3_600;
/// 40 sessions × (4 Claude + 2 Codex + 1 Cursor) = 280 native records.
/// Claude ingest stores the compact pair plus prior/assistant; Cursor adds
/// one raw row. Ordinary ingest must land at least this many raw rows, so the
/// window assertions cannot pass over a thin corpus.
const RAW_MESSAGE_FLOOR: i64 = 200;
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

/// One tool answer, resolved through the retrieve handle when the payload
/// exceeded a response frame, then unwrapped to the retained payload.
async fn answered(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    tool: &str,
    arguments: Value,
) -> Value {
    let payload = called(harness, project, tool, arguments).await;
    retained_payload(&resolved(harness, project, tool, payload).await)
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

/// Strict admission: the tool answered with a populated payload array.
///
/// Accepting a typed unavailable here would satisfy the retrieval-admission
/// claim without any index behind it, so an error response fails.
fn assert_admitted_with_results(tool: &str, response: &JsonRpcResponse, payload_key: &str) {
    assert!(
        response.error.is_none(),
        "{tool} must stay admitted with a payload, not a typed unavailable: {response:?}"
    );
    let (refused, payload) = tool_answer(response);
    assert!(!refused, "{tool} refused instead of answering: {payload}");
    let payload = retained_payload(&payload);
    let entries = payload[payload_key]
        .as_array()
        .unwrap_or_else(|| panic!("{tool} must answer as a {payload_key} array: {payload}"));
    assert!(
        !entries.is_empty(),
        "{tool} must resolve {PROBE_SYMBOL} while LCM is still converging: {payload}"
    );
}

/// One admission round while LCM converges: lexical, graph, and ordinary
/// session retrieval all answer inside the unchanged admission budget.
type AdmissionRound = (
    (Duration, JsonRpcResponse),
    (Duration, JsonRpcResponse),
    (Duration, Value),
);

/// `session` is the ordinary-session envelope after handle resolution: the
/// retained surface answers past one response frame, and a cut preview is
/// framing, not a denial.
fn assert_admission_round(round: &AdmissionRound, session_elapsed: Duration, session: &Value) {
    let ((lexical_elapsed, lexical), (graph_elapsed, graph), _) = round;
    assert_under_budget("lexical admission", *lexical_elapsed, ADMISSION_BUDGET);
    assert_admitted_with_results("tracedecay_grep", lexical, "results");
    assert_under_budget("graph admission", *graph_elapsed, ADMISSION_BUDGET);
    assert_admitted_with_results("tracedecay_body", graph, "matches");
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

/// Seeds the replay corpus and returns its origin, so the searches derive
/// their window boundary from the same arithmetic the rows were stamped with.
fn seed_preserved_profile_corpus(isolation_root: &Path, project: &Path) -> i64 {
    let home = ProductionProjectCompositionHarnessV1::transcript_source_home(isolation_root)
        .expect("composed transcript source home");
    let cwd = std::fs::canonicalize(project)
        .expect("canonical journey project")
        .to_string_lossy()
        .into_owned();
    let cursor_dir = cursor_slug(Path::new(&cwd));
    let origin = now_unix() - CORPUS_SPAN_SECS;

    for index in 0..SESSION_REPLAYS {
        let session_offset = (index as i64) * SESSION_STRIDE_SECS;
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

    origin
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

fn raw_message_count(status: &Value) -> i64 {
    lcm_status_body(status)["raw_message_count"]
        .as_i64()
        .unwrap_or_else(|| panic!("lcm_status must report raw_message_count: {status}"))
}

/// Monotone background-convergence progress read from `lcm_status`.
///
/// Ingest and summary convergence only ever add raw rows, current sessions,
/// and DAG nodes. A strictly greater reading across the admission batch is
/// positive evidence that convergence was running while retrieval was being
/// served, and unlike asserting convergence has *not* finished it does not
/// race the background worker.
#[derive(Debug, PartialEq, Eq)]
struct ConvergenceProgress {
    raw_messages: i64,
    current_sessions: i64,
    dag_nodes: i64,
}

impl ConvergenceProgress {
    fn read(status: &Value) -> Self {
        let body = lcm_status_body(status);
        Self {
            raw_messages: raw_message_count(status),
            current_sessions: body["summary_convergence"]["current_session_count"]
                .as_i64()
                .unwrap_or_default(),
            dag_nodes: body["dag"]["total_nodes"].as_i64().unwrap_or_default(),
        }
    }

    fn advanced_from(&self, earlier: &Self) -> bool {
        self.raw_messages > earlier.raw_messages
            || self.current_sessions > earlier.current_sessions
            || self.dag_nodes > earlier.dag_nodes
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

fn claude_replay_session(index: usize) -> String {
    format!("lcm-preserved-claude-{index:03}")
}

/// The window predicate the searches are asserted against, derived from the
/// same arithmetic that stamps the corpus: replay `index` sits inside the
/// trailing search window exactly when its stamp is at or after the `since`
/// boundary.
fn replay_in_search_window(index: usize) -> bool {
    CORPUS_SPAN_SECS - (index as i64) * SESSION_STRIDE_SECS <= SEARCH_WINDOW_SECS
}

fn claude_replays(in_window: bool) -> Vec<String> {
    (0..SESSION_REPLAYS)
        .filter(|index| replay_in_search_window(*index) == in_window)
        .map(claude_replay_session)
        .collect()
}

/// Every seeded session id ends in its replay index, so a returned identity
/// resolves back to the side of the window it was stamped on.
fn replay_index(session_id: &str) -> usize {
    session_id
        .rsplit('-')
        .next()
        .and_then(|suffix| suffix.parse().ok())
        .unwrap_or_else(|| panic!("seeded session id must end in its replay index: {session_id}"))
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

/// `message_search` hits carry their identity on the matched message, not on
/// the hit envelope.
fn message_hit_session_ids(payload: &Value) -> Vec<String> {
    payload["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            hit["message"]["session_id"]
                .as_str()
                .or_else(|| hit["session"]["session_id"].as_str())
                .map(str::to_owned)
        })
        .collect()
}

fn grep_hits(payload: &Value) -> &[Value] {
    payload["hits"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn grep_session_ids(payload: &Value) -> Vec<String> {
    grep_hits(payload)
        .iter()
        .filter_map(|hit| hit["session_id"].as_str().map(str::to_owned))
        .collect()
}

/// Asserts a search returned identities from one side of the window only.
///
/// The identities are parsed `session_id` fields: a substring scan of the
/// serialized evidence would pass on a truncated page that never named a
/// session at all.
fn assert_window_side(label: &str, returned: &[String], in_window: bool, payload: &Value) {
    assert!(
        !returned.is_empty(),
        "{label} must return parsed session identities: {payload}"
    );
    for session_id in returned {
        assert_eq!(
            replay_in_search_window(replay_index(session_id)),
            in_window,
            "{label} returned {session_id} from the wrong side of the window: {payload}"
        );
    }
}

/// The complement of the 12-hour window, once temporal convergence can serve
/// it.
///
/// A `stale` outcome is the projection reporting it has not caught up to those
/// generations yet; reading absence out of it would let the window assertions
/// pass on lag instead of on a window decision.
async fn wait_for_pre_window_search(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    origin: i64,
    since: i64,
) -> Value {
    let deadline = Instant::now() + CONVERGENCE_WAIT;
    loop {
        let payload = answered(
            harness,
            project,
            "tracedecay_message_search",
            json!({
                "query": DIRECT_USER_QUERY,
                "message_type": "direct_user",
                "since": origin,
                "until": since - 1,
                "limit": SESSION_REPLAYS,
                "format": "json",
            }),
        )
        .await;
        if payload["outcome"] != json!("stale") {
            return payload;
        }
        assert!(
            Instant::now() < deadline,
            "the pre-window search never left typed staleness: {payload}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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
            let status = answered(
                harness,
                project,
                "tracedecay_lcm_status",
                json!({"format": "json"}),
            )
            .await;
            let sessions = answered(
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
            .await;
            let grep = answered(
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
            .await;
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
    let origin = seed_preserved_profile_corpus(isolation.path(), &project);
    let worktree = std::fs::canonicalize(&project)
        .expect("canonical worktree")
        .to_string_lossy()
        .into_owned();

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");

    let since = origin + CORPUS_SPAN_SECS - SEARCH_WINDOW_SECS;
    let discovery = wait_for_preserved_discovery(&harness, &project, &worktree, since);
    let admissions = async {
        let read_progress = || async {
            ConvergenceProgress::read(
                &answered(
                    &harness,
                    &project,
                    "tracedecay_lcm_status",
                    json!({"format": "json"}),
                )
                .await,
            )
        };
        // Positive in-flight evidence: repeat the admission batch until the
        // status shows background convergence advanced across one batch.
        // Every round is asserted admitted, so the round that observes the
        // advance proves retrieval was served while convergence was running —
        // without asserting convergence had *not* finished, which races the
        // background worker.
        let deadline = Instant::now() + CONVERGENCE_WAIT;
        loop {
            let progress_before = read_progress().await;
            let round = tokio::join!(
                timed_raw(
                    &harness,
                    &project,
                    "tracedecay_grep",
                    json!({"pattern": PROBE_SYMBOL, "format": "json"}),
                ),
                timed_raw(
                    &harness,
                    &project,
                    "tracedecay_body",
                    json!({"symbol": PROBE_SYMBOL, "format": "json"}),
                ),
                timed_call(
                    &harness,
                    &project,
                    "tracedecay_message_search",
                    json!({"query": "billing pipeline", "limit": 5, "format": "json"}),
                ),
            );
            let (session_elapsed, session_envelope) = &round.2;
            let session = resolved(
                &harness,
                &project,
                "tracedecay_message_search",
                session_envelope.clone(),
            )
            .await;
            assert_admission_round(&round, *session_elapsed, &session);
            let progress_after = read_progress().await;
            if progress_after.advanced_from(&progress_before) {
                return (progress_before, progress_after);
            }
            assert!(
                Instant::now() < deadline,
                "background convergence never advanced while retrieval stayed admitted: \
                 {progress_after:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let ((status, sessions_for, convergence_elapsed), (progress_before, progress_after)) =
        tokio::join!(discovery, admissions);
    assert!(
        progress_after.advanced_from(&progress_before),
        "background convergence must be in flight across an admission batch: \
         {progress_before:?} -> {progress_after:?}"
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
    let search_payload =
        retained_payload(&resolved(&harness, &project, "tracedecay_message_search", search).await);
    assert_ne!(
        search_payload["status"],
        json!("error"),
        "direct-user search must stay typed, not a transport failure: {search_payload}"
    );
    let searched_sessions = message_hit_session_ids(&search_payload);
    assert_window_side(
        "12-hour direct-user search",
        &searched_sessions,
        true,
        &search_payload,
    );
    assert!(
        searched_sessions
            .iter()
            .any(|id| claude_replays(true).contains(id)),
        "12-hour search must keep the in-window Claude replays: {search_payload}"
    );

    // The exclusion above must be a window decision, not an empty corpus: the
    // complementary query over the same span returns exactly the replays the
    // 12-hour window drops, every one of them.
    let excluded_search = wait_for_pre_window_search(&harness, &project, origin, since).await;
    let excluded_sessions = message_hit_session_ids(&excluded_search);
    assert_window_side(
        "pre-window direct-user search",
        &excluded_sessions,
        false,
        &excluded_search,
    );
    for dropped in claude_replays(false) {
        assert!(
            excluded_sessions.contains(&dropped),
            "the rows the 12-hour window drops must exist and be findable: \
             {dropped} missing from {excluded_search}"
        );
    }

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
    let grep_payload =
        retained_payload(&resolved(&harness, &project, "tracedecay_lcm_grep", grep).await);
    for hit in grep_hits(&grep_payload) {
        let snippet = hit["snippet"].as_str().unwrap_or_default();
        assert!(
            snippet.chars().count() <= 8_192,
            "canonical redaction/content authority leaked an unbounded snippet: {hit}"
        );
    }
    let grepped_sessions = grep_session_ids(&grep_payload);
    assert_window_side(
        "12-hour direct-user lcm_grep",
        &grepped_sessions,
        true,
        &grep_payload,
    );
    assert!(
        grepped_sessions
            .iter()
            .any(|id| claude_replays(true).contains(id)),
        "12-hour grep must keep the in-window Claude replays: {grep_payload}"
    );

    assert!(
        raw_message_count(&status) >= RAW_MESSAGE_FLOOR,
        "ordinary ingest must land at least {RAW_MESSAGE_FLOOR} raw rows \
         before the window assertions mean anything: {status}"
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
