//! Read-only doctor runtime telemetry: cold store probes and typed
//! `tracedecay_runtime` responses served without opening project stores.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::time::{Duration, timeout};

use super::{DaemonHandshake, projectless_tool_call, write_json_rpc_response};
use crate::errors::Result;
use crate::mcp::{JsonRpcRequest, JsonRpcResponse, McpTransport};

pub(crate) const DOCTOR_GRAPH_SCHEMA_VERSION: i64 = 23;
/// `SQLITE_OPEN_URI`, which libsql does not expose through [`libsql::OpenFlags`].
pub(crate) const SQLITE_OPEN_URI: i32 = 0x0000_0040;

#[derive(Debug)]
pub(crate) struct DoctorRuntimeRequest {
    id: serde_json::Value,
}

pub(crate) fn doctor_runtime_request(request_line: &str) -> Option<DoctorRuntimeRequest> {
    let request = serde_json::from_str::<JsonRpcRequest>(request_line.trim()).ok()?;
    if request.method != "tools/call" {
        return None;
    }
    let (tool_name, arguments) = projectless_tool_call(request.params.as_ref()).ok()?;
    if tool_name != "tracedecay_runtime"
        || arguments
            .get("authority_audit")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || arguments
            .get("session_ingest_health")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || arguments.get("format").and_then(serde_json::Value::as_str) != Some("json")
    {
        return None;
    }
    Some(DoctorRuntimeRequest {
        id: request.id.unwrap_or(serde_json::Value::Null),
    })
}

fn doctor_runtime_temporal_unavailable(reason: &str) -> serde_json::Value {
    let finding = match reason {
        "project_store_missing" | "session_store_missing" => "migration_gap",
        _ => "compatibility_drift",
    };
    json!({
        "status": if reason.ends_with("_locked") { "locked" } else { "unavailable" },
        "reason": reason,
        "findings": [{
            "kind": finding,
            "count": 1,
        }],
    })
}

fn doctor_runtime_temporal_report(
    report: crate::global_db::SessionTemporalHealthReport,
) -> serde_json::Value {
    let mut value = serde_json::to_value(report).unwrap_or_else(|_| {
        doctor_runtime_temporal_unavailable("session_health_serialization_failed")
    });
    let has_reason = value
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reason| !reason.is_empty());
    let unavailable_without_findings = value.get("status").and_then(serde_json::Value::as_str)
        == Some("unavailable")
        && value
            .get("findings")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty);
    // Preserve fixed path-API reasons (for example uncheckpointed_wal). Only
    // synthesize a compatibility finding when the report is reason-less.
    if unavailable_without_findings && !has_reason {
        value["findings"] = json!([{
            "kind": "compatibility_drift",
            "count": 1,
        }]);
    }
    value
}

fn doctor_runtime_unavailable(
    project_path: Option<&Path>,
    reason: &'static str,
) -> serde_json::Value {
    json!({
        "tracedecay_version": env!("CARGO_PKG_VERSION"),
        "database": {
            "project_root": project_path,
            "quick_check_ok": null,
            "quick_check_error": reason,
            "authority_audit_ok": null,
            "authority_audit_reason": "authority_audit_not_run",
            "authority_audit_error": "authority_audit_not_run",
        },
        "doctor_runtime": {
            "status": if reason.ends_with("_locked") { "locked" } else { "unavailable" },
            "reason": reason,
            "read_only": true,
        },
        "session_temporal_health": doctor_runtime_temporal_unavailable(reason),
        "cursor_session_ingest": {
            "status": "unavailable",
            "reason": "session_store_unavailable",
        },
    })
}

pub(crate) fn doctor_runtime_tool_result(value: serde_json::Value) -> serde_json::Value {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| {
        r#"{"doctor_runtime":{"status":"unavailable","reason":"serialization_failed","read_only":true}}"#
            .to_string()
    });
    json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
    })
}

fn doctor_runtime_store_paths(
    project_path: &Path,
    profile_root: &Path,
) -> std::result::Result<(PathBuf, PathBuf), &'static str> {
    let branch = crate::branch::current_branch(project_path);
    doctor_runtime_store_paths_for_branch(project_path, profile_root, branch.as_deref())
}

fn doctor_runtime_store_paths_for_branch(
    project_path: &Path,
    profile_root: &Path,
    branch: Option<&str>,
) -> std::result::Result<(PathBuf, PathBuf), &'static str> {
    let layout = match crate::storage::read_enrollment_marker(project_path) {
        Ok(Some(marker)) => {
            crate::storage::profile_sharded_layout(project_path, profile_root, &marker)
                .map_err(|_| "project_store_schema_unsupported")?
        }
        Ok(None) => {
            if let Some(layout) =
                crate::storage::resolve_persisted_layout(project_path, profile_root)
                    .map_err(|_| "project_store_schema_unsupported")?
            {
                layout
            } else {
                let data_root = crate::config::get_tracedecay_dir(project_path);
                let legacy_paths = (
                    data_root.join(crate::config::db_filename(&data_root)),
                    data_root.join("sessions.db"),
                );
                if legacy_paths.0.is_file() {
                    return Ok(legacy_paths);
                }
                crate::storage::default_profile_sharded_layout(project_path, profile_root)
                    .map_err(|_| "project_store_schema_unsupported")?
            }
        }
        Err(_) => return Err("project_store_schema_unsupported"),
    };
    let (graph_path, _, _) = crate::tracedecay::TraceDecay::resolve_db_for_branch(
        project_path,
        &layout.data_root,
        branch,
    );
    Ok((graph_path, layout.sessions_db_path))
}

async fn doctor_read_only_database(
    db_path: &Path,
    intent: &str,
) -> std::result::Result<crate::db::Database, &'static str> {
    if !db_path.is_file() {
        return Err("store_missing");
    }
    let authority = crate::db::DatabaseAuthority::for_runtime(db_path, intent)
        .map_err(|_| "store_unavailable")?;
    crate::daemon::store_runtime::driver::GraphLibsqlCompatDriver::open(
        crate::daemon::store_runtime::driver::GraphStoreOpenMode::ReadOnly,
        db_path,
        &authority,
    )
    .await
    .map(|(database, _)| database)
    .map_err(|error| {
        let message = error.to_string().to_ascii_lowercase();
        if message.contains("database is locked")
            || message.contains("database table is locked")
            || message.contains("sqlite_busy")
        {
            "store_locked"
        } else {
            "store_unavailable"
        }
    })
}

async fn doctor_connection_i64_result(
    conn: &libsql::Connection,
    query: &str,
) -> std::result::Result<Option<i64>, libsql::Error> {
    let mut rows = conn.query(query, ()).await?;
    match rows.next().await? {
        Some(row) => row.get(0).map(Some),
        None => Ok(None),
    }
}

async fn doctor_connection_i64(conn: &libsql::Connection, query: &str) -> Option<i64> {
    doctor_connection_i64_result(conn, query)
        .await
        .ok()
        .flatten()
}

async fn doctor_connection_text(conn: &libsql::Connection, query: &str) -> Option<String> {
    let mut rows = conn.query(query, ()).await.ok()?;
    rows.next().await.ok().flatten()?.get::<String>(0).ok()
}

async fn doctor_database_i64(database: &crate::db::Database, query: &str) -> Option<i64> {
    doctor_connection_i64(database.conn(), query).await
}

async fn doctor_database_text(database: &crate::db::Database, query: &str) -> Option<String> {
    doctor_connection_text(database.conn(), query).await
}

fn doctor_sidecar_size(db_path: &Path, suffix: &str) -> u64 {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    std::fs::metadata(PathBuf::from(path)).map_or(0, |metadata| metadata.len())
}

fn doctor_graph_error_reason(error: &libsql::Error) -> &'static str {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("locked") || message.contains("busy") {
        "project_store_locked"
    } else {
        "project_store_unavailable"
    }
}

fn doctor_uses_rollback_journal(db_path: &Path) -> bool {
    let mut header = [0_u8; 20];
    std::fs::File::open(db_path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok_and(|()| header[18] == 1 && header[19] == 1)
}

async fn doctor_global_db_read_only(
    db_path: &Path,
    intent: &str,
) -> Option<crate::global_db::GlobalDb> {
    let preflight = doctor_read_only_database(db_path, intent).await.ok()?;
    let database = crate::global_db::GlobalDb::open_read_only_at(db_path).await;
    drop(preflight);
    database
}

async fn cold_doctor_graph_value(
    project_path: &Path,
    graph_path: &Path,
) -> std::result::Result<serde_json::Value, &'static str> {
    if !graph_path.is_file() {
        return Err("project_store_missing");
    }
    if !crate::storage::has_sqlite_database_header(graph_path).unwrap_or(false) {
        return Err("project_store_unavailable");
    }
    // `immutable=1` deliberately ignores a WAL. Refuse an incomplete
    // snapshot rather than quietly reporting stale graph metadata.
    if doctor_sidecar_size(graph_path, "-wal") > 0 {
        return Err("project_store_uncheckpointed_wal");
    }
    let database = if doctor_uses_rollback_journal(graph_path) {
        // A rollback-journal store can be checked with SQLite's ordinary
        // read-only lock protocol without creating WAL/SHM sidecars. This
        // preserves the typed locked result that immutable mode would hide.
        crate::db::libsql_local::open_local_database(graph_path, true)
            .await
            .map_err(|error| doctor_graph_error_reason(&error))?
    } else {
        let uri = crate::sqlite_read_snapshot::immutable_uri(graph_path)
            .map_err(|_| "project_store_unavailable")?;
        let flags = libsql::OpenFlags::SQLITE_OPEN_READ_ONLY
            | libsql::OpenFlags::from_bits_retain(SQLITE_OPEN_URI);
        crate::db::libsql_local::open_local_database_with_flags(std::path::Path::new(&uri), flags)
            .await
            .map_err(|error| doctor_graph_error_reason(&error))?
    };
    let conn = database
        .connect()
        .map_err(|error| doctor_graph_error_reason(&error))?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 0;")
        .await
        .map_err(|error| doctor_graph_error_reason(&error))?;
    let schema_version = match doctor_connection_i64_result(&conn, "PRAGMA user_version").await {
        Ok(Some(version)) => version,
        Ok(None) => return Err("project_store_unavailable"),
        Err(error) => return Err(doctor_graph_error_reason(&error)),
    };
    if schema_version != DOCTOR_GRAPH_SCHEMA_VERSION {
        return Err("project_store_schema_unsupported");
    }
    let canonical_graph_path = graph_path
        .canonicalize()
        .unwrap_or_else(|_| graph_path.to_path_buf());
    Ok(json!({
        "tracedecay_version": env!("CARGO_PKG_VERSION"),
        "process": {
            "pid": std::process::id(),
        },
        "database": {
            "project_root": project_path,
            "db_path": graph_path,
            "canonical_db_path": canonical_graph_path,
            "db_size_bytes": std::fs::metadata(graph_path).map_or(0, |metadata| metadata.len()),
            "wal_size_bytes": doctor_sidecar_size(graph_path, "-wal"),
            "shm_size_bytes": doctor_sidecar_size(graph_path, "-shm"),
            "journal_mode": doctor_connection_text(&conn, "PRAGMA journal_mode").await,
            "synchronous": doctor_connection_i64(&conn, "PRAGMA synchronous").await,
            "page_size": doctor_connection_i64(&conn, "PRAGMA page_size").await,
            "quick_check_ok": true,
            "quick_check_error": null,
            "schema_version": schema_version,
        },
        "doctor_runtime": {
            "status": "complete",
            "reason": null,
            "read_only": true,
        },
    }))
}

async fn cold_doctor_runtime_value_for_paths(
    project_path: &Path,
    graph_path: &Path,
    session_path: &Path,
) -> serde_json::Value {
    let mut value = match cold_doctor_graph_value(project_path, graph_path).await {
        Ok(value) => value,
        Err(reason) => return doctor_runtime_unavailable(Some(project_path), reason),
    };
    value["database"]["authority_audit_ok"] = json!(null);
    value["database"]["authority_audit_reason"] = json!("authority_audit_not_run");
    value["database"]["authority_audit_error"] = json!("authority_audit_not_run");
    value["session_temporal_health"] = if session_path.is_file() {
        match timeout(
            Duration::from_secs(8),
            crate::global_db::session_temporal::session_temporal_doctor_health_at(session_path),
        )
        .await
        {
            Ok(report) => doctor_runtime_temporal_report(report),
            Err(_) => doctor_runtime_temporal_unavailable("session_health_timed_out"),
        }
    } else {
        doctor_runtime_temporal_unavailable("session_store_missing")
    };
    value["cursor_session_ingest"] = json!({
        "status": "unavailable",
        "reason": "session_store_unavailable",
    });
    value["cursor_session_placeholder_paths"] = json!([]);
    value
}

async fn doctor_runtime_value(handshake: &DaemonHandshake) -> serde_json::Value {
    doctor_runtime_value_inner(handshake, false).await
}

async fn doctor_runtime_value_inner(handshake: &DaemonHandshake, cold: bool) -> serde_json::Value {
    let Some(project_path) = handshake.project_path.as_deref() else {
        return doctor_runtime_unavailable(None, "project_path_missing");
    };
    let (graph_path, session_path) =
        match doctor_runtime_store_paths(project_path, &handshake.client_identity.profile_root) {
            Ok(paths) => paths,
            Err(reason) => return doctor_runtime_unavailable(Some(project_path), reason),
        };
    if cold {
        return cold_doctor_runtime_value_for_paths(project_path, &graph_path, &session_path).await;
    }
    let graph = match doctor_read_only_database(&graph_path, "doctor graph read-only").await {
        Ok(graph) => graph,
        Err("store_missing") => {
            return doctor_runtime_unavailable(Some(project_path), "project_store_missing");
        }
        Err("store_locked") => {
            return doctor_runtime_unavailable(Some(project_path), "project_store_locked");
        }
        Err(_) => {
            return doctor_runtime_unavailable(Some(project_path), "project_store_unavailable");
        }
    };
    let schema_version = match doctor_database_i64(&graph, "PRAGMA user_version").await {
        Some(version) => version,
        None => {
            return doctor_runtime_unavailable(Some(project_path), "project_store_unavailable");
        }
    };
    if schema_version != DOCTOR_GRAPH_SCHEMA_VERSION {
        return doctor_runtime_unavailable(Some(project_path), "project_store_schema_unsupported");
    }
    let canonical_graph_path = graph_path
        .canonicalize()
        .unwrap_or_else(|_| graph_path.clone());
    let mut value = json!({
        "tracedecay_version": env!("CARGO_PKG_VERSION"),
        "process": {
            "pid": std::process::id(),
        },
        "database": {
            "project_root": project_path,
            "db_path": graph_path,
            "canonical_db_path": canonical_graph_path,
            "db_size_bytes": std::fs::metadata(&graph_path).map_or(0, |metadata| metadata.len()),
            "wal_size_bytes": doctor_sidecar_size(&graph_path, "-wal"),
            "shm_size_bytes": doctor_sidecar_size(&graph_path, "-shm"),
            "journal_mode": doctor_database_text(&graph, "PRAGMA journal_mode").await,
            "synchronous": doctor_database_i64(&graph, "PRAGMA synchronous").await,
            "page_size": doctor_database_i64(&graph, "PRAGMA page_size").await,
            "quick_check_ok": true,
            "quick_check_error": null,
            "schema_version": schema_version,
        },
        "doctor_runtime": {
            "status": "complete",
            "reason": null,
            "read_only": true,
        },
    });

    let registry = doctor_global_db_read_only(
        &handshake.client_identity.global_db_path,
        "doctor authority read-only",
    )
    .await;
    let (authority_ok, authority_reason) = match registry.as_ref() {
        Some(registry) => match registry.audit_observation_authority().await {
            Ok(()) => (Some(true), None),
            Err(_) => (Some(false), Some("authority_invariant_failed")),
        },
        None if handshake.client_identity.global_db_path.is_file() => {
            (None, Some("authority_store_unavailable"))
        }
        None => (None, Some("authority_store_missing")),
    };
    value["database"]["authority_audit_ok"] = json!(authority_ok);
    value["database"]["authority_audit_reason"] = json!(authority_reason);
    value["database"]["authority_audit_error"] = json!(authority_reason);

    let session_db =
        doctor_global_db_read_only(&session_path, "doctor session temporal read-only").await;
    value["session_temporal_health"] = match session_db.as_ref() {
        Some(db) => {
            match timeout(Duration::from_secs(8), db.session_temporal_doctor_health()).await {
                Ok(report) => doctor_runtime_temporal_report(report),
                Err(_) => doctor_runtime_temporal_unavailable("session_health_timed_out"),
            }
        }
        None if session_path.is_file() => {
            doctor_runtime_temporal_unavailable("session_store_unavailable")
        }
        None => doctor_runtime_temporal_unavailable("session_store_missing"),
    };
    value["cursor_session_ingest"] = match session_db.as_ref() {
        Some(db) => {
            serde_json::to_value(db.session_ingest_health_for_provider(Some("cursor")).await)
                .unwrap_or_else(|_| {
                    json!({
                        "status": "unavailable",
                        "reason": "session_ingest_serialization_failed",
                    })
                })
        }
        None => json!({
            "status": "unavailable",
            "reason": "session_store_unavailable",
        }),
    };
    value["cursor_session_placeholder_paths"] = match session_db.as_ref() {
        Some(db) => json!(db.literal_workspace_placeholder_transcript_paths(10).await),
        None => json!([]),
    };
    value
}

pub(crate) async fn cold_doctor_runtime_value(handshake: &DaemonHandshake) -> serde_json::Value {
    // Both stores use immutable `mode=ro` reads, so this route does not acquire
    // database authority, create locks or sidecars, apply schema, or start workers.
    doctor_runtime_value_inner(handshake, true).await
}

pub(crate) async fn write_doctor_runtime_response(
    transport: &mut impl McpTransport,
    handshake: &DaemonHandshake,
    request: DoctorRuntimeRequest,
) -> Result<()> {
    let result = doctor_runtime_tool_result(doctor_runtime_value(handshake).await);
    write_json_rpc_response(transport, &JsonRpcResponse::success(request.id, result)).await
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod doctor_runtime_route_tests {
    use std::path::{Path, PathBuf};

    use super::{cold_doctor_runtime_value, doctor_runtime_request, doctor_runtime_store_paths};
    use crate::client_identity::DaemonClientIdentity;
    use crate::daemon::DaemonHandshake;
    use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

    fn handshake(
        project_path: PathBuf,
        profile_root: PathBuf,
        global_db_path: PathBuf,
    ) -> DaemonHandshake {
        DaemonHandshake {
            project_path: Some(project_path),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity {
                profile_root,
                global_db_path,
            },
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            client_instance_id: "doctor-runtime-test".to_string(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
        }
    }

    fn filesystem_manifest(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn visit(root: &Path, current: &Path, entries: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut children = std::fs::read_dir(current)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for path in children {
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    entries.push((relative, Vec::new()));
                    visit(root, &path, entries);
                } else {
                    entries.push((relative, std::fs::read(&path).unwrap()));
                }
            }
        }

        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries
    }

    async fn checkpoint_sqlite_wal(path: &Path) {
        let database = libsql::Builder::new_local(path).build().await.unwrap();
        let connection = database.connect().unwrap();
        let mut rows = connection
            .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 0, "checkpoint must not be busy");
        assert_eq!(
            row.get::<i64>(1).unwrap(),
            row.get::<i64>(2).unwrap(),
            "checkpoint must flush every WAL frame"
        );
    }

    fn remove_sqlite_sidecars(path: &Path) {
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(sidecar));
        }
    }

    fn has_non_empty_wal(path: &Path) -> bool {
        let mut wal_path = path.as_os_str().to_os_string();
        wal_path.push("-wal");
        std::fs::metadata(PathBuf::from(wal_path))
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
    }

    #[test]
    fn only_explicit_doctor_runtime_requests_take_the_safe_route() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/call",
            "params": {
                "name": "tracedecay_runtime",
                "arguments": {
                    "format": "json",
                    "authority_audit": true,
                    "session_ingest_health": true,
                },
            },
        })
        .to_string();
        let parsed = doctor_runtime_request(&request).expect("doctor runtime request");
        assert_eq!(parsed.id, serde_json::json!(7));

        let ordinary = request.replace("\"authority_audit\":true", "\"authority_audit\":false");
        assert!(doctor_runtime_request(&ordinary).is_none());
    }

    #[tokio::test]
    async fn cold_missing_store_returns_typed_findings_without_creating_files() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let handshake = handshake(project, profile.clone(), profile.join("registry.db"));
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/reason"),
            Some(&serde_json::json!("project_store_missing"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/findings/0/kind"),
            Some(&serde_json::json!("migration_gap"))
        );
    }

    #[tokio::test]
    async fn malformed_store_returns_fixed_safe_error_without_sidecars() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("registry.db")),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize test project");
        let db_path = initialized.db_path().clone();
        drop(initialized);
        std::fs::write(&db_path, b"malformed doctor fixture").unwrap();
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = db_path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(sidecar));
        }
        let handshake = handshake(project, profile.clone(), profile.join("registry.db"));
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/reason"),
            Some(&serde_json::json!("project_store_unavailable"))
        );
        assert_eq!(
            value.pointer("/database/quick_check_error"),
            Some(&serde_json::json!("project_store_unavailable"))
        );
        assert!(!value.to_string().contains("malformed doctor fixture"));
    }

    #[tokio::test]
    async fn old_graph_schema_returns_fixed_compatibility_finding_without_migrating() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let data_root = crate::config::get_tracedecay_dir(&project);
        std::fs::create_dir_all(&data_root).unwrap();
        let db_path = data_root.join(crate::config::db_filename(&data_root));
        let legacy = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let connection = legacy.connect().unwrap();
        connection
            .execute_batch("PRAGMA user_version=1; CREATE TABLE legacy_graph(id INTEGER);")
            .await
            .unwrap();
        drop(connection);
        drop(legacy);
        let handshake = handshake(project, profile.clone(), profile.join("registry.db"));
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/reason"),
            Some(&serde_json::json!("project_store_schema_unsupported"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/findings/0/kind"),
            Some(&serde_json::json!("compatibility_drift"))
        );
    }

    #[tokio::test]
    async fn old_session_schema_returns_typed_findings_without_migrating() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("registry.db")),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize test project");
        let session_path = initialized.store_layout().sessions_db_path.clone();
        drop(initialized);
        std::fs::create_dir_all(session_path.parent().unwrap()).unwrap();
        let legacy = libsql::Builder::new_local(&session_path)
            .build()
            .await
            .unwrap();
        let connection = legacy.connect().unwrap();
        connection
            .execute("CREATE TABLE legacy_sessions(id INTEGER PRIMARY KEY)", ())
            .await
            .unwrap();
        drop(connection);
        drop(legacy);
        let handshake = handshake(project, profile.clone(), profile.join("registry.db"));
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_ne!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("complete"))
        );
        assert!(
            value
                .pointer("/session_temporal_health/findings")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|findings| !findings.is_empty())
        );
    }

    #[tokio::test]
    async fn locked_store_returns_fixed_reason_without_filesystem_changes() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("registry.db")),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize test project");
        let db_path = initialized.db_path().clone();
        drop(initialized);
        let locked = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let connection = locked.connect().unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .await
            .unwrap();
        let handshake = handshake(project, profile.clone(), profile.join("registry.db"));
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/reason"),
            Some(&serde_json::json!("project_store_locked"))
        );
        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("locked"))
        );
        assert!(!value.to_string().contains(&db_path.display().to_string()));
        connection.execute("ROLLBACK", ()).await.unwrap();
    }

    #[tokio::test]
    async fn cold_complete_route_uses_immutable_session_health_without_authority_wal_shm() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let registry_path = profile.join("registry.db");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(registry_path.clone()),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize test project");
        let graph_path = initialized.db_path().clone();
        let session_path = initialized.store_layout().sessions_db_path.clone();
        assert_eq!(
            doctor_runtime_store_paths(&project, &profile)
                .expect("resolve initialized cold Doctor store paths"),
            (graph_path.clone(), session_path.clone()),
            "cold Doctor must resolve the initialized profile-sharded store"
        );
        drop(initialized);
        checkpoint_sqlite_wal(&graph_path).await;
        // Init leaves a zero-byte sessions placeholder; install + checkpoint a
        // real temporal store so immutable=1 can observe a complete snapshot.
        {
            let session_db = crate::global_db::GlobalDb::open_at(&session_path)
                .await
                .expect("seed sessions store");
            session_db
                .checkpoint_result()
                .await
                .expect("checkpoint sessions store");
            drop(session_db);
        }
        for path in [&graph_path, &session_path, &registry_path] {
            remove_sqlite_sidecars(path);
        }
        let handshake = handshake(project, profile.clone(), registry_path.clone());
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("complete")),
            "{value}"
        );
        assert_eq!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("complete"))
        );
        assert_eq!(value.pointer("/session_temporal_health/reason"), None);
        assert_eq!(
            value.pointer("/database/authority_audit_reason"),
            Some(&serde_json::json!("authority_audit_not_run"))
        );
        for path in [
            graph_path.as_path(),
            session_path.as_path(),
            registry_path.as_path(),
        ] {
            for suffix in ["-wal", "-shm"] {
                let mut sidecar = path.as_os_str().to_os_string();
                sidecar.push(suffix);
                assert!(
                    !PathBuf::from(sidecar).exists(),
                    "cold doctor must not create {suffix} for {}",
                    path.display()
                );
            }
        }
    }

    #[tokio::test]
    async fn doctor_store_paths_follow_the_active_branch_database() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-b", "main"])
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(profile.join("registry.db")),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize branch-aware Doctor fixture");
        let layout = initialized.store_layout().clone();
        let default_graph = initialized.db_path().clone();
        drop(initialized);

        let branch_relpath = "branches/feature_doctor.db";
        let branch_graph = layout.data_root.join(branch_relpath);
        std::fs::create_dir_all(branch_graph.parent().unwrap()).unwrap();
        std::fs::copy(&default_graph, &branch_graph).unwrap();
        let mut meta = crate::branch_meta::BranchMeta::new_for_dir(&layout.data_root, "main");
        meta.add_branch("feature/doctor", branch_relpath, "main");
        crate::branch_meta::save_branch_meta(&layout.data_root, &meta).unwrap();

        assert_eq!(
            super::doctor_runtime_store_paths_for_branch(
                &project,
                &profile,
                Some("feature/doctor"),
            )
            .expect("resolve branch-aware Doctor paths"),
            (branch_graph, layout.sessions_db_path)
        );
    }

    #[tokio::test]
    async fn cold_uncheckpointed_session_wal_is_unavailable_without_artifacts() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let registry_path = profile.join("registry.db");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(registry_path.clone()),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize test project");
        let graph_path = initialized.db_path().clone();
        let session_path = initialized.store_layout().sessions_db_path.clone();
        drop(initialized);
        checkpoint_sqlite_wal(&graph_path).await;
        for path in [&graph_path, &registry_path] {
            remove_sqlite_sidecars(path);
        }
        let session_db = crate::global_db::GlobalDb::open_at(&session_path)
            .await
            .expect("create an uncheckpointed temporal store");
        assert!(
            has_non_empty_wal(&session_path),
            "fixture must retain a non-empty temporal WAL"
        );
        let handshake = handshake(project, profile.clone(), registry_path);
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("complete"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/reason"),
            Some(&serde_json::json!("uncheckpointed_wal"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/findings"),
            Some(&serde_json::json!([]))
        );
        drop(session_db);
    }

    #[tokio::test]
    async fn cold_uncheckpointed_graph_wal_is_unavailable_without_artifacts() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let registry_path = profile.join("registry.db");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(registry_path.clone()),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize test project");
        let graph_path = initialized.db_path().clone();
        drop(initialized);
        let graph_db = libsql::Builder::new_local(&graph_path)
            .build()
            .await
            .unwrap();
        let graph_conn = graph_db.connect().unwrap();
        graph_conn
            .execute(
                "CREATE TABLE cold_doctor_wal_probe(id INTEGER PRIMARY KEY)",
                (),
            )
            .await
            .unwrap();
        assert!(
            has_non_empty_wal(&graph_path),
            "fixture must retain a non-empty graph WAL"
        );
        let handshake = handshake(project, profile.clone(), registry_path);
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/doctor_runtime/reason"),
            Some(&serde_json::json!("project_store_uncheckpointed_wal"))
        );
        drop(graph_conn);
        drop(graph_db);
    }

    #[tokio::test]
    async fn cold_uninitialized_sessions_store_reports_fixed_reason_without_artifacts() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let registry_path = profile.join("registry.db");
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile.clone()),
            global_db_path: Some(registry_path.clone()),
        };
        let initialized = TraceDecay::init_with_options(&project, options)
            .await
            .expect("initialize test project");
        let graph_path = initialized.db_path().clone();
        let session_path = initialized.store_layout().sessions_db_path.clone();
        drop(initialized);
        checkpoint_sqlite_wal(&graph_path).await;
        for path in [&graph_path, &registry_path] {
            remove_sqlite_sidecars(path);
        }
        std::fs::create_dir_all(session_path.parent().unwrap()).unwrap();
        std::fs::write(&session_path, []).unwrap();
        assert!(
            session_path.is_file(),
            "fixture must provide an uninitialized sessions placeholder"
        );
        assert!(
            !crate::storage::has_sqlite_database_header(&session_path).unwrap_or(true),
            "sessions placeholder must not be a SQLite database yet"
        );
        let handshake = handshake(project, profile.clone(), registry_path);
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("complete"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/reason"),
            Some(&serde_json::json!("session_store_uninitialized"))
        );
    }
}
