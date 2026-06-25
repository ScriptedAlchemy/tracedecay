mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use common::{
    create_runtime, get_json, http_agent, pick_free_port, response_to_json, tempdir_or_panic,
    wait_for_dashboard, EnvVarGuard, GLOBAL_DB_ENV, GLOBAL_DB_ENV_LOCK,
};
use serde_json::Value;
use tempfile::TempDir;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::dashboard;
use tracedecay::errors::TraceDecayError;
use tracedecay::global_db::GlobalDb;
use tracedecay::memory::encoding::HolographicEncoder;
use tracedecay::sessions::lcm::{LcmSourceRef, LcmSummaryNodeDraft};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::storage::{write_enrollment_marker, EnrollmentMarker, StorageMode};
use tracedecay::tracedecay::TraceDecay;

/// Longer than 200 chars on purpose: list/projection payloads truncate
/// `content` at 200, so this fact proves the `/fact/{id}` detail endpoint
/// returns the full text.
const LONG_FACT_CONTENT: &str = "LCM dashboard empty states need explicit copy. \
The drawer, search results, charts, and overview panels must each explain why \
they are empty and what action will populate them, because first-run users \
otherwise assume the integration is broken when the store simply has no rows yet.";

struct DashboardFixture {
    _tmp: TempDir,
    _env_guard: EnvVarGuard,
    _data_dir_guard: EnvVarGuard,
    base_url: String,
    project_root: std::path::PathBuf,
    project_db_path: std::path::PathBuf,
    server: DashboardServer,
}

impl Drop for DashboardFixture {
    fn drop(&mut self) {
        self.server.stop();
    }
}

struct DashboardServer {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl DashboardServer {
    fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DashboardServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_dashboard_server(cg: TraceDecay, port: u16) -> DashboardServer {
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let thread = thread::spawn(move || {
        let runtime = create_runtime();
        runtime.block_on(async move {
            let result = dashboard::run_until_shutdown(&cg, "127.0.0.1", port, false, async move {
                let _ = shutdown_rx.await;
            })
            .await;
            let _ = cg.checkpoint().await;
            cg.close();
            let _ = result;
        });
    });
    DashboardServer {
        shutdown: Some(shutdown),
        thread: Some(thread),
    }
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            panic!("failed to create {}: {err}", parent.display());
        }
    }
    if let Err(err) = fs::write(path, content) {
        panic!("failed to write {}: {err}", path.display());
    }
}

async fn setup_project(project_root: &Path) -> TraceDecay {
    write_file(
        &project_root.join("src/lib.rs"),
        "pub fn seed_fixture() -> &'static str { \"dashboard\" }\n",
    );
    match TraceDecay::init(project_root).await {
        Ok(cg) => cg,
        Err(err) => panic!("failed to initialize tracedecay fixture project: {err}"),
    }
}

fn blob_param(bytes: Vec<u8>) -> libsql::Value {
    libsql::Value::Blob(bytes)
}

async fn seed_memory_fixture(cg: &TraceDecay) {
    let conn = cg.db().conn();
    let vec_a = match HolographicEncoder::serialize(&[0.20, 0.35, 0.50]) {
        Ok(value) => value,
        Err(err) => panic!("failed to serialize vec_a: {err}"),
    };
    let vec_b = match HolographicEncoder::serialize(&[0.21, 0.34, 0.49]) {
        Ok(value) => value,
        Err(err) => panic!("failed to serialize vec_b: {err}"),
    };
    let vec_c = match HolographicEncoder::serialize(&[2.1, -1.2, 0.9]) {
        Ok(value) => value,
        Err(err) => panic!("failed to serialize vec_c: {err}"),
    };
    let bank_a = match HolographicEncoder::serialize(&[0.1, 0.2, 0.3]) {
        Ok(value) => value,
        Err(err) => panic!("failed to serialize bank_a: {err}"),
    };
    let bank_b = match HolographicEncoder::serialize(&[0.4, 0.5, 0.6]) {
        Ok(value) => value,
        Err(err) => panic!("failed to serialize bank_b: {err}"),
    };

    let inserts = [
        (
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at, hrr_vector, hrr_algebra, hrr_dim)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            libsql::params![
                101_i64,
                "Cache invalidation policy must be explicit",
                "project",
                "[\"cache\",\"policy\"]",
                0.97_f64,
                8_i64,
                5_i64,
                1_700_000_000_i64,
                1_700_000_100_i64,
                blob_param(vec_a.clone()),
                "amari_fhrr",
                HolographicEncoder::DIMENSIONS as i64
            ],
        ),
        (
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at, hrr_vector, hrr_algebra, hrr_dim)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            libsql::params![
                102_i64,
                "Cache invalidation policy must stay explicit",
                "project",
                "[\"cache\",\"policy\"]",
                0.95_f64,
                6_i64,
                4_i64,
                1_700_000_010_i64,
                1_700_000_110_i64,
                blob_param(vec_b.clone()),
                "amari_fhrr",
                HolographicEncoder::DIMENSIONS as i64
            ],
        ),
        (
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at, hrr_vector, hrr_algebra, hrr_dim)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            libsql::params![
                103_i64,
                LONG_FACT_CONTENT,
                "tool",
                "[\"lcm\",\"ux\"]",
                0.76_f64,
                3_i64,
                2_i64,
                1_700_000_020_i64,
                1_700_000_120_i64,
                blob_param(vec_c.clone()),
                "amari_fhrr",
                HolographicEncoder::DIMENSIONS as i64
            ],
        ),
    ];
    for (sql, params) in inserts {
        if let Err(err) = conn.execute(sql, params).await {
            panic!("failed to insert memory fact: {err}");
        }
    }

    let entity_rows = [
        (
            201_i64,
            "CachePolicy",
            "cachepolicy",
            "concept",
            "[\"cache policy\"]",
        ),
        (202_i64, "LCMTab", "lcmtab", "feature", "[\"lcm tab\"]"),
        (203_i64, "SimilarityView", "similarityview", "feature", "[]"),
    ];
    for (entity_id, name, normalized_name, entity_type, aliases) in entity_rows {
        if let Err(err) = conn
            .execute(
                "INSERT INTO memory_entities
                    (entity_id, name, normalized_name, entity_type, aliases, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                libsql::params![
                    entity_id,
                    name,
                    normalized_name,
                    entity_type,
                    aliases,
                    1_700_000_050_i64
                ],
            )
            .await
        {
            panic!("failed to insert memory entity: {err}");
        }
    }

    let joins = [
        (101_i64, 201_i64),
        (102_i64, 201_i64),
        (103_i64, 202_i64),
        (103_i64, 203_i64),
    ];
    for (fact_id, entity_id) in joins {
        if let Err(err) = conn
            .execute(
                "INSERT INTO memory_fact_entities (fact_id, entity_id) VALUES (?1, ?2)",
                libsql::params![fact_id, entity_id],
            )
            .await
        {
            panic!("failed to insert memory_fact_entities row: {err}");
        }
    }

    // The "project" bank's stored fact_count is deliberately stale (5 vs the
    // 2 live project facts): bank counts are denormalized snapshots from the
    // last bundle rebuild, and the overview API must report live membership.
    let bank_rows = [("project", bank_a, 5_i64), ("tool", bank_b, 1_i64)];
    for (name, vector, fact_count) in bank_rows {
        if let Err(err) = conn
            .execute(
                "INSERT INTO memory_banks
                    (bank_name, vector, hrr_dim, fact_count, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                libsql::params![
                    name,
                    blob_param(vector),
                    3_i64,
                    fact_count,
                    1_700_000_130_i64
                ],
            )
            .await
        {
            panic!("failed to insert memory bank: {err}");
        }
    }
}

async fn seed_lcm_fixture(global_db: &GlobalDb, project_path: &Path) {
    let session = SessionRecord {
        provider: "cursor".to_string(),
        session_id: "sess-dashboard-1".to_string(),
        project_key: "tracedecay-fixture".to_string(),
        project_path: project_path.display().to_string(),
        title: Some("Dashboard fixture session".to_string()),
        started_at: Some(1_700_001_000),
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

    let messages = [
        SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "msg-1".to_string(),
            session_id: "sess-dashboard-1".to_string(),
            role: "user".to_string(),
            timestamp: Some(1_700_001_010),
            ordinal: 1,
            text: "Need a vector projection for memory similarity.".to_string(),
            kind: Some("chat".to_string()),
            model: Some("gpt".to_string()),
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
        SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "msg-2".to_string(),
            session_id: "sess-dashboard-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_700_001_020),
            ordinal: 2,
            text: "Similarity pair detected for cache policy facts.".to_string(),
            kind: Some("chat".to_string()),
            model: Some("gpt".to_string()),
            tool_names: Some("tracedecay_search".to_string()),
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
        SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id: "msg-3".to_string(),
            session_id: "sess-dashboard-1".to_string(),
            role: "assistant".to_string(),
            timestamp: Some(1_700_001_030),
            ordinal: 3,
            text: "LCM tab should render non-empty overview cards.".to_string(),
            kind: Some("chat".to_string()),
            model: Some("gpt".to_string()),
            tool_names: Some("tracedecay_lcm_status".to_string()),
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
    ];

    for message in messages {
        if !global_db.upsert_session_message(&message).await {
            panic!(
                "failed to upsert LCM message fixture {}",
                message.message_id
            );
        }
    }

    let msg_1 = match global_db.lcm_load_raw_message("cursor", "msg-1").await {
        Some(record) => record.store_id,
        None => panic!("missing seeded message msg-1"),
    };
    let msg_2 = match global_db.lcm_load_raw_message("cursor", "msg-2").await {
        Some(record) => record.store_id,
        None => panic!("missing seeded message msg-2"),
    };

    let draft = LcmSummaryNodeDraft {
        provider: "cursor".to_string(),
        conversation_id: "conv-dashboard".to_string(),
        session_id: "sess-dashboard-1".to_string(),
        depth: 1,
        summary_text: "Vector projection summary for cache policy similarities.".to_string(),
        source_refs: vec![
            LcmSourceRef::RawMessage { store_id: msg_1 },
            LcmSourceRef::RawMessage { store_id: msg_2 },
        ],
        source_token_count: 180,
        summary_token_count: 72,
        source_time_start: Some(1_700_001_010),
        source_time_end: Some(1_700_001_030),
        expand_hint: Some("Use summary detail drawer".to_string()),
        metadata_json: Some(
            "{\"category\":\"analysis\",\"tags\":[\"vector\"],\"entities\":[\"cache\"]}"
                .to_string(),
        ),
    };
    if let Err(err) = global_db.lcm_insert_summary_node(draft).await {
        panic!("failed to insert summary node fixture: {err}");
    }
}

fn post_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = match agent.post(url).send_empty() {
        Ok(response) => response,
        Err(err) => panic!("POST {url} failed: {err}"),
    };
    response_to_json(response)
}

fn post_json_body(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response = match agent.post(url).send_json(body) {
        Ok(response) => response,
        Err(err) => panic!("POST {url} (with body) failed: {err}"),
    };
    response_to_json(response)
}

fn patch_json_body(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response = match agent.patch(url).send_json(body) {
        Ok(response) => response,
        Err(err) => panic!("PATCH {url} (with body) failed: {err}"),
    };
    response_to_json(response)
}

fn delete_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = match agent.delete(url).call() {
        Ok(response) => response,
        Err(err) => panic!("DELETE {url} failed: {err}"),
    };
    response_to_json(response)
}

struct FakeCodexAppServer {
    _temp: TempDir,
    bin: PathBuf,
}

impl FakeCodexAppServer {
    fn new_memory_curator() -> Self {
        let temp = tempdir_or_panic();
        let bin = temp.path().join("codex");
        let script = r#"#!/usr/bin/env python3
import json
import os
import sys

if len(sys.argv) != 2 or sys.argv[1] != "app-server":
    sys.exit(42)
if os.environ.get("TRACEDECAY_CODEX_SUMMARY_CHILD") != "1":
    sys.exit(43)

for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        print(json.dumps({"id": msg.get("id"), "result": {}}), flush=True)
    elif method == "thread/start":
        print(json.dumps({
            "id": msg.get("id"),
            "result": {"thread": {"id": "thread-dashboard", "model": "dashboard-fake-model"}}
        }), flush=True)
    elif method == "turn/start":
        payload = {
            "ops": [{
                "cluster_id": "cluster-0000",
                "op": "delete",
                "fact_id": 102,
                "confidence": 0.98,
                "reason": "near duplicate of fact 101"
            }]
        }
        print(json.dumps({
            "method": "item/agentMessage/delta",
            "params": {"delta": json.dumps(payload), "model": "dashboard-fake-model"}
        }), flush=True)
        print(json.dumps({"method": "turn/completed"}), flush=True)
        break
"#;
        write_file(&bin, script);
        make_executable(&bin);
        Self { _temp: temp, bin }
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .unwrap_or_else(|err| panic!("failed to stat {}: {err}", path.display()))
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .unwrap_or_else(|err| panic!("failed to chmod {}: {err}", path.display()));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

async fn start_dashboard_fixture(seed_lcm: bool) -> DashboardFixture {
    let tmp = tempdir_or_panic();
    let tmp_root = tmp
        .path()
        .canonicalize()
        .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
    let project_root = tmp_root.join("project");
    let global_db_path = tmp_root.join("global").join("global.db");
    let profile_root = tmp_root.join("profile").join(".tracedecay");
    let env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
    let data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
    if let Err(err) = write_enrollment_marker(
        &project_root,
        &EnrollmentMarker {
            project_id: "dashboard_fixture".to_string(),
            storage_mode: StorageMode::ProfileSharded,
        },
    ) {
        panic!("failed to enroll dashboard fixture in profile storage: {err}");
    }

    let cg = setup_project(&project_root).await;
    seed_memory_fixture(&cg).await;

    let global_db = match GlobalDb::open_at(&global_db_path).await {
        Some(db) => db,
        None => panic!(
            "failed to open temporary global DB at {}",
            global_db_path.display()
        ),
    };
    drop(global_db);
    if seed_lcm {
        let session_store = open_project_session_store(&project_root).await;
        seed_lcm_fixture(&session_store, &project_root).await;
        drop(session_store);
    }

    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let project_db_path = cg.store_layout().graph_db_path.clone();
    let server = spawn_dashboard_server(cg, port);

    let agent = http_agent();
    wait_for_dashboard(&agent, &base_url).await;

    DashboardFixture {
        _tmp: tmp,
        _env_guard: env_guard,
        _data_dir_guard: data_dir_guard,
        base_url,
        project_root,
        project_db_path,
        server,
    }
}

/// Counts rows in the fixture's project DB matching `sql` (a SELECT COUNT query
/// with one `?1` bind), via a fresh read connection. Used to prove hard deletes
/// actually removed rows (and their entity links) from the store that
/// `tracedecay_fact_store` recall reads.
async fn count_in_project_db(fixture: &DashboardFixture, sql: &str, fact_id: i64) -> i64 {
    let db = match libsql::Builder::new_local(&fixture.project_db_path)
        .build()
        .await
    {
        Ok(db) => db,
        Err(err) => panic!("failed to open project DB for verification: {err}"),
    };
    let conn = match db.connect() {
        Ok(conn) => conn,
        Err(err) => panic!("failed to connect to project DB: {err}"),
    };
    let mut rows = match conn.query(sql, libsql::params![fact_id]).await {
        Ok(rows) => rows,
        Err(err) => panic!("verification query failed: {err}"),
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<i64>(0).unwrap_or(-1),
        Ok(None) => -1,
        Err(err) => panic!("verification row read failed: {err}"),
    }
}

async fn string_in_project_db(
    fixture: &DashboardFixture,
    sql: &str,
    fact_id: i64,
) -> Option<String> {
    let conn = project_db_conn(fixture).await;
    let mut rows = match conn.query(sql, libsql::params![fact_id]).await {
        Ok(rows) => rows,
        Err(err) => panic!("verification query failed: {err}"),
    };
    match rows.next().await {
        Ok(Some(row)) => row.get::<String>(0).ok(),
        Ok(None) => None,
        Err(err) => panic!("verification row read failed: {err}"),
    }
}

async fn project_db_conn(fixture: &DashboardFixture) -> libsql::Connection {
    let db = match libsql::Builder::new_local(&fixture.project_db_path)
        .build()
        .await
    {
        Ok(db) => db,
        Err(err) => panic!("failed to open project DB directly: {err}"),
    };
    let conn = match db.connect() {
        Ok(conn) => conn,
        Err(err) => panic!("failed to connect to project DB directly: {err}"),
    };
    // The running dashboard can write to this store concurrently; wait out
    // transient write locks instead of failing the fixture mutation.
    if let Err(err) = conn.execute_batch("PRAGMA busy_timeout = 5000;").await {
        panic!("failed to set busy_timeout on project DB connection: {err}");
    }
    conn
}

/// Swaps a fact's vector the way every production re-encode does: alongside
/// an `updated_at` bump (`update_fact` / `update_fact_vector` always bump it;
/// the startup repair only fills NULL vectors, which changes the vectored
/// count instead). The similarity cache fingerprint is metadata-only and
/// relies on exactly that contract.
async fn set_fact_vector_and_bump_updated_at(
    fixture: &DashboardFixture,
    fact_id: i64,
    phases: &[f64],
) {
    let conn = project_db_conn(fixture).await;
    let vector = match HolographicEncoder::serialize(phases) {
        Ok(vector) => vector,
        Err(err) => panic!("failed to serialize replacement vector: {err}"),
    };
    if let Err(err) = conn
        .execute(
            "UPDATE memory_facts
             SET hrr_vector = ?1, hrr_algebra = 'amari_fhrr', hrr_dim = ?2,
                 updated_at = updated_at + 1
             WHERE fact_id = ?3",
            libsql::params![blob_param(vector), phases.len() as i64, fact_id],
        )
        .await
    {
        panic!("failed to update fact vector fixture: {err}");
    }
}

async fn clear_fact_vector_without_touching_updated_at(fixture: &DashboardFixture, fact_id: i64) {
    let conn = project_db_conn(fixture).await;
    if let Err(err) = conn
        .execute(
            "UPDATE memory_facts
             SET hrr_vector = NULL
             WHERE fact_id = ?1",
            libsql::params![fact_id],
        )
        .await
    {
        panic!("failed to clear fact vector fixture: {err}");
    }
}

async fn set_fact_access_without_touching_updated_at(
    fixture: &DashboardFixture,
    fact_id: i64,
    access_count: i64,
    last_recalled_at: i64,
) {
    let conn = project_db_conn(fixture).await;
    if let Err(err) = conn
        .execute(
            "UPDATE memory_facts
             SET access_count = ?1, last_recalled_at = ?2
             WHERE fact_id = ?3",
            libsql::params![access_count, last_recalled_at, fact_id],
        )
        .await
    {
        panic!("failed to update fact access fixture: {err}");
    }
}

fn git(project: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(project)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit_all(project: &Path, message: &str) {
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=TraceDecay Test",
            "-c",
            "user.email=tracedecay-test@example.com",
            "commit",
            "-m",
            message,
        ],
    );
}

async fn index_all_retrying_sync_lock(cg: &TraceDecay, context: &str) {
    for attempt in 0..20 {
        match cg.index_all().await {
            Ok(_) => return,
            Err(TraceDecayError::SyncLock { .. }) if attempt < 19 => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(err) => panic!("{context}: {err}"),
        }
    }
}

#[test]
fn dashboard_memory_repairs_vectors_and_invalidates_similarity_cache() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, initial) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/similarity?min_similarity=0.99&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(
            initial["pairs"].as_array().map(Vec::len),
            Some(3),
            "dashboard startup should repair stale seeded vectors before similarity reads"
        );

        set_fact_access_without_touching_updated_at(&fixture, 102, 7, 1_700_000_500).await;
        let (status, curate_after_access) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        let access_action = curate_after_access["actions"]
            .as_array()
            .and_then(|actions| {
                actions
                    .iter()
                    .find(|action| action["fact_id"].as_i64() == Some(102))
            })
            .unwrap_or_else(|| {
                panic!("expected dry-run delete action for fact 102: {curate_after_access}")
            });
        assert_eq!(
            access_action["access_count"], 7,
            "access-only updates must invalidate cached curation metadata"
        );

        set_fact_vector_and_bump_updated_at(&fixture, 103, &[0.20, 0.35, 0.50]).await;
        let (status, repaired_cache) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/similarity?min_similarity=0.99&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(
            repaired_cache["pairs"].as_array().map(Vec::len),
            Some(1),
            "updated_at-only vector rewrites must invalidate the similarity cache even when the rewritten fact no longer participates in the repaired 2048-dim pair set; got {repaired_cache}"
        );

        clear_fact_vector_without_touching_updated_at(&fixture, 103).await;
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let project_root = fixture.project_root.clone();
        let cg = match TraceDecay::open(&project_root).await {
            Ok(cg) => cg,
            Err(err) => panic!("failed to reopen fixture project: {err}"),
        };
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;
        let (status, _capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        server.stop();
        assert_eq!(status, 200);
        let repaired = count_in_project_db(
            &fixture,
            "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1 AND hrr_vector IS NOT NULL",
            103,
        )
        .await;
        assert_eq!(
            repaired, 1,
            "dashboard startup should repair NULL HRR vectors before memory reads"
        );
    });
}

#[test]
fn dashboard_reports_resolved_branch_db_path() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let project_root = tmp.path().join("project");
        let global_db_path = tmp.path().join("global").join("global.db");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);

        fs::create_dir_all(project_root.join("src"))
            .unwrap_or_else(|err| panic!("failed to create src dir: {err}"));
        git(&project_root, &["init", "-b", "main"]);
        fs::write(
            project_root.join("src/lib.rs"),
            "pub fn main_branch_symbol() {}\n",
        )
        .unwrap_or_else(|err| panic!("failed to write fixture lib.rs: {err}"));
        commit_all(&project_root, "initial commit");

        let main = match TraceDecay::init(&project_root).await {
            Ok(cg) => cg,
            Err(err) => panic!("failed to initialize fixture project: {err}"),
        };
        index_all_retrying_sync_lock(&main, "failed to index main branch fixture").await;
        drop(main);

        git(&project_root, &["checkout", "-b", "feature/dashboard-path"]);
        fs::write(
            project_root.join("src/feature.rs"),
            "pub fn feature_branch_symbol() {}\n",
        )
        .unwrap_or_else(|err| panic!("failed to write feature fixture: {err}"));
        if let Err(err) =
            TraceDecay::add_branch_tracking(&project_root, "feature/dashboard-path").await
        {
            panic!("failed to track feature branch: {err}");
        }
        let cg = match TraceDecay::open(&project_root).await {
            Ok(cg) => cg,
            Err(err) => panic!("failed to open feature branch fixture: {err}"),
        };
        let expected = cg.db_path().display().to_string();
        assert!(
            expected.replace('\\', "/").contains("/branches/"),
            "fixture should serve a branch DB path, got {expected}"
        );

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        server.stop();
        assert_eq!(status, 200);
        assert_eq!(capabilities["graph_db"], expected);
    });
}

#[test]
fn dashboard_uses_project_memory_db_and_branch_graph_db() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let project_root = tmp.path().join("project");
        let global_db_path = tmp.path().join("global").join("global.db");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);

        fs::create_dir_all(project_root.join("src"))
            .unwrap_or_else(|err| panic!("failed to create src dir: {err}"));
        git(&project_root, &["init", "-b", "main"]);
        fs::write(
            project_root.join("src/lib.rs"),
            "pub fn main_branch_symbol() {}\n",
        )
        .unwrap_or_else(|err| panic!("failed to write fixture lib.rs: {err}"));
        commit_all(&project_root, "initial commit");

        let main = match TraceDecay::init(&project_root).await {
            Ok(cg) => cg,
            Err(err) => panic!("failed to initialize fixture project: {err}"),
        };
        index_all_retrying_sync_lock(&main, "failed to index main branch fixture").await;
        drop(main);

        git(&project_root, &["checkout", "-b", "feature/dashboard-storage"]);
        fs::write(
            project_root.join("src/feature.rs"),
            "pub fn feature_branch_symbol() {}\n",
        )
        .unwrap_or_else(|err| panic!("failed to write feature fixture: {err}"));
        if let Err(err) =
            TraceDecay::add_branch_tracking(&project_root, "feature/dashboard-storage").await
        {
            panic!("failed to track feature branch: {err}");
        }

        let cg = match TraceDecay::open(&project_root).await {
            Ok(cg) => cg,
            Err(err) => panic!("failed to open feature branch fixture: {err}"),
        };
        let project_db_path = cg.store_layout().graph_db_path.clone();
        let project_db = libsql::Builder::new_local(&project_db_path)
            .build()
            .await
            .unwrap_or_else(|err| panic!("failed to open project DB: {err}"));
        let project_conn = project_db
            .connect()
            .unwrap_or_else(|err| panic!("failed to connect to project DB: {err}"));
        project_conn
            .execute(
                "INSERT INTO memory_facts
                    (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                libsql::params![
                    9001_i64,
                    "Project memory must survive branch dashboard routing",
                    "project",
                    "[\"dashboard\",\"storage\"]",
                    0.99_f64,
                    1_i64,
                    1_i64,
                    1_700_100_000_i64,
                    1_700_100_000_i64,
                ],
            )
            .await
            .unwrap_or_else(|err| panic!("failed to seed project memory fact: {err}"));

        let branch_db_path = cg.db_path();
        assert!(
            branch_db_path
                .display()
                .to_string()
                .replace('\\', "/")
                .contains("/branches/"),
            "fixture should serve a branch graph DB path, got {}",
            branch_db_path.display()
        );

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["memory_db"], project_db_path.display().to_string());
        assert_eq!(capabilities["graph_db"], branch_db_path.display().to_string());

        let (status, memory) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/?limit=5&graph_limit=5"),
        );
        assert_eq!(status, 200);
        assert_eq!(memory["holographic"]["overview"]["facts"], 1);

        let (status, memory_status) =
            get_json(&agent, &format!("{base_url}/api/plugins/holographic/status"));
        assert_eq!(status, 200);
        assert_eq!(memory_status["path"], project_db_path.display().to_string());
        assert_eq!(memory_status["memory"]["fact_count"], 1);

        let (status, graph_search) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/graph/search?q=feature_branch_symbol"),
        );
        server.stop();
        assert_eq!(status, 200);
        assert!(
            graph_search["total"].as_i64().unwrap_or_default() > 0,
            "graph search should read the branch graph DB"
        );
    });
}

#[test]
fn graph_bad_params_and_missing_neighbors_return_json_errors() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, bad_query) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/search?limit=not-a-number",
                fixture.base_url
            ),
        );
        assert_eq!(status, 400);
        assert!(
            bad_query["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("limit"),
            "bad graph query rejection must be JSON with detail, got {bad_query}"
        );

        let (status, missing_neighbors) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/graph/node/missing-node/neighbors",
                fixture.base_url
            ),
        );
        assert_eq!(status, 404);
        assert!(
            missing_neighbors["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("missing-node"),
            "missing-neighbor body should carry the requested id"
        );
    });
}

#[test]
fn dashboard_plugin_manifest_assets_are_served() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, plugins) = get_json(
            &agent,
            &format!("{}/api/dashboard/plugins", fixture.base_url),
        );
        assert_eq!(status, 200);
        for plugin in plugins
            .as_array()
            .unwrap_or_else(|| panic!("expected plugin manifest array"))
        {
            let name = plugin["name"]
                .as_str()
                .unwrap_or_else(|| panic!("plugin name should be a string: {plugin}"));
            for key in ["entry", "css"] {
                let Some(asset) = plugin[key].as_str() else {
                    continue;
                };
                let url = format!("{}/dashboard-plugins/{name}/{asset}", fixture.base_url);
                let response = agent
                    .get(&url)
                    .call()
                    .unwrap_or_else(|err| panic!("GET {url} failed: {err}"));
                assert_eq!(
                    response.status().as_u16(),
                    200,
                    "advertised plugin asset should be served: {name} {asset}"
                );
            }
        }
    });
}

#[test]
fn holographic_dashboard_endpoints_return_seeded_payloads() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=cache&limit=5&graph_limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["providers"]["memory_provider"], "tracedecay");
        assert_eq!(overview["holographic"]["overview"]["facts"], 3);
        assert_eq!(overview["holographic"]["overview"]["banks"], 3);
        assert_eq!(overview["holographic"]["overview"]["entities"], 3);
        // Bank list counts must be live (consistent with the header fact
        // count). The stored bundle snapshot still stays exposed as
        // bundled_fact_count, but startup backfill rebuilds now refresh the
        // seeded project bank to the live membership count.
        let memory_banks = overview["holographic"]["overview"]["memory_banks"]
            .as_array()
            .unwrap_or_else(|| panic!("expected memory_banks array"));
        let project_bank = memory_banks
            .iter()
            .find(|bank| bank["bank_name"] == "project")
            .unwrap_or_else(|| panic!("expected project bank in memory_banks"));
        assert_eq!(
            project_bank["fact_count"], 2,
            "bank list must report live membership counts"
        );
        assert_eq!(
            project_bank["bundled_fact_count"], 2,
            "startup bank rebuild should refresh the bundled project snapshot to the live membership count"
        );
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array in overview payload"));
        assert_eq!(facts.len(), 2, "query should filter to cache facts only");
        // Access tracking is part of every fact payload (seeded rows carry
        // the column defaults).
        assert!(
            facts
                .iter()
                .all(|fact| fact["access_count"].is_number()
                    && fact.get("last_recalled_at").is_some()),
            "fact list rows must surface access_count and last_recalled_at"
        );
        let graph_nodes = overview["holographic"]["graph"]["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected graph nodes array"));
        assert!(
            graph_nodes.iter().any(|node| node["kind"] == "entity"),
            "graph should include entity nodes"
        );
        let growth = overview["holographic"]["overview"]["growth"]
            .as_array()
            .unwrap_or_else(|| panic!("expected growth series array"));
        assert!(
            !growth.is_empty(),
            "growth should cover seeded historical facts"
        );
        assert!(
            growth.iter().all(|day| day["cumulative_facts"].is_number()),
            "growth points should include cumulative fact counts"
        );
        assert_eq!(
            growth
                .last()
                .and_then(|day| day["cumulative_facts"].as_i64()),
            Some(3),
            "last cumulative growth point should include all seeded facts"
        );

        let (status, projection) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/projection?limit=5000",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(projection["limit"], 2000);
        assert_eq!(projection["method"], "pca");
        assert_eq!(projection["dim"], 2048);
        let projection_points = projection["points"]
            .as_array()
            .unwrap_or_else(|| panic!("expected projection points array"));
        assert!(
            projection_points.len() >= 2,
            "projection should include at least two PCA points"
        );
        assert!(
            projection_points[0]["x"].is_number() && projection_points[0]["y"].is_number(),
            "projection points should include numeric x/y coordinates"
        );
        let project_point = projection_points
            .iter()
            .find(|point| point["fact_id"].as_i64() == Some(101))
            .unwrap_or_else(|| panic!("expected projection point for fact 101"));
        assert_eq!(project_point["bank_name"], "project");
        assert!(
            project_point["bank_id"].is_number(),
            "projection point should include numeric bank_id"
        );
        assert_eq!(project_point["entity_count"], 1);
        assert_eq!(project_point["connection_count"], 1);
        let tool_point = projection_points
            .iter()
            .find(|point| point["fact_id"].as_i64() == Some(103))
            .unwrap_or_else(|| panic!("expected projection point for fact 103"));
        assert_eq!(tool_point["entity_count"], 2);
        assert_eq!(tool_point["connection_count"], 2);

        let (status, similarity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/similarity?min_similarity=0.0&limit=5000",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(similarity["limit"], 2000);
        assert_eq!(similarity["min_similarity"], 0.0);
        assert_eq!(similarity["dim"], 2048);
        assert_eq!(similarity["count"], 3);
        assert_eq!(similarity["total_pairs"], 3);
        let pairs = similarity["pairs"]
            .as_array()
            .unwrap_or_else(|| panic!("expected similarity pairs array"));
        assert_eq!(
            pairs.len(),
            3,
            "min_similarity=0 should return pairs below the previous 0.5 floor"
        );
        let duplicate_pair = pairs
            .iter()
            .find(|pair| pair["classification"] == "likely_duplicate")
            .unwrap_or_else(|| panic!("expected likely_duplicate similarity pair"));
        let duplicate_similarity = duplicate_pair["similarity"]
            .as_f64()
            .unwrap_or_else(|| panic!("expected numeric similarity"));
        assert!(
            duplicate_similarity < 1.0 && duplicate_similarity > 0.9999,
            "similarity should retain full precision instead of rounding to four decimals"
        );
        let distribution = &similarity["score_distribution"];
        let bins = distribution["bins"]
            .as_array()
            .unwrap_or_else(|| panic!("expected score distribution bins"));
        assert!(!bins.is_empty(), "score distribution should include bins");
        let binned_pairs: i64 = bins
            .iter()
            .map(|bin| bin["count"].as_i64().unwrap_or(0))
            .sum();
        assert_eq!(distribution["total_pairs"], 3);
        assert_eq!(
            binned_pairs, 3,
            "distribution bins should cover every computed pair"
        );
        assert_eq!(
            distribution["min"], distribution["min_score"],
            "bins should adapt to the observed score range"
        );
        assert_eq!(
            distribution["max"], distribution["max_score"],
            "bins should adapt to the observed score range"
        );
        let occupied_bins = bins
            .iter()
            .filter(|bin| bin["count"].as_i64().unwrap_or(0) > 0)
            .count();
        assert!(
            occupied_bins >= 2,
            "adaptive binning should spread near-duplicate and unrelated pairs across bins"
        );
        assert!(
            pairs
                .iter()
                .any(|pair| pair["classification"] == "likely_duplicate"),
            "fixture vectors should produce a likely_duplicate pair"
        );

        let (status, curation_status) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(curation_status["config"]["enabled"], true);

        let (status, curation_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=75",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(curation_activity["count"], 0);
        assert_eq!(curation_activity["events"], Value::Array(Vec::new()));

        let (status, curation_preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(curation_preview["report"].is_null());
        assert_eq!(curation_preview["stale"], false);

        // Curation dry-run should return a valid plan (the fixture has a likely-duplicate pair).
        let (status, curate) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(curate["ran"], true);
        assert_eq!(curate["dry_run"], true);
        assert!(
            curate["actions"].as_array().is_some(),
            "curate dry-run should return an actions array"
        );
        // The deterministic hygiene candidate section is always present.
        for key in ["secret_like", "transient", "supersession"] {
            assert!(
                curate["hygiene_candidates"][key].as_array().is_some(),
                "curate dry-run should include hygiene_candidates.{key} proposals"
            );
        }
    });
}

#[test]
fn holographic_fact_detail_returns_full_content_and_entities() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        assert!(
            LONG_FACT_CONTENT.chars().count() > 200,
            "fixture must exceed the 200-char list/projection truncation"
        );

        // The projection payload truncates content at 200 chars by design.
        let (status, projection) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/projection?limit=2000",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let truncated_point = projection["points"]
            .as_array()
            .and_then(|points| {
                points
                    .iter()
                    .find(|point| point["fact_id"].as_i64() == Some(103))
            })
            .unwrap_or_else(|| panic!("expected projection point for fact 103"));
        assert_eq!(
            truncated_point["content"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .count(),
            200,
            "projection content stays truncated at 200 chars"
        );

        // The detail endpoint returns the complete row plus linked entities.
        let (status, detail) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/103", fixture.base_url),
        );
        assert_eq!(status, 200);
        assert_eq!(detail["error"], "");
        assert_eq!(detail["fact"]["fact_id"], 103);
        assert_eq!(detail["fact"]["category"], "tool");
        assert_eq!(detail["fact"]["content"], LONG_FACT_CONTENT);
        assert_eq!(detail["fact"]["has_hrr"], 1);
        assert_eq!(detail["fact"]["trust_score"], 0.76);
        assert!(
            detail["fact"]["access_count"].is_number(),
            "fact detail must surface access_count"
        );
        assert!(
            detail["fact"].get("last_recalled_at").is_some(),
            "fact detail must surface last_recalled_at"
        );
        let entities = detail["fact"]["entities"]
            .as_array()
            .unwrap_or_else(|| panic!("expected entities array in fact detail"));
        let entity_names: Vec<&str> = entities
            .iter()
            .filter_map(|entity| entity["name"].as_str())
            .collect();
        assert_eq!(
            entity_names,
            vec!["LCMTab", "SimilarityView"],
            "fact detail must list linked entities sorted by name"
        );

        // Unknown ids are a 404 with the FastAPI-style detail body.
        let (status, missing) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/fact/99999", fixture.base_url),
        );
        assert_eq!(status, 404);
        assert!(
            missing["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("99999"),
            "404 body should carry the requested fact id"
        );
    });
}

#[test]
fn holographic_fact_trust_history_returns_feedback_trail_and_empty_for_unreviewed_facts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let conn = project_db_conn(&fixture).await;
        conn.execute(
            "INSERT INTO memory_feedback_events
                (fact_id, action, trust_delta, old_trust, new_trust, created_at, source, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            libsql::params![
                103_i64,
                "helpful",
                0.05_f64,
                0.71_f64,
                0.76_f64,
                1_700_000_450_i64,
                "dashboard-test",
                "confirmed durable"
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("failed to insert helpful feedback row: {err}"));
        conn.execute(
            "INSERT INTO memory_feedback_events
                (fact_id, action, trust_delta, old_trust, new_trust, created_at, source, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            libsql::params![
                103_i64,
                "unhelpful",
                -0.10_f64,
                0.76_f64,
                0.66_f64,
                1_700_000_460_i64,
                "dashboard-test",
                libsql::Value::Null
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("failed to insert unhelpful feedback row: {err}"));

        let agent = http_agent();
        let (status, history) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/103/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(history["error"], "");
        assert_eq!(history["fact_id"], 103);
        let trail = history["trust_history"]
            .as_array()
            .unwrap_or_else(|| panic!("expected trust_history array: {history}"));
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0]["timestamp"], 1_700_000_450_i64);
        assert_eq!(trail[0]["action"], "helpful");
        assert_eq!(trail[0]["old_trust"], 0.71);
        assert_eq!(trail[0]["new_trust"], 0.76);
        assert_eq!(trail[0]["delta"], 0.05);
        assert_eq!(trail[0]["source"], "dashboard-test");
        assert_eq!(trail[0]["note"], "confirmed durable");
        assert_eq!(trail[1]["action"], "unhelpful");
        assert!(trail[1]["note"].is_null());

        let (status, empty_history) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/101/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(empty_history["fact_id"], 101);
        assert_eq!(
            empty_history["trust_history"]
                .as_array()
                .map(|rows| rows.len()),
            Some(0)
        );

        let (status, missing) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/fact/99999/trust-history",
                fixture.base_url
            ),
        );
        assert_eq!(status, 404);
        assert!(
            missing["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("99999"),
            "404 body should carry the requested fact id"
        );
    });
}

#[test]
fn curate_hygiene_scans_unvectored_facts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let conn = project_db_conn(&fixture).await;
        conn.execute(
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, created_at, updated_at, source, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            libsql::params![
                901_i64,
                "api_key=Zx9mQ4tR7wLp2NvK8sBd1FgH",
                "project",
                "[]",
                0.5_f64,
                1_700_000_200_i64,
                1_700_000_200_i64,
                "test",
                "{}"
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("failed to insert unvectored hygiene fact: {err}"));

        let agent = http_agent();
        let (status, curate) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );

        assert_eq!(status, 200);
        let secret_like = curate["hygiene_candidates"]["secret_like"]
            .as_array()
            .unwrap_or_else(|| panic!("expected hygiene_candidates.secret_like array"));
        let secret_candidate = secret_like
            .iter()
            .find(|action| action["fact_id"].as_i64() == Some(901))
            .unwrap_or_else(|| {
                panic!("hygiene scan must include secret-like facts without HRR vectors: {curate}")
            });
        assert_eq!(secret_candidate["status"], "candidate");
        assert_eq!(secret_candidate["review_required"], true);
        assert_eq!(secret_candidate["recommended_op"], "delete");

        let (status, applied) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 200);
        assert!(applied["hygiene_candidates"]["secret_like"]
            .as_array()
            .is_some_and(|candidates| candidates
                .iter()
                .any(|candidate| candidate["fact_id"].as_i64() == Some(901))));
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
                901,
            )
            .await,
            1,
            "deterministic curate apply must not delete hygiene candidates without explicit review"
        );
    });
}

#[test]
fn curation_delete_lifecycle() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        // --- Dry-run curation: expect a delete plan for the likely-duplicate pair ---
        let (status, dry) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(dry["ran"], true);
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["llm_calls"], 0);
        let actions = dry["actions"]
            .as_array()
            .unwrap_or_else(|| panic!("expected actions array"));
        assert!(
            !actions.is_empty(),
            "fixture with likely-duplicate vectors should produce at least one delete action"
        );
        assert_eq!(actions[0]["op"], "delete");
        assert!(
            actions[0]["fact_id"].is_number(),
            "action must have fact_id"
        );
        assert!(
            actions[0]["duplicate_of"].is_number(),
            "action must reference the surviving duplicate"
        );
        let planned_delete_id = actions[0]["fact_id"]
            .as_i64()
            .unwrap_or_else(|| panic!("fact_id must be an integer"));
        assert_eq!(dry["counts"]["delete"], actions.len() as i64);
        assert_eq!(dry["coverage"]["active_total"], 3);

        // Preview should now be available and fresh.
        let (status, preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(
            !preview["report"].is_null(),
            "preview should be non-null after a dry-run"
        );
        assert_eq!(preview["stale"], false);

        // Curation status should reflect the preview timestamp.
        let (status, curation_status) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(curation_status["config"]["enabled"], true);
        assert!(
            !curation_status["state"]["last_preview_at"].is_null(),
            "last_preview_at should be set after dry-run"
        );

        let (status, dry_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=75",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(dry_activity["error"], "");
        assert_eq!(dry_activity["limit"], 75);
        let dry_events = dry_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected dry-run activity events array"));
        assert_eq!(
            dry_activity["count"].as_u64(),
            Some(dry_events.len() as u64)
        );
        assert!(
            !dry_events.is_empty(),
            "dry-run curation should emit activity events"
        );
        let dry_phases: Vec<_> = dry_events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "start",
            "evidence",
            "backend",
            "validation",
            "report",
            "finish",
        ] {
            assert!(
                dry_phases.contains(&phase),
                "dry-run curation should emit {phase} activity; phases={dry_phases:?}"
            );
        }
        assert!(
            dry_events.iter().any(|event| {
                event["phase"] == "finish"
                    && event["dry_run"] == true
                    && event["message"]
                        .as_str()
                        .is_some_and(|message| !message.is_empty())
                    && event["ts"].as_str().is_some_and(|ts| !ts.is_empty())
            }),
            "dry-run curation should emit a finish activity event"
        );

        // --- Apply curation: hard-delete the duplicate ---
        let (status, applied) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 200);
        assert_eq!(applied["ran"], true);
        assert_eq!(applied["dry_run"], false);
        assert!(
            applied["applied_counts"]["delete"].as_i64().unwrap_or(0) > 0,
            "apply should report at least one deleted fact"
        );

        let (status, apply_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=75",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let apply_events = apply_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected apply activity events array"));
        assert_eq!(
            apply_activity["count"].as_u64(),
            Some(apply_events.len() as u64)
        );
        assert!(
            apply_events.len() > dry_events.len(),
            "apply should append activity events after dry-run events"
        );
        let apply_phases: Vec<_> = apply_events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in ["queued", "backend", "validation", "report", "apply"] {
            assert!(
                apply_phases.contains(&phase),
                "apply curation should emit {phase} activity; phases={apply_phases:?}"
            );
        }
        assert!(
            apply_events
                .iter()
                .rev()
                .any(|event| event["phase"] == "finish" && event["dry_run"] == false),
            "apply curation should emit a finish activity event"
        );

        let (status, status_after_apply) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(status_after_apply["state"]["run_count"], 1);
        assert!(
            status_after_apply["state"]["last_run_at"]
                .as_str()
                .is_some_and(|ts| !ts.is_empty()),
            "last_run_at should be set after apply"
        );
        assert!(
            status_after_apply["state"]["last_run_summary"]
                .as_str()
                .is_some_and(|summary| summary.contains("deleted")),
            "last_run_summary should describe the apply result"
        );
        assert!(
            status_after_apply["snapshots"]
                .as_array()
                .is_some_and(|snapshots| !snapshots.is_empty()),
            "status snapshots should include recent apply history"
        );

        // --- Overview should show fewer facts and not contain the deleted one ---
        let (status, overview) = get_json(
            &agent,
            &format!("{}/api/plugins/holographic/", fixture.base_url),
        );
        assert_eq!(status, 200);
        let fact_count = overview["holographic"]["overview"]["facts"]
            .as_i64()
            .unwrap_or(3);
        assert!(
            fact_count < 3,
            "overview fact count should decrease after deletion"
        );
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array"));
        assert!(
            facts
                .iter()
                .all(|fact| fact["fact_id"].as_i64() != Some(planned_delete_id)),
            "deleted fact must not appear in the overview fact list"
        );

        // --- The row and its entity links must be gone from the store that
        //     tracedecay_fact_store recall reads (hard delete, not soft). ---
        let remaining = count_in_project_db(
            &fixture,
            "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
            planned_delete_id,
        )
        .await;
        assert_eq!(
            remaining, 0,
            "deleted fact row must be gone from memory_facts"
        );
        let remaining_links = count_in_project_db(
            &fixture,
            "SELECT COUNT(*) FROM memory_fact_entities WHERE fact_id = ?1",
            planned_delete_id,
        )
        .await;
        assert_eq!(
            remaining_links, 0,
            "entity links of a deleted fact must be cleaned up"
        );

        // Apply invalidates the saved preview.
        let (status, preview_after) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(preview_after["report"].is_null());
    });
}

#[test]
fn curation_preview_marks_same_count_updates_stale() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, dry) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(dry["dry_run"], true);

        let conn = project_db_conn(&fixture).await;
        conn.execute(
            "UPDATE memory_facts
             SET content = content || ' after preview', updated_at = updated_at + 1
             WHERE fact_id = 101",
            (),
        )
        .await
        .unwrap();

        let (status, preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(
            preview["stale"], true,
            "same-count edits must stale previews"
        );
        assert!(
            preview["stale_reason"]
                .as_str()
                .unwrap_or_default()
                .contains("changed"),
            "stale response should explain the memory store changed: {preview}"
        );
    });
}

#[test]
fn memory_oplog_endpoint_lists_recent_operations() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        // Fresh fixture: no operations recorded yet.
        let (status, empty) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/oplog?limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(empty["count"], 0);
        assert_eq!(empty["error"], "");

        // An explicit-ops delete writes a per-fact "remove" row plus a
        // "curate_apply" summary row.
        let (status, applied) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate/apply", fixture.base_url),
            &serde_json::json!({
                "ops": [{ "op": "delete", "fact_id": 103, "reason": "oplog fixture" }]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(applied["counts"]["deleted"], 1);

        let (status, oplog) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/oplog?limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(oplog["error"], "");
        let events = oplog["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected oplog events array"));
        assert_eq!(events.len(), 2, "expected remove + curate_apply rows");

        // Newest first: the curate_apply summary follows the per-fact remove.
        assert_eq!(events[0]["op"], "curate_apply");
        assert_eq!(events[0]["detail"]["deleted"], 1);
        assert_eq!(events[1]["op"], "remove");
        assert_eq!(events[1]["fact_id"], 103);
        let remove_detail = events[1]["detail"].to_string();
        assert!(
            remove_detail.contains("content_hash"),
            "remove rows must carry a content hash: {remove_detail}"
        );
        assert!(
            !remove_detail.contains("empty states"),
            "remove rows must not leak deleted fact content: {remove_detail}"
        );
        assert!(
            events.iter().all(|event| event["ts"].is_number()),
            "every oplog row carries a timestamp"
        );
    });
}

#[test]
fn curate_apply_ops_contract() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let apply_url = format!("{}/api/plugins/holographic/curate/apply", fixture.base_url);

        // Merge: fact 102 into 101 with rewritten content, plus an explicit
        // delete of 103, plus an invalid delete — partial failure stays per-op.
        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [
                    {
                        "op": "merge",
                        "winner_id": 101,
                        "loser_ids": [102],
                        "merged_content": "Cache invalidation policy must be explicit (merged)"
                    },
                    { "op": "delete", "fact_id": 103, "reason": "manual cleanup" },
                    { "op": "delete", "fact_id": 99999 },
                    { "op": "frobnicate" }
                ]
            }),
        );
        assert_eq!(status, 200, "partial failures must not fail the request");
        let results = response["results"]
            .as_array()
            .unwrap_or_else(|| panic!("expected results array"));
        assert_eq!(results.len(), 4);

        assert_eq!(results[0]["op"], "merge");
        assert_eq!(
            results[0]["status"], "merged",
            "merge op failed: {response}"
        );
        assert_eq!(results[0]["content_updated"], true);
        assert_eq!(results[0]["deleted_loser_ids"], serde_json::json!([102]));

        assert_eq!(results[1]["op"], "delete");
        assert_eq!(results[1]["status"], "deleted");
        assert_eq!(results[1]["fact_id"], 103);

        assert_eq!(results[2]["status"], "error");
        assert!(
            results[2]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("not found"),
            "invalid fact_id must produce a per-op not-found error"
        );

        assert_eq!(results[3]["status"], "error");
        assert!(
            results[3]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("unsupported op"),
            "unknown op kinds must produce a per-op error"
        );

        assert_eq!(response["counts"]["deleted"], 1);
        assert_eq!(response["counts"]["merged"], 1);
        assert_eq!(response["counts"]["errors"], 2);

        let (status, apply_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=25",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let apply_events = apply_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected generic apply activity events array"));
        assert!(
            apply_events.iter().any(|event| {
                event["phase"] == "finish"
                    && event["dry_run"] == false
                    && event["message"].as_str().is_some_and(|message| {
                        message.contains("Explicit apply completed")
                            && message.contains("1 delete")
                            && message.contains("1 merge")
                            && message.contains("2 op(s) errored")
                    })
                    && event["ts"].as_str().is_some_and(|ts| !ts.is_empty())
            }),
            "/curate/apply should emit a finish activity event: {apply_activity}"
        );
        for phase in ["queued", "apply", "validation", "report"] {
            assert!(
                apply_events
                    .iter()
                    .any(|event| event["phase"].as_str() == Some(phase)),
                "/curate/apply should emit {phase} activity: {apply_activity}"
            );
        }
        assert!(
            apply_events.iter().any(|event| {
                event["phase"] == "rejection"
                    && event["level"] == "warning"
                    && event["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("2 explicit curation op(s)"))
            }),
            "/curate/apply should emit a rejection activity event for invalid ops: {apply_activity}"
        );

        let (status, rejected_only) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [
                    { "op": "delete", "fact_id": 99999 },
                    { "op": "frobnicate" }
                ]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(rejected_only["counts"]["deleted"], 0);
        assert_eq!(rejected_only["counts"]["merged"], 0);
        assert_eq!(rejected_only["counts"]["errors"], 2);
        let (status, rejected_activity) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/activity?limit=25",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let rejected_events = rejected_activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected rejected activity events array: {rejected_activity}"));
        for phase in ["queued", "apply", "validation", "rejection", "report", "failure"] {
            assert!(
                rejected_events
                    .iter()
                    .any(|event| event["phase"].as_str() == Some(phase)),
                "all-rejected apply should emit {phase} activity: {rejected_activity}"
            );
        }
        assert!(
            rejected_events.iter().any(|event| {
                    event["phase"] == "finish"
                        && event["dry_run"] == false
                        && event["message"].as_str().is_some_and(|message| {
                            message.contains("0 delete")
                                && message.contains("0 merge")
                                && message.contains("2 op(s) errored")
                        })
            }),
            "all-rejected apply requests should still emit a terminal finish event: {rejected_activity}"
        );

        let (status, apply_status) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/status",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(apply_status["state"]["run_count"], 2);
        assert!(
            apply_status["state"]["last_run_at"]
                .as_str()
                .is_some_and(|ts| !ts.is_empty()),
            "last_run_at should be set after /curate/apply"
        );
        let summary = apply_status["state"]["last_run_summary"]
            .as_str()
            .unwrap_or_default();
        assert!(
            summary.contains("Explicit apply completed")
                && summary.contains("0 delete")
                && summary.contains("0 merge")
                && summary.contains("2 op(s) errored"),
            "/curate/apply should drive the status summary: {apply_status}"
        );
        assert!(
            apply_status["snapshots"]
                .as_array()
                .is_some_and(|snapshots| {
                    snapshots.iter().any(|snapshot| {
                        snapshot["summary"]
                            .as_str()
                            .is_some_and(|summary| summary.contains("Explicit apply completed"))
                    })
                }),
            "/curate/apply should appear in status snapshots: {apply_status}"
        );

        // Hard deletes: rows + entity links gone from the project DB.
        for gone_id in [102_i64, 103] {
            let remaining = count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
                gone_id,
            )
            .await;
            assert_eq!(remaining, 0, "fact {gone_id} must be hard-deleted");
            let links = count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_fact_entities WHERE fact_id = ?1",
                gone_id,
            )
            .await;
            assert_eq!(links, 0, "entity links of fact {gone_id} must be gone");
        }

        // Winner survived with merged content.
        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/?q=merged&limit=10",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array"));
        assert!(
            facts.iter().any(|fact| {
                fact["fact_id"].as_i64() == Some(101)
                    && fact["content"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("(merged)")
            }),
            "winner fact must survive with the merged content"
        );

        // Merge with a missing winner: per-op error, losers untouched.
        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [{ "op": "merge", "winner_id": 4242, "loser_ids": [101] }]
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(response["results"][0]["status"], "error");
        assert_eq!(response["counts"]["errors"], 1);
        let survivor = count_in_project_db(
            &fixture,
            "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
            101,
        )
        .await;
        assert_eq!(
            survivor, 1,
            "loser must be untouched when the winner is missing"
        );

        // Malformed body (no ops field) is the only whole-request failure mode.
        let (status, _) = post_json(&agent, &apply_url);
        assert!(
            status == 400 || status == 415 || status == 422,
            "missing/malformed body should be rejected, got {status}"
        );
    });
}

#[test]
fn curate_apply_merge_with_missing_loser_is_atomic() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let apply_url = format!("{}/api/plugins/holographic/curate/apply", fixture.base_url);

        let (status, dry) = post_json_body(
            &agent,
            &format!("{}/api/plugins/holographic/curate", fixture.base_url),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(dry["dry_run"], true);

        let original_winner = string_in_project_db(
            &fixture,
            "SELECT content FROM memory_facts WHERE fact_id = ?1",
            101,
        )
        .await
        .expect("winner content");

        let (status, response) = post_json_body(
            &agent,
            &apply_url,
            &serde_json::json!({
                "ops": [{
                    "op": "merge",
                    "winner_id": 101,
                    "loser_ids": [102, 99999],
                    "merged_content": "Cache invalidation policy should not partially merge"
                }]
            }),
        );
        assert_eq!(status, 200, "per-op failures stay in-band");
        assert_eq!(response["counts"]["deleted"], 0);
        assert_eq!(response["counts"]["merged"], 0);
        assert_eq!(response["counts"]["errors"], 1);
        assert_eq!(response["results"][0]["op"], "merge");
        assert_eq!(response["results"][0]["status"], "error");
        assert!(
            response["results"][0]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("loser fact 99999 not found"),
            "missing loser should be reported before mutation: {response}"
        );

        let winner_after = string_in_project_db(
            &fixture,
            "SELECT content FROM memory_facts WHERE fact_id = ?1",
            101,
        )
        .await
        .expect("winner content after failed merge");
        assert_eq!(
            winner_after, original_winner,
            "failed merge must not update winner content"
        );
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_facts WHERE fact_id = ?1",
                102,
            )
            .await,
            1,
            "failed merge must not delete valid losers"
        );
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_oplog WHERE fact_id = ?1",
                101,
            )
            .await,
            0,
            "failed merge must not write a winner update oplog"
        );
        assert_eq!(
            count_in_project_db(
                &fixture,
                "SELECT COUNT(*) FROM memory_oplog WHERE fact_id = ?1",
                102,
            )
            .await,
            0,
            "failed merge must not write loser delete oplogs"
        );

        let (status, preview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/holographic/curation/preview",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert!(
            !preview["report"].is_null(),
            "failed merge must not clear saved preview"
        );
        assert_eq!(
            preview["stale"], false,
            "unchanged store should leave preview fresh"
        );
    });
}

#[test]
fn lcm_endpoints_cover_seeded_fts_and_like_fallback() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(true).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?q=vector&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["exists"], true);
        assert_eq!(
            overview["storage_scope"], "profile_sharded",
            "LCM serves the resolved project session store even when TRACEDECAY_GLOBAL_DB is set for accounting"
        );
        assert_eq!(overview["overview"]["messages_total"], 3);
        assert_eq!(overview["overview"]["sessions_total"], 1);
        assert_eq!(overview["overview"]["summary_nodes_total"], 1);
        assert_eq!(
            overview["overview"]["compression"]["source_token_count"],
            180
        );
        assert_eq!(overview["overview"]["compression"]["token_count"], 72);
        let latest_sessions = overview["latest_sessions"]
            .as_array()
            .unwrap_or_else(|| panic!("expected latest_sessions array"));
        assert_eq!(latest_sessions.len(), 1);
        let matches_messages = overview["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected overview.matches.messages array"));
        assert!(
            !matches_messages.is_empty(),
            "overview?q=vector should return message matches"
        );

        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=vector&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["engine"], "fts");
        let search_messages = search["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.messages array"));
        let search_nodes = search["matches"]["summary_nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.summary_nodes array"));
        assert!(
            !search_messages.is_empty(),
            "FTS search should match seeded messages"
        );
        assert!(
            !search_nodes.is_empty(),
            "FTS search should match seeded summary nodes"
        );

        let (status, like_search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=!!!&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(like_search["engine"], "like");
    });
}

#[test]
fn lcm_endpoints_return_empty_state_when_no_rows_exist() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();

        let (status, overview) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/overview?limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["exists"], true);
        assert_eq!(overview["overview"]["messages_total"], 0);
        assert_eq!(overview["overview"]["summary_nodes_total"], 0);
        assert_eq!(
            overview["latest_sessions"],
            Value::Array(Vec::new()),
            "empty LCM store should have no latest sessions"
        );
        assert_eq!(
            overview["latest_summary_nodes"],
            Value::Array(Vec::new()),
            "empty LCM store should have no summary nodes"
        );

        let (status, search) = get_json(
            &agent,
            &format!(
                "{}/api/plugins/hermes-lcm/search?q=vector&limit=20",
                fixture.base_url
            ),
        );
        assert_eq!(status, 200);
        assert_eq!(search["engine"], "fts");
        assert_eq!(
            search["matches"]["messages"],
            Value::Array(Vec::new()),
            "empty LCM store search should have zero message matches"
        );
        assert_eq!(
            search["matches"]["summary_nodes"],
            Value::Array(Vec::new()),
            "empty LCM store search should have zero summary-node matches"
        );
    });
}

/// Opens (creating if needed) the resolved project session store — profile
/// sharded by default, project-local only for explicit or legacy projects.
async fn open_project_session_store(project_root: &Path) -> GlobalDb {
    let db_path = tracedecay::sessions::cursor::project_session_db_path(project_root);
    match GlobalDb::open_at(&db_path).await {
        Some(db) => db,
        None => panic!(
            "failed to open project session store at {}",
            db_path.display()
        ),
    }
}

/// Without a `TRACEDECAY_GLOBAL_DB` override the dashboard must serve the
/// resolved project session store, profile-sharded by default, and report it
/// via the additive `storage_scope` payload field.
#[test]
fn lcm_serves_project_session_store_without_global_override() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::unset(GLOBAL_DB_ENV);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let session_store = open_project_session_store(&project_root).await;
        let expected_session_path =
            tracedecay::sessions::cursor::project_session_db_path(&project_root);
        seed_lcm_fixture(&session_store, &project_root).await;
        drop(session_store);

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);

        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["lcm_scope"], "profile_sharded");
        assert_eq!(capabilities["features"]["lcm"], true);
        let lcm_db = capabilities["lcm_db"]
            .as_str()
            .unwrap_or_else(|| panic!("expected capabilities.lcm_db string"));
        assert!(
            Path::new(lcm_db) == expected_session_path,
            "capabilities.lcm_db should be the resolved project session store, got {lcm_db}"
        );

        let (status, overview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/overview?limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["storage_scope"], "profile_sharded");
        assert_eq!(overview["exists"], true);
        assert_eq!(overview["overview"]["messages_total"], 3);
        assert_eq!(overview["overview"]["sessions_total"], 1);
        assert_eq!(overview["overview"]["summary_nodes_total"], 1);
        let path = overview["path"]
            .as_str()
            .unwrap_or_else(|| panic!("expected overview.path string"));
        assert!(
            Path::new(path) == expected_session_path,
            "overview.path should be the resolved project session store, got {path}"
        );

        let (status, search) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/search?q=vector&limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(search["storage_scope"], "profile_sharded");
        let search_messages = search["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.messages array"));
        assert!(
            !search_messages.is_empty(),
            "project-store search should match seeded messages"
        );

        server.stop();
    });
}

/// `TRACEDECAY_GLOBAL_DB` pins savings/accounting, but LCM sessions still
/// come from the resolved project store that transcript ingest writes.
#[test]
fn lcm_project_store_wins_over_global_accounting_override() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
        let cg = setup_project(&project_root).await;
        // The project store has rows; the overridden global accounting store has none.
        let session_store = open_project_session_store(&project_root).await;
        let expected_session_path =
            tracedecay::sessions::cursor::project_session_db_path(&project_root);
        seed_lcm_fixture(&session_store, &project_root).await;
        drop(session_store);

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);

        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["lcm_scope"], "profile_sharded");

        let (status, overview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/overview?limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["storage_scope"], "profile_sharded");
        assert_eq!(overview["exists"], true);
        assert_eq!(
            overview["overview"]["messages_total"], 3,
            "LCM must serve the project store, not the empty accounting DB"
        );
        let path = overview["path"]
            .as_str()
            .unwrap_or_else(|| panic!("expected overview.path string"));
        assert!(
            Path::new(path) == expected_session_path,
            "expected resolved project session DB path, got {path}"
        );

        server.stop();
    });
}

/// The dry-run curation preview must survive a dashboard restart: it is
/// mirrored to the resolved dashboard sidecar path and re-hydrated by
/// `build_state`, and applying curation clears both the memory copy and the
/// sidecar.
#[test]
fn curation_preview_persists_across_dashboard_restarts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        seed_memory_fixture(&cg).await;
        let agent = http_agent();
        let sidecar = cg
            .store_layout()
            .dashboard_root
            .join("curation_preview.json");

        async fn start_server(cg: TraceDecay) -> (String, DashboardServer) {
            let port = pick_free_port();
            let base_url = format!("http://127.0.0.1:{port}");
            let server = spawn_dashboard_server(cg, port);
            (base_url, server)
        }

        fn stop_server(mut server: DashboardServer) {
            server.stop();
        }

        async fn reopen_project(project_root: &Path) -> TraceDecay {
            match TraceDecay::open(project_root).await {
                Ok(cg) => cg,
                Err(err) => panic!("failed to reopen fixture project: {err}"),
            }
        }

        // Server 1: a dry-run saves the preview and writes the sidecar.
        let (base_url, server) = start_server(cg).await;
        wait_for_dashboard(&agent, &base_url).await;
        let (status, curate) = post_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curate"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(curate["dry_run"], true);
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(!preview["report"].is_null(), "dry-run must save a preview");
        let saved_at = preview["saved_at"].clone();
        assert!(saved_at.is_string(), "preview must carry saved_at");
        stop_server(server);
        assert!(
            sidecar.exists(),
            "dry-run must persist the preview sidecar at {}",
            sidecar.display()
        );

        // Server 2 (fresh state): the preview is re-hydrated from disk.
        let cg = reopen_project(&project_root).await;
        let (base_url, server) = start_server(cg).await;
        wait_for_dashboard(&agent, &base_url).await;
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(
            !preview["report"].is_null(),
            "preview must survive a server restart"
        );
        assert_eq!(
            preview["saved_at"], saved_at,
            "re-hydrated preview must keep its original timestamp"
        );
        assert_eq!(
            preview["stale"], false,
            "fact count is unchanged, so the restored preview is not stale"
        );
        let (status, status_payload) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/status"),
        );
        assert_eq!(status, 200);
        assert_eq!(
            status_payload["state"]["last_preview_at"], saved_at,
            "curation status must reflect the restored preview"
        );

        // Applying curation clears both the in-memory copy and the sidecar.
        let (status, applied) = post_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curate"),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 200);
        assert_eq!(applied["dry_run"], false);
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(preview["report"].is_null(), "apply must clear the preview");
        assert!(
            !sidecar.exists(),
            "apply must remove the persisted preview sidecar"
        );
        stop_server(server);

        // Server 3: nothing is restored after the apply cleared the sidecar.
        let cg = reopen_project(&project_root).await;
        let (base_url, server) = start_server(cg).await;
        wait_for_dashboard(&agent, &base_url).await;
        let (status, preview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/preview"),
        );
        assert_eq!(status, 200);
        assert!(
            preview["report"].is_null(),
            "no preview may reappear after curation was applied"
        );
        stop_server(server);
    });
}

#[test]
fn automation_config_is_dashboard_controllable_and_persistent() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
        let missing_codex_bin = tmp_root.join("missing-codex");
        let _codex_bin_guard = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &missing_codex_bin);

        let mut global_config = tracedecay::user_config::UserConfig::default();
        global_config.automation.enabled = true;
        global_config.automation.backend =
            tracedecay::automation::config::AutomationBackend::CodexAppServer;
        global_config.automation.model = Some("global-model".to_string());
        assert!(global_config.save(), "global user config should save");

        let cg = setup_project(&project_root).await;
        let sidecar = cg
            .store_layout()
            .dashboard_root
            .join("automation_config.json");
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let config_url = format!("{base_url}/api/plugins/holographic/curation/config");
        let (status, config) = get_json(&agent, &config_url);
        assert_eq!(status, 200);
        assert_eq!(config["global"]["enabled"], true);
        assert_eq!(config["global"]["backend"], "codex_app_server");
        assert_eq!(config["global"]["model"], "global-model");
        assert!(config["project"].is_null());
        assert_eq!(config["effective"]["model"], "global-model");
        assert_eq!(config["backend_availability"]["available"], false);
        assert_eq!(
            config["backend_availability"]["executable"],
            missing_codex_bin.display().to_string()
        );
        assert_eq!(
            config["effective"]["tasks"]["memory_curator"]["enabled"],
            false
        );

        let patch = serde_json::json!({
            "model": "project-model",
            "timeout_secs": 90,
            "scheduler_tick_secs": 15,
            "memory_curator": { "enabled": true, "schedule": "manual" }
        });
        let (status, saved) = patch_json_body(&agent, &config_url, &patch);
        assert_eq!(status, 200);
        assert_eq!(saved["project"]["model"], "project-model");
        assert_eq!(saved["effective"]["model"], "project-model");
        assert_eq!(saved["effective"]["timeout_secs"], 90);
        assert_eq!(saved["effective"]["scheduler_tick_secs"], 15);
        assert_eq!(
            saved["effective"]["tasks"]["memory_curator"]["schedule"],
            "manual"
        );
        assert!(sidecar.exists(), "PATCH must persist a project sidecar");

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["automation"], true);
        assert_eq!(capabilities["features"]["llm_curation"], true);
        assert_eq!(capabilities["automation"]["mode"], "standalone_backend");
        assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
        assert_eq!(capabilities["automation"]["host_mode"], "standalone");
        assert_eq!(
            capabilities["automation"]["availability"]["available"],
            false
        );
        assert_eq!(
            capabilities["automation"]["availability"]["executable"],
            missing_codex_bin.display().to_string()
        );
        assert!(
            capabilities["automation"]["availability"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("was not found")),
            "capabilities should explain unavailable app-server backend: {capabilities}"
        );

        let scheduler_url = format!("{base_url}/api/automation/scheduler/status");
        let (status, scheduler) = get_json(&agent, &scheduler_url);
        assert_eq!(status, 200);
        assert_eq!(scheduler["status"], "configured");
        assert_eq!(scheduler["paused"], false);
        assert_eq!(scheduler["scheduler_tick_secs"], 15);
        assert!(
            scheduler["tasks"]
                .as_array()
                .is_some_and(|tasks| tasks.iter().any(|task| {
                    task["task"] == "memory_curator"
                        && task["due"] == false
                        && task["skip_reason"] == "scheduler_schedule_manual"
                })),
            "manual memory curator should be visible as a skipped scheduler task: {scheduler}"
        );

        let (status, paused) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/scheduler/pause"),
            &serde_json::json!({}),
        );
        assert_eq!(status, 200);
        assert_eq!(paused["paused"], true);
        assert_eq!(paused["status"], "paused");
        assert_eq!(paused["enabled"], true);
        assert!(
            paused["tasks"]
                .as_array()
                .is_some_and(|tasks| tasks.iter().all(|task| {
                    task["due"] == false && task["skip_reason"] == "scheduler_paused"
                })),
            "paused scheduler should not mark any task due: {paused}"
        );
        let (status, config_after_pause) = get_json(&agent, &config_url);
        assert_eq!(status, 200);
        assert_eq!(
            config_after_pause["effective"]["enabled"], true,
            "scheduler pause must not disable automation config"
        );
        let (status, resumed) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/scheduler/resume"),
            &serde_json::json!({}),
        );
        assert_eq!(status, 200);
        assert_eq!(resumed["paused"], false);
        assert_eq!(resumed["status"], "configured");

        let hermes_patch = serde_json::json!({
            "host_mode": "delegated_host"
        });
        let (status, saved) = patch_json_body(&agent, &config_url, &hermes_patch);
        assert_eq!(status, 200);
        assert_eq!(saved["effective"]["host_mode"], "delegated_host");
        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["automation"], true);
        assert_eq!(
            capabilities["features"]["llm_curation"],
            false,
            "delegated-host mode delegates intelligence and must not advertise TraceDecay-owned LLM curation"
        );
        assert_eq!(capabilities["automation"]["mode"], "delegated_host");
        assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
        assert_eq!(capabilities["automation"]["host_mode"], "delegated_host");

        let legacy_host_mode_patch = serde_json::json!({
            "host_mode": "hermes_hosted"
        });
        let (status, legacy_saved) =
            patch_json_body(&agent, &config_url, &legacy_host_mode_patch);
        assert_eq!(status, 200);
        assert_eq!(
            legacy_saved["effective"]["host_mode"],
            "delegated_host",
            "legacy hermes_hosted config must normalize to the provider-agnostic delegated_host mode"
        );

        let external_patch = serde_json::json!({
            "backend": "external_command",
            "host_mode": "standalone"
        });
        let (status, rejected) = patch_json_body(&agent, &config_url, &external_patch);
        assert_eq!(status, 400);
        assert_eq!(rejected["validation_errors"][0]["field"], "backend");
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("external_command")),
            "external backend rejection should explain the unsupported backend: {rejected}"
        );
        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["automation"], true);
        assert_eq!(capabilities["features"]["llm_curation"], false);
        assert_eq!(capabilities["automation"]["mode"], "delegated_host");
        assert_eq!(capabilities["automation"]["backend"], "codex_app_server");
        assert_eq!(capabilities["automation"]["host_mode"], "delegated_host");

        let (status, saved_auto_apply) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "require_dashboard_approval": false,
                "auto_apply_memory_ops": true
            }),
        );
        assert_eq!(
            status, 200,
            "explicit memory auto-apply should save: {saved_auto_apply}"
        );
        assert_eq!(
            saved_auto_apply["effective"]["require_dashboard_approval"],
            false
        );
        assert_eq!(saved_auto_apply["effective"]["auto_apply_memory_ops"], true);

        let (status, rejected) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "modle": "typo-model"
            }),
        );
        assert_eq!(status, 400);
        assert_eq!(rejected["validation_errors"][0]["field"], "modle");
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("unknown field `modle`")),
            "unknown top-level field should be rejected clearly: {rejected}"
        );

        let (status, rejected) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "memory_curator": { "schedul": "manual" }
            }),
        );
        assert_eq!(status, 400);
        assert_eq!(rejected["validation_errors"][0]["field"], "schedul");
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("unknown field `schedul`")),
            "unknown nested task field should be rejected clearly: {rejected}"
        );
        server.stop();

        let cg = TraceDecay::open(&project_root)
            .await
            .unwrap_or_else(|err| panic!("failed to reopen fixture project: {err}"));
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;
        let (status, restored) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
        );
        assert_eq!(status, 200);
        assert_eq!(restored["project"]["model"], "project-model");
        assert_eq!(restored["effective"]["model"], "project-model");
        assert_eq!(
            restored["effective"]["tasks"]["memory_curator"]["enabled"],
            true
        );
        let (status, reset) = delete_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
        );
        assert_eq!(status, 200);
        assert!(reset["project"].is_null());
        assert_eq!(reset["effective"]["model"], "global-model");
        assert_eq!(
            reset["effective"]["tasks"]["memory_curator"]["enabled"],
            false
        );
        assert!(!sidecar.exists(), "DELETE must remove project sidecar");
        let (status, reset_capabilities) =
            get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(reset_capabilities["automation"]["mode"], "standalone_backend");
        assert_eq!(
            reset_capabilities["automation"]["backend"],
            "codex_app_server"
        );
        server.stop();
    });
}

#[test]
fn managed_skills_are_dashboard_controllable_and_persistent() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let fixture = start_dashboard_fixture(false).await;
        let agent = http_agent();
        let base_url = &fixture.base_url;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["features"]["managed_skills"], true);

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, empty) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(empty["count"], 0);
        assert_eq!(empty["skills"].as_array().map(Vec::len), Some(0));

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repo Hygiene",
            "summary": "Keep repository maintenance tasks consistent.",
            "category": "workflow",
            "body_markdown": "Use this when cleaning generated changes.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [99, 104, 101, 99, 107]
                }
            ],
            "provenance": {
                "source": "automation_run",
                "actor": "dashboard-test",
                "run_id": "run-dashboard-1"
            }
        });
        let (status, created) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/skills/draft"),
            &draft,
        );
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["id"], "repo-hygiene");
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert!(created["skill"]["metadata"]["created_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(created["skill"]["metadata"]["updated_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert_eq!(created["usage_summary"]["view_count"], 0);
        assert_eq!(
            created["skill"]["metadata"]["provenance"]["run_id"],
            "run-dashboard-1"
        );
        let profile_root = tracedecay::storage::default_profile_root().unwrap();
        let skill = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        tracedecay::automation::skill_usage::record_skill_usage(
            &profile_root,
            &skill,
            tracedecay::automation::skill_usage::SkillUsageAction::Use,
            "dashboard-test",
            vec!["cursor".to_string(), "codex".to_string()],
            Some("cursor".to_string()),
            None,
        )
        .await
        .unwrap();
        let global_db = GlobalDb::open()
            .await
            .expect("dashboard fixture global db opens");
        global_db
            .append_analytics_event(&tracedecay::global_db::AnalyticsEventInsert {
                provider: "mcp".to_string(),
                project_id: GlobalDb::canonical_project_key(&fixture.project_root),
                session_id: Some("dashboard-skill-session".to_string()),
                timestamp: tracedecay::tracedecay::current_timestamp(),
                event_kind: "mcp_tool_call".to_string(),
                hook_name: None,
                tool_name: Some("tracedecay_skill_view".to_string()),
                tool_category: None,
                skill_name: None,
                hint_category: None,
                hint_id: None,
                outcome: Some("success".to_string()),
                metadata_json: Some(
                    serde_json::json!({
                        "function": {
                            "name": "tracedecay_skill_view",
                            "arguments": { "id": "repo-hygiene" }
                        }
                    })
                    .to_string(),
                ),
            })
            .await
            .unwrap();

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["usage_summaries"][0]["view_count"], 1);
        assert_eq!(listed["usage_summaries"][0]["use_count"], 1);
        assert_eq!(
            listed["usage_summaries"][0]["targets"],
            serde_json::json!(["codex", "cursor", "mcp"])
        );
        assert_eq!(listed["stale_recommendations"][0]["skill_id"], "repo-hygiene");
        assert_eq!(listed["stale_recommendations"][0]["stale"], false);
        assert_eq!(listed["stale_recommendations"][0]["recommendation"], "keep");
        assert_eq!(
            listed["improvement_recommendations"][0]["skill_id"],
            "repo-hygiene"
        );
        assert_eq!(
            listed["improvement_recommendations"][0]["recommendation"],
            "none"
        );

        let skill_url = format!("{base_url}/api/automation/skills/repo-hygiene");
        let (status, viewed) = get_json(&agent, &skill_url);
        assert_eq!(status, 200);
        assert_eq!(
            viewed["skill"]["body_markdown"],
            "Use this when cleaning generated changes."
        );
        assert_eq!(viewed["usage_summary"]["use_count"], 1);
        assert_eq!(viewed["stale_recommendation"]["recommendation"], "keep");
        assert_eq!(viewed["improvement_recommendation"]["recommendation"], "none");

        let (status, approved) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);
        assert_eq!(approved["skill"]["metadata"]["state"], "active");
        assert_eq!(
            approved["skill"]["metadata"]["created_at"],
            created["skill"]["metadata"]["created_at"]
        );
        assert!(
            approved["skill"]["metadata"]["updated_at"]
                .as_i64()
                .unwrap_or_default()
                >= created["skill"]["metadata"]["updated_at"]
                    .as_i64()
                    .unwrap_or_default()
        );

        let (status, missing_checksum) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "summary": "Updated after dashboard review.",
                "body_markdown": "Use this when cleaning generated changes and record focused checks.",
                "pinned": true
            }),
        );
        assert_eq!(status, 400);
        assert!(missing_checksum["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("base_checksum")));

        let (status, patched) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "base_checksum": approved["skill"]["metadata"]["checksum"],
                "summary": "Updated after dashboard review.",
                "body_markdown": "Use this when cleaning generated changes and record focused checks.",
                "pinned": true
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            patched["skill"]["metadata"]["summary"],
            "Keep repository maintenance tasks consistent."
        );
        assert_eq!(patched["skill"]["metadata"]["state"], "active");
        assert_eq!(patched["skill"]["metadata"]["pinned"], false);
        assert_eq!(
            patched["skill"]["pending_update"]["metadata"]["summary"],
            "Updated after dashboard review."
        );
        assert_eq!(patched["skill"]["pending_update"]["metadata"]["pinned"], true);
        assert_eq!(
            patched["skill"]["pending_update"]["base_checksum"],
            approved["skill"]["metadata"]["checksum"]
        );
        assert_eq!(
            patched["skill"]["metadata"]["created_at"],
            created["skill"]["metadata"]["created_at"]
        );

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, updated) = post_json(&agent, &format!("{skill_url}/{action}"));
            assert_eq!(status, 200, "{action} should succeed");
            assert_eq!(updated["skill"]["metadata"]["state"], expected_state);
        }

        let persisted = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        assert_eq!(
            persisted.metadata.state,
            tracedecay::automation::managed_skills::ManagedSkillState::PendingApproval
        );
    });
}

#[test]
fn managed_skills_are_dashboard_controllable_with_explicit_approval() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, initial) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(initial["count"], 0);

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository checks focused.",
            "category": "maintenance",
            "body_markdown": "Run focused tests before broad suites.",
            "pinned": true
        });
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert_eq!(created["skill"]["metadata"]["pinned"], true);
        assert_eq!(
            created["skill"]["metadata"]["provenance"]["source"],
            "user_draft"
        );

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");
        assert_eq!(listed["skills"][0]["metadata"]["state"], "pending_approval");

        let skill_url = format!("{base_url}/api/automation/skills/repo-hygiene");
        let (status, updated) = patch_json_body(
            &agent,
            &skill_url,
            &serde_json::json!({
                "summary": "Updated with review evidence.",
                "body_markdown": "Record the narrow command that covers each change."
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            updated["skill"]["metadata"]["summary"],
            "Updated with review evidence."
        );
        assert_eq!(updated["skill"]["metadata"]["state"], "pending_approval");

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, payload) = post_json_body(
                &agent,
                &format!("{base_url}/api/automation/skills/repo-hygiene/{action}"),
                &serde_json::json!({}),
            );
            assert_eq!(status, 200, "{action} should succeed: {payload}");
            assert_eq!(payload["skill"]["metadata"]["state"], expected_state);
        }

        let skill_dir = profile_root
            .join("agent_managed")
            .join("skills")
            .join("repo-hygiene");
        assert!(skill_dir.join("skill.json").is_file());
        assert!(skill_dir.join("SKILL.md").is_file());
        server.stop();
    });
}

#[test]
fn automation_config_patch_does_not_rewrite_invalid_project_sidecar() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let sidecar = cg
            .store_layout()
            .dashboard_root
            .join("automation_config.json");
        let invalid_config = br#"{"enabled":true,"modle":"typo"}"#;
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, invalid_config).unwrap();

        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let (status, rejected) = patch_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
            &serde_json::json!({ "timeout_secs": 120 }),
        );
        assert_eq!(status, 500);
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("failed to parse automation config")),
            "invalid persisted config should block PATCH with a parse error: {rejected}"
        );
        assert_eq!(
            std::fs::read(&sidecar).unwrap(),
            invalid_config,
            "failed PATCH must not rewrite the invalid sidecar"
        );

        server.stop();
    });
}

#[test]
fn curation_agent_plan_skips_when_automation_is_disabled_and_records_history() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let config_url = format!("{base_url}/api/plugins/holographic/curation/config");
        let (status, saved_config) = patch_json_body(
            &agent,
            &config_url,
            &serde_json::json!({
                "enabled": false,
                "backend": "codex_app_server",
                "host_mode": "delegated_host",
                "model": "queued-model"
            }),
        );
        assert_eq!(status, 200, "config patch should succeed: {saved_config}");
        assert_eq!(saved_config["effective"]["backend"], "codex_app_server");
        assert_eq!(saved_config["effective"]["host_mode"], "delegated_host");
        assert_eq!(saved_config["effective"]["model"], "queued-model");

        let (status, payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/agent-plan"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 200);
        assert_eq!(payload["status"], "skipped");
        assert_eq!(payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(payload["ledger_record"]["error"], "automation_disabled");
        assert_eq!(payload["report"]["reason"], "automation_disabled");

        let (status, memory_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/memory-curator"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202);
        assert_eq!(memory_payload["status"], "queued");
        assert_eq!(memory_payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(memory_payload["ledger_record"]["task"], "memory_curator");
        assert_eq!(
            memory_payload["ledger_record"]["backend"],
            "codex_app_server"
        );
        assert_eq!(
            memory_payload["ledger_record"]["host_mode"],
            "delegated_host"
        );
        assert_eq!(memory_payload["ledger_record"]["model"], "queued-model");

        let (status, session_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/session-reflection"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202);
        assert_eq!(session_payload["status"], "queued");
        assert_eq!(session_payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(
            session_payload["ledger_record"]["task"],
            "session_reflector"
        );
        assert_eq!(
            session_payload["ledger_record"]["backend"],
            "codex_app_server"
        );
        assert_eq!(
            session_payload["ledger_record"]["host_mode"],
            "delegated_host"
        );
        assert_eq!(session_payload["ledger_record"]["model"], "queued-model");

        let (status, skill_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/skill-writing"),
            &serde_json::json!({
                "dry_run": true,
                "provider": "cursor",
                "query": "workflow corrections",
                "evidence_limit": 7
            }),
        );
        assert_eq!(status, 202);
        assert_eq!(skill_payload["status"], "queued");
        assert_eq!(skill_payload["ledger_record"]["trigger"], "dashboard");
        assert_eq!(skill_payload["ledger_record"]["task"], "skill_writer");
        assert_eq!(
            skill_payload["ledger_record"]["backend"],
            "codex_app_server"
        );
        assert_eq!(
            skill_payload["ledger_record"]["host_mode"],
            "delegated_host"
        );
        assert_eq!(skill_payload["ledger_record"]["model"], "queued-model");

        let (status, rejected) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/session-reflection"),
            &serde_json::json!({ "dry_run": false }),
        );
        assert_eq!(status, 400);
        assert!(
            rejected["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("dry_run=true")),
            "dry-run guard should explain the approval-only contract: {rejected}"
        );

        let run_ids = [
            memory_payload["run_id"].as_str().unwrap().to_string(),
            session_payload["run_id"].as_str().unwrap().to_string(),
            skill_payload["run_id"].as_str().unwrap().to_string(),
        ];
        let mut records = Vec::new();
        let mut terminal_count = 0;
        for _ in 0..200 {
            records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            terminal_count = records
                .iter()
                .filter(|record| {
                    run_ids.contains(&record.run_id)
                        && record.status.is_terminal()
                        && record.error.as_deref() == Some("automation_disabled")
                })
                .count();
            if terminal_count == run_ids.len() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            terminal_count,
            run_ids.len(),
            "dashboard automation jobs did not reach terminal skipped records: {records:#?}"
        );
        assert_eq!(records.len(), 4);
        let tasks: Vec<_> = records.iter().map(|record| record.task).collect();
        assert_eq!(
            tasks,
            [
                tracedecay::automation::backend::AgentTaskKind::SkillWriter,
                tracedecay::automation::backend::AgentTaskKind::SessionReflector,
                tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
                tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
            ]
        );
        for record in &records {
            assert_eq!(
                record.trigger,
                tracedecay::automation::run_ledger::AutomationTrigger::Dashboard
            );
            assert_eq!(
                record.status,
                tracedecay::automation::run_ledger::AutomationRunStatus::Skipped
            );
            assert_eq!(record.error.as_deref(), Some("automation_disabled"));
            assert_eq!(record.backend, "codex_app_server");
            assert_eq!(record.host_mode.as_deref(), Some("delegated_host"));
            assert_eq!(record.model.as_deref(), Some("queued-model"));
        }

        let (status, runs) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/runs?limit=5"),
        );
        assert_eq!(status, 200);
        assert_eq!(runs["count"], 4);
        assert_eq!(runs["limit"], 5);
        assert_eq!(runs["records"][0]["trigger"], "dashboard");
        assert_eq!(runs["records"][0]["status"], "skipped");
        assert_eq!(runs["records"][0]["error"], "automation_disabled");

        let (status, activity) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/activity"),
        );
        assert_eq!(status, 200);
        let events = activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected activity events array: {activity}"));
        let phases: Vec<_> = events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "evidence",
            "backend",
            "validation",
            "apply",
            "report",
            "finish",
        ] {
            assert!(
                phases.contains(&phase),
                "agent-plan should emit {phase} activity; phases={phases:?}, activity={activity}"
            );
        }
        let memory_skip_phases: Vec<_> = events
            .iter()
            .filter(|event| {
                event["message"].as_str().is_some_and(|message| {
                    message
                        .to_ascii_lowercase()
                        .contains("dashboard memory-curator automation run")
                })
            })
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "evidence",
            "backend",
            "validation",
            "apply",
            "report",
            "finish",
        ] {
            assert!(
                memory_skip_phases.contains(&phase),
                "queued memory-curator skip should emit {phase} activity; phases={memory_skip_phases:?}, activity={activity}"
            );
        }
        for task_label in ["session-reflector", "skill-writer"] {
            let task_skip_phases: Vec<_> = events
                .iter()
                .filter(|event| {
                    event["message"].as_str().is_some_and(|message| {
                        message
                            .to_ascii_lowercase()
                            .contains(&format!("dashboard {task_label} automation run"))
                    })
                })
                .filter_map(|event| event["phase"].as_str())
                .collect();
            for phase in [
                "queued",
                "evidence",
                "backend",
                "validation",
                "apply",
                "report",
                "finish",
            ] {
                assert!(
                    task_skip_phases.contains(&phase),
                    "queued {task_label} skip should emit {phase} activity; phases={task_skip_phases:?}, activity={activity}"
                );
            }
        }
        assert!(
            events.iter().any(|event| event["message"]
                .as_str()
                .is_some_and(|message| message
                    .contains("Dashboard memory-curator automation run skipped"))),
            "dashboard memory-curator queued skip should emit visible activity: {activity}"
        );
        assert!(
            events.iter().any(|event| event["phase"] == "report"),
            "agent-plan should write a visible curation activity event: {activity}"
        );
        assert!(
            events.iter().any(|event| {
                event["phase"] == "finish"
                    && event["dry_run"] == true
                    && event["message"].as_str().is_some_and(|message| {
                        message.contains("Finished standalone memory-curator agent plan")
                    })
            }),
            "agent-plan should emit a terminal finish activity event: {activity}"
        );

        let (status, runs) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/runs"),
        );
        assert_eq!(status, 200);
        assert_eq!(runs["count"], 4);
        assert!(
            runs["records"].as_array().is_some_and(|records| records
                .iter()
                .any(|record| record["run_id"] == memory_payload["run_id"]
                    && record["status"] == "skipped")),
            "memory-curator run should remain visible in newest-first history: {runs}"
        );
        server.stop();
    });
}

#[test]
fn dashboard_session_and_skill_runs_emit_activity_when_evidence_is_unavailable() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let (status, config) = patch_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
            &serde_json::json!({
                "enabled": true,
                "backend": "codex_app_server",
                "host_mode": "standalone",
                "session_reflector": { "enabled": true, "schedule": "manual" },
                "skill_writer": { "enabled": true, "schedule": "manual" }
            }),
        );
        assert_eq!(status, 200, "automation config patch failed: {config}");

        let (status, session_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/session-reflection"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202, "session run should queue: {session_payload}");
        let session_run_id = session_payload["run_id"].as_str().unwrap().to_string();
        let mut records = Vec::new();
        let mut session_terminal = false;
        for _ in 0..400 {
            records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            session_terminal = records.iter().any(|record| {
                record.run_id == session_run_id && record.status.is_terminal()
            });
            if session_terminal {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            session_terminal,
            "session-reflector job did not reach a terminal record: {records:#?}"
        );

        let (status, skill_payload) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/skill-writing"),
            &serde_json::json!({ "dry_run": true }),
        );
        assert_eq!(status, 202, "skill run should queue: {skill_payload}");
        let skill_run_id = skill_payload["run_id"].as_str().unwrap().to_string();

        let run_ids = [session_run_id, skill_run_id];
        let mut terminal_count = 0;
        for _ in 0..400 {
            records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            terminal_count = records
                .iter()
                .filter(|record| run_ids.contains(&record.run_id) && record.status.is_terminal())
                .count();
            if terminal_count == run_ids.len() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(
            terminal_count,
            run_ids.len(),
            "dashboard automation jobs did not reach terminal records: {records:#?}"
        );
        for run_id in &run_ids {
            let terminal = records
                .iter()
                .find(|record| record.run_id == *run_id && record.status.is_terminal())
                .unwrap_or_else(|| panic!("missing terminal record for {run_id}: {records:#?}"));
            assert_eq!(
                terminal.status,
                tracedecay::automation::run_ledger::AutomationRunStatus::Skipped
            );
            assert!(
                terminal.error.as_deref().is_some_and(|reason| reason
                    == "lcm_not_ingested"
                    || reason == "no_session_evidence"
                    || reason == "no_skill_writer_evidence"),
                "unexpected evidence skip reason: {terminal:#?}"
            );
        }

        let (status, activity) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/activity?limit=50"),
        );
        assert_eq!(status, 200);
        let events = activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected activity events array: {activity}"));
        for task_label in ["session-reflector", "skill-writer"] {
            let task_phases: Vec<_> = events
                .iter()
                .filter(|event| {
                    event["message"].as_str().is_some_and(|message| {
                        message
                            .to_ascii_lowercase()
                            .contains(&format!("dashboard {task_label} automation run"))
                    })
                })
                .filter_map(|event| event["phase"].as_str())
                .collect();
            for phase in [
                "queued",
                "evidence",
                "backend",
                "validation",
                "apply",
                "report",
                "finish",
            ] {
                assert!(
                    task_phases.contains(&phase),
                    "queued {task_label} run should emit {phase} activity; phases={task_phases:?}, activity={activity}"
                );
            }
        }

        server.stop();
    });
}

#[test]
fn final_self_improvement_smoke_covers_manual_curation_skill_approval_and_dashboard_review() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);
        let fake_codex = FakeCodexAppServer::new_memory_curator();
        let _codex_bin_guard = EnvVarGuard::set("TRACEDECAY_CODEX_BIN", &fake_codex.bin);

        let cg = setup_project(&project_root).await;
        seed_memory_fixture(&cg).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let (status, config) = patch_json_body(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/config"),
            &serde_json::json!({
                "enabled": true,
                "backend": "codex_app_server",
                "host_mode": "standalone",
                "model": "dashboard-configured-model",
                "memory_curator": { "enabled": true, "schedule": "manual" }
            }),
        );
        assert_eq!(status, 200, "automation config patch failed: {config}");
        assert_eq!(config["effective"]["enabled"], true);
        assert_eq!(config["effective"]["backend"], "codex_app_server");

        let (status, queued) = post_json_body(
            &agent,
            &format!("{base_url}/api/automation/run/memory-curator"),
            &serde_json::json!({
                "dry_run": true,
                "max_clusters": 4,
                "min_confidence": 0.5
            }),
        );
        assert_eq!(status, 202, "dashboard automation run failed: {queued}");
        assert_eq!(queued["status"], "queued");
        let run_id = queued["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("queued response should include run_id: {queued}"))
            .to_string();

        let mut record = None;
        for _ in 0..200 {
            let records = tracedecay::automation::run_ledger::load_run_records(&dashboard_root, 10)
                .await
                .unwrap();
            record = records
                .into_iter()
                .find(|record| record.run_id == run_id && record.status.is_terminal());
            if record.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let record = record.unwrap_or_else(|| {
            panic!("dashboard automation run did not reach a terminal ledger record")
        });
        assert_eq!(
            record.status,
            tracedecay::automation::run_ledger::AutomationRunStatus::Succeeded
        );
        assert_eq!(record.accepted_count, 1);
        assert_eq!(record.rejected_count, 0);
        assert_eq!(record.artifacts.len(), 6);

        let artifact_url = format!("{base_url}/api/automation/runs/{run_id}/artifacts");
        let (status, listed) = get_json(&agent, &artifact_url);
        assert_eq!(status, 200, "artifact list failed: {listed}");
        assert_eq!(listed["count"], 6);
        assert_eq!(listed["artifact_chain"]["complete"], true);
        assert_eq!(
            listed["artifact_chain"]["present_kinds"],
            serde_json::json!([
                "traces",
                "feedback",
                "generated_evals",
                "validation_gate",
                "optimizer_diagnosis",
                "codex_handoff"
            ])
        );

        let (status, evals) = get_json(&agent, &format!("{artifact_url}/generated_evals"));
        assert_eq!(status, 200, "generated eval artifact failed: {evals}");
        assert_eq!(evals["payload"]["format"], "tracedecay_automation_eval:v1");
        assert_eq!(evals["payload"]["runner"]["status"], "passed");
        assert_eq!(
            evals["payload"]["runner"]["results"][0]["status"],
            "passed"
        );
        assert_eq!(evals["payload"]["promotion"]["state"], "validated");
        assert_eq!(
            evals["payload"]["eval_definitions"][0]["eval_id"],
            "memory_curator:accepted:0"
        );

        let (status, gate) = get_json(&agent, &format!("{artifact_url}/validation_gate"));
        assert_eq!(status, 200, "validation gate artifact failed: {gate}");
        assert_eq!(gate["payload"]["task_validation"]["decision"], "passed");
        assert_eq!(
            gate["payload"]["improvement_gate"]["decision"],
            "ready_for_handoff"
        );
        assert_eq!(
            gate["payload"]["improvement_gate"]["generated_evals_status"],
            "passed"
        );

        let (status, handoff) = get_json(&agent, &format!("{artifact_url}/codex_handoff"));
        assert_eq!(status, 200, "Codex handoff artifact failed: {handoff}");
        assert_eq!(handoff["payload"]["status"], "ready_for_review");
        assert_eq!(
            handoff["payload"]["machine_summary"]["next_stage"],
            "codex_review"
        );
        assert_eq!(
            handoff["payload"]["artifact_manifest"]["api_list"],
            format!("/api/automation/runs/{run_id}/artifacts")
        );
        assert!(
            handoff["payload"]["artifact_manifest"]["refs"]
                .as_array()
                .is_some_and(|refs| refs
                    .iter()
                    .any(|reference| reference["kind"] == "optimizer_diagnosis")),
            "handoff should preserve upstream artifact refs: {handoff}"
        );

        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, created_skill) = post_json_body(
            &agent,
            &skills_url,
            &serde_json::json!({
                "id": "final-smoke-review",
                "title": "Final smoke review",
                "summary": "Review self-improvement run artifacts and approval state.",
                "category": "workflow",
                "body_markdown": "Check the run ledger, generated evals, validation gate, and pending skill approval before applying changes.",
                "targets": ["codex"],
                "provenance": {
                    "source": "automation_run",
                    "actor": "dashboard-smoke",
                    "run_id": run_id
                }
            }),
        );
        assert_eq!(status, 200, "skill draft should be accepted: {created_skill}");
        assert_eq!(
            created_skill["skill"]["metadata"]["state"],
            "pending_approval"
        );
        assert_eq!(
            created_skill["skill"]["metadata"]["provenance"]["run_id"],
            run_id
        );

        let (status, approved_skill) = post_json(
            &agent,
            &format!("{base_url}/api/automation/skills/final-smoke-review/approve"),
        );
        assert_eq!(status, 200, "skill approval should succeed: {approved_skill}");
        assert_eq!(approved_skill["skill"]["metadata"]["state"], "active");

        let (status, skill_detail) = get_json(
            &agent,
            &format!("{base_url}/api/automation/skills/final-smoke-review"),
        );
        assert_eq!(status, 200, "approved skill should remain reviewable: {skill_detail}");
        assert_eq!(skill_detail["skill"]["metadata"]["state"], "active");
        assert_eq!(
            skill_detail["skill"]["metadata"]["provenance"]["source"],
            "automation_run"
        );

        let (status, runs) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/runs?limit=5"),
        );
        assert_eq!(status, 200);
        assert!(
            runs["records"]
                .as_array()
                .is_some_and(
                    |records| records.iter().any(|record| record["run_id"] == run_id
                        && record["status"] == "succeeded"
                        && record["artifacts"]
                            .as_array()
                            .is_some_and(|artifacts| artifacts.len() == 6))
                ),
            "successful dashboard automation run should be visible in history: {runs}"
        );

        let (status, activity) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/holographic/curation/activity?limit=20"),
        );
        assert_eq!(status, 200);
        let activity_events = activity["events"]
            .as_array()
            .unwrap_or_else(|| panic!("expected curation activity events: {activity}"));
        let activity_phases: Vec<_> = activity_events
            .iter()
            .filter_map(|event| event["phase"].as_str())
            .collect();
        for phase in [
            "queued",
            "evidence",
            "backend",
            "validation",
            "apply",
            "report",
            "finish",
        ] {
            assert!(
                activity_phases.contains(&phase),
                "successful dashboard automation run should emit {phase} activity; phases={activity_phases:?}, activity={activity}"
            );
        }

        server.stop();
    });
}

#[test]
fn automation_run_artifact_api_serves_verified_sidecar_payloads() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let dashboard_root = cg.store_layout().dashboard_root.clone();
        let run_id = "artifact_api_run";
        let created_at = "2026-06-24T00:00:00Z";
        let artifact = tracedecay::automation::run_ledger::write_run_artifact(
            &dashboard_root,
            run_id,
            tracedecay::automation::run_ledger::AutomationRunArtifactKind::CodexHandoff,
            &serde_json::json!({
                "schema_version": 1,
                "run_id": run_id,
                "status": "ready_for_review",
                "next_actions": ["review dashboard artifact payload"]
            }),
            Some("handoff ready".to_string()),
            created_at,
        )
        .await
        .unwrap();
        tracedecay::automation::run_ledger::append_run_record(
            &dashboard_root,
            &tracedecay::automation::run_ledger::AutomationRunLedgerRecord {
                schema_version: 2,
                run_id: run_id.to_string(),
                trigger: tracedecay::automation::run_ledger::AutomationTrigger::ManualCli,
                task: tracedecay::automation::backend::AgentTaskKind::MemoryCurator,
                task_key: Some("memory_curator".to_string()),
                backend: "codex_app_server".to_string(),
                host_mode: Some("standalone".to_string()),
                prompt_version: Some("memory_curator:v1".to_string()),
                response_schema: None,
                strict_json: None,
                model: Some("test-model".to_string()),
                status: tracedecay::automation::run_ledger::AutomationRunStatus::Succeeded,
                evidence_hash: Some("sha256:evidence".to_string()),
                input_hash: Some("sha256:input".to_string()),
                output_hash: Some("sha256:output".to_string()),
                proposed_ops: None,
                applied_ops: None,
                rejected_ops: None,
                validation_report: None,
                reviewed_count: 1,
                accepted_count: 1,
                rejected_count: 0,
                skipped_count: 0,
                error: None,
                error_classification: None,
                error_retryable: None,
                fallback_status: None,
                report_ref: None,
                artifacts: vec![artifact],
                started_at: created_at.to_string(),
                completed_at: created_at.to_string(),
            },
        )
        .await
        .unwrap();

        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let artifact_url = format!("{base_url}/api/automation/runs/{run_id}/artifacts");
        let (status, listed) = get_json(&agent, &artifact_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["artifacts"][0]["kind"], "codex_handoff");
        assert_eq!(listed["artifacts"][0]["summary"], "handoff ready");
        assert_eq!(listed["artifact_chain"]["complete"], false);
        assert_eq!(
            listed["artifact_chain"]["expected_kinds"],
            serde_json::json!([
                "traces",
                "feedback",
                "generated_evals",
                "validation_gate",
                "optimizer_diagnosis",
                "codex_handoff"
            ])
        );
        assert_eq!(
            listed["artifact_chain"]["present_kinds"],
            serde_json::json!(["codex_handoff"])
        );

        let (status, payload) = get_json(&agent, &format!("{artifact_url}/codex_handoff"));
        assert_eq!(status, 200);
        assert_eq!(payload["artifact"]["kind"], "codex_handoff");
        assert_eq!(payload["payload"]["run_id"], run_id);
        assert_eq!(payload["payload"]["status"], "ready_for_review");

        let (status, missing) = get_json(&agent, &format!("{artifact_url}/validation_gate"));
        assert_eq!(status, 404);
        assert!(missing["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("not found")));

        let artifact_path = tracedecay::automation::run_ledger::run_artifact_path(
            &dashboard_root,
            run_id,
            tracedecay::automation::run_ledger::AutomationRunArtifactKind::CodexHandoff,
        )
        .unwrap();
        std::fs::write(&artifact_path, "{\"tampered\":true}\n").unwrap();
        let (status, tampered) = get_json(&agent, &format!("{artifact_url}/codex_handoff"));
        assert_eq!(status, 500);
        assert!(tampered["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("hash mismatch")));

        server.stop();
    });
}

#[test]
fn managed_skill_dashboard_api_persists_and_updates_lifecycle() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository maintenance guidance current.",
            "category": "maintenance",
            "body_markdown": "Use focused checks before changing generated files.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [45, 32, 114, 117, 110, 32, 116, 101, 115, 116, 115, 10]
                }
            ],
            "provenance": {
                "source": "user_draft",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let (status, created) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        assert_eq!(created["skill"]["metadata"]["state"], "pending_approval");
        assert!(created["skill"]["metadata"]["created_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(created["skill"]["metadata"]["updated_at"]
            .as_i64()
            .is_some_and(|value| value > 0));
        assert!(
            profile_root
                .join("agent_managed/skills/repo-hygiene/SKILL.md")
                .is_file(),
            "drafting a managed skill must persist a SKILL.md package"
        );

        let (status, listed) = get_json(&agent, &skills_url);
        assert_eq!(status, 200);
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["skills"][0]["metadata"]["id"], "repo-hygiene");

        let (status, viewed) = get_json(
            &agent,
            &format!("{base_url}/api/automation/skills/repo-hygiene"),
        );
        assert_eq!(status, 200);
        assert_eq!(viewed["skill"]["metadata"]["id"], "repo-hygiene");

        for (action, expected_state) in [
            ("approve", "active"),
            ("disable", "disabled"),
            ("archive", "archived"),
            ("restore", "pending_approval"),
        ] {
            let (status, response) = post_json(
                &agent,
                &format!("{base_url}/api/automation/skills/repo-hygiene/{action}"),
            );
            assert_eq!(status, 200);
            assert_eq!(response["skill"]["metadata"]["state"], expected_state);
        }
        server.stop();
    });
}

#[test]
fn managed_skill_dashboard_api_controls_staged_updates() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let tmp_root = tmp
            .path()
            .canonicalize()
            .unwrap_or_else(|err| panic!("failed to canonicalize temp root: {err}"));
        let project_root = tmp_root.join("project");
        let global_db_path = tmp_root.join("global").join("global.db");
        let profile_root = tmp_root.join("profile").join(".tracedecay");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);
        let _data_dir_guard = EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root);

        let cg = setup_project(&project_root).await;
        let agent = http_agent();
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mut server = spawn_dashboard_server(cg, port);
        wait_for_dashboard(&agent, &base_url).await;

        let draft = serde_json::json!({
            "id": "repo-hygiene",
            "title": "Repository hygiene",
            "summary": "Keep repository maintenance guidance current.",
            "category": "maintenance",
            "body_markdown": "Use focused checks before changing generated files.",
            "support_files": [
                {
                    "path": "references/checklist.md",
                    "bytes": [45, 32, 114, 117, 110, 32, 116, 101, 115, 116, 115, 10]
                }
            ],
            "provenance": {
                "source": "user_draft",
                "actor": "dashboard",
                "run_id": null
            }
        });
        let skills_url = format!("{base_url}/api/automation/skills");
        let skill_url = format!("{skills_url}/repo-hygiene");
        let (status, _) = post_json_body(&agent, &skills_url, &draft);
        assert_eq!(status, 200);
        let (status, _) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);

        let active = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        let base_checksum = active.metadata.checksum.clone();
        tracedecay::automation::managed_skills::stage_managed_skill_update(
            &profile_root,
            "repo-hygiene",
            &base_checksum,
            tracedecay::automation::managed_skills::ManagedSkillUpdate {
                summary: Some("Stage dashboard-visible generated guidance.".to_string()),
                body_markdown: Some(
                    "Review the run ledger before applying generated edits.".to_string(),
                ),
                support_files: Some(vec![
                    tracedecay::automation::managed_skills::ManagedSupportFile::new(
                        "templates/review.md",
                        b"review body".to_vec(),
                    )
                    .unwrap(),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (status, staged_view) = get_json(&agent, &skill_url);
        assert_eq!(status, 200);
        assert_eq!(staged_view["skill"]["metadata"]["state"], "active");
        assert_eq!(
            staged_view["skill"]["metadata"]["summary"],
            "Keep repository maintenance guidance current."
        );
        assert_eq!(
            staged_view["skill"]["pending_update"]["metadata"]["summary"],
            "Stage dashboard-visible generated guidance."
        );
        let skill_dir = profile_root.join("agent_managed/skills/repo-hygiene");
        assert!(skill_dir.join("references/checklist.md").is_file());
        assert!(!skill_dir.join("templates/review.md").exists());

        let (status, discarded) = post_json(&agent, &format!("{skill_url}/discard-update"));
        assert_eq!(status, 200);
        assert!(discarded["skill"]["pending_update"].is_null());
        assert_eq!(
            discarded["skill"]["metadata"]["summary"],
            "Keep repository maintenance guidance current."
        );

        let active = tracedecay::automation::managed_skills::load_managed_skill(
            &profile_root,
            "repo-hygiene",
        )
        .await
        .unwrap();
        tracedecay::automation::managed_skills::stage_managed_skill_update(
            &profile_root,
            "repo-hygiene",
            &active.metadata.checksum,
            tracedecay::automation::managed_skills::ManagedSkillUpdate {
                summary: Some("Approve dashboard-visible generated guidance.".to_string()),
                body_markdown: Some(
                    "Review the run ledger before applying generated edits.".to_string(),
                ),
                support_files: Some(vec![
                    tracedecay::automation::managed_skills::ManagedSupportFile::new(
                        "templates/review.md",
                        b"review body".to_vec(),
                    )
                    .unwrap(),
                ]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let (status, approved) = post_json(&agent, &format!("{skill_url}/approve"));
        assert_eq!(status, 200);
        assert_eq!(approved["skill"]["metadata"]["state"], "active");
        assert_eq!(
            approved["skill"]["metadata"]["summary"],
            "Approve dashboard-visible generated guidance."
        );
        assert!(approved["skill"]["pending_update"].is_null());
        assert!(!skill_dir.join("references/checklist.md").exists());
        assert!(skill_dir.join("templates/review.md").is_file());

        server.stop();
    });
}
