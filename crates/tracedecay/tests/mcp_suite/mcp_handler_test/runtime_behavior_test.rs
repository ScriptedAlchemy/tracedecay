//! Observed `tracedecay_runtime` responses.
//!
//! Calls go through the production MCP dispatch (`handle_tool_call`), the
//! same entry the server uses for `tools/call`. Assertions are the JSON a
//! caller reads, not which helpers ran.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_project::project::TraceDecay;

use crate::support::{extract_json, handle_tool_call, setup_empty_project};

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn observed_file_len(path: &Path) -> u64 {
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("stat {}: {error}", path.display()),
    }
}

async fn runtime_payload(cg: &TraceDecay, args: Value) -> Value {
    let result = handle_tool_call(cg, "tracedecay_runtime", args, None, None)
        .await
        .expect("tracedecay_runtime call");
    assert_eq!(result.value["content"][0]["type"], "text");
    extract_json(&result.value)
}

fn assert_unavailable_census(database: &Value) {
    assert_eq!(
        database["generation_census"],
        json!({
            "state": "unavailable",
            "reason": "authority_unavailable",
        }),
        "a handler call with no census reader publishes the typed unavailable census, not a fabricated count"
    );
}

/// A bare call reports this process and the admitted store files, and does
/// not invent doctor, session, or audit sections.
#[tokio::test]
async fn runtime_reports_this_process_and_the_admitted_store_files() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let graph_db = cg.store_layout().graph_db_path.clone();
    let dirty = with_suffix(&graph_db, ".dirty");
    assert!(
        !dirty.exists(),
        "this fixture starts without a dirty marker"
    );

    let payload = runtime_payload(&cg, json!({ "format": "json" })).await;

    assert_eq!(
        payload["host_os"],
        std::env::consts::OS,
        "the snapshot names the host the process is running on"
    );
    assert_eq!(
        payload["tracedecay_version"].as_str(),
        Some(tracedecay_project::version::build_version().expect("fixture product runtime"))
    );
    assert!(payload.get("doctor_report").is_none());
    assert!(payload.get("session_temporal_health").is_none());
    assert!(payload.get("cursor_session_ingest").is_none());
    assert!(payload.get("cursor_session_placeholder_paths").is_none());

    let database = &payload["database"];
    assert_eq!(database["project_root"], json!(cg.project_root()));
    assert_eq!(database["db_path"], json!(graph_db));
    assert_eq!(
        database["canonical_db_path"],
        json!(graph_db.canonicalize().expect("graph db exists"))
    );
    assert_eq!(
        database["db_size_bytes"],
        observed_file_len(&graph_db),
        "db size is the file the admitted layout owns"
    );
    assert_eq!(
        database["wal_size_bytes"],
        observed_file_len(&with_suffix(&graph_db, "-wal"))
    );
    assert_eq!(
        database["shm_size_bytes"],
        observed_file_len(&with_suffix(&graph_db, "-shm"))
    );
    assert_eq!(database["journal_mode"], "wal");
    // The telemetry query reads the admitted connection, whose SQLite default
    // is FULL (2). That is not the writer lane's NORMAL (1).
    assert_eq!(database["synchronous"], 2);
    assert_eq!(database["quick_check_ok"], Value::Null);
    assert_eq!(database["quick_check_error"], Value::Null);
    assert_eq!(
        database["dirty_marker"],
        json!({
            "path": dirty,
            "exists": false,
            "parsed": false,
            "owner_pid": null,
            "epoch": null,
            "state": null,
            "schema": null,
        })
    );
    assert_unavailable_census(database);
    assert!(database.get("authority_audit_ok").is_none());
    assert!(database.get("authority_audit_reason").is_none());
    assert!(database.get("authority_audit_error").is_none());

    let process = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let payload = runtime_payload(&cg, json!({ "format": "json" })).await;
            let state = payload["process"]["state"].as_str().map(str::to_owned);
            if matches!(state.as_deref(), Some("sampled" | "stale")) {
                break payload["process"].clone();
            }
            assert_eq!(
                payload["process"],
                json!({ "state": "not_yet_sampled" }),
                "before the sampler finishes the process object is exactly not_yet_sampled, with no invented pid"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("background sampler publishes a process observation");
    assert_eq!(
        process["pid"],
        u64::from(std::process::id()),
        "a completed sample reports this process"
    );
}

/// Opt-in arguments add one typed section and leave the others off.
#[tokio::test]
async fn runtime_opt_in_sections_are_exact_typed_payloads() {
    let (cg, _env, _dir) = setup_empty_project().await;

    let baseline = runtime_payload(&cg, json!({ "format": "json" })).await;
    assert!(baseline.get("doctor_report").is_none());
    assert!(baseline.get("session_temporal_health").is_none());
    assert!(baseline.get("cursor_session_ingest").is_none());
    assert_eq!(baseline["database"]["quick_check_ok"], Value::Null);

    let doctor = runtime_payload(&cg, json!({ "format": "json", "doctor_report": true })).await;
    assert_eq!(
        doctor["doctor_report"],
        json!({
            "kind": "unsupported",
            "table_growth_evidence": [],
            "schema_convergences": [],
        })
    );
    assert!(doctor.get("session_temporal_health").is_none());
    assert!(doctor.get("cursor_session_ingest").is_none());
    assert!(doctor["database"].get("authority_audit_ok").is_none());

    let temporal = runtime_payload(
        &cg,
        json!({ "format": "json", "session_temporal_health": true }),
    )
    .await;
    assert_eq!(
        temporal["session_temporal_health"],
        json!({
            "status": "unavailable",
            "findings": [],
        })
    );
    assert!(temporal.get("doctor_report").is_none());
    assert!(temporal.get("cursor_session_ingest").is_none());
    assert!(temporal["database"].get("authority_audit_ok").is_none());

    let ingest = runtime_payload(
        &cg,
        json!({ "format": "json", "session_ingest_health": true }),
    )
    .await;
    assert_eq!(
        ingest["cursor_session_ingest"],
        json!({
            "status": "unavailable",
            "reason": "session_store_denied",
            "message": "this request is not authorized to read the admitted project session store",
        })
    );
    assert!(ingest.get("cursor_session_placeholder_paths").is_none());
    assert!(ingest.get("doctor_report").is_none());
    assert!(ingest.get("session_temporal_health").is_none());

    let audit = runtime_payload(&cg, json!({ "format": "json", "authority_audit": true })).await;
    let database = &audit["database"];
    assert_eq!(database["authority_audit_ok"], Value::Null);
    assert_eq!(
        database["authority_audit_reason"],
        "authority_store_unavailable"
    );
    assert_eq!(
        database["authority_audit_error"],
        "authoritative global registry is unavailable"
    );
    assert_eq!(database["quick_check_ok"], true);
    assert_eq!(database["quick_check_error"], Value::Null);
    assert_eq!(
        audit["session_temporal_health"],
        json!({
            "status": "unavailable",
            "findings": [],
        })
    );
    assert!(audit.get("doctor_report").is_none());
    assert!(audit.get("cursor_session_ingest").is_none());
    assert_unavailable_census(database);
}
