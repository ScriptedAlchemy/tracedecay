//! Read-only doctor runtime telemetry: cold store probes and typed
//! `tracedecay_runtime` responses served without opening project stores.

use std::path::{Path, PathBuf};

use serde_json::json;
use tokio::time::{Duration, timeout};

use super::{DaemonHandshake, projectless_tool_call, write_json_rpc_response};
use crate::application::semantic_runtime::{
    SemanticConfigurationPinV1, SemanticFallbackReasonV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};
use crate::errors::Result;
use crate::mcp::{JsonRpcRequest, JsonRpcResponse, McpTransport};

pub(crate) const DOCTOR_GRAPH_SCHEMA_VERSION: i64 = 24;

#[derive(Debug)]
pub(crate) struct DoctorRuntimeRequest {
    id: serde_json::Value,
    startup_health_only: bool,
}

pub(crate) fn doctor_runtime_request(request_line: &str) -> Option<DoctorRuntimeRequest> {
    let request = serde_json::from_str::<JsonRpcRequest>(request_line.trim()).ok()?;
    if request.method != "tools/call" {
        return None;
    }
    let (tool_name, arguments) = projectless_tool_call(request.params.as_ref()).ok()?;
    if tool_name != "tracedecay_runtime"
        || arguments.get("format").and_then(serde_json::Value::as_str) != Some("json")
    {
        return None;
    }
    let startup_health_only = arguments
        .get("startup_health")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let full_doctor = arguments
        .get("authority_audit")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && arguments
            .get("session_ingest_health")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
    if !startup_health_only && !full_doctor {
        return None;
    }
    Some(DoctorRuntimeRequest {
        id: request.id.unwrap_or(serde_json::Value::Null),
        startup_health_only,
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
        "tracedecay_version": crate::version::build_version(),
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
        "semantic_runtime": doctor_semantic_runtime_status(project_path, None),
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

async fn doctor_literal_workspace_placeholder_paths(
    database: &crate::global_db::RegisteredGlobalDb,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(mut rows) = database
        .read_connection()
        .query(
            "SELECT DISTINCT transcript_path FROM sessions
             WHERE transcript_path IS NOT NULL
               AND transcript_path != ''
               AND (transcript_path LIKE '%${workspaceFolder}%'
                    OR transcript_path LIKE '%$workspaceFolder%')
             ORDER BY transcript_path
             LIMIT ?1",
            [i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
    else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            paths.push(path);
        }
    }
    paths
}

fn doctor_sidecar_size(db_path: &Path, suffix: &str) -> u64 {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    std::fs::metadata(PathBuf::from(path)).map_or(0, |metadata| metadata.len())
}

async fn doctor_runtime_value(
    handshake: &DaemonHandshake,
    store_administration: &super::StoreAdministration,
    startup_health_only: bool,
) -> serde_json::Value {
    doctor_runtime_value_inner(
        handshake,
        Some(store_administration),
        startup_health_only,
    )
    .await
}

async fn doctor_runtime_value_inner(
    handshake: &DaemonHandshake,
    store_administration: Option<&super::StoreAdministration>,
    startup_health_only: bool,
) -> serde_json::Value {
    let Some(project_path) = handshake.project_path.as_deref() else {
        return doctor_runtime_unavailable(None, "project_path_missing");
    };
    let (graph_path, session_path) =
        match doctor_runtime_store_paths(project_path, &handshake.client_identity.profile_root) {
            Ok(paths) => paths,
            Err(reason) => return doctor_runtime_unavailable(Some(project_path), reason),
        };
    let Some(store_administration) = store_administration else {
        let reason = if graph_path.is_file() {
            "project_store_authority_unavailable"
        } else {
            "project_store_missing"
        };
        return doctor_runtime_unavailable(Some(project_path), reason);
    };
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.to_path_buf());
    let canonical_graph_path = graph_path
        .canonicalize()
        .unwrap_or_else(|_| graph_path.clone());
    let graph = store_administration
        .mounted_project_graphs()
        .await
        .into_iter()
        .find(|graph| {
            let mounted_graph_path = graph.db_path();
            graph
                .project_root()
                .canonicalize()
                .unwrap_or_else(|_| graph.project_root().to_path_buf())
                == canonical_project_path
                && mounted_graph_path
                    .canonicalize()
                    .unwrap_or(mounted_graph_path)
                    == canonical_graph_path
        });
    let Some(graph) = graph else {
        let reason = if graph_path.is_file() {
            "project_store_authority_unavailable"
        } else {
            "project_store_missing"
        };
        return doctor_runtime_unavailable(Some(project_path), reason);
    };
    let (quick_check_ok, quick_check_error) = match graph.quick_check_report().await {
        Ok(None) => (true, None),
        Ok(Some(problem)) => (false, Some(problem)),
        Err(_) => {
            return doctor_runtime_unavailable(Some(project_path), "project_store_unavailable");
        }
    };
    let page_counts = graph.storage_page_counts().await.ok();
    let db_size_bytes = page_counts
        .map(|(page_size, page_count, _)| page_size.saturating_mul(page_count))
        .unwrap_or_default();
    let page_size = page_counts.map(|(page_size, _, _)| page_size);
    let mut value = json!({
        "tracedecay_version": crate::version::build_version(),
        "process": {
            "pid": std::process::id(),
        },
        "database": {
            "project_root": project_path,
            "db_path": graph_path,
            "canonical_db_path": canonical_graph_path,
            "db_size_bytes": db_size_bytes,
            "wal_size_bytes": doctor_sidecar_size(&graph_path, "-wal"),
            "shm_size_bytes": doctor_sidecar_size(&graph_path, "-shm"),
            "journal_mode": null,
            "synchronous": null,
            "page_size": page_size,
            "quick_check_ok": quick_check_ok,
            "quick_check_error": quick_check_error,
            "schema_version": DOCTOR_GRAPH_SCHEMA_VERSION,
        },
        "doctor_runtime": {
            "status": "complete",
            "reason": null,
            "read_only": true,
        },
    });
    if startup_health_only {
        return value;
    }

    let registry = store_administration
        .registered_profile_database()
        .await
        .ok();
    let (authority_ok, authority_reason) = match registry.as_ref() {
        // Registered attachment validates the authority schema contract before
        // publication, so a retained handle is itself the completed audit.
        Some(_) => (Some(true), None),
        None if handshake.client_identity.global_db_path.is_file() => {
            (None, Some("authority_store_unavailable"))
        }
        None => (None, Some("authority_store_missing")),
    };
    value["database"]["authority_audit_ok"] = json!(authority_ok);
    value["database"]["authority_audit_reason"] = json!(authority_reason);
    value["database"]["authority_audit_error"] = json!(authority_reason);

    let canonical_session_path = session_path
        .canonicalize()
        .unwrap_or_else(|_| session_path.clone());
    let session_db = store_administration
        .mounted_registered_session_databases()
        .await
        .into_iter()
        .find(|database| {
            database
                .db_path()
                .canonicalize()
                .unwrap_or_else(|_| database.db_path().to_path_buf())
                == canonical_session_path
        });
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
        Some(db) => match db.cursor_session_ingest_health().await {
            Ok(health) => serde_json::to_value(health).unwrap_or_else(|error| {
                json!({
                    "status": "unavailable",
                    "reason": "session_ingest_serialization_failed",
                    "message": error.to_string(),
                })
            }),
            Err(error) => json!({
                "status": "unavailable",
                "reason": "session_ingest_query_failed",
                "message": error,
            }),
        },
        None => json!({
            "status": "unavailable",
            "reason": "session_store_unavailable",
        }),
    };
    value["cursor_session_placeholder_paths"] = match session_db.as_ref() {
        Some(db) => json!(doctor_literal_workspace_placeholder_paths(db, 10).await),
        None => json!([]),
    };
    let semantic_configuration = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .ok()
        .and_then(|pinned| {
            SemanticConfigurationPinV1::from_current(
                &crate::application::configuration::ConfigurationCurrentStateV1 {
                    revision_id: pinned.revision_id,
                    snapshot: pinned.snapshot,
                },
            )
            .ok()
        });
    value["semantic_runtime"] =
        doctor_semantic_runtime_status(Some(project_path), semantic_configuration);
    value
}

fn doctor_semantic_runtime_status(
    project_path: Option<&Path>,
    configuration: Option<SemanticConfigurationPinV1>,
) -> serde_json::Value {
    if let Some(project_path) = project_path
        && let Some(status) =
            crate::application::semantic_runtime::project_semantic_application_status(
                project_path,
                configuration.clone(),
            )
    {
        return serde_json::to_value(status)
            .unwrap_or_else(|_| json!({ "state": { "state": "unavailable" } }));
    }
    if configuration.is_none() {
        return serde_json::to_value(SemanticRuntimeStatusV1::new(
            None,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::ConfigurationUnavailable,
            },
        ))
        .unwrap_or_else(|_| json!({ "state": { "state": "unavailable" } }));
    }
    let Some(owner) = crate::semantic_code::shared_lifecycle_owner() else {
        return serde_json::to_value(SemanticRuntimeStatusV1::new(
            None,
            SemanticRuntimeStateV1::Unavailable {
                reason: SemanticFallbackReasonV1::RuntimeUnavailable,
            },
        ))
        .unwrap_or_else(|_| json!({ "state": { "state": "unavailable" } }));
    };
    let status = owner.status();
    let state = match status.state.as_ref() {
        Some(lifecycle) => crate::semantic_code::lifecycle_to_runtime_state(lifecycle),
        None => SemanticRuntimeStateV1::Unavailable {
            reason: SemanticFallbackReasonV1::ConfigurationUnavailable,
        },
    };
    serde_json::to_value(SemanticRuntimeStatusV1::new(configuration, state))
        .unwrap_or_else(|_| json!({ "state": { "state": "unavailable" } }))
}

pub(crate) async fn cold_doctor_runtime_value(handshake: &DaemonHandshake) -> serde_json::Value {
    // Owned stores are never path-opened as a fallback. Without the daemon's
    // retained runtime authority Doctor reports explicit unavailability.
    doctor_runtime_value_inner(handshake, None, false).await
}

pub(crate) async fn write_doctor_runtime_response(
    transport: &mut impl McpTransport,
    handshake: &DaemonHandshake,
    store_administration: &super::StoreAdministration,
    request: DoctorRuntimeRequest,
) -> Result<()> {
    let result = doctor_runtime_tool_result(
        doctor_runtime_value(
            handshake,
            store_administration,
            request.startup_health_only,
        )
        .await,
    );
    write_json_rpc_response(transport, &JsonRpcResponse::success(request.id, result)).await
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod doctor_runtime_route_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rusqlite::Connection;

    use super::{cold_doctor_runtime_value, doctor_runtime_request, doctor_runtime_store_paths};
    use crate::client_identity::DaemonClientIdentity;
    use crate::daemon::DaemonHandshake;
    use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

    static REGISTERED_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

    async fn registered_project_session_database(
        profile_root: &Path,
        project_root: &Path,
    ) -> (
        crate::db::DaemonDatabaseScope,
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
        Arc<crate::global_db::RegisteredGlobalDb>,
    ) {
        let identity = crate::daemon::profile_identity::load_or_create(profile_root)
            .expect("load test profile identity");
        let nonce = REGISTERED_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let scope =
            crate::db::enter_daemon_database_scope(profile_root, nonce, "core-doctor-test-runtime")
                .expect("enter test daemon database scope");
        let registry =
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity,
            )
            .await
            .expect("open test session runtime registry");
        let layout = crate::storage::resolve_persisted_layout(project_root, profile_root)
            .expect("resolve test project layout")
            .expect("test project must be enrolled");
        let project_id = tracedecay_store::ProjectId::new(
            layout.identity.project_id.expect("test project identity"),
        )
        .expect("valid test project identity");
        let database = registry
            .project_sessions(
                project_id,
                [project_root.to_path_buf(), layout.project_root],
            )
            .await
            .expect("mount registered test project sessions");
        (scope, registry, database)
    }

    async fn initialize_test_project(
        project_root: &Path,
        profile_root: &Path,
    ) -> crate::storage::StoreLayout {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700))
                .expect("secure fixture profile root");
        }
        let options = TraceDecayOpenOptions {
            profile_root: Some(profile_root.to_path_buf()),
            global_db_path: Some(profile_root.join("registry.db")),
        };
        let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
            profile_root,
            "core Doctor fixture initialization",
        )
        .expect("acquire fixture lifecycle authority");
        let _database_scope = crate::db::enter_maintenance_database_scope(
            &lifecycle,
            profile_root,
            "core Doctor fixture initialization",
        )
        .expect("enter fixture maintenance database scope");
        let initialized =
            TraceDecay::init_with_exclusive_maintenance(project_root, options, &lifecycle)
                .await
                .expect("initialize core Doctor fixture");
        let layout = initialized.store_layout().clone();
        initialized.close();
        layout
    }

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
        let connection = Connection::open(path).unwrap();
        let (busy, log_frames, checkpointed_frames) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", (), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap();
        assert_eq!(busy, 0, "checkpoint must not be busy");
        assert_eq!(
            log_frames, checkpointed_frames,
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
        std::fs::metadata(PathBuf::from(wal_path)).is_ok_and(|metadata| metadata.len() > 0)
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
        assert!(!parsed.startup_health_only);

        let ordinary = request.replace("\"authority_audit\":true", "\"authority_audit\":false");
        assert!(doctor_runtime_request(&ordinary).is_none());

        let startup = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {
                "name": "tracedecay_runtime",
                "arguments": {
                    "format": "json",
                    "startup_health": true,
                },
            },
        })
        .to_string();
        let parsed = doctor_runtime_request(&startup).expect("startup health runtime request");
        assert_eq!(parsed.id, serde_json::json!(8));
        assert!(parsed.startup_health_only);
    }

    #[test]
    fn semantic_status_without_configuration_is_valid_unavailable() {
        let value = super::doctor_semantic_runtime_status(None, None);
        let status: crate::application::semantic_runtime::SemanticRuntimeStatusV1 =
            serde_json::from_value(value).expect("semantic runtime status");

        assert_eq!(status.validate(), Ok(()));
        assert!(matches!(
            status.state,
            crate::application::semantic_runtime::SemanticRuntimeStateV1::Unavailable {
                reason: crate::application::semantic_runtime::SemanticFallbackReasonV1::ConfigurationUnavailable,
            }
        ));
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
        let layout = initialize_test_project(&project, &profile).await;
        let db_path = layout.graph_db_path;
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
            Some(&serde_json::json!("project_store_authority_unavailable"))
        );
        assert_eq!(
            value.pointer("/database/quick_check_error"),
            Some(&serde_json::json!("project_store_authority_unavailable"))
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
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch("PRAGMA user_version=1; CREATE TABLE legacy_graph(id INTEGER);")
            .unwrap();
        drop(connection);
        let handshake = handshake(project, profile.clone(), profile.join("registry.db"));
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/reason"),
            Some(&serde_json::json!("project_store_authority_unavailable"))
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
        let layout = initialize_test_project(&project, &profile).await;
        let session_path = layout.sessions_db_path;
        std::fs::create_dir_all(session_path.parent().unwrap()).unwrap();
        let connection = Connection::open(&session_path).unwrap();
        connection
            .execute("CREATE TABLE legacy_sessions(id INTEGER PRIMARY KEY)", ())
            .unwrap();
        drop(connection);
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
        let layout = initialize_test_project(&project, &profile).await;
        let db_path = layout.graph_db_path;
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE;")
            .unwrap();
        let handshake = handshake(project, profile.clone(), profile.join("registry.db"));
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/reason"),
            Some(&serde_json::json!("project_store_authority_unavailable"))
        );
        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert!(!value.to_string().contains(&db_path.display().to_string()));
        connection.execute("ROLLBACK", ()).unwrap();
    }

    #[tokio::test]
    async fn cold_complete_route_uses_immutable_session_health_without_authority_wal_shm() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let registry_path = profile.join("registry.db");
        let layout = initialize_test_project(&project, &profile).await;
        let graph_path = layout.graph_db_path.clone();
        let session_path = layout.sessions_db_path.clone();
        assert_eq!(
            doctor_runtime_store_paths(&project, &profile)
                .expect("resolve initialized cold Doctor store paths"),
            (graph_path.clone(), session_path.clone()),
            "cold Doctor must resolve the initialized profile-sharded store"
        );
        checkpoint_sqlite_wal(&graph_path).await;
        // Init leaves a zero-byte sessions placeholder; install + checkpoint a
        // real temporal store so immutable=1 can observe a complete snapshot.
        let (scope, registry, session_db) =
            registered_project_session_database(&profile, &project).await;
        assert_eq!(session_db.db_path(), session_path);
        drop(session_db);
        drop(registry);
        drop(scope);
        checkpoint_sqlite_wal(&session_path).await;
        for path in [&graph_path, &session_path, &registry_path] {
            remove_sqlite_sidecars(path);
        }
        let handshake = handshake(project, profile.clone(), registry_path.clone());
        let before = filesystem_manifest(root.path());

        let value = cold_doctor_runtime_value(&handshake).await;

        assert_eq!(filesystem_manifest(root.path()), before);
        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("unavailable")),
            "{value}"
        );
        assert_eq!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/reason"),
            Some(&serde_json::json!("project_store_authority_unavailable"))
        );
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
        let layout = initialize_test_project(&project, &profile).await;
        let default_graph = layout.graph_db_path.clone();

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
        let layout = initialize_test_project(&project, &profile).await;
        let graph_path = layout.graph_db_path;
        let session_path = layout.sessions_db_path;
        checkpoint_sqlite_wal(&graph_path).await;
        for path in [&graph_path, &registry_path] {
            remove_sqlite_sidecars(path);
        }
        let (_scope, _registry, session_db) =
            registered_project_session_database(&profile, &project).await;
        session_db
            .writer_connection()
            .expect("registered session writer")
            .execute(
                "CREATE TABLE cold_doctor_session_wal_probe(id INTEGER PRIMARY KEY)",
                (),
            )
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
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/reason"),
            Some(&serde_json::json!("project_store_authority_unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/findings"),
            Some(&serde_json::json!([{
                "kind": "compatibility_drift",
                "count": 1,
            }]))
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
        let layout = initialize_test_project(&project, &profile).await;
        let graph_path = layout.graph_db_path;
        let graph_conn = Connection::open(&graph_path).unwrap();
        graph_conn
            .execute(
                "CREATE TABLE cold_doctor_wal_probe(id INTEGER PRIMARY KEY)",
                (),
            )
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
            Some(&serde_json::json!("project_store_authority_unavailable"))
        );
        drop(graph_conn);
    }

    #[tokio::test]
    async fn cold_uninitialized_sessions_store_reports_fixed_reason_without_artifacts() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let registry_path = profile.join("registry.db");
        let layout = initialize_test_project(&project, &profile).await;
        let graph_path = layout.graph_db_path;
        let session_path = layout.sessions_db_path;
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
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/status"),
            Some(&serde_json::json!("unavailable"))
        );
        assert_eq!(
            value.pointer("/session_temporal_health/reason"),
            Some(&serde_json::json!("project_store_authority_unavailable"))
        );
    }
}
