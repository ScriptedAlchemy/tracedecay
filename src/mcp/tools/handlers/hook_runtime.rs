use serde_json::{Value, json};
use std::path::Path;

use crate::automation::config_error;
use crate::automation::run_ledger::AutomationRunStatus;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::mcp::tools::ToolResult;
use crate::sessions::claude_observation::ClaudeObservationIngestError;
use crate::tracedecay::TraceDecay;

use super::{SessionAuthorities, rendered_tool_json};

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error(format!("missing required parameter `{key}`")))
}

pub async fn handle_hook_runtime(
    cg: &TraceDecay,
    args: Value,
    global_db: Option<&GlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    let output = match action {
        "reset_counter" => {
            cg.reset_local_counter().await?;
            json!({ "action": action, "reset": true })
        }
        "accounting_receipt" => accounting_receipt(cg, global_db).await?,
        "ingest_transcript" => {
            if args.get("user_scope").and_then(Value::as_bool) == Some(true) {
                return Err(config_error(
                    "user transcript ingest requires projectless daemon routing",
                ));
            }
            ingest_transcript(Some(cg), &args, None, global_db, session_authorities).await?
        }
        "user_review" | "hermes_receipt" => {
            return Err(config_error(format!(
                "hook action `{action}` requires projectless daemon routing"
            )));
        }
        "codex_compact" => {
            codex_compact(cg, &args, required_project_db(session_authorities)?).await?
        }
        "cursor_compact" => {
            cursor_compact(&args, required_project_db(session_authorities)?).await?
        }
        other => {
            return Err(config_error(format!(
                "unknown hook runtime action: {other}"
            )));
        }
    };
    Ok(rendered_tool_json(Some(cg.project_root()), &args, &output))
}

pub async fn handle_projectless_hook_runtime(
    args: Value,
    profile_root: &Path,
    global_db: &GlobalDb,
    session_authorities: SessionAuthorities<'_>,
) -> Result<ToolResult> {
    let action = required_str(&args, "action")?;
    if !projectless_action_allowed(action, &args) {
        return Err(config_error(format!(
            "projectless hook runtime action `{action}` is forbidden"
        )));
    }
    let output = match action {
        "ingest_transcript" => {
            ingest_transcript(
                None,
                &args,
                Some(profile_root),
                Some(global_db),
                session_authorities,
            )
            .await?
        }
        "user_review" => user_review(&args, profile_root).await?,
        "hermes_receipt" => {
            hermes_receipt(&args, profile_root, required_user_db(session_authorities)?).await?
        }
        _ => unreachable!("projectless hook action validated above"),
    };
    Ok(rendered_tool_json(None, &args, &output))
}

fn projectless_action_allowed(action: &str, args: &Value) -> bool {
    matches!(action, "user_review" | "hermes_receipt")
        || (action == "ingest_transcript"
            && args.get("user_scope").and_then(Value::as_bool) == Some(true))
}

fn required_project_db(authorities: SessionAuthorities<'_>) -> Result<&GlobalDb> {
    authorities
        .project
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon project session database is unavailable"))
}

fn required_user_db(authorities: SessionAuthorities<'_>) -> Result<&GlobalDb> {
    authorities
        .user
        .map(AsRef::as_ref)
        .ok_or_else(|| config_error("daemon user session database is unavailable"))
}

async fn codex_compact(cg: &TraceDecay, args: &Value, db: &GlobalDb) -> Result<Value> {
    let event_json = required_str(args, "event_json")?;
    if let Some(source) = crate::sessions::codex::CodexSource::new() {
        crate::sessions::source::try_ingest_source(db, &source, cg.project_root(), None)
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
    }
    let session_id = serde_json::from_str::<Value>(event_json)
        .ok()
        .as_ref()
        .and_then(|value| {
            ["session_id", "conversation_id", "thread_id"]
                .iter()
                .find_map(|key| value.get(*key).and_then(Value::as_str))
                .map(str::to_string)
        });
    let mut pending = db
        .pending_codex_compaction_summary_requests(session_id.as_deref(), 1)
        .await
        .map_err(|error| config_error(format!("load Codex compaction request failed: {error}")))?;
    let Some(pending) = pending.pop() else {
        return Ok(json!({
            "action": "codex_compact",
            "status": "skipped",
            "reason": "no pending compaction summary",
        }));
    };
    let config = crate::sessions::codex_app_server::CodexAppServerSummaryConfig::from_env();
    let summary = crate::sessions::codex_app_server::summarize_with_codex_app_server(
        &pending.request,
        &config,
    )
    .map_err(|error| config_error(format!("Codex summary failed: {error}")))?;
    db.replace_codex_compaction_summary(
        &pending.node_id,
        &summary.text,
        "codex_app_server",
        summary.model.as_deref().or(config.model.as_deref()),
    )
    .await
    .map_err(|error| config_error(format!("store Codex compaction summary failed: {error}")))?;
    Ok(json!({
        "action": "codex_compact",
        "status": "completed",
        "node_id": pending.node_id,
    }))
}

async fn cursor_compact(args: &Value, db: &GlobalDb) -> Result<Value> {
    let event_json = required_str(args, "event_json")?;
    let parsed: Value = serde_json::from_str(event_json)?;
    let session_id = ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error("Cursor preCompact event omitted session id"))?;
    let ingest =
        crate::sessions::cursor::try_ingest_cursor_transcript_event_capped(event_json, db, None)
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
    let messages_to_compact = event_usize(&parsed, &["messages_to_compact", "compact_count"]);
    if messages_to_compact == Some(0) {
        return Ok(cursor_compact_skipped("no messages to compact"));
    }
    let message_count = event_usize(&parsed, &["message_count", "messages_count"]);
    let fresh_tail_count = message_count
        .zip(messages_to_compact)
        .map(|(count, compact)| count.saturating_sub(compact));
    let current_tokens = event_i64(&parsed, &["context_tokens", "current_tokens", "tokens"]);
    let context_length = event_i64(&parsed, &["context_window_size", "context_length"]);
    let first = db
        .lcm_compress(cursor_lcm_request(
            session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::HermesAuxiliary,
            None,
        ))
        .await
        .map_err(|error| config_error(format!("prepare Cursor compaction failed: {error}")))?;
    let Some(summary_request) = first.summary_request else {
        return Ok(cursor_compact_skipped(first.reason));
    };
    let config = crate::sessions::cursor_agent::CursorAgentSummaryConfig::from_env();
    let summary =
        crate::sessions::cursor_agent::summarize_with_cursor_agent(&summary_request, &config)
            .map_err(|error| config_error(format!("cursor-agent summary failed: {error}")))?;
    let second = db
        .lcm_compress(cursor_lcm_request(
            session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            crate::sessions::lcm::LcmSummarizerMode::Provided {
                summary_text: summary,
                route: Some("cursor_agent".to_string()),
            },
            first.frontier.current_frontier_store_id.or(Some(0)),
        ))
        .await
        .map_err(|error| config_error(format!("store Cursor compaction failed: {error}")))?;
    Ok(json!({
        "status": second.status,
        "reason": second.reason,
        "summary_nodes_created": second.summary_nodes_created,
        "summary_node_ids": second.summary_nodes.into_iter().map(|node| node.node_id).collect::<Vec<_>>(),
        "messages_upserted": ingest.messages_upserted,
    }))
}

fn cursor_compact_skipped(reason: impl Into<String>) -> Value {
    json!({
        "status": "skipped",
        "reason": reason.into(),
        "summary_nodes_created": 0,
        "summary_node_ids": [],
    })
}

fn event_i64(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_str()?.parse().ok())
    })
}

fn event_usize(value: &Value, keys: &[&str]) -> Option<usize> {
    event_i64(value, keys).and_then(|value| usize::try_from(value).ok())
}

fn cursor_lcm_request(
    session_id: &str,
    current_tokens: Option<i64>,
    context_length: Option<i64>,
    max_source_messages: Option<usize>,
    fresh_tail_count: Option<usize>,
    summarizer: crate::sessions::lcm::LcmSummarizerMode,
    expected_current_frontier_store_id: Option<i64>,
) -> crate::sessions::lcm::LcmCompressionRequest {
    crate::sessions::lcm::LcmCompressionRequest {
        provider: "cursor".to_string(),
        session_id: session_id.to_string(),
        messages: Vec::new(),
        current_tokens,
        focus_topic: Some("Cursor context compaction".to_string()),
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length,
        reserve_tokens_floor: None,
        summarizer,
    }
}

async fn accounting_receipt(cg: &TraceDecay, global_db: Option<&GlobalDb>) -> Result<Value> {
    let global_db = global_db.ok_or_else(|| {
        config_error("daemon accounting database is unavailable; local fallback is forbidden")
    })?;
    let stats = crate::accounting::parser::ingest(global_db).await;
    let tokens_saved = cg.get_tokens_saved().await.unwrap_or(0);
    let efficiency = if tokens_saved + stats.tokens_consumed > 0 {
        (tokens_saved as f64 / (tokens_saved + stats.tokens_consumed) as f64) * 100.0
    } else {
        0.0
    };
    Ok(json!({
        "action": "accounting_receipt",
        "turns_inserted": stats.turns_inserted,
        "cost_usd": stats.cost_usd,
        "tokens_consumed": stats.tokens_consumed,
        "tokens_saved": tokens_saved,
        "efficiency": efficiency,
    }))
}

async fn ingest_transcript(
    cg: Option<&TraceDecay>,
    args: &Value,
    profile_root: Option<&Path>,
    global_db: Option<&GlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let provider = required_str(args, "provider")?;
    let user_scope = args
        .get("user_scope")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_new_bytes = args.get("max_new_bytes").and_then(Value::as_u64);
    let mut claude_observation_stats = None;
    let messages_upserted = match (provider, user_scope) {
        ("claude", true) => {
            let profile_root =
                profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let session_id = required_str(args, "session_id")?.to_string();
            let db = required_user_db(session_authorities)?;
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            let stats = crate::sessions::claude_observation::ingest_user_sessions(
                db,
                profile_root,
                Some(session_id),
                roots,
                Some(
                    max_new_bytes
                        .unwrap_or(crate::sessions::claude_observation::CLAUDE_HOOK_MAX_NEW_BYTES),
                ),
                crate::application::observation::ObservationCancellation::default(),
            )
            .await
            .map_err(|error| map_claude_observation_ingest_error(&error))?;
            let messages_upserted = stats.transcript.messages_upserted;
            claude_observation_stats = Some(stats);
            messages_upserted
        }
        ("codex", true) => {
            let profile_root =
                profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let session_id = required_str(args, "session_id")?.to_string();
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            crate::sessions::try_ingest_user_codex_sessions_with_db(
                required_user_db(session_authorities)?,
                profile_root,
                Some(session_id),
                roots,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?
            .messages_upserted
        }
        ("cursor", true) => {
            profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let event_json = required_str(args, "event_json")?;
            let db = required_user_db(session_authorities)?;
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            crate::sessions::cursor::try_ingest_cursor_user_transcript_event_capped_with_registered_roots(
                event_json,
                db,
                max_new_bytes,
                &roots,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?
            .messages_upserted
        }
        ("cursor", false) => {
            cg.ok_or_else(|| config_error("project transcript ingest requires a project"))?;
            let event_json = required_str(args, "event_json")?;
            let db = required_project_db(session_authorities)?;
            crate::sessions::cursor::try_ingest_cursor_transcript_event_capped(
                event_json,
                db,
                max_new_bytes,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?
            .messages_upserted
        }
        ("kiro", true) => {
            let profile_root =
                profile_root.ok_or_else(|| config_error("missing client profile"))?;
            let global_db = global_db.ok_or_else(|| config_error("missing client registry"))?;
            let db = required_user_db(session_authorities)?;
            let source = crate::sessions::kiro::KiroSource::new()
                .ok_or_else(|| config_error("Kiro transcript source is unavailable"))?;
            let roots = crate::sessions::registered_project_roots_from(global_db)
                .await
                .ok_or_else(|| config_error("daemon project registry is unavailable"))?;
            crate::sessions::source::try_ingest_source(
                db,
                &source.for_user_scope(roots),
                profile_root,
                max_new_bytes,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?
            .messages_upserted
        }
        ("kiro", false) => {
            let cg =
                cg.ok_or_else(|| config_error("project transcript ingest requires a project"))?;
            let db = required_project_db(session_authorities)?;
            crate::sessions::kiro::try_ingest_kiro_for_project(db, cg.project_root(), max_new_bytes)
                .await
                .map_err(|error| map_transcript_ingest_error(&error))?
                .messages_upserted
        }
        _ => {
            return Err(config_error(format!(
                "unsupported transcript route: provider={provider} user_scope={user_scope}"
            )));
        }
    };
    let mut output = json!({
        "action": "ingest_transcript",
        "provider": provider,
        "user_scope": user_scope,
        "completed": true,
        "messages_upserted": messages_upserted,
    });
    if let Some(stats) = claude_observation_stats {
        output["observations_committed"] = json!(stats.observations_committed);
        output["observation_duplicates"] = json!(stats.observation_duplicates);
        output["cursor_advances"] = json!(stats.cursor_advances);
        output["cursor_duplicates"] = json!(stats.cursor_duplicates);
        output["records_rejected"] = json!(stats.records_rejected);
        output["records_quarantined"] = json!(stats.records_quarantined);
        output["projections_completed"] = json!(stats.projections_completed);
        output["projections_skipped"] = json!(stats.projections_skipped);
        output["projection_duplicates"] = json!(stats.projection_duplicates);
        output["deferred_sources"] = json!(stats.deferred_sources);
        output["source_bytes_scanned"] = json!(stats.source_bytes_scanned);
    }
    Ok(output)
}

fn map_transcript_ingest_error(
    error: &crate::sessions::source::TranscriptIngestError,
) -> TraceDecayError {
    let failure = crate::sessions::classify_transcript_ingest_failure("requested", "hook", error);
    TraceDecayError::hook_runtime(
        failure.reason_code,
        failure.retryable,
        format!("transcript ingest failed: {}", failure.reason_code),
    )
}

fn map_claude_observation_ingest_error(error: &ClaudeObservationIngestError) -> TraceDecayError {
    let failure = crate::sessions::classify_claude_observation_failure(error);
    TraceDecayError::hook_runtime(failure.reason_code, failure.retryable, error.to_string())
}

pub(crate) fn structured_hook_error_data(error: &TraceDecayError) -> Option<Value> {
    let (reason_code, retryable, detail) = error.hook_runtime_context()?;
    Some(json!({
        "tool": "tracedecay_hook_runtime",
        "reason_code": reason_code,
        "retryable": retryable,
        "detail": detail,
    }))
}

async fn user_review(args: &Value, profile_root: &Path) -> Result<Value> {
    use crate::automation::run_ledger::AutomationTrigger;

    let provider = required_str(args, "provider")?;
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let run_id = args
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    if crate::automation::scheduler::load_scheduler_control(
        &crate::automation::runner::user_automation_root(profile_root),
    )
    .await?
    .paused
    {
        return Ok(json!({ "action": "user_review", "status": "paused" }));
    }
    let run = run_user_review(
        profile_root,
        provider,
        session_id,
        run_id,
        AutomationTrigger::HostReceipt,
    )
    .await?;
    Ok(json!({
        "action": "user_review",
        "status": "completed",
        "session_reflector": run.session_reflector.ledger_record.status,
        "memory_curator": run.memory_curator.ledger_record.status,
        "skill_writer": run.skill_writer.ledger_record.status,
    }))
}

async fn run_user_review(
    profile_root: &std::path::Path,
    provider: &str,
    session_id: Option<String>,
    run_id: Option<String>,
    trigger: crate::automation::run_ledger::AutomationTrigger,
) -> Result<crate::automation::runner::UserSessionAutomationRun> {
    use crate::automation::backend::CodexAppServerBackend;
    use crate::automation::runner::{
        MemoryCuratorAutomationOptions, SessionReflectorAutomationOptions,
        SkillWriterAutomationOptions, UserSessionAutomationOptions,
        run_user_session_automation_with_backend,
    };

    let global = crate::user_config::UserConfig::load().automation;
    let config = crate::automation::config::effective_user_automation_config(
        profile_root,
        &global,
        crate::user_config::automation_is_configured(),
    )
    .await?;
    let backend = CodexAppServerBackend::from_automation_config(&config);
    run_user_session_automation_with_backend(
        profile_root,
        &config,
        &backend,
        UserSessionAutomationOptions {
            session_reflector: SessionReflectorAutomationOptions {
                trigger,
                run_id,
                provider: provider.to_string(),
                session_id,
                ..SessionReflectorAutomationOptions::default()
            },
            memory_curator: MemoryCuratorAutomationOptions {
                trigger,
                ..MemoryCuratorAutomationOptions::default()
            },
            skill_writer: SkillWriterAutomationOptions {
                trigger,
                provider: provider.to_string(),
                ..SkillWriterAutomationOptions::default()
            },
        },
    )
    .await
}

async fn hermes_receipt(args: &Value, profile_root: &Path, session_db: &GlobalDb) -> Result<Value> {
    let event: crate::daemon::DaemonHookEvent = serde_json::from_value(
        args.get("event")
            .cloned()
            .ok_or_else(|| config_error("missing required parameter `event`"))?,
    )?;
    let receipt = event
        .receipt
        .clone()
        .ok_or_else(|| config_error("Hermes event omitted receipt"))?;
    let dashboard_root = crate::automation::runner::user_automation_root(profile_root);
    match event.event.as_str() {
        "terminalReceipt" | "turnCompleted" => {
            crate::automation::host_receipts::record(&dashboard_root, event.route, receipt).await?;
            Ok(json!({ "action": "hermes_receipt", "status": "recorded" }))
        }
        "turnIngested" => {
            let watermark = receipt
                .transcript_watermark
                .as_deref()
                .ok_or_else(|| config_error("Hermes turnIngested omitted transcript watermark"))?;
            crate::automation::host_receipts::mark_turn_ingested(
                &dashboard_root,
                event.route,
                watermark,
            )
            .await?;
            let Some(ready) =
                crate::automation::host_receipts::oldest_ready(&dashboard_root).await?
            else {
                return Ok(json!({ "action": "hermes_receipt", "status": "ingested" }));
            };
            if session_db
                .lcm_load_raw_message("hermes", &ready.transcript_watermark)
                .await
                .is_none()
            {
                return Ok(json!({ "action": "hermes_receipt", "status": "awaiting_transcript" }));
            }
            if crate::automation::scheduler::load_scheduler_control(&dashboard_root)
                .await?
                .paused
            {
                return Ok(json!({ "action": "hermes_receipt", "status": "paused" }));
            }
            let session_id = ready
                .pending
                .route
                .as_ref()
                .and_then(|route| route.session_id.clone());
            let run = run_user_review(
                profile_root,
                "hermes",
                session_id,
                Some(format!("user_host_receipt_{}", ready.pending.generation)),
                crate::automation::run_ledger::AutomationTrigger::HostReceipt,
            )
            .await?;
            if run.session_reflector.ledger_record.status == AutomationRunStatus::Succeeded
                && run.memory_curator.ledger_record.status != AutomationRunStatus::Failed
                && run.skill_writer.ledger_record.status == AutomationRunStatus::Succeeded
            {
                crate::automation::host_receipts::mark_consumed(
                    &dashboard_root,
                    &ready.pending.session_key,
                    ready.pending.generation,
                )
                .await?;
            }
            Ok(json!({ "action": "hermes_receipt", "status": "reviewed" }))
        }
        other => Err(config_error(format!(
            "unsupported Hermes receipt event: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use tracedecay_domain::{
        CanonicalObservationIdV1, ObservationCollisionOutcomeV1, PayloadDigestV1,
    };
    use tracedecay_store::{ObservationStoreError, ProjectionStoreError};

    use super::*;
    use crate::application::observation::{
        CaptureClaudeObservationRequestError, ObservationApplicationError,
    };

    #[test]
    fn required_str_rejects_missing_and_empty_values() {
        assert!(required_str(&json!({}), "action").is_err());
        assert!(required_str(&json!({ "action": "" }), "action").is_err());
        assert_eq!(
            required_str(&json!({ "action": "reset_counter" }), "action").unwrap(),
            "reset_counter"
        );
    }

    #[test]
    fn projectless_runtime_rejects_project_database_actions() {
        assert!(!projectless_action_allowed("reset_counter", &json!({})));
        assert!(!projectless_action_allowed(
            "ingest_transcript",
            &json!({ "user_scope": false }),
        ));
        assert!(projectless_action_allowed(
            "ingest_transcript",
            &json!({ "user_scope": true }),
        ));
    }

    #[test]
    fn cursor_compaction_response_matches_hook_contract() {
        let value = cursor_compact_skipped("no messages to compact");
        let outcome: crate::hooks::CursorPreCompactOutcome = serde_json::from_value(value).unwrap();
        assert_eq!(outcome.status, "skipped");
        assert_eq!(outcome.reason, "no messages to compact");
        assert_eq!(outcome.summary_nodes_created, 0);
        assert!(outcome.summary_node_ids.is_empty());
    }

    #[test]
    fn session_authority_roles_fail_closed_independently() {
        let none = SessionAuthorities::default();
        assert!(required_project_db(none).is_err());
        assert!(required_user_db(none).is_err());
    }

    #[test]
    fn cursor_event_numbers_accept_numeric_and_string_forms() {
        let event = json!({ "tokens": "42", "message_count": 7 });
        assert_eq!(event_i64(&event, &["tokens"]), Some(42));
        assert_eq!(event_usize(&event, &["message_count"]), Some(7));
    }

    #[test]
    fn claude_observation_request_errors_are_bounded_hook_errors() {
        let error = ClaudeObservationIngestError::Request(
            CaptureClaudeObservationRequestError::SourceRangeMismatch,
        );
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation request is invalid"));
        assert!(!rendered.contains("source range"));
        let data = structured_hook_error_data(&mapped).unwrap();
        assert_eq!(data["reason_code"], "observation_request_invalid");
        assert_eq!(data["retryable"], false);
    }

    #[test]
    fn claude_observation_store_errors_keep_bounded_context_without_source_detail() {
        let error = ClaudeObservationIngestError::Store(ObservationStoreError::Storage {
            operation: "private store operation",
            source: Box::new(io::Error::other("private store source detail")),
        });
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation store operation failed"));
        assert!(!rendered.contains("private store operation"));
        assert!(!rendered.contains("private store source detail"));
        let data = structured_hook_error_data(&mapped).unwrap();
        assert_eq!(data["reason_code"], "observation_storage_failed");
        assert_eq!(data["retryable"], true);
    }

    #[test]
    fn claude_observation_application_store_errors_keep_bounded_context() {
        let error = ClaudeObservationIngestError::Application(ObservationApplicationError::Store(
            ObservationStoreError::Storage {
                operation: "private application store operation",
                source: Box::new(io::Error::other("private application store source detail")),
            },
        ));
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation application failed"));
        assert!(!rendered.contains("private application store operation"));
        assert!(!rendered.contains("private application store source detail"));
    }

    #[test]
    fn unavailable_persisted_observation_is_a_bounded_hook_error() {
        let error = ClaudeObservationIngestError::Application(
            ObservationApplicationError::PersistedObservationUnavailable,
        );
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation application failed"));
        assert!(!rendered.contains("persisted Claude observation"));
    }

    #[test]
    fn claude_observation_projection_errors_keep_bounded_context_without_source_detail() {
        let error = ClaudeObservationIngestError::Projection(ProjectionStoreError::Storage {
            operation: "private projection operation",
            source: Box::new(io::Error::other("private projection source detail")),
        });
        let mapped = map_claude_observation_ingest_error(&error);
        let rendered = mapped.to_string();

        assert!(rendered.contains("Claude observation projection failed"));
        assert!(!rendered.contains("private projection operation"));
        assert!(!rendered.contains("private projection source detail"));
    }

    #[test]
    fn claude_observation_failures_expose_stable_retry_contracts() {
        let cases = [
            (
                ClaudeObservationIngestError::Store(ObservationStoreError::CursorConflict {
                    expected: Box::new(None),
                    actual: Box::new(None),
                }),
                "observation_cursor_conflict",
                true,
            ),
            (
                ClaudeObservationIngestError::Store(ObservationStoreError::ObservationCollision {
                    observation_id: Box::new(
                        CanonicalObservationIdV1::new(format!("sha256:{}", "1".repeat(64)))
                            .unwrap(),
                    ),
                    existing_digest: Box::new(
                        PayloadDigestV1::new(format!("sha256:{}", "2".repeat(64))).unwrap(),
                    ),
                    candidate_digest: Box::new(
                        PayloadDigestV1::new(format!("sha256:{}", "3".repeat(64))).unwrap(),
                    ),
                    outcome: ObservationCollisionOutcomeV1::IdentityCollision,
                }),
                "observation_identity_collision",
                false,
            ),
            (
                ClaudeObservationIngestError::Store(
                    ObservationStoreError::SanitizationReceiptCollision,
                ),
                "sanitization_receipt_collision",
                false,
            ),
            (
                ClaudeObservationIngestError::Application(ObservationApplicationError::Cancelled),
                "observation_cancelled",
                true,
            ),
            (
                ClaudeObservationIngestError::Projection(ProjectionStoreError::Gap {
                    expected: 4,
                    actual: 6,
                }),
                "observation_projection_checkpoint_gap",
                false,
            ),
        ];

        for (error, reason_code, retryable) in cases {
            let mapped = map_claude_observation_ingest_error(&error);
            let data = structured_hook_error_data(&mapped).unwrap();
            assert_eq!(data["reason_code"], reason_code);
            assert_eq!(data["retryable"], retryable);
        }
    }

    #[test]
    fn transcript_hook_errors_keep_bounded_retry_data_without_cursor_detail() {
        let error = crate::sessions::source::TranscriptIngestError::CursorKeyMismatch {
            expected: "private expected cursor".to_string(),
            actual: "private actual cursor".to_string(),
        };
        let mapped = map_transcript_ingest_error(&error);
        let data = structured_hook_error_data(&mapped).unwrap();

        assert_eq!(data["reason_code"], "transcript_cursor_key_mismatch");
        assert_eq!(data["retryable"], false);
        let rendered = data.to_string();
        assert!(!rendered.contains("private expected cursor"));
        assert!(!rendered.contains("private actual cursor"));
    }
}
