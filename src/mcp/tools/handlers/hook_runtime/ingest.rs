use crate::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionScope,
    HostAdmissionStatus,
};
use crate::application::observation::ObservationCancellation;
use crate::automation::config_error;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::sessions::source::TranscriptSource;
use crate::tracedecay::TraceDecay;
use serde_json::{Value, json};
use std::path::Path;
use tracedecay_domain::{ObservationScopeV1, ProjectId};

use super::super::SessionAuthorities;

use super::errors::{map_claude_observation_ingest_error, map_transcript_ingest_error};
use super::{required_project_db, required_str};

mod kernels;

use kernels::{TranscriptCaptureContext, TranscriptCaptureOutcome, transcript_capture_kernel};

fn host_admission_facade<'a>(
    cg: Option<&TraceDecay>,
    scope: HostAdmissionScope,
    authorities: SessionAuthorities<'a>,
) -> Result<HostAdmissionFacade<'a>> {
    let authority = match scope {
        HostAdmissionScope::Project => match (
            authorities.project,
            authorities.profile_identity,
            authorities.project_registered,
        ) {
            (Some(_), Some(identity), registered) => {
                let project_id = project_observation_id(
                    cg.ok_or_else(|| config_error("project admission requires a project"))?,
                )?;
                match registered {
                    Some(registered) => HostAdmissionAuthorities::for_project(
                        identity.brain_id().clone(),
                        identity.profile_id().clone(),
                        project_id,
                        registered,
                    ),
                    None => HostAdmissionAuthorities::unavailable_for_project(
                        identity.brain_id().clone(),
                        identity.profile_id().clone(),
                        project_id,
                    ),
                }
            }
            (Some(_), None, _) | (None, _, _) => HostAdmissionAuthorities::default(),
        },
        HostAdmissionScope::Profile => match (
            authorities.user,
            authorities.profile_identity,
            authorities.profile_registered,
        ) {
            (Some(_), Some(identity), Some(registered)) => HostAdmissionAuthorities::for_profile(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
                registered,
            ),
            (Some(_), Some(identity), None) => HostAdmissionAuthorities::unavailable_for_profile(
                identity.brain_id().clone(),
                identity.profile_id().clone(),
            ),
            (Some(_), None, _) | (None, _, _) => HostAdmissionAuthorities::default(),
        },
    };
    Ok(HostAdmissionFacade::new(authority))
}

fn project_observation_id(cg: &TraceDecay) -> Result<ProjectId> {
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| config_error("project observation identity is unavailable"))?;
    ProjectId::new(project_id.to_string())
        .map_err(|_| config_error("project observation identity is invalid"))
}

/// Admits every Codex rollout that belongs to `project_root` under one shared
/// byte budget, reporting whether any source was left unfinished.
///
/// `max_new_bytes` is a budget for the whole pass, not an allowance per
/// rollout: spending it across sources is what keeps one large rollout from
/// silently consuming the cap and reporting the pass as complete.
async fn admit_codex_project_rollouts(
    admission: &HostAdmissionFacade<'_>,
    source: &crate::sessions::codex::CodexSource,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> Result<bool> {
    let mut budget = max_new_bytes;
    let mut deferred = false;
    let mut paths = source.transcript_paths(project_root).into_iter().peekable();
    while let Some(path) = paths.next() {
        let progress =
            crate::sessions::codex::try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
                &path,
                project_root,
                project_id.clone(),
                admission,
                budget,
                cancellation,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
        deferred |= progress.source_deferred;
        if let Some(remaining) = budget.as_mut() {
            *remaining = remaining.saturating_sub(progress.bytes_consumed);
            if *remaining == 0 {
                deferred |= paths.peek().is_some();
                break;
            }
        }
    }
    Ok(deferred)
}

async fn drain_host_observation_projections(
    admission: &HostAdmissionFacade<'_>,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> Result<u64> {
    let stats =
        crate::sessions::claude_observation::drain_projection_queue(admission, scope, cancellation)
            .await
            .map_err(|error| map_claude_observation_ingest_error(&error))?;
    Ok(stats.transcript.messages_upserted)
}

pub(super) async fn codex_compact(
    cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let event_json = required_str(args, "event_json")?;
    let db = required_project_db(session_authorities)?;
    if let Some(source) = crate::sessions::codex::CodexSource::new() {
        let project_id = project_observation_id(cg)?;
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission =
            host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
        for path in source.transcript_paths(cg.project_root()) {
            crate::sessions::codex::try_admit_codex_jsonl_observations_for_project_with_admission(
                &path,
                cg.project_root(),
                project_id.clone(),
                &admission,
                None,
            )
            .await
            .map_err(|error| map_transcript_ingest_error(&error))?;
        }
        let cancellation = ObservationCancellation::default();
        drain_host_observation_projections(&admission, &scope, &cancellation).await?;
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
    let published = db
        .publish_codex_compaction_summary_successor(
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
        "node_id": published.node_id,
        "predecessor_node_id": pending.node_id,
    }))
}

pub(super) async fn cursor_compact(
    cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let event_json = required_str(args, "event_json")?;
    let db = required_project_db(session_authorities)?;
    let project_id = project_observation_id(cg)?;
    let admission =
        host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
    let parsed: Value = serde_json::from_str(event_json)?;
    let session_id = ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error("Cursor preCompact event omitted session id"))?;
    let ingest = crate::sessions::cursor::try_ingest_cursor_transcript_event_capped_with_admission(
        event_json, project_id, &admission, None,
    )
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

pub(super) async fn accounting_receipt(
    cg: &TraceDecay,
    global_db: Option<&RegisteredGlobalDb>,
) -> Result<Value> {
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

pub(super) async fn ingest_transcript(
    cg: Option<&TraceDecay>,
    args: &Value,
    profile_root: Option<&Path>,
    global_db: Option<&RegisteredGlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let provider = required_str(args, "provider")?;
    let user_scope = args
        .get("user_scope")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_new_bytes = args.get("max_new_bytes").and_then(Value::as_u64);
    let admission_scope = if user_scope {
        HostAdmissionScope::Profile
    } else {
        HostAdmissionScope::Project
    };
    let facade = host_admission_facade(cg, admission_scope, session_authorities)?;
    let admission = facade.accept_replay(provider, admission_scope);
    match admission.status {
        HostAdmissionStatus::Unavailable => {
            let (reason_code, retryable) = match admission.reason_code {
                Some("project_authority_unbound" | "registered_authority_unavailable") => {
                    ("authority_unavailable", true)
                }
                reason_code => (
                    reason_code.unwrap_or("authority_unavailable"),
                    admission.retryable,
                ),
            };
            return Err(TraceDecayError::hook_runtime(
                reason_code,
                retryable,
                "daemon observation authority is unavailable",
            ));
        }
        HostAdmissionStatus::Unknown => {
            return Err(TraceDecayError::hook_runtime(
                admission.reason_code.unwrap_or("unknown_provider"),
                admission.retryable,
                "transcript provider is unsupported",
            ));
        }
        _ => {}
    }
    let cancellation = ObservationCancellation::default();
    // Unregistered routes are reported with the same typed `unknown_provider`
    // admission status the probe uses, not a generic configuration error.
    let kernel = transcript_capture_kernel(provider, user_scope).ok_or_else(|| {
        TraceDecayError::hook_runtime(
            "unknown_provider",
            false,
            "transcript provider is unsupported",
        )
    })?;
    let capture = kernel
        .capture(TranscriptCaptureContext {
            cg,
            args,
            profile_root,
            global_db,
            session_authorities,
            facade: &facade,
            max_new_bytes,
            cancellation: &cancellation,
        })
        .await?;
    let TranscriptCaptureOutcome {
        messages_upserted,
        snapshot: snapshot_capture,
        claude_observation: claude_observation_stats,
        source_deferred,
    } = capture;
    let authority_changed = messages_upserted > 0
        || snapshot_capture
            .as_ref()
            .is_some_and(|capture| capture.stats.messages_upserted > 0)
        || claude_observation_stats
            .as_ref()
            .is_some_and(|stats| stats.observations_committed > 0 || stats.cursor_advances > 0);
    let exact_duplicate = !authority_changed
        && claude_observation_stats
            .as_ref()
            .is_some_and(|stats| stats.observation_duplicates > 0 || stats.cursor_duplicates > 0);
    let deferred_by_byte_cap = source_deferred
        || snapshot_capture
            .as_ref()
            .is_some_and(|capture| capture.deferred_by_byte_cap);
    let admission = complete_ingest_admission(
        admission,
        authority_changed,
        exact_duplicate,
        deferred_by_byte_cap,
    );
    let mut output = json!({
        "action": "ingest_transcript",
        "provider": provider,
        "user_scope": user_scope,
        "completed": !deferred_by_byte_cap,
        "status": admission.status,
        "admission": admission,
        "messages_upserted": messages_upserted,
    });
    if let Some(capture) = snapshot_capture {
        output["observations_committed"] = json!(capture.stats.messages_upserted);
        output["bytes_consumed"] = json!(capture.bytes_consumed);
        output["deferred_by_byte_cap"] = json!(capture.deferred_by_byte_cap);
    }
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

pub(super) fn complete_ingest_admission(
    admission: HostAdmissionOutcome,
    authority_changed: bool,
    exact_duplicate: bool,
    deferred_by_byte_cap: bool,
) -> HostAdmissionOutcome {
    if deferred_by_byte_cap {
        HostAdmissionOutcome::retained_backpressured("ingest_pass_backpressured")
    } else if admission.status == HostAdmissionStatus::AcceptedForReplay {
        HostAdmissionOutcome::replay_completed(authority_changed, exact_duplicate)
    } else {
        admission
    }
}

#[cfg(test)]
mod tests;
