//! `tracedecay_runtime` — daemon, store, and session-observation health, including the optional doctor report.

use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_application::semantic_runtime::project_lifecycle_status;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDb;

use crate::{McpDoctorReportV1, McpToolContext, ToolResult, generic_tool_result};

/// Bound for the session-temporal doctor probe so a wedged sessions DB cannot
/// monopolize a `tracedecay_runtime` request indefinitely.
const SESSION_TEMPORAL_HEALTH_BUDGET: Duration = Duration::from_secs(8);

async fn session_temporal_health_value(
    project_session_db: Option<&tracedecay_global_db::RegisteredGlobalDb>,
) -> Value {
    match project_session_db {
        Some(db) => match tokio::time::timeout(
            SESSION_TEMPORAL_HEALTH_BUDGET,
            db.session_temporal_doctor_health(),
        )
        .await
        {
            Ok(health) => serde_json::to_value(health).unwrap_or_else(|_| {
                json!({
                    "status": "unavailable",
                    "findings": [],
                    "message": "session temporal health serialization failed",
                })
            }),
            Err(_) => json!({
                "status": "timed_out",
                "findings": [],
                "message": "session temporal health exceeded deadline",
            }),
        },
        None => json!({
            "status": "unavailable",
            "findings": [],
        }),
    }
}

/// Runs the exhaustive observation-authority audit for the routed project
/// owner.
///
/// Returns `(ok, typed reason, observed detail)`. `ok` is tri-state: `Some(true)`
/// only when the audit ran and passed, `Some(false)` when it ran and failed, and
/// `None` when it could not run at all. The typed reason uses the vocabulary
/// Doctor already understands (`authority_invariant_failed`,
/// `authority_store_unavailable`) so the CLI can classify without parsing the
/// free-form detail.
async fn observation_authority_audit(
    registry: Option<&tracedecay_global_db::RegisteredGlobalDb>,
) -> (Option<bool>, Option<&'static str>, Option<String>) {
    match registry {
        Some(registry) => {
            let audit = match registry.read_snapshot().await {
                Ok(snapshot) => {
                    tracedecay_global_db::schema_stages::validate_observation_authority_connection(
                        &snapshot,
                    )
                    .await
                }
                Err(error) => Err(TraceDecayError::Database {
                    operation: "begin observation authority audit".to_string(),
                    message: error.to_string(),
                }),
            };
            match audit {
                Ok(()) => (Some(true), None, None),
                Err(error) => (
                    Some(false),
                    Some("authority_invariant_failed"),
                    Some(error.to_string()),
                ),
            }
        }
        // This handler is only reached with a routed project owner, so a missing
        // handle means the registry could not be attached here; the daemon core
        // route is the producer that can distinguish a store that is absent on
        // disk (`authority_store_missing`).
        None => (
            None,
            Some("authority_store_unavailable"),
            Some("authoritative global registry is unavailable".to_string()),
        ),
    }
}

/// Registered-runtime implementation of literal workspace-placeholder paths
/// over a registered read snapshot.
async fn literal_workspace_placeholder_transcript_paths(
    conn: &impl tracedecay_runtime_core::db::engine::QueryExecutor,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(mut rows) = conn
        .query(
            "SELECT DISTINCT transcript_path FROM sessions
             WHERE transcript_path IS NOT NULL
               AND transcript_path != ''
               AND (transcript_path LIKE '%${workspaceFolder}%'
                    OR transcript_path LIKE '%$workspaceFolder%')
             ORDER BY transcript_path
             LIMIT ?1",
            tracedecay_runtime_core::db::engine::params![i64::try_from(limit).unwrap_or(i64::MAX)],
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

fn attach_doctor_report(value: &mut Value, report: McpDoctorReportV1<'_>) {
    value["doctor_report"] = match report {
        McpDoctorReportV1::Read(admitted) => json!({
            "kind": "observed",
            "report": admitted.report,
            "table_growth_evidence": admitted.table_growth_evidence,
        }),
        McpDoctorReportV1::ReadFailed => json!({
            "kind": "unknown",
            "table_growth_evidence": [],
        }),
        McpDoctorReportV1::NotAttached => json!({
            "kind": "unsupported",
            "table_growth_evidence": [],
        }),
    };
}

pub async fn collect_database_snapshot(
    ctx: &McpToolContext<'_>,
    include_integrity: bool,
    generation_census: Option<
        &tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot,
    >,
) -> Result<tracedecay_session_memory::runtime_telemetry::DatabaseSnapshot> {
    let database = ctx.graph_database();
    let db_path = ctx.graph_db_path();
    let store_runtime = ctx.store_runtime();
    let collected = tracedecay_runtime_core::store_telemetry::collect_store_telemetry(
        database,
        ctx.project_root().to_path_buf(),
        db_path.to_path_buf(),
        include_integrity,
    )
    .await?;
    let generation_census = generation_census.cloned().unwrap_or(
        tracedecay_session_memory::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
            reason: tracedecay_session_memory::runtime_telemetry::GenerationCensusUnavailableReason::AuthorityUnavailable,
        },
    );
    Ok(
        tracedecay_session_memory::runtime_telemetry::DatabaseSnapshot::from_collected(
            collected,
            tracedecay_session_memory::runtime_telemetry::read_dirty_marker(
                &tracedecay_session_memory::runtime_telemetry::with_suffix(db_path, ".dirty"),
            ),
            generation_census,
            tracedecay_session_memory::runtime_telemetry::RuntimeRegistrySnapshot::from_projection(
                store_runtime.runtime_telemetry(),
            ),
        ),
    )
}

async fn collect_runtime_snapshot(
    ctx: &McpToolContext<'_>,
    include_integrity: bool,
    tracedecay_version: &str,
) -> Result<tracedecay_session_memory::runtime_telemetry::RuntimeSnapshot> {
    tracedecay_session_memory::runtime_telemetry::read_cached_process_sample();
    let database =
        collect_database_snapshot(ctx, include_integrity, ctx.generation_census()).await?;
    let process = tracedecay_session_memory::runtime_telemetry::read_cached_process_sample_at_response_boundary()
        .await;
    Ok(
        tracedecay_session_memory::runtime_telemetry::RuntimeSnapshot {
            captured_at: tracedecay_session_memory::runtime_telemetry::unix_epoch_secs()?,
            tracedecay_version: tracedecay_version.to_owned(),
            host_os: std::env::consts::OS.to_owned(),
            process,
            database,
        },
    )
}

/// Surfaces process and database telemetry so users hitting unexpected
/// CPU/RAM pressure can attach a structured snapshot to a bug report.
#[hotpath::measure(label = "mcp.health.runtime.total")]
pub async fn handle_runtime(
    ctx: &McpToolContext<'_>,
    args: Value,
    registry: Option<&RegisteredGlobalDb>,
    tracedecay_version: &str,
) -> Result<ToolResult> {
    let authority_audit = args
        .get("authority_audit")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let snap = hotpath::future!(
        collect_runtime_snapshot(ctx, authority_audit, tracedecay_version),
        label = "mcp.health.runtime.telemetry"
    )
    .await?;
    // A snapshot that cannot be serialized is a contract bug, not an empty
    // status: swallowing it into `{}` made doctor report "omitted database
    // telemetry" with no trace of the cause.
    let mut value = serde_json::to_value(&snap).map_err(|error| TraceDecayError::Config {
        message: format!("runtime telemetry snapshot could not be serialized: {error}"),
    })?;
    // Doctor historically keys temporal health off `authority_audit`. Keep that
    // coupling, and also allow an explicit independent opt-in.
    let include_session_temporal_health = authority_audit
        || args
            .get("session_temporal_health")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if authority_audit || include_session_temporal_health {
        let (authority, temporal) = tokio::join!(
            async {
                if authority_audit {
                    Some(
                        hotpath::future!(
                            observation_authority_audit(registry),
                            label = "mcp.health.runtime.authority_audit"
                        )
                        .await,
                    )
                } else {
                    None
                }
            },
            async {
                if include_session_temporal_health {
                    Some(
                        hotpath::future!(
                            session_temporal_health_value(
                                ctx.authorized_project_session_db()
                                    .map(|(lease, _)| lease.as_ref()),
                            ),
                            label = "mcp.health.runtime.session_temporal"
                        )
                        .await,
                    )
                } else {
                    None
                }
            }
        );
        if let Some((authority_audit_ok, authority_audit_reason, authority_audit_error)) = authority
            && let Some(database) = value.get_mut("database").and_then(Value::as_object_mut)
        {
            database.insert("authority_audit_ok".to_string(), json!(authority_audit_ok));
            database.insert(
                "authority_audit_reason".to_string(),
                json!(authority_audit_reason),
            );
            database.insert(
                "authority_audit_error".to_string(),
                json!(authority_audit_error),
            );
        }
        if let Some(temporal) = temporal {
            value["session_temporal_health"] = temporal;
        }
    }
    if args
        .get("session_ingest_health")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        match ctx.authorized_project_session_db() {
            Some((lease, _)) => {
                let db = lease.as_ref();
                value["cursor_session_ingest"] = match hotpath::future!(
                    db.cursor_session_ingest_health(),
                    label = "mcp.health.runtime.session_ingest"
                )
                .await
                {
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
                };
                match hotpath::future!(
                    db.read_snapshot(),
                    label = "mcp.health.runtime.session_snapshot"
                )
                .await
                {
                    Ok(snapshot) => {
                        value["cursor_session_placeholder_paths"] = json!(
                            hotpath::future!(
                                literal_workspace_placeholder_transcript_paths(&snapshot, 10),
                                label = "mcp.health.runtime.placeholder_paths"
                            )
                            .await
                        );
                    }
                    Err(_) => {
                        value["cursor_session_placeholder_paths"] = json!([]);
                    }
                }
            }
            None => {
                value["cursor_session_ingest"] = json!({
                    "status": "unavailable",
                    "reason": "session_store_denied",
                    "message": "this request is not authorized to read the admitted project session store",
                });
            }
        }
    }
    if args
        .get("doctor_report")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        attach_doctor_report(&mut value, ctx.doctor_report());
    }
    let semantic_configuration = hotpath::future!(
        ctx.configuration_runtime().client().current(),
        label = "mcp.health.runtime.semantic"
    )
    .await
    .ok()
    .and_then(|pinned| {
        tracedecay_application::semantic_runtime::SemanticConfigurationPinV1::from_current(
            &pinned.into_current_state(),
        )
        .ok()
    });
    value["semantic_runtime"] = serde_json::to_value(
        tracedecay_application::semantic_runtime::resolve_project_semantic_runtime_status(
            Some(ctx.project_root()),
            semantic_configuration,
        ),
    )
    .unwrap_or_else(|_| json!({}));
    value["semantic_model"] = json!(project_lifecycle_status(ctx.project_root()));
    Ok(generic_tool_result(
        Some(ctx.project_root()),
        &args,
        &value,
        vec![],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requested_doctor_report_is_typed_unavailable_without_reader() {
        let mut value = json!({});

        attach_doctor_report(&mut value, McpDoctorReportV1::NotAttached);

        assert_eq!(
            value["doctor_report"],
            json!({
                "kind": "unsupported",
                "table_growth_evidence": [],
            })
        );
    }
}
