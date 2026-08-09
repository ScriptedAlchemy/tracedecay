use crate::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionScope,
    HostAdmissionStatus,
};
use crate::application::observation::ObservationCancellation;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tracedecay_agent_hosts::automation::config_error;
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_sessions::runtime::source::TranscriptSource;
use tracedecay_usecases::session::lcm::{
    LcmAuthorityOutcome, LcmAuthorityPayload, LcmAuthorityRequest, LcmAuthorityUnavailableReason,
    LcmCompactionCommand, LcmCompressionEvidence, LcmHostProtocol, LcmTranscriptIngestCommand,
};

use super::super::SessionAuthorities;

use super::errors::{map_claude_observation_ingest_error, map_transcript_ingest_error};
use super::required_str;

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
    source: &tracedecay_sessions::runtime::codex::CodexSource,
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
            tracedecay_sessions::runtime::codex::try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
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
    let stats = tracedecay_sessions::runtime::claude_observation::drain_projection_queue(
        admission,
        scope,
        cancellation,
    )
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
    let Some(authority) = session_authorities.project_lcm else {
        return Ok(compaction_authority_unavailable("codex_compact"));
    };
    let parsed = serde_json::from_str::<Value>(event_json).ok();
    let session_id = parsed.as_ref().and_then(|value| {
        ["session_id", "conversation_id", "thread_id"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .map(str::to_string)
    });
    let Some(session_id) = session_id else {
        return Ok(json!({
            "action": "codex_compact",
            "status": "unavailable",
            "reason": "host_session_identity_unavailable",
            "messages_upserted": 0,
        }));
    };
    // The daemon compaction compresses the session's durable history, so the
    // rollout must land in the owning store through the canonical ingest
    // route before pressure evidence is evaluated; compacting an unfilled
    // store would report an empty success.
    let messages_upserted = admit_codex_rollouts_for_compaction(cg, session_authorities).await?;
    let current_tokens = parsed
        .as_ref()
        .and_then(|event| event_i64(event, &["context_tokens", "current_tokens", "tokens"]));
    let context_length = parsed
        .as_ref()
        .and_then(|event| event_i64(event, &["context_window_size", "context_length"]));
    let Some(response) = authority
        .execute(pressure_only_command(
            "codex",
            &session_id,
            current_tokens,
            context_length,
            None,
            None,
            LcmHostProtocol::CodexContextCompacted {
                protocol_revision: "codex.context-compacted.v1".to_owned(),
                event_digest: tracedecay_domain::canonical_sha256(&event_json)
                    .map_err(|error| config_error(format!("digest Codex event failed: {error}")))?,
            },
        ))
        .await
    else {
        return Ok(compaction_authority_unavailable("codex_compact"));
    };
    let mut output = compaction_response_json("codex_compact", &response);
    output["messages_upserted"] = json!(messages_upserted);
    Ok(output)
}

/// The project-open catch-up scans the same Codex sources concurrently, so a
/// compaction-time pass tolerates a bounded window of cursor CAS losses and
/// still-mounting write authorities before treating the ingest as failed.
const COMPACTION_INGEST_ATTEMPTS: usize = 10;
const COMPACTION_INGEST_RETRY_DELAY: Duration = Duration::from_millis(400);

/// Typed reasons that mean a peer ingestor or the project-open sequence is
/// advancing the same durable state: the source cursor CAS rejected this
/// pass, the shared writer was saturated, or a write authority was still
/// mounting. Each converges once the peer pass settles, so the bounded retry
/// re-reads rather than surfacing a terminal failure.
const COMPACTION_INGEST_CONVERGING_REASONS: &[&str] = &[
    "cursor_conflict",
    "authority_write_failed",
    "authority_unavailable",
    "external_source_runtime_unavailable",
    "external_source_commit_failed",
];

/// Lands the project's Codex rollouts in the owning store through the
/// canonical ingest route ahead of a compaction, reporting the exact upserted
/// message count. Admission failures stay typed instead of letting the
/// compaction run against missing history.
///
/// Retries are bounded to typed retryable classifications: the project-open
/// codex catch-up advances the same source cursors, so a CAS conflict (or the
/// open sequence still mounting its write authorities) is peer progress this
/// pass re-reads on the next attempt, never a terminal state.
async fn admit_codex_rollouts_for_compaction(
    cg: &TraceDecay,
    session_authorities: SessionAuthorities<'_>,
) -> Result<u64> {
    let mut last_retryable = None;
    for attempt in 0..COMPACTION_INGEST_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(COMPACTION_INGEST_RETRY_DELAY).await;
        }
        match admit_codex_rollouts_once(cg, session_authorities).await {
            Ok(messages_upserted) => return Ok(messages_upserted),
            Err(error) => {
                let converging =
                    error
                        .hook_runtime_context()
                        .is_some_and(|(reason, retryable, _)| {
                            retryable || COMPACTION_INGEST_CONVERGING_REASONS.contains(&reason)
                        });
                if !converging {
                    return Err(error);
                }
                last_retryable = Some(error);
            }
        }
    }
    Err(last_retryable.unwrap_or_else(|| {
        config_error("codex compaction ingest kept racing past its retry budget")
    }))
}

async fn admit_codex_rollouts_once(
    cg: &TraceDecay,
    session_authorities: SessionAuthorities<'_>,
) -> Result<u64> {
    let facade = host_admission_facade(Some(cg), HostAdmissionScope::Project, session_authorities)?;
    let admission = facade.accept_replay("codex", HostAdmissionScope::Project);
    match admission.status {
        HostAdmissionStatus::Unavailable => {
            return Err(TraceDecayError::hook_runtime(
                admission.reason_code.unwrap_or("authority_unavailable"),
                true,
                "daemon observation authority is unavailable for compaction ingest",
            ));
        }
        HostAdmissionStatus::Unknown => {
            return Err(TraceDecayError::hook_runtime(
                admission.reason_code.unwrap_or("unknown_provider"),
                admission.retryable,
                "codex transcript provider is unsupported",
            ));
        }
        _ => {}
    }
    let source = tracedecay_sessions::runtime::codex::CodexSource::new()
        .ok_or_else(|| config_error("Codex transcript source is unavailable"))?;
    let project_id = project_observation_id(cg)?;
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let cancellation = ObservationCancellation::default();
    admit_codex_project_rollouts(
        &facade,
        &source,
        cg.project_root(),
        project_id,
        None,
        &cancellation,
    )
    .await?;
    drain_host_observation_projections(&facade, &scope, &cancellation).await
}

pub(super) async fn claude_compact(
    args: &Value,
    _session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    required_str(args, "event_json")?;
    Ok(json!({
        "action": "claude_compact",
        "status": "unavailable",
        "reason": "claude_postcompact_provenance_unavailable",
        "summary_nodes_created": 0,
        "summary_node_ids": [],
    }))
}

pub(super) async fn cursor_compact(
    _cg: &TraceDecay,
    args: &Value,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let event_json = required_str(args, "event_json")?;
    let Some(authority) = session_authorities.project_lcm else {
        return Ok(compaction_authority_unavailable("cursor_compact"));
    };
    let parsed: Value = serde_json::from_str(event_json)?;
    let session_id = ["session_id", "conversation_id", "chat_id"]
        .iter()
        .find_map(|key| parsed.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| config_error("Cursor preCompact event omitted session id"))?;
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
    let Some(response) = authority
        .execute(pressure_only_command(
            "cursor",
            session_id,
            current_tokens,
            context_length,
            messages_to_compact,
            fresh_tail_count,
            LcmHostProtocol::CursorPreCompact {
                protocol_revision: "cursor.precompact.v1".to_owned(),
                event_digest: tracedecay_domain::canonical_sha256(&event_json).map_err(
                    |error| config_error(format!("digest Cursor event failed: {error}")),
                )?,
            },
        ))
        .await
    else {
        return Ok(compaction_authority_unavailable("cursor_compact"));
    };
    Ok(compaction_response_json("cursor_compact", &response))
}

fn compaction_authority_unavailable(action: &str) -> Value {
    json!({
        "action": action,
        "status": "unavailable",
        "reason": "lcm_daemon_authority_unavailable",
        "summary_nodes_created": 0,
        "summary_node_ids": [],
    })
}

fn compaction_response_json(
    action: &str,
    response: &tracedecay_usecases::session::lcm::LcmAuthorityResponse,
) -> Value {
    if let (LcmAuthorityOutcome::Ready, Some(LcmAuthorityPayload::Compaction(compression))) =
        (&response.outcome, &response.payload)
    {
        return json!({
            "action": action,
            "status": compression.status,
            "reason": compression.reason,
            "summary_nodes_created": compression.summary_nodes_created,
            "summary_node_ids": compression
                .summary_nodes
                .iter()
                .map(|node| node.node_id.clone())
                .collect::<Vec<_>>(),
            "relation_projection_status": compression.relation_projection_status,
            "retry_status": compression.retry_status,
            "authority_outcome": response.outcome,
            "committed_state": response.receipt.committed_state,
            "messages_upserted": 0,
        });
    }
    json!({
        "action": action,
        "status": "unavailable",
        "reason": compaction_unavailable_reason(&response.outcome),
        "authority_outcome": response.outcome,
        "committed_state": response.receipt.committed_state,
        "summary_nodes_created": 0,
        "summary_node_ids": [],
        "messages_upserted": 0,
    })
}

fn compaction_unavailable_reason(outcome: &LcmAuthorityOutcome) -> &'static str {
    if matches!(
        outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::HostPayloadUnavailable
        }
    ) {
        "host_payload_unavailable"
    } else {
        "lcm_daemon_authority_rejected"
    }
}

fn cursor_compact_skipped(reason: impl Into<String>) -> Value {
    json!({
        "status": "skipped",
        "reason": reason.into(),
        "summary_nodes_created": 0,
        "summary_node_ids": [],
        "relation_projection_status": "not_applicable",
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

fn pressure_only_command(
    provider: &str,
    session_id: &str,
    current_tokens: Option<i64>,
    context_length: Option<i64>,
    max_source_messages: Option<usize>,
    fresh_tail_count: Option<usize>,
    protocol: LcmHostProtocol,
) -> LcmAuthorityRequest {
    LcmAuthorityRequest::Compact(LcmCompactionCommand {
        preflight: tracedecay_sessions::runtime::lcm::LcmPreflightRequest {
            provider: provider.to_owned(),
            session_id: session_id.to_string(),
            messages: Vec::new(),
            current_tokens,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
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
        },
        evidence: LcmCompressionEvidence::PressureOnly { protocol },
    })
}

pub(super) async fn accounting_receipt(
    cg: &TraceDecay,
    provider_usage_db: &RegisteredGlobalDb,
) -> Result<Value> {
    let scope = ObservationScopeV1::Project {
        project_id: project_observation_id(cg)?,
    };
    let usage = crate::application::provider_usage::provider_usage_aggregate(
        provider_usage_db,
        &scope,
        None,
        None,
    )
    .await;
    let prices = crate::application::provider_pricing::load_table();
    let priced = crate::application::provider_usage::price_provider_usage(&usage, prices, 0);
    let complete =
        priced.coverage == crate::application::provider_usage::ProviderUsageCoverageV1::Complete;
    let tokens_consumed = complete
        .then(|| {
            priced
                .total_input_tokens?
                .checked_add(priced.total_output_tokens?)
        })
        .flatten();
    let tokens_saved = cg
        .get_tokens_saved()
        .await
        .map_err(|error| config_error(format!("failed to read saved tokens: {error}")))?;
    let efficiency = tokens_consumed.and_then(|consumed| {
        let denominator = tokens_saved.checked_add(consumed)?;
        (denominator > 0).then_some((tokens_saved as f64 / denominator as f64) * 100.0)
    });
    Ok(json!({
        "action": "accounting_receipt",
        "coverage": priced.coverage,
        "watermark": usage.upper_observation_sequence,
        "provider_usage_events": priced.usage_events,
        "cost_usd": priced.total_cost_usd,
        "pricing_status": if priced.total_cost_usd.is_some() { "priced" } else { "unavailable" },
        "pricing_revision": priced.pricing_revision,
        "tokens_consumed": tokens_consumed,
        "tokens_saved": tokens_saved,
        "efficiency": efficiency,
    }))
}

pub(super) async fn ingest_transcript(
    cg: Option<&TraceDecay>,
    args: &Value,
    profile_root: Option<&Path>,
    global_db: Option<&RegisteredGlobalDb>,
    accounting_db: Option<&RegisteredGlobalDb>,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let cancellation = ObservationCancellation::default();
    ingest_transcript_with_cancellation(
        cg,
        args,
        profile_root,
        global_db,
        accounting_db,
        session_authorities,
        &cancellation,
    )
    .await
}

pub(crate) async fn ingest_transcript_with_cancellation(
    cg: Option<&TraceDecay>,
    args: &Value,
    profile_root: Option<&Path>,
    global_db: Option<&RegisteredGlobalDb>,
    accounting_db: Option<&RegisteredGlobalDb>,
    session_authorities: SessionAuthorities<'_>,
    cancellation: &ObservationCancellation,
) -> Result<Value> {
    let provider = required_str(args, "provider")?;
    let user_scope = args
        .get("user_scope")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if provider == "hermes" && args.get("messages").is_some() {
        return ingest_hermes_callback_turn(args, user_scope, session_authorities).await;
    }
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
            cancellation,
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
    // Project-scope ingest is the production moment new post-hint session
    // activity becomes durable, so settle emitted hook hints into
    // `hint_outcome` analytics events here. Best-effort: unavailable or
    // failed settlement is a typed field on the output, never an ingest
    // failure.
    if let Some(cg) = cg
        && !user_scope
    {
        let settlement = crate::application::hint_outcomes::settle_project_hint_outcomes(
            accounting_db,
            session_authorities.project.map(std::sync::Arc::as_ref),
            crate::analytics_bridge::hook_import_sources(Some(cg.project_root())),
            cg.project_root(),
            crate::tracedecay::current_timestamp(),
        )
        .await;
        output["hint_outcomes"] = settlement.as_json();
    }
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

async fn ingest_hermes_callback_turn(
    args: &Value,
    user_scope: bool,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let session_id = required_str(args, "session_id")?;
    let messages = args
        .get("messages")
        .and_then(Value::as_array)
        .filter(|messages| !messages.is_empty())
        .ok_or_else(|| config_error("Hermes turn callback requires non-empty messages"))?
        .clone();
    let authority = if user_scope {
        session_authorities.profile_lcm
    } else {
        session_authorities.project_lcm
    };
    let Some(authority) = authority else {
        return Ok(json!({
            "action": "ingest_transcript",
            "provider": "hermes",
            "user_scope": user_scope,
            "status": "unavailable",
            "reason": "lcm_daemon_authority_unavailable",
        }));
    };
    let event_digest = tracedecay_domain::canonical_sha256(&(&"hermes", &session_id, &messages))
        .map_err(|error| config_error(format!("digest Hermes turn failed: {error}")))?;
    let request = LcmAuthorityRequest::Ingest(LcmTranscriptIngestCommand {
        preflight: tracedecay_sessions::runtime::lcm::LcmPreflightRequest {
            provider: "hermes".to_owned(),
            session_id: session_id.to_owned(),
            messages,
            current_tokens: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
        },
        protocol_revision: "hermes.turn-completed.v1".to_owned(),
        event_digest,
    });
    let Some(response) = authority.execute(request).await else {
        return Ok(json!({
            "action": "ingest_transcript",
            "provider": "hermes",
            "user_scope": user_scope,
            "status": "unavailable",
            "reason": "lcm_daemon_authority_unavailable",
        }));
    };
    let status = if response.outcome == LcmAuthorityOutcome::Ready
        && matches!(response.payload, Some(LcmAuthorityPayload::Ingest(_)))
    {
        "committed"
    } else {
        "unavailable"
    };
    Ok(json!({
        "action": "ingest_transcript",
        "provider": "hermes",
        "user_scope": user_scope,
        "status": status,
        "authority_outcome": response.outcome,
        "committed_state": response.receipt.committed_state,
    }))
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
