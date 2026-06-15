//! Regression tests for the LCM dashboard API fixes:
//! externalized-message rendering/search, summary-FTS column qualification,
//! session pagination + ordinal ordering, accurate engine reporting, and the
//! additive search pagination / message-enrichment fields.

mod common;

use std::path::Path;

use common::{
    create_runtime, get_json, http_agent, message_record_at, pick_free_port, tempdir_or_panic,
    wait_for_dashboard, EnvVarGuard, GLOBAL_DB_ENV, GLOBAL_DB_ENV_LOCK,
};
use serde_json::Value;
use tempfile::TempDir;
use tracedecay::dashboard;
use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::lcm::{LcmSourceRef, LcmStorageKind, LcmSummaryNodeDraft};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::TraceDecay;

const PROVIDER: &str = "cursor";
const SESSION_ID: &str = "sess-lcm-fixes";
const NEEDLE: &str = "zebraneedle";

struct DashboardFixture {
    _tmp: TempDir,
    _env_guard: EnvVarGuard,
    base_url: String,
    server: tokio::task::JoinHandle<()>,
    global_db_path: std::path::PathBuf,
    /// node_id of the summary node referencing msg-c.
    linked_node_id: String,
}

impl Drop for DashboardFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn setup_project(project_root: &Path) -> TraceDecay {
    if let Err(err) = std::fs::create_dir_all(project_root.join("src")) {
        panic!("failed to create project src dir: {err}");
    }
    if let Err(err) = std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn seed_fixture() -> &'static str { \"lcm-fixes\" }\n",
    ) {
        panic!("failed to write fixture lib.rs: {err}");
    }
    match TraceDecay::init(project_root).await {
        Ok(cg) => cg,
        Err(err) => panic!("failed to initialize tracedecay fixture project: {err}"),
    }
}

fn message(
    message_id: &str,
    role: &str,
    ordinal: i64,
    timestamp: i64,
    text: &str,
) -> SessionMessageRecord {
    message_record_at(
        PROVIDER,
        message_id,
        SESSION_ID,
        role,
        ordinal,
        Some(timestamp),
        text,
        "chat",
        Some("test-model"),
        None,
        None,
        None,
        None,
    )
}

async fn store_id_of(global_db: &GlobalDb, message_id: &str) -> i64 {
    match global_db.lcm_load_raw_message(PROVIDER, message_id).await {
        Some(record) => record.store_id,
        None => panic!("missing seeded message {message_id}"),
    }
}

fn summary_draft(
    summary_text: &str,
    expand_hint: Option<&str>,
    metadata_json: Option<&str>,
    source_time_end: i64,
    source_refs: Vec<LcmSourceRef>,
) -> LcmSummaryNodeDraft {
    LcmSummaryNodeDraft {
        provider: PROVIDER.to_string(),
        conversation_id: "conv-lcm-fixes".to_string(),
        session_id: SESSION_ID.to_string(),
        depth: 1,
        summary_text: summary_text.to_string(),
        source_refs,
        source_token_count: 100,
        summary_token_count: 40,
        source_time_start: Some(1_700_002_000),
        source_time_end: Some(source_time_end),
        expand_hint: expand_hint.map(str::to_string),
        metadata_json: metadata_json.map(str::to_string),
    }
}

/// Seeds the session, four messages (two same-second messages inserted out of
/// ordinal order, one with tool metadata, one externalized tool payload with
/// `content = NULL`), and three summary nodes. Returns the node_id of the
/// summary node that references msg-c.
async fn seed_lcm_fixture(
    global_db: &GlobalDb,
    storage_root: &Path,
    project_path: &Path,
) -> String {
    let session = SessionRecord {
        provider: PROVIDER.to_string(),
        session_id: SESSION_ID.to_string(),
        project_key: "tracedecay-lcm-fixes".to_string(),
        project_path: project_path.display().to_string(),
        title: Some("LCM fixes session".to_string()),
        started_at: Some(1_700_002_000),
        ended_at: None,
        transcript_path: None,
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    if !global_db.upsert_session(&session).await {
        panic!("failed to upsert session fixture");
    }

    // msg-b (ordinal 2) is inserted BEFORE msg-a (ordinal 1) with an
    // identical timestamp, so (timestamp, store_id) ordering would transpose
    // them while ordinal ordering must not.
    let msg_b = message(
        "msg-b",
        "assistant",
        2,
        1_700_002_000,
        "beta ordering message shared",
    );
    let msg_a = message(
        "msg-a",
        "user",
        1,
        1_700_002_000,
        "alpha ordering message shared",
    );
    let mut msg_c = message(
        "msg-c",
        "assistant",
        3,
        1_700_002_100,
        "gamma vector message shared",
    );
    msg_c.tool_names = Some("tracedecay_search".to_string());
    msg_c.metadata_json = Some("{\"fixture_marker\":\"msg-c-meta\"}".to_string());
    for msg in [&msg_b, &msg_a, &msg_c] {
        if !global_db.upsert_session_message(msg).await {
            panic!("failed to upsert message fixture {}", msg.message_id);
        }
    }

    // Externalized tool payload: > 256k chars of tool output forces
    // storage_kind = 'external' with content = NULL in lcm_raw_messages.
    // The searchable needle is planted into snippet_text/index_text after
    // ingest (see plant_external_needle), modeling the review scenario of a
    // NULL-content row whose derived text is the only searchable surface.
    let mut external_text = String::from("externalized tool payload head ");
    external_text.push_str(&"x".repeat(300_000));
    let mut external = message("msg-x", "tool", 4, 1_700_002_200, &external_text);
    external.kind = Some("tool_result".to_string());
    if let Err(err) = global_db
        .lcm_store(storage_root)
        .ingest_raw_message(&external)
        .await
    {
        panic!("failed to ingest externalized message fixture: {err}");
    }
    match global_db.lcm_load_raw_message(PROVIDER, "msg-x").await {
        Some(record) => assert!(
            matches!(record.storage_kind, LcmStorageKind::External),
            "fixture message msg-x must be externalized"
        ),
        None => panic!("missing seeded message msg-x"),
    }

    let store_a = store_id_of(global_db, "msg-a").await;
    let store_b = store_id_of(global_db, "msg-b").await;
    let store_c = store_id_of(global_db, "msg-c").await;

    // Node 1 carries the metadata_json over-match bait ("category":"general")
    // and an expand_hint term; nodes 2 and 3 exist for pagination.
    let node_1 = match global_db
        .lcm_insert_summary_node(summary_draft(
            "vector projection summary for caching decisions",
            Some("expandhint drilldown"),
            Some("{\"category\":\"general\",\"tags\":[\"vector\"]}"),
            1_700_002_300,
            vec![LcmSourceRef::RawMessage { store_id: store_c }],
        ))
        .await
    {
        Ok(node) => node.node_id,
        Err(err) => panic!("failed to insert summary node 1: {err}"),
    };
    for (text, time_end, store_id) in [
        ("second summary block two", 1_700_002_400_i64, store_a),
        ("third summary block three", 1_700_002_500_i64, store_b),
    ] {
        if let Err(err) = global_db
            .lcm_insert_summary_node(summary_draft(
                text,
                None,
                None,
                time_end,
                vec![LcmSourceRef::RawMessage { store_id }],
            ))
            .await
        {
            panic!("failed to insert summary node fixture: {err}");
        }
    }
    node_1
}

async fn open_raw_conn(global_db_path: &Path) -> libsql::Connection {
    let db = match libsql::Builder::new_local(global_db_path).build().await {
        Ok(db) => db,
        Err(err) => panic!("failed to open global db directly: {err}"),
    };
    let conn = match db.connect() {
        Ok(conn) => conn,
        Err(err) => panic!("failed to connect to global db directly: {err}"),
    };
    // The running dashboard writes to this same store (e.g. the token-count
    // warm task's sidecar upserts); wait out transient write locks instead
    // of failing the fixture mutation.
    if let Err(err) = conn.execute_batch("PRAGMA busy_timeout = 5000;").await {
        panic!("failed to set busy_timeout on raw connection: {err}");
    }
    conn
}

/// Rewrites the externalized message's derived text columns to contain the
/// search needle. The FTS update trigger keeps the index in sync, so the row
/// is findable through both FTS and LIKE while content stays NULL.
async fn plant_external_needle(global_db_path: &Path) {
    let conn = open_raw_conn(global_db_path).await;
    let derived = format!("{NEEDLE} externalized snippet preview");
    if let Err(err) = conn
        .execute(
            "UPDATE lcm_raw_messages
             SET snippet_text = ?1, index_text = ?1
             WHERE provider = ?2 AND message_id = 'msg-x'",
            libsql::params![derived.as_str(), PROVIDER],
        )
        .await
    {
        panic!("failed to plant needle into externalized message: {err}");
    }
}

/// Drops the raw-message FTS table/triggers so message FTS queries fail and
/// the search endpoint must take the LIKE fallback for messages while node
/// FTS keeps working (the engine-accuracy scenario).
async fn drop_raw_message_fts(global_db_path: &Path) {
    let conn = open_raw_conn(global_db_path).await;
    if let Err(err) = conn
        .execute_batch(
            "DROP TRIGGER IF EXISTS lcm_raw_messages_fts_insert;
             DROP TRIGGER IF EXISTS lcm_raw_messages_fts_delete;
             DROP TRIGGER IF EXISTS lcm_raw_messages_fts_update;
             DROP TABLE IF EXISTS lcm_raw_messages_fts;",
        )
        .await
    {
        panic!("failed to drop raw message FTS objects: {err}");
    }
}

async fn corrupt_summary_node_metadata(global_db_path: &Path, node_id: &str) {
    let conn = open_raw_conn(global_db_path).await;
    if let Err(err) = conn
        .execute(
            "UPDATE lcm_summary_nodes SET metadata_json = '{not-json' WHERE node_id = ?1",
            libsql::params![node_id],
        )
        .await
    {
        panic!("failed to corrupt summary metadata fixture: {err}");
    }
}

async fn start_fixture(break_message_fts: bool) -> DashboardFixture {
    let tmp = tempdir_or_panic();
    let project_root = tmp.path().join("project");
    let global_db_path = tmp.path().join("global").join("global.db");
    let env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);

    let cg = setup_project(&project_root).await;

    let global_db = match GlobalDb::open_at(&global_db_path).await {
        Some(db) => db,
        None => panic!(
            "failed to open temporary global DB at {}",
            global_db_path.display()
        ),
    };
    let storage_root = tmp.path().join("lcm-storage");
    if let Err(err) = std::fs::create_dir_all(&storage_root) {
        panic!("failed to create LCM storage root: {err}");
    }
    let linked_node_id = seed_lcm_fixture(&global_db, &storage_root, &project_root).await;
    drop(global_db);

    plant_external_needle(&global_db_path).await;
    if break_message_fts {
        drop_raw_message_fts(&global_db_path).await;
    }

    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let server = tokio::spawn(async move {
        let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
    });

    let agent = http_agent();
    wait_for_dashboard(&agent, &base_url).await;

    DashboardFixture {
        _tmp: tmp,
        _env_guard: env_guard,
        base_url,
        server,
        global_db_path,
        linked_node_id,
    }
}

fn as_array<'a>(value: &'a Value, what: &str) -> &'a Vec<Value> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("expected {what} to be an array, got {value}"))
}

fn message_by_id<'a>(messages: &'a [Value], message_id: &str) -> &'a Value {
    messages
        .iter()
        .find(|row| row["message_id"] == message_id)
        .unwrap_or_else(|| panic!("expected message {message_id} in payload"))
}

/// Inserts a raw LCM message with `timestamp = NULL` (the shape legacy Cursor
/// ingest produced) directly into the store the dashboard is serving.
async fn insert_undated_message(global_db_path: &Path) {
    let conn = open_raw_conn(global_db_path).await;
    if let Err(err) = conn
        .execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, timestamp,
                content, content_hash, storage_kind, payload_ref, snippet_text,
                index_text, legacy_source, legacy_truncated, metadata_json
             )
             VALUES (?1, 'msg-undated', ?2, 'user', 99, NULL,
                     'undated legacy message', 'hash-undated', 'inline', NULL,
                     'undated legacy message', 'undated legacy message', 0, 0, NULL)",
            libsql::params![PROVIDER, SESSION_ID],
        )
        .await
    {
        panic!("failed to insert undated message fixture: {err}");
    }
}

#[test]
fn timeline_excludes_null_timestamps_and_reports_undated_count() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        insert_undated_message(&fixture.global_db_path).await;
        let agent = http_agent();

        let (status, timeline) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/timeline?bucket=day&limit=400",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);

        // All four dated fixture messages share 2023-11-14; the NULL-timestamp
        // row must not appear as a bucket (the old behavior rendered it as a
        // single fake bar) and is reported via the explicit aggregate instead.
        let buckets = as_array(&timeline["buckets"], "timeline buckets");
        assert_eq!(buckets.len(), 1, "expected one dated bucket: {timeline}");
        assert_eq!(buckets[0]["bucket"], "2023-11-14");
        assert_eq!(buckets[0]["count"], 4);
        assert!(
            buckets.iter().all(|bucket| !bucket["bucket"].is_null()),
            "NULL timestamps must never surface as a bucket: {timeline}"
        );
        assert_eq!(timeline["undated"]["count"], 1, "undated: {timeline}");
        assert!(
            timeline["undated"]["token_estimate"].as_i64().unwrap_or(0) > 0,
            "undated token estimate should count the row's text: {timeline}"
        );

        // Session-scoped queries keep both aggregates scoped.
        let (status, scoped) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/timeline?bucket=day&session_id={SESSION_ID}",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(scoped["undated"]["count"], 1, "scoped undated: {scoped}");
        let (status, other) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/timeline?bucket=day&session_id=other-session",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(other["undated"]["count"], 0, "other-session: {other}");
        assert!(as_array(&other["buckets"], "other buckets").is_empty());
    });
}

#[test]
fn malformed_summary_metadata_surfaces_json_error_instead_of_empty_rows() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        corrupt_summary_node_metadata(&fixture.global_db_path, &fixture.linked_node_id).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 422);
        assert!(
            overview["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("malformed metadata_json"),
            "malformed summary metadata must surface a JSON error detail, got {overview}"
        );
    });
}

#[test]
fn lcm_bad_params_and_missing_resources_return_json_errors() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        let agent = http_agent();

        let (status, bad_query) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=shared&limit=not-a-number",
                fixture.base_url
            ),
        );
        assert_eq!(status, 400);
        assert!(
            bad_query["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("limit"),
            "bad query rejection must be JSON with detail, got {bad_query}"
        );

        let (status, missing_session) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/session/missing-session",
                fixture.base_url
            ),
        );
        assert_eq!(status, 404);
        assert!(
            missing_session["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("missing-session"),
            "missing session body should carry the requested id"
        );

        let (status, missing_node) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/node/missing-node",
                fixture.base_url
            ),
        );
        assert_eq!(status, 404);
        assert!(
            missing_node["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("missing-node"),
            "missing node body should carry the requested id"
        );
    });
}

#[test]
fn session_endpoint_orders_by_ordinal_paginates_nodes_and_enriches_messages() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        let agent = http_agent();

        let (status, session) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/session/{SESSION_ID}?limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(session["counts"]["message_count"], 4);
        assert_eq!(session["counts"]["summary_node_count"], 3);

        // Fix 5: ordinal ordering. msg-b was inserted before msg-a with the
        // same timestamp; (timestamp, store_id) ordering would return b
        // first, ordinal ordering must return a first.
        let messages = as_array(&session["messages"], "session.messages");
        assert_eq!(messages.len(), 4);
        let ids: Vec<&str> = messages
            .iter()
            .map(|row| row["message_id"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            ids,
            vec!["msg-a", "msg-b", "msg-c", "msg-x"],
            "messages must be ordered by ordinal, not (timestamp, store_id)"
        );

        // Fix 1: the externalized message (content = NULL in the store) must
        // render via the snippet_text fallback.
        let external = message_by_id(messages, "msg-x");
        let content = external["content"].as_str().unwrap_or_default();
        assert!(
            content.contains(NEEDLE),
            "externalized message content must fall back to snippet_text, got {content:?}"
        );
        assert!(
            external["token_estimate"].as_i64().unwrap_or(0) > 0,
            "externalized message must have a non-zero token estimate"
        );
        assert_eq!(external["storage_kind"], "external");

        // Wishlist 2: richer message metadata.
        let enriched = message_by_id(messages, "msg-c");
        assert_eq!(enriched["ordinal"], 3);
        assert_eq!(enriched["pinned"], 0);
        assert_eq!(enriched["tool_name"], "tracedecay_search");
        assert!(
            enriched["metadata_json"]
                .as_str()
                .unwrap_or_default()
                .contains("fixture_marker"),
            "metadata_json must be exposed on message rows"
        );

        // Wishlist 3: message → summary-node linkage.
        let linked = as_array(&enriched["summary_node_ids"], "summary_node_ids");
        assert!(
            linked
                .iter()
                .any(|id| id == fixture.linked_node_id.as_str()),
            "msg-c must link back to its summary node {}",
            fixture.linked_node_id
        );

        // Fix 3: summary nodes are paginated with the same limit/offset
        // scheme as messages.
        let (status, page_1) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/session/{SESSION_ID}?limit=2",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(as_array(&page_1["summary_nodes"], "summary_nodes").len(), 2);
        assert_eq!(page_1["has_more_summary_nodes"], true);
        assert_eq!(page_1["has_more"], true);

        let (status, page_2) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/session/{SESSION_ID}?limit=2&offset=2",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(as_array(&page_2["summary_nodes"], "summary_nodes").len(), 1);
        assert_eq!(page_2["has_more_summary_nodes"], false);
        assert_eq!(page_2["has_more_messages"], false);
        assert_eq!(page_2["has_more"], false);

        // The node endpoint shares MESSAGE_COLUMNS; its source messages must
        // also carry the parsed linkage array.
        let (status, node) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/node/{}",
                fixture.base_url, fixture.linked_node_id
            ),
        );
        assert_eq!(status, 200);
        let node_messages = as_array(&node["sources"]["messages"], "node.sources.messages");
        assert_eq!(node_messages.len(), 1);
        assert_eq!(node_messages[0]["message_id"], "msg-c");
        assert!(
            as_array(&node_messages[0]["summary_node_ids"], "summary_node_ids")
                .iter()
                .any(|id| id == fixture.linked_node_id.as_str()),
            "node source message must include summary_node_ids"
        );
    });
}

#[test]
fn search_matches_externalized_messages_and_qualifies_summary_fts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(false).await;
        let agent = http_agent();

        // Fix 1 (FTS mode): the externalized message is indexed via
        // index_text and must surface with non-empty fallback content.
        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q={NEEDLE}&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["engine"], "fts");
        assert_eq!(search["engine_detail"]["messages"], "fts");
        assert_eq!(search["engine_detail"]["summary_nodes"], "fts");
        let matches = as_array(&search["matches"]["messages"], "search message matches");
        assert_eq!(
            matches.len(),
            1,
            "needle must match the externalized message"
        );
        assert!(
            matches[0]["content"]
                .as_str()
                .unwrap_or_default()
                .contains(NEEDLE),
            "externalized search hit must render fallback content"
        );
        assert_eq!(search["total"]["messages"], 1);

        // Fix 1 (overview LIKE mode): the overview search is always LIKE and
        // must also find the externalized message via index/snippet text.
        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?q={NEEDLE}&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let overview_matches =
            as_array(&overview["matches"]["messages"], "overview message matches");
        assert_eq!(
            overview_matches.len(),
            1,
            "overview LIKE search must match the externalized message"
        );

        // Fix 2: "general" only appears inside summary metadata_json
        // ("category":"general"); the qualified MATCH must not return it.
        let (status, general) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=general&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(general["engine"], "fts");
        assert_eq!(
            as_array(&general["matches"]["summary_nodes"], "summary node matches").len(),
            0,
            "metadata_json-only terms must not match summary nodes"
        );
        assert_eq!(general["total"]["summary_nodes"], 0);

        // Fix 2 (positive cases): summary_text and expand_hint still match.
        for query in ["caching", "expandhint"] {
            let (status, hit) = get_json(
                &agent,
                &format!(
                    "{}/api/plugins/hermes-lcm/search?q={query}&limit=20",
                    fixture.base_url
                ),
            );
            assert_eq!(status, 200);
            assert_eq!(
                as_array(&hit["matches"]["summary_nodes"], "summary node matches").len(),
                1,
                "query {query} must match the summary node"
            );
        }

        // Wishlist 1: totals + offset pagination across the full result set.
        let (status, page_1) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=shared&limit=1",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(page_1["limit"], 1);
        assert_eq!(page_1["offset"], 0);
        assert_eq!(page_1["total"]["messages"], 3);
        let page_1_rows = as_array(&page_1["matches"]["messages"], "page 1 matches");
        assert_eq!(page_1_rows.len(), 1);

        let (status, page_2) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=shared&limit=1&offset=1",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(page_2["offset"], 1);
        assert_eq!(page_2["total"]["messages"], 3);
        let page_2_rows = as_array(&page_2["matches"]["messages"], "page 2 matches");
        assert_eq!(page_2_rows.len(), 1);
        assert_ne!(
            page_1_rows[0]["store_id"], page_2_rows[0]["store_id"],
            "offset pagination must advance through the result set"
        );

        let (status, page_4) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=shared&limit=1&offset=3",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(
            as_array(&page_4["matches"]["messages"], "past-the-end matches").len(),
            0,
            "offset past the result set must return an empty page"
        );
        assert_eq!(page_4["total"]["messages"], 3);
    });
}

#[test]
fn search_engine_flag_reports_like_fallback_accurately() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_fixture(true).await;
        let agent = http_agent();

        // Fix 4: with the raw-message FTS table dropped, message search must
        // fall back to LIKE while node FTS still works; the top-level engine
        // flag must report the worst case instead of claiming "fts".
        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=shared&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["engine_detail"]["messages"], "like");
        assert_eq!(search["engine_detail"]["summary_nodes"], "fts");
        assert_eq!(
            search["engine"], "like",
            "engine must not claim fts when messages fell back to LIKE"
        );
        assert_eq!(
            as_array(&search["matches"]["messages"], "LIKE fallback matches").len(),
            3,
            "LIKE fallback must still match the seeded messages"
        );
        assert_eq!(search["total"]["messages"], 3);

        // Fix 1 (LIKE mode): the externalized message has content = NULL and
        // must still be found through index_text/snippet_text.
        let (status, needle) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q={NEEDLE}&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(needle["engine_detail"]["messages"], "like");
        let rows = as_array(&needle["matches"]["messages"], "needle LIKE matches");
        assert_eq!(
            rows.len(),
            1,
            "externalized message must be searchable in LIKE mode"
        );
        assert!(
            rows[0]["content"]
                .as_str()
                .unwrap_or_default()
                .contains(NEEDLE),
            "LIKE hit must render fallback content"
        );
        assert!(
            rows[0]["snippet"]
                .as_str()
                .unwrap_or_default()
                .contains(NEEDLE),
            "LIKE snippet must come from the fallback content"
        );
    });
}
