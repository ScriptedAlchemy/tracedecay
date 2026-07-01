#![allow(dead_code, unused_imports)]

pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::process::Command;
pub(crate) use std::thread;

pub(crate) use crate::common::{
    create_runtime, fake_codex_bin, get_json, http_agent, http_agent_with_timeout,
    install_fake_codex_launcher, pick_free_port, response_to_json, tempdir_or_panic,
    wait_for_dashboard, EnvVarGuard, GLOBAL_DB_ENV, GLOBAL_DB_ENV_LOCK,
};
pub(crate) use serde_json::Value;
pub(crate) use tempfile::TempDir;
pub(crate) use tracedecay::config::USER_DATA_DIR_ENV;
pub(crate) use tracedecay::dashboard;
pub(crate) use tracedecay::errors::TraceDecayError;
pub(crate) use tracedecay::global_db::GlobalDb;
pub(crate) use tracedecay::memory::encoding::HolographicEncoder;
pub(crate) use tracedecay::sessions::lcm::{LcmSourceRef, LcmSummaryNodeDraft};
pub(crate) use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
pub(crate) use tracedecay::storage::{write_enrollment_marker, EnrollmentMarker, StorageMode};
pub(crate) use tracedecay::tracedecay::TraceDecay;

/// Longer than 200 chars on purpose: list/projection payloads truncate
/// `content` at 200, so this fact proves the `/fact/{id}` detail endpoint
/// returns the full text.
pub(crate) const LONG_FACT_CONTENT: &str = "LCM dashboard empty states need explicit copy. \
The drawer, search results, charts, and overview panels must each explain why \
they are empty and what action will populate them, because first-run users \
otherwise assume the integration is broken when the store simply has no rows yet.";

pub(crate) struct DashboardFixture {
    pub(crate) _tmp: TempDir,
    pub(crate) _env_guard: EnvVarGuard,
    pub(crate) _data_dir_guard: EnvVarGuard,
    pub(crate) base_url: String,
    pub(crate) project_root: std::path::PathBuf,
    pub(crate) project_db_path: std::path::PathBuf,
    pub(crate) server: DashboardServer,
}

impl Drop for DashboardFixture {
    fn drop(&mut self) {
        self.server.stop();
    }
}

pub(crate) struct DashboardServer {
    pub(crate) shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    pub(crate) thread: Option<thread::JoinHandle<()>>,
}

impl DashboardServer {
    pub(crate) fn stop(&mut self) {
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

pub(crate) fn spawn_dashboard_server(cg: TraceDecay, port: u16) -> DashboardServer {
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

pub(crate) fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            panic!("failed to create {}: {err}", parent.display());
        }
    }
    if let Err(err) = fs::write(path, content) {
        panic!("failed to write {}: {err}", path.display());
    }
}

pub(crate) async fn setup_project(project_root: &Path) -> TraceDecay {
    write_file(
        &project_root.join("src/lib.rs"),
        "pub fn seed_fixture() -> &'static str { \"dashboard\" }\n",
    );
    match TraceDecay::init(project_root).await {
        Ok(cg) => cg,
        Err(err) => panic!("failed to initialize tracedecay fixture project: {err}"),
    }
}

pub(crate) fn blob_param(bytes: Vec<u8>) -> libsql::Value {
    libsql::Value::Blob(bytes)
}

pub(crate) async fn seed_memory_fixture(cg: &TraceDecay) {
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

    if let Err(err) = conn.execute("BEGIN IMMEDIATE", ()).await {
        panic!("failed to begin memory fixture transaction: {err}");
    }

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

    if let Err(err) = conn.execute("COMMIT", ()).await {
        let _ = conn.execute("ROLLBACK", ()).await;
        panic!("failed to commit memory fixture transaction: {err}");
    }
}

pub(crate) async fn seed_lcm_fixture(global_db: &GlobalDb, project_path: &Path) {
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

pub(crate) fn post_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = match agent.post(url).send_empty() {
        Ok(response) => response,
        Err(err) => panic!("POST {url} failed: {err}"),
    };
    response_to_json(response)
}

pub(crate) fn post_json_body(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response = match agent.post(url).send_json(body) {
        Ok(response) => response,
        Err(err) => panic!("POST {url} (with body) failed: {err}"),
    };
    response_to_json(response)
}

pub(crate) fn patch_json_body(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response = match agent.patch(url).send_json(body) {
        Ok(response) => response,
        Err(err) => panic!("PATCH {url} (with body) failed: {err}"),
    };
    response_to_json(response)
}

pub(crate) fn delete_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = match agent.delete(url).call() {
        Ok(response) => response,
        Err(err) => panic!("DELETE {url} failed: {err}"),
    };
    response_to_json(response)
}

pub(crate) struct FakeCodexAppServer {
    pub(crate) _temp: TempDir,
    pub(crate) bin: PathBuf,
}

impl FakeCodexAppServer {
    pub(crate) fn new_memory_curator() -> Self {
        let temp = tempdir_or_panic();
        let script_path = temp.path().join("codex.py");
        let bin = fake_codex_bin(temp.path());
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
        write_file(&script_path, script);
        install_fake_codex_launcher(&script_path, &bin);
        Self { _temp: temp, bin }
    }
}

pub(crate) async fn start_dashboard_fixture(seed_lcm: bool) -> DashboardFixture {
    start_dashboard_fixture_with_options(seed_lcm, true).await
}

pub(crate) async fn start_dashboard_fixture_without_memory() -> DashboardFixture {
    start_dashboard_fixture_with_options(false, false).await
}

async fn start_dashboard_fixture_with_options(
    seed_lcm: bool,
    seed_memory: bool,
) -> DashboardFixture {
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
    if seed_memory {
        seed_memory_fixture(&cg).await;
    }

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
pub(crate) async fn count_in_project_db(
    fixture: &DashboardFixture,
    sql: &str,
    fact_id: i64,
) -> i64 {
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

pub(crate) async fn string_in_project_db(
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

pub(crate) async fn project_db_conn(fixture: &DashboardFixture) -> libsql::Connection {
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
pub(crate) async fn set_fact_vector_and_bump_updated_at(
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

pub(crate) async fn clear_fact_vector_without_touching_updated_at(
    fixture: &DashboardFixture,
    fact_id: i64,
) {
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

pub(crate) async fn set_fact_access_without_touching_updated_at(
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

pub(crate) fn git(project: &Path, args: &[&str]) {
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

pub(crate) fn commit_all(project: &Path, message: &str) {
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

pub(crate) async fn index_all_retrying_sync_lock(cg: &TraceDecay, context: &str) {
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

/// Opens (creating if needed) the resolved project session store — profile
/// sharded by default, project-local only for explicit or legacy projects.
pub(crate) async fn open_project_session_store(project_root: &Path) -> GlobalDb {
    let db_path = tracedecay::sessions::cursor::project_session_db_path(project_root);
    match GlobalDb::open_at(&db_path).await {
        Some(db) => db,
        None => panic!(
            "failed to open project session store at {}",
            db_path.display()
        ),
    }
}
