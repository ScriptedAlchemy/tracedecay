use std::ffi::OsString;
use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tokensave::dashboard;
use tokensave::global_db::GlobalDb;
use tokensave::memory::encoding::HolographicEncoder;
use tokensave::sessions::lcm::{LcmSourceRef, LcmSummaryNodeDraft};
use tokensave::sessions::{SessionMessageRecord, SessionRecord};
use tokensave::tokensave::TokenSave;

static GLOBAL_DB_ENV_LOCK: Mutex<()> = Mutex::new(());
const GLOBAL_DB_ENV: &str = "TOKENSAVE_GLOBAL_DB";

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

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

fn tempdir_or_panic() -> TempDir {
    match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("failed to create temp dir: {err}"),
    }
}

fn create_runtime() -> tokio::runtime::Runtime {
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => panic!("failed to create tokio runtime: {err}"),
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

async fn setup_project(project_root: &Path) -> TokenSave {
    write_file(
        &project_root.join("src/lib.rs"),
        "pub fn seed_fixture() -> &'static str { \"dashboard\" }\n",
    );
    match TokenSave::init(project_root).await {
        Ok(cg) => cg,
        Err(err) => panic!("failed to initialize tokensave fixture project: {err}"),
    }
}

fn blob_param(bytes: Vec<u8>) -> libsql::Value {
    libsql::Value::Blob(bytes)
}

async fn seed_memory_fixture(cg: &TokenSave) {
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
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at, hrr_vector, hrr_dim)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                3_i64
            ],
        ),
        (
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at, hrr_vector, hrr_dim)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                3_i64
            ],
        ),
        (
            "INSERT INTO memory_facts
                (fact_id, content, category, tags, trust_score, retrieval_count, helpful_count, created_at, updated_at, hrr_vector, hrr_dim)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            libsql::params![
                103_i64,
                "LCM dashboard empty states need explicit copy",
                "tool",
                "[\"lcm\",\"ux\"]",
                0.76_f64,
                3_i64,
                2_i64,
                1_700_000_020_i64,
                1_700_000_120_i64,
                blob_param(vec_c.clone()),
                3_i64
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

    let bank_rows = [("project", bank_a, 2_i64), ("tool", bank_b, 1_i64)];
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
        project_key: "tokensave-fixture".to_string(),
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
            tool_names: Some("tokensave_search".to_string()),
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
            tool_names: Some("tokensave_lcm_status".to_string()),
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

fn pick_free_port() -> u16 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => panic!("failed to bind free local port: {err}"),
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => panic!("failed to read bound local address: {err}"),
    };
    drop(listener);
    port
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .into()
}

fn response_to_json(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let body = match response.body_mut().read_to_string() {
        Ok(body) => body,
        Err(err) => panic!("failed to read response body: {err}"),
    };
    let parsed = match serde_json::from_str::<Value>(&body) {
        Ok(value) => value,
        Err(err) => panic!("failed to decode JSON body `{body}`: {err}"),
    };
    (status, parsed)
}

fn get_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(err) => panic!("GET {url} failed: {err}"),
    };
    response_to_json(response)
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

async fn wait_for_dashboard(agent: &ureq::Agent, base_url: &str) {
    let probe = format!("{base_url}/api/capabilities");
    for _ in 0..80 {
        if agent.get(&probe).call().is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("dashboard server did not become ready at {base_url}");
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
    let project_db_path = project_root.join(".tokensave").join("tokensave.db");
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
/// `tokensave_fact_store` recall reads.
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
        assert_eq!(overview["providers"]["memory_provider"], "tokensave");
        assert_eq!(overview["holographic"]["overview"]["facts"], 3);
        assert_eq!(overview["holographic"]["overview"]["banks"], 2);
        assert_eq!(overview["holographic"]["overview"]["entities"], 3);
        let facts = overview["holographic"]["facts"]
            .as_array()
            .unwrap_or_else(|| panic!("expected facts array in overview payload"));
        assert_eq!(facts.len(), 2, "query should filter to cache facts only");
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
        //     tokensave_fact_store recall reads (hard delete, not soft). ---
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
