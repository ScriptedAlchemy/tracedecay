//! `tracedecay_status`, `tracedecay_active_project`, and the daemon-only `tracedecay_admin_sync` entry point.

use super::*;

/// Daemon-only sync entry point used by the first-party CLI. It is deliberately
/// not advertised in the MCP catalog: external agents should rely on the
/// daemon watcher while the CLI can request an explicit serialized refresh.
pub(crate) async fn handle_admin_sync(
    cg: &TraceDecay,
    args: Value,
    reconcile_sink: Option<&crate::mcp::server::CodeIndexReconcileSink>,
) -> Result<ToolResult> {
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let reconcile_sink = reconcile_sink.ok_or_else(|| {
        TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "admin sync requires the daemon code-index scheduler",
        )
    })?;
    if !reconcile_sink(cg.project_root().to_path_buf()).await {
        return Err(TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "admin sync was not accepted by the code-index scheduler",
        ));
    }
    let output = json!({
        "requested_mode": if force { "force" } else { "refresh" },
        "reconcile_scope": "authoritative_project",
        "status": "queued",
        "project_root": cg.project_root(),
    });
    let text = serde_json::to_string(&output)?;
    Ok(ToolResult::new(
        json!({
            "content": [{
                "type": "text",
                "text": text,
            }]
        }),
        Vec::new(),
    ))
}

fn status_arg_flag(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn attach_compact_branch_summary(cg: &TraceDecay, output: &mut Value) {
    // Avoid `branch_diagnostics()` — compact CLI status only needs the
    // already-resolved serving identity retained on TraceDecay.
    // Do not alias open/active into current/live: those are distinct under drift.
    if let Some(active) = cg.active_branch() {
        output["active_branch"] = json!(active);
    }
    if let Some(serving) = cg.serving_branch() {
        output["serving_branch"] = json!(serving);
    }
}

fn attach_full_branch_status(cg: &TraceDecay, output: &mut Value) {
    let branch_diagnostics = cg.branch_diagnostics();
    if let Some(open_branch) = branch_diagnostics.open_active_branch.as_deref() {
        output["active_branch"] = json!(open_branch);
    }
    if let Some(current_branch) = branch_diagnostics.current_branch.as_deref() {
        output["current_branch"] = json!(current_branch);
        output["live_branch"] = json!(current_branch);
    }
    if let Some(serving_branch) = branch_diagnostics.serving_branch.as_deref() {
        output["serving_branch"] = json!(serving_branch);
    }
    if let Some(parent) = branch_diagnostics
        .branches
        .iter()
        .find(|entry| entry.is_serving)
        .and_then(|entry| entry.parent.as_deref())
    {
        output["parent_branch"] = json!(parent);
    }
    output["branch_drifted"] = json!(branch_diagnostics.branch_drifted);
    output["branch_resolution"] = json!(branch_diagnostics.branch_resolution.clone());
    output["tracked_branch_count"] = json!(branch_diagnostics.tracked_branch_count);
    if branch_diagnostics.branch_drifted {
        output["branch_mismatch"] = json!({
            "git_branch": branch_diagnostics.current_branch,
            "indexed_branch": branch_diagnostics.open_active_branch,
            "serving_branch": branch_diagnostics.serving_branch,
        });
    }
    if !branch_diagnostics.warnings.is_empty() {
        output["branch_warnings"] = json!(branch_diagnostics.warnings);
    }
}

/// Serialize the generation census exactly as the CLI decoder reads it back.
///
/// [`crate::runtime_telemetry::GenerationCensusSnapshot`] is the single wire
/// authority for the `graph_statistics` field: this route serializes it and
/// `tracedecay status` deserializes the same Rust type, so the two sides
/// cannot drift.
pub(crate) async fn graph_statistics_value(
    generation_census_reader: Option<&crate::runtime_telemetry::GenerationCensusReader>,
) -> Result<Value> {
    let census = match generation_census_reader {
        Some(reader) => reader().await,
        None => crate::runtime_telemetry::GenerationCensusSnapshot::Unavailable {
            reason:
                crate::runtime_telemetry::GenerationCensusUnavailableReason::AuthorityUnavailable,
        },
    };
    Ok(serde_json::to_value(&census)?)
}

/// Handles `tracedecay_status` tool calls.
pub(crate) async fn handle_status(
    cg: &TraceDecay,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
    project_session_db: Option<&RegisteredGlobalDb>,
    code_index_freshness_reader: Option<
        &crate::dashboard::code_index_freshness_api::CodeIndexFreshnessReader,
    >,
    generation_census_reader: Option<&crate::runtime_telemetry::GenerationCensusReader>,
) -> Result<ToolResult> {
    if status_arg_flag(&args, "admission_only", false) {
        let mut output = json!({
            "project_admitted": true,
            "project_root": cg.project_root(),
        });
        if let Some(ss) = server_stats {
            output["server"] = ss;
        }
        if let Some(prefix) = scope_prefix {
            output["scope_prefix"] = json!(prefix);
        }
        return Ok(generic_tool_result(
            Some(cg.project_root()),
            &args,
            &output,
            vec![],
        ));
    }

    let include_branch_diagnostics = status_arg_flag(&args, "include_branch_diagnostics", true);
    let include_storage_health = status_arg_flag(&args, "include_storage_health", true);
    let include_session_ingest = status_arg_flag(&args, "include_session_ingest", true);
    let include_staleness = status_arg_flag(&args, "include_staleness", true);

    let mut output = json!({
        "project_root": cg.project_root(),
        "graph_statistics": graph_statistics_value(generation_census_reader).await?,
    });
    let code_index_freshness = match code_index_freshness_reader {
        Some(reader) => match reader(cg.project_root().to_path_buf()).await {
            Some(freshness) => {
                let authoritative = freshness.latest_generation_id.is_some()
                    && freshness.coverage == "complete"
                    && freshness.staleness_state.as_deref() == Some("fresh");
                if !authoritative {
                    output["code_index_freshness_warning"] = json!(
                        "graph counts are not authoritative until the scheduler seals a complete fresh generation"
                    );
                }
                json!({
                    "status": if authoritative { "current" } else { "warming" },
                    "worktree": freshness,
                })
            }
            None => json!({
                "status": "unavailable",
                "reason": "code_index_scheduler_not_mounted",
            }),
        },
        None => json!({
            "status": "unavailable",
            "reason": "code_index_scheduler_authority_not_attached",
        }),
    };
    output["code_index_freshness"] = code_index_freshness;
    if include_storage_health {
        let mut storage_health =
            serde_json::to_value(crate::runtime_telemetry::collect_database(cg, false).await?)
                .unwrap_or_else(|_| json!({}));
        if server_stats.is_some() {
            storage_health["daemon_owner_pid"] = json!(std::process::id());
            storage_health["daemon_generation"] = json!(crate::runtime_identity::process_run_id());
        }
        output["storage_health"] = storage_health;
    }
    if let Some(ss) = server_stats {
        output["server"] = ss;
    }

    if include_branch_diagnostics {
        attach_full_branch_status(cg, &mut output);
    } else {
        attach_compact_branch_summary(cg, &mut output);
    }

    // Session-transcript ingest health (recall trust): last ingest time and
    // any un-ingested transcript backlog from the project sessions.db.
    if include_session_ingest {
        let session_db_path = cg.store_layout().sessions_db_path.clone();
        if session_db_path.exists() {
            match project_session_db {
                None => {
                    // The store exists but the daemon did not retain its authority;
                    // fail closed instead of opening a second connection here.
                    output["session_ingest"] = json!({
                        "status": "unavailable",
                        "reason": "session_store_unavailable",
                        "message": "daemon project session authority is unavailable",
                    });
                }
                Some(db) => match db.cursor_session_ingest_health().await {
                    Ok(ingest) => {
                        output["session_ingest"] =
                            serde_json::to_value(&ingest).unwrap_or_else(|error| {
                                json!({
                                    "status": "unavailable",
                                    "reason": "session_ingest_serialization_failed",
                                    "message": error.to_string(),
                                })
                            });
                        // `session_ingest` stays cursor-scoped so it keeps matching the
                        // doctor-owned signal. Historical catch-up is measured across
                        // providers and remains explicitly partial while the retained
                        // daemon authority drains its bounded backlog.
                        if let Some(catch_up) = historical_session_catch_up(db).await {
                            output["session_history_catch_up"] = catch_up;
                        }
                    }
                    Err(error) => {
                        output["session_ingest"] = json!({
                            "status": "unavailable",
                            "reason": "session_ingest_query_failed",
                            "message": error,
                        });
                    }
                },
            }
        }
    }

    if include_staleness {
        output["git_staleness"] = json!({
            "status": "unavailable",
            "reason": "sealed_generation_git_watermark_not_published",
            "message": "the verified code generation does not publish a Git commit watermark",
        });
    }

    if let Some(prefix) = scope_prefix {
        output["scope_prefix"] = json!(prefix);
    }

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        vec![],
        || render_status_md(&output),
    ))
}

/// Reports daemon-owned historical warming when any provider's backlog exceeds
/// the ordinary catch-up threshold, so partial recall is never read as current.
async fn historical_session_catch_up(db: &RegisteredGlobalDb) -> Option<Value> {
    match db.session_ingest_health_for_provider(None).await {
        Ok(ingest) => historical_session_catch_up_state(&ingest),
        Err(error) => Some(json!({
            "status": "unavailable",
            "coverage": "unknown",
            "authority": "daemon",
            "reason": "historical_backlog_measurement_failed",
            "message": error,
        })),
    }
}

fn historical_session_catch_up_state(ingest: &SessionIngestHealth) -> Option<Value> {
    use std::collections::BTreeSet;

    const THRESHOLD: u64 =
        tracedecay_sessions::runtime::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES;
    let warming = ingest.max_transcript_pending_bytes > THRESHOLD;
    let observed = &ingest.observed_providers;
    let configured = observed
        .iter()
        .map(String::as_str)
        .chain(
            ingest
                .provider_coverage
                .iter()
                .map(|coverage| coverage.provider.as_str()),
        )
        .collect::<BTreeSet<_>>();
    let unobserved = configured
        .iter()
        .copied()
        .filter(|provider| !observed.iter().any(|observed| observed == provider))
        .collect::<Vec<_>>();
    let coverage_incomplete =
        ingest.provider_coverage.iter().any(|coverage| {
            coverage.state != crate::global_db::SessionProviderCoverageState::Complete
        }) || observed.iter().any(|provider| {
            tracedecay_sessions::runtime::SessionProvider::parse(provider).is_some_and(|provider| {
                provider.writes_typed_history_coverage()
                    && !ingest.provider_coverage.iter().any(|coverage| {
                        coverage.provider == provider.id()
                            && coverage.state
                                == crate::global_db::SessionProviderCoverageState::Complete
                    })
            })
        });
    let any_provider_available = ingest.provider_coverage.iter().any(|coverage| {
        coverage.state != crate::global_db::SessionProviderCoverageState::Unavailable
    });
    let source_unavailable = observed.is_empty() && !any_provider_available;
    Some(json!({
        "status": if source_unavailable {
            "unavailable"
        } else if warming || coverage_incomplete {
            "warming"
        } else {
            "current"
        },
        "coverage": if source_unavailable || warming || coverage_incomplete {
            "partial"
        } else {
            "complete"
        },
        "authority": "daemon",
        "reason": if source_unavailable {
            "historical_sources_unobserved"
        } else if warming {
            "historical_transcript_backlog"
        } else if coverage_incomplete {
            "historical_provider_coverage_incomplete"
        } else {
            "historical_catch_up_current"
        },
        "providers": observed,
        "provider_coverage": ingest.provider_coverage,
        "unobserved_providers": unobserved,
        "max_transcript_pending_bytes": ingest.max_transcript_pending_bytes,
        "pending_bytes": ingest.pending_bytes,
        "pending_transcripts": ingest.pending_transcripts,
        "message": if source_unavailable {
            "No durable historical source rows or provider frontiers are currently observable."
        } else if warming || coverage_incomplete {
            "Historical session recall is partially available while the daemon continues bounded background catch-up."
        } else {
            "Historical session recall catch-up is current."
        },
    }))
}

fn render_status_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Project Status");
    if let Some(obj) = value.as_object() {
        let mut warnings: Vec<String> = Vec::new();
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for k in keys {
            let v = &obj[k];
            if k.contains("warning")
                && let Some(s) = v.as_str()
            {
                warnings.push(s.to_string());
                continue;
            }
            match v {
                Value::String(s) => {
                    md.field(k, s);
                }
                Value::Number(n) => {
                    md.field(k, &n.to_string());
                }
                Value::Bool(b) => {
                    md.field(k, &b.to_string());
                }
                Value::Array(a) => {
                    md.field(k, &format!("{} item(s)", a.len()));
                }
                Value::Object(o) => {
                    md.field(k, &format!("{{{} field(s)}}", o.len()));
                }
                Value::Null => {}
            }
        }
        if !warnings.is_empty() {
            md.blank().heading(3, "Warnings");
            for w in &warnings {
                md.bullet(w);
            }
        }
    }
    md.render()
}

fn active_project_context(
    cg: &TraceDecay,
    branch: &BranchDiagnostics,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
) -> Value {
    let project_root = cg.project_root();
    let layout = cg.store_layout();
    let graph_db_path = cg.db_path();
    let mut output = json!({
        "project_root": display_path(project_root),
        "resolution_source": "active_project",
        "storage": {
            "class": store_kind_name(&layout.store_kind),
            "mode": storage_mode_name(&layout.storage_mode),
            "data_root": display_path(&layout.data_root),
            "config_path": display_path(&layout.config_path),
            "graph_db_path": display_path(&graph_db_path),
            "graph_db_exists": graph_db_path.exists(),
            "graph_db_size_bytes": graph_db_path.metadata().map_or(0, |metadata| metadata.len()),
            "sessions_db_path": display_path(&layout.sessions_db_path),
            "response_handle_root": display_path(&layout.response_handle_root),
            "lcm_payload_root": display_path(&layout.lcm_payload_root),
        },
        "branch": {
            "current_branch": branch.current_branch.clone(),
            "open_active_branch": branch.open_active_branch.clone(),
            "serving_branch": branch.serving_branch.clone(),
            "branch_resolution": branch.branch_resolution.clone(),
            "branch_drifted": branch.branch_drifted,
            "tracked_branch_count": branch.tracked_branch_count,
            "warnings": branch.warnings.clone(),
        }
    });
    if let Some(prefix) = scope_prefix {
        output["scope_prefix"] = json!(prefix);
    }
    if let Some(stats) = server_stats {
        output["server"] = stats;
    }
    output
}

fn storage_mode_name(mode: &StorageMode) -> &'static str {
    match mode {
        StorageMode::ProjectLocal => "project_local",
        StorageMode::ProfileSharded => "profile_sharded",
    }
}

fn store_kind_name(kind: &StoreKind) -> &'static str {
    match kind {
        StoreKind::CodeProject => "code_project",
    }
}

/// Handles `tracedecay_active_project` tool calls.
pub(crate) fn handle_active_project(
    cg: &TraceDecay,
    args: &Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
) -> ToolResult {
    let branch = cg.branch_diagnostics();
    let output = active_project_context(cg, &branch, server_stats, scope_prefix);
    generic_tool_result(Some(cg.project_root()), args, &output, vec![])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::global_db::{
        SessionIngestHealth, SessionProviderCoverage, SessionProviderCoverageState,
    };
    use crate::runtime_telemetry::{
        GenerationCensusReader, GenerationCensusSnapshot, GenerationCensusUnavailableReason,
    };

    use super::{graph_statistics_value, historical_session_catch_up_state};

    /// The daemon serializes `graph_statistics` and `tracedecay status`
    /// deserializes it as the same Rust type. This round-trip is the wire
    /// contract: if either side drifts, this test fails before a user sees a
    /// `missing field` decode error.
    #[tokio::test]
    async fn graph_statistics_round_trips_the_cli_status_decode() {
        let absent = graph_statistics_value(None)
            .await
            .expect("typed absence serializes");
        let decoded: GenerationCensusSnapshot =
            serde_json::from_value(absent).expect("CLI decodes typed absence");
        assert_eq!(
            decoded,
            GenerationCensusSnapshot::Unavailable {
                reason: GenerationCensusUnavailableReason::AuthorityUnavailable,
            }
        );

        let observed = GenerationCensusSnapshot::Observed {
            statistics: crate::code_index::production::CodeIndexGenerationStatisticsV1 {
                source_total_bytes: 1_024,
                symbol_count: 12,
                edge_count: 7,
            },
        };
        let reader_census = observed.clone();
        let reader: GenerationCensusReader = Arc::new(move || {
            let census = reader_census.clone();
            Box::pin(async move { census })
        });
        let value = graph_statistics_value(Some(&reader))
            .await
            .expect("observed census serializes");
        let decoded: GenerationCensusSnapshot =
            serde_json::from_value(value).expect("CLI decodes observed census");
        assert_eq!(decoded, observed);
    }

    #[test]
    fn historical_backlog_is_typed_daemon_owned_warming() {
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["cursor".into()],
            pending_transcripts: 2,
            pending_bytes: 12_000_000,
            max_transcript_pending_bytes:
                tracedecay_sessions::runtime::SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES + 1,
            ..SessionIngestHealth::default()
        })
        .expect("backlog exceeds threshold");

        assert_eq!(state["status"], "warming");
        assert_eq!(state["coverage"], "partial");
        assert_eq!(state["authority"], "daemon");
        assert!(!state.to_string().contains("sessions ingest"));
    }

    #[test]
    fn historical_status_names_database_and_discovery_backed_providers() {
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["kimi".into(), "opencode".into()],
            ..SessionIngestHealth::default()
        })
        .expect("incomplete historical coverage remains visible");
        let providers = state["providers"].as_array().unwrap();

        assert!(providers.iter().any(|provider| provider == "kimi"));
        assert!(providers.iter().any(|provider| provider == "opencode"));
        assert_eq!(state["status"], "warming");
        assert_eq!(state["coverage"], "partial");
        assert_eq!(state["reason"], "historical_provider_coverage_incomplete");
    }

    #[test]
    fn historical_status_does_not_wait_for_non_coverage_provider_writers() {
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["cursor".into()],
            ..SessionIngestHealth::default()
        })
        .expect("legacy provider backlog authority remains visible");

        assert_eq!(state["status"], "current");
        assert_eq!(state["coverage"], "complete");
    }

    #[test]
    fn historical_status_is_current_only_after_every_provider_sweep_completes() {
        let provider_coverage = tracedecay_sessions::runtime::SessionProvider::ALL
            .iter()
            .map(|provider| SessionProviderCoverage {
                provider: provider.id().to_owned(),
                state: SessionProviderCoverageState::Complete,
                deferred_units: 0,
            })
            .collect();
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["kimi".into()],
            provider_coverage,
            ..SessionIngestHealth::default()
        })
        .expect("complete provider coverage remains visible");

        assert_eq!(state["status"], "current");
        assert_eq!(state["coverage"], "complete");
    }

    #[test]
    fn explicit_partial_provider_sweep_never_reports_current() {
        let provider_coverage = tracedecay_sessions::runtime::SessionProvider::ALL
            .iter()
            .map(|provider| SessionProviderCoverage {
                provider: provider.id().to_owned(),
                state: if provider.id() == "opencode" {
                    SessionProviderCoverageState::Partial
                } else {
                    SessionProviderCoverageState::Complete
                },
                deferred_units: u64::from(provider.id() == "opencode"),
            })
            .collect();
        let state = historical_session_catch_up_state(&SessionIngestHealth {
            observed_providers: vec!["opencode".into()],
            provider_coverage,
            ..SessionIngestHealth::default()
        })
        .expect("partial provider coverage remains visible");

        assert_eq!(state["status"], "warming");
        assert_eq!(state["coverage"], "partial");
    }

    #[test]
    fn historical_status_does_not_fabricate_provider_readiness() {
        let state = historical_session_catch_up_state(&SessionIngestHealth::default())
            .expect("missing historical authority remains visible");

        assert_eq!(state["status"], "unavailable");
        assert_eq!(state["coverage"], "partial");
        assert!(state["providers"].as_array().unwrap().is_empty());
    }
}
