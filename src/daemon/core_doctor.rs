//! Read-only doctor runtime telemetry: cold store probes and typed
//! `tracedecay_runtime` responses served without opening project stores.

use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::time::{Duration, timeout};

use super::core_lifecycle::DaemonActivity;
use super::{DaemonHandshake, projectless_tool_call, write_json_rpc_response};
use crate::application::semantic_runtime::{
    SemanticConfigurationPinV1, SemanticFallbackReasonV1, SemanticRuntimeStateV1,
    SemanticRuntimeStatusV1,
};
use crate::errors::Result;
use crate::mcp::{JsonRpcRequest, JsonRpcResponse, McpTransport};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum DoctorGraphSchemaState {
    ReleasedV0067,
    PreviousV2Candidate,
    Current,
    Unsupported,
}

fn doctor_graph_schema_state(actual: i64) -> DoctorGraphSchemaState {
    match actual {
        18 => DoctorGraphSchemaState::ReleasedV0067,
        // V2 development builds stamped 24 through 26 before the final shape
        // settled; they are refused at open but diagnosed distinctly from
        // stores no supported binary ever produced.
        24..=26 => DoctorGraphSchemaState::PreviousV2Candidate,
        actual if actual == i64::from(crate::db::migrations::SCHEMA_VERSION) => {
            DoctorGraphSchemaState::Current
        }
        _ => DoctorGraphSchemaState::Unsupported,
    }
}

#[derive(Debug)]
pub(crate) struct DoctorRuntimeRequest {
    id: serde_json::Value,
    startup_health_only: bool,
    doctor_report_requested: bool,
}

impl DoctorRuntimeRequest {
    pub(crate) fn doctor_report_requested(&self) -> bool {
        self.doctor_report_requested
    }

    pub(crate) fn should_serve_from_core(&self, doctor_report_ready: bool) -> bool {
        !self.doctor_report_requested || !doctor_report_ready
    }
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
    let doctor_report_requested = arguments
        .get("doctor_report")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if !startup_health_only && !full_doctor && !doctor_report_requested {
        return None;
    }
    Some(DoctorRuntimeRequest {
        id: request.id.unwrap_or(serde_json::Value::Null),
        startup_health_only,
        doctor_report_requested,
    })
}

fn doctor_runtime_temporal_unavailable(reason: &str) -> serde_json::Value {
    json!({
        "status": if reason.ends_with("_locked") { "locked" } else { "unavailable" },
        "reason": reason,
    })
}

fn doctor_runtime_temporal_report(
    report: crate::global_db::SessionTemporalHealthReport,
) -> serde_json::Value {
    let value = serde_json::to_value(report).unwrap_or_else(|_| {
        doctor_runtime_temporal_unavailable("session_health_serialization_failed")
    });
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

fn doctor_runtime_store_layout(
    project_path: &Path,
    profile_root: &Path,
) -> std::result::Result<(PathBuf, PathBuf), &'static str> {
    let layout = crate::storage::resolve_layout(project_path, profile_root)
        .map_err(|_| "project_store_schema_unsupported")?;
    Ok((layout.graph_db_path, layout.sessions_db_path))
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

fn doctor_runtime_coverage(startup_health_only: bool) -> Option<serde_json::Value> {
    startup_health_only.then(|| {
        json!({
            "status": "partial",
            "reason": "startup_health_only",
        })
    })
}

async fn doctor_runtime_value(
    handshake: &DaemonHandshake,
    store_administration: &super::StoreAdministration,
    startup_health_only: bool,
    git_watcher_health: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut value =
        doctor_runtime_value_inner(handshake, Some(store_administration), startup_health_only)
            .await;
    value["git_watcher"] = git_watcher_health.unwrap_or_else(|| {
        json!({
            "status": "unavailable",
            "coverage": null,
            "reason": "watcher_runtime_unavailable",
        })
    });
    value
}

async fn doctor_runtime_value_inner(
    handshake: &DaemonHandshake,
    store_administration: Option<&super::StoreAdministration>,
    startup_health_only: bool,
) -> serde_json::Value {
    let Some(project_path) = handshake.project_path.as_deref() else {
        return doctor_runtime_unavailable(None, "project_path_missing");
    };
    let (expected_graph_path, session_path) =
        match doctor_runtime_store_layout(project_path, &handshake.client_identity.profile_root) {
            Ok(paths) => paths,
            Err(reason) => return doctor_runtime_unavailable(Some(project_path), reason),
        };
    let Some(store_administration) = store_administration else {
        let reason = if expected_graph_path.is_file() {
            "project_store_authority_unavailable"
        } else {
            "project_store_missing"
        };
        return doctor_runtime_unavailable(Some(project_path), reason);
    };
    let canonical_project_path = project_path
        .canonicalize()
        .unwrap_or_else(|_| project_path.to_path_buf());
    let graph = store_administration
        .mounted_project_graphs()
        .await
        .into_iter()
        .find(|graph| {
            graph
                .project_root()
                .canonicalize()
                .unwrap_or_else(|_| graph.project_root().to_path_buf())
                == canonical_project_path
        });
    let Some(graph) = graph else {
        let reason = if expected_graph_path.is_file() {
            "project_store_authority_unavailable"
        } else {
            "project_store_missing"
        };
        return doctor_runtime_unavailable(Some(project_path), reason);
    };
    let graph_path = graph.db_path();
    let canonical_graph_path = graph_path
        .canonicalize()
        .unwrap_or_else(|_| graph_path.clone());
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
    let schema_version = match graph
        .db()
        .query_scalar_i64("Doctor project schema inspection", "PRAGMA user_version")
        .await
    {
        Ok(version) => version,
        Err(_) => {
            return doctor_runtime_unavailable(Some(project_path), "project_schema_unavailable");
        }
    };
    let schema_state = doctor_graph_schema_state(schema_version);
    let expected_schema_version = crate::db::migrations::SCHEMA_VERSION;
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
            "page_size": page_size,
            "quick_check_ok": quick_check_ok,
            "quick_check_error": quick_check_error,
            "schema_version": schema_version,
            "expected_schema_version": expected_schema_version,
            "schema_state": schema_state,
            "schema_drift": (schema_state != DoctorGraphSchemaState::Current),
        },
        "doctor_runtime": {
            "status": "complete",
            "reason": null,
            "read_only": true,
        },
    });
    if let Some(coverage) = doctor_runtime_coverage(startup_health_only) {
        value["doctor_runtime"]["coverage"] = coverage;
        return value;
    }

    let registry = store_administration
        .registered_profile_database()
        .await
        .ok();
    // Doctor asked for the exhaustive observation-authority audit, so run it.
    // A retained registry handle only proves the schema contract held at
    // publication; it is not evidence that the invariant pass ran now.
    let (authority_ok, authority_reason, authority_detail) = match registry.as_ref() {
        Some(registry) => match registry.read_snapshot().await {
            Ok(snapshot) => {
                match crate::global_db::schema_stages::validate_observation_authority_connection(
                    &snapshot,
                )
                .await
                {
                    Ok(()) => (Some(true), None, None),
                    Err(error) => (
                        Some(false),
                        Some("authority_invariant_failed"),
                        Some(error.to_string()),
                    ),
                }
            }
            Err(error) => (
                None,
                Some("authority_store_unavailable"),
                Some(error.to_string()),
            ),
        },
        None if handshake.client_identity.global_db_path.is_file() => {
            (None, Some("authority_store_unavailable"), None)
        }
        None => (None, Some("authority_store_missing"), None),
    };
    value["database"]["authority_audit_ok"] = json!(authority_ok);
    value["database"]["authority_audit_reason"] = json!(authority_reason);
    // `authority_audit_error` carries the observed detail when there is one and
    // otherwise mirrors the typed reason so readers that only know the older
    // key still see the same vocabulary.
    value["database"]["authority_audit_error"] =
        json!(authority_detail.or_else(|| authority_reason.map(str::to_string)));

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
    if let Some(db) = session_db.as_ref() {
        let health_budget = Duration::from_secs(8);
        let (temporal, cursor_ingest, placeholder_paths) = tokio::join!(
            timeout(health_budget, db.session_temporal_doctor_health()),
            timeout(health_budget, db.cursor_session_ingest_health()),
            timeout(
                health_budget,
                doctor_literal_workspace_placeholder_paths(db, 10)
            ),
        );
        value["session_temporal_health"] = match temporal {
            Ok(report) => doctor_runtime_temporal_report(report),
            Err(_) => doctor_runtime_temporal_unavailable("session_health_timed_out"),
        };
        value["cursor_session_ingest"] = match cursor_ingest {
            Ok(Ok(health)) => serde_json::to_value(health).unwrap_or_else(|error| {
                json!({
                    "status": "unavailable",
                    "reason": "session_ingest_serialization_failed",
                    "message": error.to_string(),
                })
            }),
            Ok(Err(error)) => json!({
                "status": "unavailable",
                "reason": "session_ingest_query_failed",
                "message": error,
            }),
            Err(_) => json!({
                "status": "unavailable",
                "reason": "session_ingest_timed_out",
            }),
        };
        value["cursor_session_placeholder_paths"] = match placeholder_paths {
            Ok(paths) => json!(paths),
            Err(_) => json!([]),
        };
    } else {
        value["session_temporal_health"] = if session_path.is_file() {
            doctor_runtime_temporal_unavailable("session_store_unavailable")
        } else {
            doctor_runtime_temporal_unavailable("session_store_missing")
        };
        value["cursor_session_ingest"] = json!({
            "status": "unavailable",
            "reason": "session_store_unavailable",
        });
        value["cursor_session_placeholder_paths"] = json!([]);
    }
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

#[cfg(test)]
pub(crate) async fn cold_doctor_runtime_value(handshake: &DaemonHandshake) -> serde_json::Value {
    // Owned stores are never path-opened as a fallback. Without the daemon's
    // retained runtime authority Doctor reports explicit unavailability.
    doctor_runtime_value_inner(handshake, None, false).await
}

pub(in crate::daemon) async fn write_doctor_runtime_response(
    transport: &mut impl McpTransport,
    handshake: &DaemonHandshake,
    store_administration: &super::StoreAdministration,
    request: DoctorRuntimeRequest,
    git_watcher_health: Option<serde_json::Value>,
) -> Result<()> {
    let mut value = doctor_runtime_value(
        handshake,
        store_administration,
        request.startup_health_only,
        git_watcher_health,
    )
    .await;
    if request.doctor_report_requested() && value.get("doctor_report").is_none() {
        value["doctor_report"] = json!({
            "kind": "unknown",
            "reason": "doctor_report_owner_warming",
            "table_growth_evidence": [],
        });
    }
    let result = doctor_runtime_tool_result(value);
    write_json_rpc_response(transport, &JsonRpcResponse::success(request.id, result)).await
}

/// Serve a Doctor runtime request from the daemon core while the routed
/// project owner has not published its Doctor report. Both broker paths share
/// this route; only the cached-server fetch differs, so the caller supplies it
/// as a probe. Returns the activity guard when the request falls through to
/// the broker's regular routing, or `None` once the core response has been
/// written and the connection is complete.
pub(super) async fn serve_core_doctor_runtime_request<T, Probe, ProbeFuture>(
    transport: &mut T,
    handshake: &DaemonHandshake,
    store_administration: &super::StoreAdministration,
    setup_activity: DaemonActivity,
    first_request_line: &str,
    git_watcher_health: Option<serde_json::Value>,
    doctor_report_ready: Probe,
) -> Result<Option<DaemonActivity>>
where
    T: McpTransport,
    Probe: FnOnce() -> ProbeFuture,
    ProbeFuture: std::future::Future<Output = Result<bool>>,
{
    let Some(request) = doctor_runtime_request(first_request_line) else {
        return Ok(Some(setup_activity));
    };
    let report_ready = if request.doctor_report_requested() {
        doctor_report_ready().await?
    } else {
        false
    };
    if !request.should_serve_from_core(report_ready) {
        return Ok(Some(setup_activity));
    }
    drop(setup_activity);
    write_doctor_runtime_response(
        transport,
        handshake,
        store_administration,
        request,
        git_watcher_health,
    )
    .await?;
    Ok(None)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod doctor_runtime_route_tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use rusqlite::Connection;

    use super::{
        cold_doctor_runtime_value, doctor_runtime_coverage, doctor_runtime_request,
        serve_core_doctor_runtime_request,
    };
    use crate::client_identity::DaemonClientIdentity;
    use crate::daemon::{DaemonHandshake, DaemonLifecycle, StoreAdministration};
    use crate::mcp::server::McpServerConstructionContext;
    use crate::mcp::{McpServer, McpTransport};
    use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

    static REGISTERED_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

    struct DoctorRouteTransport {
        lifecycle: DaemonLifecycle,
        output: String,
        idle_before_write: bool,
    }

    impl McpTransport for DoctorRouteTransport {
        async fn read_line(&mut self) -> std::io::Result<Option<String>> {
            Ok(None)
        }

        async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.idle_before_write = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                self.lifecycle.wait_for_idle(),
            )
            .await
            .is_ok();
            self.output.push_str(line);
            Ok(())
        }

        async fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
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

    fn doctor_report_request_line() -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {
                "name": "tracedecay_runtime",
                "arguments": {
                    "format": "json",
                    "authority_audit": true,
                    "doctor_report": true,
                    "session_ingest_health": false,
                },
            },
        })
        .to_string()
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

    #[test]
    fn startup_health_coverage_is_explicitly_partial() {
        assert_eq!(
            doctor_runtime_coverage(true),
            Some(serde_json::json!({
                "status": "partial",
                "reason": "startup_health_only",
            }))
        );
        assert_eq!(doctor_runtime_coverage(false), None);
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
        assert!(!parsed.doctor_report_requested());
        assert!(parsed.should_serve_from_core(true));

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
        assert!(!parsed.doctor_report_requested());
    }

    #[test]
    fn requested_doctor_report_uses_core_only_until_the_ready_owner_is_published() {
        let request = doctor_report_request_line();
        let parsed = doctor_runtime_request(&request).expect("Doctor report request");

        assert!(parsed.doctor_report_requested());
        assert!(parsed.should_serve_from_core(false));
        assert!(!parsed.should_serve_from_core(true));
    }

    /// The comprehensive Doctor request (`authority_audit` +
    /// `session_ingest_health`, no `doctor_report`) is always served from the
    /// core, even when a cached core-stage server has already published a ready
    /// Doctor report. That makes the core route — not the routed project owner
    /// — the producer Doctor reads its authority verdict from in the common
    /// case, so the core route must run the real audit rather than infer one.
    #[test]
    fn comprehensive_request_is_served_from_core_even_with_a_ready_report() {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 11,
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
        let parsed = doctor_runtime_request(&request).expect("comprehensive Doctor request");

        assert!(!parsed.doctor_report_requested());
        for report_ready in [false, true] {
            assert!(
                parsed.should_serve_from_core(report_ready),
                "comprehensive requests never fall through to the routed owner \
                 (doctor_report_ready={report_ready})"
            );
        }
    }

    #[tokio::test]
    async fn unix_doctor_probe_drops_activity_before_core_response_write() {
        let root = tempfile::TempDir::new().expect("fixture root");
        let profile = root.path().join("profile");
        let mut handshake = handshake(
            root.path().join("project"),
            profile.clone(),
            profile.join("registry.db"),
        );
        handshake.project_path = None;
        let lifecycle = DaemonLifecycle::default();
        let setup_activity = lifecycle.try_enter().expect("setup activity");
        lifecycle.begin_draining();
        let mut transport = DoctorRouteTransport {
            lifecycle,
            output: String::new(),
            idle_before_write: false,
        };
        let store_administration = StoreAdministration::default();

        let outcome = serve_core_doctor_runtime_request(
            &mut transport,
            &handshake,
            &store_administration,
            setup_activity,
            &doctor_report_request_line(),
            Some(serde_json::json!({
                "status": "degraded",
                "coverage": "degraded_poll",
                "reason": "watch_capacity_reached",
            })),
            || async { Ok(false) },
        )
        .await
        .expect("serve core Doctor response");

        assert!(outcome.is_none());
        assert!(transport.idle_before_write);
        assert!(!transport.output.is_empty());
        assert!(transport.output.contains(r#""kind":"unknown""#));
        assert!(
            transport
                .output
                .contains(r#""reason":"doctor_report_owner_warming""#)
        );
        assert!(
            transport.output.contains("git_watcher") && transport.output.contains("degraded_poll"),
            "the production core Doctor response must expose watcher health"
        );
    }

    #[tokio::test]
    async fn portable_doctor_probe_preserves_activity_for_ready_owner_fallthrough() {
        let root = tempfile::TempDir::new().expect("fixture root");
        let profile = root.path().join("profile");
        let handshake = handshake(
            root.path().join("project"),
            profile.clone(),
            profile.join("registry.db"),
        );
        let lifecycle = DaemonLifecycle::default();
        let setup_activity = lifecycle.try_enter().expect("setup activity");
        let mut transport = DoctorRouteTransport {
            lifecycle: lifecycle.clone(),
            output: String::new(),
            idle_before_write: false,
        };
        let store_administration = StoreAdministration::default();

        let outcome = serve_core_doctor_runtime_request(
            &mut transport,
            &handshake,
            &store_administration,
            setup_activity,
            &doctor_report_request_line(),
            None,
            || async { Ok(true) },
        )
        .await
        .expect("fall through to ready owner");

        assert!(outcome.is_some());
        assert!(transport.output.is_empty());
        drop(outcome);
        lifecycle.begin_draining();
        tokio::time::timeout(std::time::Duration::from_secs(1), lifecycle.wait_for_idle())
            .await
            .expect("fallthrough activity drops with caller ownership");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn fast_runtime_snapshot_uses_retained_liveness_without_storage_audit() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        initialize_test_project(&project, &profile).await;
        let handshake = handshake(
            project.clone(),
            profile.clone(),
            profile.join("registry.db"),
        );
        let store_administration = StoreAdministration::default().with_profile_identity(
            crate::daemon::profile_identity::load_or_create(&profile)
                .expect("load fixture profile identity"),
        );
        let _database_scope = crate::db::enter_daemon_database_scope(
            &profile,
            REGISTERED_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed),
            "core-doctor-fast-runtime-health",
        )
        .expect("enter daemon database scope");
        let graph =
            super::super::open_project_for_handshake(&project, &handshake, &store_administration)
                .await
                .expect("open retained project graph");
        let key = crate::daemon::ProjectServerKey::from_open_project(&graph, &handshake)
            .expect("project server key");
        let route_live = Arc::new(AtomicBool::new(true));
        let server = McpServer::new_with_context(
            McpServerConstructionContext::direct(graph, None)
                .with_project_server_live(Arc::clone(&route_live)),
        )
        .await;
        store_administration
            .project_servers()
            .lock()
            .await
            .insert(key, server);
        let value =
            super::doctor_runtime_value(&handshake, &store_administration, false, None).await;

        assert_eq!(
            value.pointer("/doctor_runtime/status"),
            Some(&serde_json::json!("live")),
            "fast runtime health must project retained liveness without probing SQLite"
        );
        assert_eq!(
            value.pointer("/database/quick_check_ok"),
            Some(&serde_json::Value::Null),
            "the core runtime snapshot must not run quick_check"
        );
        assert!(
            value.pointer("/database/wal_size_bytes").is_none()
                && value.pointer("/database/shm_size_bytes").is_none(),
            "runtime health must not expose SQLite sidecar implementation details"
        );
    }

    #[tokio::test]
    async fn cold_missing_store_reports_unavailable_without_creating_files() {
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
            value.pointer("/session_temporal_health/findings"),
            None,
            "a missing current store is an unavailable state, not a migration finding"
        );
    }

    #[tokio::test]
    async fn malformed_store_returns_fixed_safe_error_without_mutation() {
        let root = tempfile::TempDir::new().unwrap();
        let project = root.path().join("project");
        let profile = root.path().join("profile");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let layout = initialize_test_project(&project, &profile).await;
        let db_path = layout.graph_db_path;
        std::fs::write(&db_path, b"malformed doctor fixture").unwrap();
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
    async fn doctor_store_paths_ignore_an_active_branch_database() {
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
        assert!(
            std::process::Command::new("git")
                .args(["checkout", "-b", "feature/doctor"])
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );

        assert_eq!(
            super::doctor_runtime_store_layout(&project, &profile)
                .expect("resolve canonical Doctor store paths"),
            (default_graph, layout.sessions_db_path),
            "Doctor must not follow branch-specific database paths"
        );
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
        let session_path = layout.sessions_db_path;
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
