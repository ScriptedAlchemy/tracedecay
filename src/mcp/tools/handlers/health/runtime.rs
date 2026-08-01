//! `tracedecay_runtime` — daemon, store, and session-observation health, including the optional doctor report.

use super::*;

/// Bound for the session-temporal doctor probe so a wedged sessions DB cannot
/// monopolize a `tracedecay_runtime` request indefinitely.
const SESSION_TEMPORAL_HEALTH_BUDGET: Duration = Duration::from_secs(8);

async fn session_temporal_health_value(
    project_session_db: Option<&crate::global_db::RegisteredGlobalDb>,
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
    registry: Option<&crate::global_db::RegisteredGlobalDb>,
) -> (Option<bool>, Option<&'static str>, Option<String>) {
    match registry {
        Some(registry) => {
            let audit = match registry.read_snapshot().await {
                Ok(snapshot) => {
                    crate::global_db::schema_stages::validate_observation_authority_connection(
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
    conn: &impl crate::db::engine::QueryExecutor,
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
            crate::db::engine::params![i64::try_from(limit).unwrap_or(i64::MAX)],
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

/// Handles `tracedecay_runtime` tool calls.
///
/// Issue #80 — surface process and database telemetry so users hitting
/// unexpected CPU/RAM pressure can attach a structured snapshot to a
/// bug report. The MCP wrapper just delegates to `runtime_telemetry`.
async fn attach_doctor_report(
    value: &mut Value,
    reader: Option<&crate::dashboard::DoctorReportReader>,
) {
    value["doctor_report"] = match reader {
        Some(reader) => match reader().await {
            Ok(admitted) => json!({
                "kind": "observed",
                "report": admitted.report,
                "table_growth_evidence": admitted.table_growth_evidence,
            }),
            Err(_) => json!({
                "kind": "unknown",
                "table_growth_evidence": [],
            }),
        },
        None => json!({
            "kind": "unsupported",
            "table_growth_evidence": [],
        }),
    };
}

pub(crate) async fn handle_runtime(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&crate::global_db::RegisteredGlobalDb>,
    project_session_db: Option<&crate::global_db::RegisteredGlobalDb>,
    doctor_report_reader: Option<&crate::dashboard::DoctorReportReader>,
) -> Result<ToolResult> {
    let authority_audit = args
        .get("authority_audit")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let snap = crate::runtime_telemetry::collect_with_integrity(cg, authority_audit).await?;
    let mut value = serde_json::to_value(&snap).unwrap_or_else(|_| json!({}));
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
                    Some(observation_authority_audit(registry).await)
                } else {
                    None
                }
            },
            async {
                if include_session_temporal_health {
                    Some(session_temporal_health_value(project_session_db).await)
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
        match project_session_db {
            Some(db) => {
                value["cursor_session_ingest"] = match db.cursor_session_ingest_health().await {
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
                match db.read_snapshot().await {
                    Ok(snapshot) => {
                        value["cursor_session_placeholder_paths"] = json!(
                            literal_workspace_placeholder_transcript_paths(&snapshot, 10).await
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
                    "reason": "session_store_unavailable",
                    "message": "daemon project session authority is unavailable",
                });
            }
        }
    }
    if args
        .get("doctor_report")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        attach_doctor_report(&mut value, doctor_report_reader).await;
    }
    let semantic_configuration = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .ok()
        .and_then(|pinned| {
            crate::application::semantic_runtime::SemanticConfigurationPinV1::from_current(
                &crate::application::configuration::ConfigurationCurrentStateV1 {
                    revision_id: pinned.revision_id,
                    snapshot: pinned.snapshot,
                },
            )
            .ok()
        });
    if let Some(semantic) =
        crate::application::semantic_runtime::project_semantic_application_status(
            cg.project_root(),
            semantic_configuration,
        )
    {
        value["semantic_runtime"] = serde_json::to_value(&semantic).unwrap_or_else(|_| json!({}));
    }
    Ok(generic_tool_result(
        Some(cg.project_root()),
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

        attach_doctor_report(&mut value, None).await;

        assert_eq!(
            value["doctor_report"],
            json!({
                "kind": "unsupported",
                "table_growth_evidence": [],
            })
        );
    }
}
