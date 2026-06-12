mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::{
    create_runtime, get_json, http_agent, pick_free_port, response_to_json, tempdir_or_panic,
    wait_for_dashboard, EnvVarGuard, GLOBAL_DB_ENV, GLOBAL_DB_ENV_LOCK,
};
use serde_json::Value;
use tempfile::TempDir;
use tracedecay::branch;
use tracedecay::dashboard;
use tracedecay::global_db::GlobalDb;
use tracedecay::memory::encoding::HolographicEncoder;
use tracedecay::sessions::lcm::{LcmSourceRef, LcmSummaryNodeDraft};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
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
    base_url: String,
    project_db_path: std::path::PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for DashboardFixture {
    fn drop(&mut self) {
        self.server.abort();
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

async fn start_dashboard_fixture(seed_lcm: bool) -> DashboardFixture {
    let tmp = tempdir_or_panic();
    let project_root = tmp.path().join("project");
    let global_db_path = tmp.path().join("global").join("global.db");
    let env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);

    let cg = setup_project(&project_root).await;
    seed_memory_fixture(&cg).await;

    let global_db = match GlobalDb::open_at(&global_db_path).await {
        Some(db) => db,
        None => panic!(
            "failed to open temporary global DB at {}",
            global_db_path.display()
        ),
    };
    if seed_lcm {
        seed_lcm_fixture(&global_db, &project_root).await;
    }
    drop(global_db);

    let port = pick_free_port();
    let base_url = format!("http://127.0.0.1:{port}");
    let project_db_path = project_root.join(".tracedecay").join("tracedecay.db");
    let server = tokio::spawn(async move {
        let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
    });

    let agent = http_agent();
    wait_for_dashboard(&agent, &base_url).await;

    DashboardFixture {
        _tmp: tmp,
        _env_guard: env_guard,
        base_url,
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
            Some(1),
            "fixture starts with one near-duplicate pair"
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
        assert!(
            repaired_cache["pairs"].as_array().map_or(0, Vec::len) >= 3,
            "re-encoded vectors (updated_at bump) must invalidate the similarity cache, got {repaired_cache}"
        );

        clear_fact_vector_without_touching_updated_at(&fixture, 103).await;
        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let project_root = fixture
            .project_db_path
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| panic!("fixture DB path should be under .tracedecay"))
            .to_path_buf();
        let cg = match TraceDecay::open(&project_root).await {
            Ok(cg) => cg,
            Err(err) => panic!("failed to reopen fixture project: {err}"),
        };
        let server = tokio::spawn(async move {
            let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
        });
        wait_for_dashboard(&agent, &base_url).await;
        let (status, _capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        server.abort();
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
        if let Err(err) = main.index_all().await {
            panic!("failed to index main branch fixture: {err}");
        }
        drop(main);

        git(&project_root, &["checkout", "-b", "feature/dashboard-path"]);
        fs::write(
            project_root.join("src/feature.rs"),
            "pub fn feature_branch_symbol() {}\n",
        )
        .unwrap_or_else(|err| panic!("failed to write feature fixture: {err}"));
        if let Err(err) = branch::add_branch_tracking(&project_root, "feature/dashboard-path").await
        {
            panic!("failed to track feature branch: {err}");
        }
        let cg = match TraceDecay::open(&project_root).await {
            Ok(cg) => cg,
            Err(err) => panic!("failed to open feature branch fixture: {err}"),
        };
        let expected = cg.db_path().display().to_string();
        assert!(
            expected
                .replace('\\', "/")
                .contains(".tracedecay/branches/"),
            "fixture should serve a branch DB path, got {expected}"
        );

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let server = tokio::spawn(async move {
            let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
        });
        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        server.abort();
        assert_eq!(status, 200);
        assert_eq!(capabilities["memory_db"], expected);
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
        assert_eq!(overview["holographic"]["overview"]["banks"], 2);
        assert_eq!(overview["holographic"]["overview"]["entities"], 3);
        // Bank list counts must be live (consistent with the header fact
        // count), not the stale stored bundle snapshot — which stays exposed
        // as bundled_fact_count.
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
            project_bank["bundled_fact_count"], 5,
            "stale bundled snapshot must stay available for staleness UIs"
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
        assert_eq!(projection["dim"], 3);
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
        assert_eq!(similarity["dim"], 3);
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
            overview["storage_scope"], "global",
            "TRACEDECAY_GLOBAL_DB override fixtures serve the global scope"
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

/// Opens (creating if needed) the project-local session store at
/// `<project>/.tracedecay/sessions.db` — the DB transcript ingest writes to.
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
/// project-local `.tracedecay/sessions.db` (where Cursor hooks and the
/// catch-up sweep ingest transcripts), and report it via the additive
/// `storage_scope` payload field.
#[test]
fn lcm_serves_project_session_store_without_global_override() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let project_root = tmp.path().join("project");
        let _env_guard = EnvVarGuard::unset(GLOBAL_DB_ENV);

        let cg = setup_project(&project_root).await;
        let session_store = open_project_session_store(&project_root).await;
        seed_lcm_fixture(&session_store, &project_root).await;
        drop(session_store);

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let server = tokio::spawn(async move {
            let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
        });

        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["lcm_scope"], "project_local");
        assert_eq!(capabilities["features"]["lcm"], true);
        let lcm_db = capabilities["lcm_db"]
            .as_str()
            .unwrap_or_else(|| panic!("expected capabilities.lcm_db string"));
        assert!(
            lcm_db
                .replace('\\', "/")
                .ends_with(".tracedecay/sessions.db"),
            "capabilities.lcm_db should be the project session store, got {lcm_db}"
        );

        let (status, overview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/overview?limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["storage_scope"], "project_local");
        assert_eq!(overview["exists"], true);
        assert_eq!(overview["overview"]["messages_total"], 3);
        assert_eq!(overview["overview"]["sessions_total"], 1);
        assert_eq!(overview["overview"]["summary_nodes_total"], 1);
        let path = overview["path"]
            .as_str()
            .unwrap_or_else(|| panic!("expected overview.path string"));
        assert!(
            path.replace('\\', "/").ends_with(".tracedecay/sessions.db"),
            "overview.path should be the project session store, got {path}"
        );

        let (status, search) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/search?q=vector&limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(search["storage_scope"], "project_local");
        let search_messages = search["matches"]["messages"]
            .as_array()
            .unwrap_or_else(|| panic!("expected search.matches.messages array"));
        assert!(
            !search_messages.is_empty(),
            "project-store search should match seeded messages"
        );

        server.abort();
    });
}

/// An explicit `TRACEDECAY_GLOBAL_DB` override pins the dashboard to that
/// store even when the project-local session store exists and has rows —
/// the contract the smoke harness and the Hermes wrapper rely on.
#[test]
fn lcm_global_override_wins_over_project_store() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let project_root = tmp.path().join("project");
        let global_db_path = tmp.path().join("global").join("global.db");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);

        let cg = setup_project(&project_root).await;
        // The project store has rows; the overridden global store has none.
        let session_store = open_project_session_store(&project_root).await;
        seed_lcm_fixture(&session_store, &project_root).await;
        drop(session_store);

        let port = pick_free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let server = tokio::spawn(async move {
            let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
        });

        let agent = http_agent();
        wait_for_dashboard(&agent, &base_url).await;

        let (status, capabilities) = get_json(&agent, &format!("{base_url}/api/capabilities"));
        assert_eq!(status, 200);
        assert_eq!(capabilities["lcm_scope"], "global");

        let (status, overview) = get_json(
            &agent,
            &format!("{base_url}/api/plugins/hermes-lcm/overview?limit=20"),
        );
        assert_eq!(status, 200);
        assert_eq!(overview["storage_scope"], "global");
        assert_eq!(overview["exists"], true);
        assert_eq!(
            overview["overview"]["messages_total"], 0,
            "override must serve the pinned (empty) store, not the project store"
        );
        let path = overview["path"]
            .as_str()
            .unwrap_or_else(|| panic!("expected overview.path string"));
        assert_eq!(path, global_db_path.display().to_string());

        server.abort();
    });
}

/// The dry-run curation preview must survive a dashboard restart: it is
/// mirrored to `.tracedecay/dashboard/curation_preview.json` and re-hydrated
/// by `build_state`, and applying curation clears both the memory copy and
/// the sidecar.
#[test]
fn curation_preview_persists_across_dashboard_restarts() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime = create_runtime();
    runtime.block_on(async {
        let tmp = tempdir_or_panic();
        let project_root = tmp.path().join("project");
        let global_db_path = tmp.path().join("global").join("global.db");
        let _env_guard = EnvVarGuard::set(GLOBAL_DB_ENV, &global_db_path);

        let cg = setup_project(&project_root).await;
        seed_memory_fixture(&cg).await;
        let agent = http_agent();
        let sidecar = project_root
            .join(".tracedecay")
            .join("dashboard")
            .join("curation_preview.json");

        async fn start_server(cg: TraceDecay) -> (String, tokio::task::JoinHandle<()>) {
            let port = pick_free_port();
            let base_url = format!("http://127.0.0.1:{port}");
            let server = tokio::spawn(async move {
                let _ = dashboard::run(&cg, "127.0.0.1", port, false).await;
            });
            (base_url, server)
        }

        async fn stop_server(server: tokio::task::JoinHandle<()>) {
            server.abort();
            let _ = server.await;
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
        stop_server(server).await;
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
        stop_server(server).await;

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
        stop_server(server).await;
    });
}
