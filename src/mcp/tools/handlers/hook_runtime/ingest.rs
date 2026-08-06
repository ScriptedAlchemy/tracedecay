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
use super::required_str;

mod kernels;

use kernels::{TranscriptCaptureContext, TranscriptCaptureOutcome, transcript_capture_kernel};

const HOST_COMPACTION_ADMISSION_MAX_BYTES: u64 = 1024 * 1024;

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

struct ClaudeCompactEvent {
    session_id: String,
    compact_summary: String,
    current_tokens: Option<i64>,
    context_length: Option<i64>,
}

fn parse_claude_compact_event(args: &Value) -> Result<ClaudeCompactEvent> {
    if required_str(args, "provider")? != "claude" {
        return Err(config_error(
            "Claude compact action requires provider `claude`",
        ));
    }
    let event_json = required_str(args, "event_json")?;
    let event: Value = serde_json::from_str(event_json)
        .map_err(|_| config_error("Claude compact event is not valid JSON"))?;
    if event.get("hook_event_name").and_then(Value::as_str) != Some("PostCompact") {
        return Err(config_error(
            "Claude compact action requires a PostCompact event",
        ));
    }
    let event_summary = event
        .get("compact_summary")
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
        .ok_or_else(|| config_error("Claude PostCompact event omitted its native summary"))?;
    let requested_summary = required_str(args, "compact_summary")?;
    if event_summary != requested_summary {
        return Err(config_error(
            "Claude compact summary does not exactly match the native event",
        ));
    }
    let session_id = event
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| config_error("Claude PostCompact event omitted session id"))?;
    Ok(ClaudeCompactEvent {
        session_id: session_id.to_owned(),
        compact_summary: event_summary.to_owned(),
        current_tokens: event_i64(&event, &["context_tokens", "current_tokens", "tokens"]),
        context_length: event_i64(&event, &["context_window_size", "context_length"]),
    })
}

pub(super) async fn claude_compact(
    cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    require_project_compaction_scope(args, "claude")?;
    let event = parse_claude_compact_event(args)?;
    let db = session_authorities
        .project
        .ok_or_else(|| config_error("daemon project session database is unavailable"))?;
    let event_json = required_str(args, "event_json")?;
    crate::daemon::lcm_host_effects::enqueue_lcm_host_effect(
        db,
        crate::daemon::lcm_host_effects::LcmHostEffectAdmission {
            provider: "claude",
            session_id: &event.session_id,
            event_json,
            compact_summary: Some(&event.compact_summary),
            current_tokens: event.current_tokens,
            context_length: event.context_length,
            max_source_messages: None,
            fresh_tail_count: None,
            source_ready: false,
        },
    )
    .await?;
    let project_id = project_observation_id(cg)?;
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let admission =
        host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
    let source = crate::sessions::claude::ClaudeSource::new()
        .ok_or_else(|| config_error("Claude transcript source is unavailable"))?;
    let cancellation = ObservationCancellation::default();
    let ingest =
        crate::sessions::claude_observation::ingest_source_with_observations_with_admission(
            &source,
            cg.project_root(),
            scope,
            &admission,
            Some(HOST_COMPACTION_ADMISSION_MAX_BYTES),
            cancellation,
        )
        .await
        .map_err(|error| map_claude_observation_ingest_error(&error))?;
    let source_ready = ingest.deferred_sources == 0;

    let receipt = crate::daemon::lcm_host_effects::enqueue_lcm_host_effect(
        db,
        crate::daemon::lcm_host_effects::LcmHostEffectAdmission {
            provider: "claude",
            session_id: &event.session_id,
            event_json,
            compact_summary: Some(&event.compact_summary),
            current_tokens: event.current_tokens,
            context_length: event.context_length,
            max_source_messages: None,
            fresh_tail_count: None,
            source_ready,
        },
    )
    .await?;
    Ok(json!({
        "action": "claude_compact",
        "provider": "claude",
        "event_id": receipt.event_id,
        "event_digest": receipt.event_digest,
        "status": receipt.status,
        "reason": receipt.reason,
        "retryable": receipt.retryable,
        "summary_node_ids": receipt.summary_node_ids,
        "relation_projection_status": "pending",
        "messages_upserted": ingest.transcript.messages_upserted,
        "observations_committed": ingest.observations_committed,
        "source_deferred": !source_ready,
    }))
}

pub(super) async fn codex_compact(
    cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    require_project_compaction_scope(args, "codex")?;
    let event_json = required_str(args, "event_json")?;
    let db = session_authorities
        .project
        .ok_or_else(|| config_error("daemon project session database is unavailable"))?;
    let parsed: Value = serde_json::from_str(event_json)?;
    let session_id = ["session_id", "conversation_id", "thread_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error("Codex compact event omitted session id"))?;
    let current_tokens = event_i64(&parsed, &["context_tokens", "current_tokens", "tokens"]);
    let context_length = event_i64(&parsed, &["context_window_size", "context_length"]);
    crate::daemon::lcm_host_effects::enqueue_lcm_host_effect(
        db,
        crate::daemon::lcm_host_effects::LcmHostEffectAdmission {
            provider: "codex",
            session_id,
            event_json,
            compact_summary: None,
            current_tokens,
            context_length,
            max_source_messages: None,
            fresh_tail_count: None,
            source_ready: false,
        },
    )
    .await?;
    let mut messages_upserted = 0_u64;
    let mut source_ready = false;
    if let Some(source) = crate::sessions::codex::CodexSource::new() {
        let project_id = project_observation_id(cg)?;
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission =
            host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
        let cancellation = ObservationCancellation::default();
        let source_deferred = admit_codex_project_rollouts(
            &admission,
            &source,
            cg.project_root(),
            project_id,
            Some(HOST_COMPACTION_ADMISSION_MAX_BYTES),
            &cancellation,
        )
        .await?;
        source_ready = !source_deferred;
        messages_upserted =
            drain_host_observation_projections(&admission, &scope, &cancellation).await?;
    }
    let receipt = crate::daemon::lcm_host_effects::enqueue_lcm_host_effect(
        db,
        crate::daemon::lcm_host_effects::LcmHostEffectAdmission {
            provider: "codex",
            session_id,
            event_json,
            compact_summary: None,
            current_tokens,
            context_length,
            max_source_messages: None,
            fresh_tail_count: None,
            source_ready,
        },
    )
    .await?;
    Ok(json!({
        "action": "codex_compact",
        "provider": "codex",
        "event_id": receipt.event_id,
        "event_digest": receipt.event_digest,
        "status": receipt.status,
        "reason": receipt.reason,
        "retryable": receipt.retryable,
        "summary_node_ids": receipt.summary_node_ids,
        "relation_projection_status": "pending",
        "messages_upserted": messages_upserted,
        "source_deferred": !source_ready,
    }))
}

pub(super) async fn cursor_compact(
    cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    require_project_compaction_scope(args, "cursor")?;
    let event_json = required_str(args, "event_json")?;
    let db = session_authorities
        .project
        .ok_or_else(|| config_error("daemon project session database is unavailable"))?;
    let project_id = project_observation_id(cg)?;
    let admission =
        host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
    let parsed: Value = serde_json::from_str(event_json)?;
    let session_id = ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error("Cursor preCompact event omitted session id"))?;
    let messages_to_compact = event_usize(&parsed, &["messages_to_compact", "compact_count"]);
    let message_count = event_usize(&parsed, &["message_count", "messages_count"]);
    let fresh_tail_count = message_count
        .zip(messages_to_compact)
        .map(|(count, compact)| count.saturating_sub(compact));
    let current_tokens = event_i64(&parsed, &["context_tokens", "current_tokens", "tokens"]);
    let context_length = event_i64(&parsed, &["context_window_size", "context_length"]);
    crate::daemon::lcm_host_effects::enqueue_lcm_host_effect(
        db,
        crate::daemon::lcm_host_effects::LcmHostEffectAdmission {
            provider: "cursor",
            session_id,
            event_json,
            compact_summary: None,
            current_tokens,
            context_length,
            max_source_messages: messages_to_compact,
            fresh_tail_count,
            source_ready: false,
        },
    )
    .await?;
    let ingest = crate::sessions::cursor::try_ingest_cursor_transcript_event_capped_with_admission(
        event_json,
        project_id,
        &admission,
        Some(HOST_COMPACTION_ADMISSION_MAX_BYTES),
    )
    .await
    .map_err(|error| map_transcript_ingest_error(&error))?;
    let receipt = crate::daemon::lcm_host_effects::enqueue_lcm_host_effect(
        db,
        crate::daemon::lcm_host_effects::LcmHostEffectAdmission {
            provider: "cursor",
            session_id,
            event_json,
            compact_summary: None,
            current_tokens,
            context_length,
            max_source_messages: messages_to_compact,
            fresh_tail_count,
            source_ready: !ingest.source_deferred,
        },
    )
    .await?;
    Ok(json!({
        "action": "cursor_compact",
        "provider": "cursor",
        "event_id": receipt.event_id,
        "event_digest": receipt.event_digest,
        "status": receipt.status,
        "reason": receipt.reason,
        "retryable": receipt.retryable,
        "summary_node_ids": receipt.summary_node_ids,
        "relation_projection_status": "pending",
        "messages_upserted": ingest.messages_upserted,
        "source_deferred": ingest.source_deferred,
    }))
}

fn require_project_compaction_scope(args: &Value, provider: &str) -> Result<()> {
    if required_str(args, "provider")? != provider {
        return Err(config_error(
            "project host compaction action does not match provider",
        ));
    }
    if args.get("user_scope").and_then(Value::as_bool) == Some(true) {
        return Err(config_error(
            "project host compaction cannot use profile scope",
        ));
    }
    Ok(())
}

pub(super) async fn profile_host_compact(
    args: &Value,
    profile_root: &Path,
    global_db: &RegisteredGlobalDb,
    session_authorities: SessionAuthorities<'_>,
    action: &str,
) -> Result<Value> {
    if args.get("user_scope").and_then(Value::as_bool) != Some(true) {
        return Err(config_error(
            "profile host compaction requires user_scope `true`",
        ));
    }
    let provider = required_str(args, "provider")?;
    let expected_provider = action
        .strip_suffix("_compact")
        .ok_or_else(|| config_error("invalid profile host compaction action"))?;
    if provider != expected_provider {
        return Err(config_error(
            "profile host compaction action does not match provider",
        ));
    }
    let event_json = required_str(args, "event_json")?;
    let parsed: Value = serde_json::from_str(event_json)
        .map_err(|_| config_error("profile host compaction event is not valid JSON"))?;
    let session_id = required_str(args, "session_id")?;
    let native_session_id = ["session_id", "conversation_id", "thread_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str));
    if native_session_id.is_some_and(|native| native != session_id) {
        return Err(config_error(
            "profile host compaction session does not match the native event",
        ));
    }
    let compact_summary = if provider == "claude" {
        let parsed_claude = parse_claude_compact_event(args)?;
        if parsed_claude.session_id != session_id {
            return Err(config_error(
                "profile Claude compaction session does not match the native event",
            ));
        }
        Some(parsed_claude.compact_summary)
    } else {
        None
    };
    let mut bounded_args = args.clone();
    bounded_args["max_new_bytes"] = json!(HOST_COMPACTION_ADMISSION_MAX_BYTES);
    let ingest = ingest_transcript(
        None,
        &bounded_args,
        Some(profile_root),
        Some(global_db),
        session_authorities,
    )
    .await?;
    let messages_upserted = ingest
        .get("messages_upserted")
        .and_then(Value::as_u64)
        .ok_or_else(|| config_error("profile transcript admission omitted its message count"))?;
    let source_ready = ingest.get("completed").and_then(Value::as_bool).ok_or_else(|| {
        config_error("profile transcript admission omitted its completion state")
    })?;
    let messages_to_compact = (provider == "cursor")
        .then(|| event_usize(&parsed, &["messages_to_compact", "compact_count"]))
        .flatten();
    let message_count = event_usize(&parsed, &["message_count", "messages_count"]);
    let fresh_tail_count = message_count
        .zip(messages_to_compact)
        .map(|(count, compact)| count.saturating_sub(compact));
    let user_db = session_authorities
        .user
        .ok_or_else(|| config_error("daemon user session database is unavailable"))?;
    let receipt = crate::daemon::lcm_host_effects::enqueue_lcm_host_effect(
        user_db,
        crate::daemon::lcm_host_effects::LcmHostEffectAdmission {
            provider,
            session_id,
            event_json,
            compact_summary: compact_summary.as_deref(),
            current_tokens: event_i64(
                &parsed,
                &["context_tokens", "current_tokens", "tokens"],
            ),
            context_length: event_i64(
                &parsed,
                &["context_window_size", "context_length"],
            ),
            max_source_messages: messages_to_compact,
            fresh_tail_count,
            source_ready,
        },
    )
    .await?;
    Ok(json!({
        "action": action,
        "provider": provider,
        "user_scope": true,
        "event_id": receipt.event_id,
        "event_digest": receipt.event_digest,
        "status": receipt.status,
        "reason": receipt.reason,
        "retryable": receipt.retryable,
        "summary_node_ids": receipt.summary_node_ids,
        "relation_projection_status": "pending",
        "messages_upserted": messages_upserted,
        "source_deferred": !source_ready,
    }))
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
