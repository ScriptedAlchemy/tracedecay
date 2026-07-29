//! Codex rollout observation admission and canonical-envelope normalization.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1,
    CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1,
    CanonicalWorkflowEvidenceKindV1, CanonicalWorkflowSemanticKindV1, ObservationId,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProjectId,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use super::PROVIDER;
use super::context::CodexContextState;
use super::events;
use super::meta::{nested_string_field, session_meta_with_provenance, string_field};
use super::records::{response_item_tool_name, timestamp_from_record};
use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::application::observation::ObservationCancellation;
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::sessions::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionRequest, PersistedCursorUpdate,
    admit_jsonl_observations, canonical_message_role, canonical_native_observation_id,
    canonical_u64,
};
use crate::sessions::shared::path_belongs_to_project;
use crate::sessions::source::{TranscriptIngestError, TranscriptIngestResult};

const CODEX_OBSERVATION_RETENTION: &str = "retention.provider-observation";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CodexJsonlAdmissionProgress {
    pub bytes_consumed: u64,
    pub source_deferred: bool,
}

/// Admit a Codex rollout for one exact project identity.
///
/// The scheduler supplies the already-resolved project id; each complete record
/// is routed by the rollout's current Codex cwd, including context reconstructed
/// before a resumed byte cursor.
pub async fn try_admit_codex_jsonl_observations_for_project(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let admission = HostAdmissionFacade::new(HostAdmissionAuthorities::unregistered_for_project(
        project_id.clone(),
    ));
    try_admit_codex_jsonl_observations_for_project_with_admission(
        path,
        project_root,
        project_id,
        &admission,
        max_new_bytes,
    )
    .await
}

/// Project admission with authority prepared by the caller.
///
/// The project scheduler constructs this facade from its authoritative project
/// identity and may attach additional admission evidence before source routing.
pub async fn try_admit_codex_jsonl_observations_for_project_with_admission(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    admission: &HostAdmissionFacade<'_>,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
        path,
        project_root,
        project_id,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
    path: &Path,
    project_root: &Path,
    project_id: ProjectId,
    admission: &HostAdmissionFacade<'_>,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        CodexObservationAdmission::Project {
            root: project_root,
            project_id,
        },
        admission,
        max_new_bytes,
        cancellation,
    )
    .await
}

/// Admit Codex records that are not attributable to any registered project.
///
/// A scheduler may constrain this pass to one session while it catches up a
/// profile-owned rollout.
pub async fn try_admit_codex_jsonl_observations_for_profile(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let admission = HostAdmissionFacade::new(HostAdmissionAuthorities::unregistered_for_profile());
    try_admit_codex_jsonl_observations_for_profile_with_admission(
        path,
        session_id,
        registered_roots,
        &admission,
        max_new_bytes,
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_profile_with_admission(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    admission: &HostAdmissionFacade<'_>,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
        path,
        session_id,
        registered_roots,
        admission,
        max_new_bytes,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
    path: &Path,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    admission: &HostAdmissionFacade<'_>,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        CodexObservationAdmission::Profile {
            session_id,
            registered_roots,
        },
        admission,
        max_new_bytes,
        cancellation,
    )
    .await
}

pub(crate) enum CodexObservationAdmission<'a> {
    Project {
        root: &'a Path,
        project_id: ProjectId,
    },
    Profile {
        session_id: Option<&'a str>,
        registered_roots: &'a [PathBuf],
    },
}

impl CodexObservationAdmission<'_> {
    pub(crate) fn scope(&self) -> ObservationScopeV1 {
        match self {
            Self::Project { project_id, .. } => ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
            Self::Profile { .. } => ObservationScopeV1::Profile,
        }
    }

    pub(crate) fn accepts(&self, cwd: Option<&Path>) -> bool {
        match self {
            Self::Project { root, .. } => cwd.is_some_and(|cwd| path_belongs_to_project(cwd, root)),
            Self::Profile {
                registered_roots, ..
            } => cwd.is_none_or(|cwd| {
                !registered_roots
                    .iter()
                    .any(|root| path_belongs_to_project(cwd, root))
            }),
        }
    }

    pub(crate) fn accepts_session(&self, session_id: &str) -> bool {
        !matches!(self, Self::Profile { session_id: Some(expected), .. } if *expected != session_id)
    }

    fn projection_project_path<'b>(&'b self, cwd: Option<&'b Path>) -> Option<&'b Path> {
        match self {
            Self::Project { root, .. } => Some(root),
            Self::Profile { .. } => cwd,
        }
    }
}

async fn try_admit_codex_jsonl_observations(
    path: &Path,
    admission_scope: CodexObservationAdmission<'_>,
    admission: &HostAdmissionFacade<'_>,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    if cancellation.is_cancelled() {
        return Ok(CodexJsonlAdmissionProgress {
            bytes_consumed: 0,
            source_deferred: true,
        });
    }
    let parsed_meta = session_meta_with_provenance(path).ok_or_else(|| {
        TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        }
    })?;
    let native_thread_id = parsed_meta.native_thread_id;
    let meta = parsed_meta.meta;
    if !admission_scope.accepts_session(&meta.session_id) {
        return Ok(CodexJsonlAdmissionProgress::default());
    }
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)?,
        SessionId::new(meta.session_id.clone())?,
    )?;
    let scope = admission_scope.scope();
    let request = JsonlObservationAdmissionRequest::new(
        PROVIDER,
        path,
        admission,
        source,
        scope,
        RetentionClass::new(CODEX_OBSERVATION_RETENTION)?,
    )
    .with_max_new_bytes(max_new_bytes)
    .with_persisted_cursor_update(PersistedCursorUpdate::Replace)
    .with_cancellation(cancellation.clone());
    let progress = admit_jsonl_observations(
        request,
        |scan| {
            if scan.resumed {
                CodexContextState::scan_prior(path, scan.start_offset, &meta)
            } else {
                CodexContextState::from_meta(&meta)
            }
        },
        |context_state, bytes, range, _| {
            let mut stable_record_id = None;
            let mut non_durable_reason = None;
            let parsed = parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    context_state.observe_context_record(&native, path, &meta);
                    if !admission_scope.accepts(context_state.cwd.as_deref()) {
                        non_durable_reason = Some(ObservationCoverageReason::OutOfScope);
                        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                    }
                    if !codex_observation_record_supported(&native) {
                        non_durable_reason = Some(ObservationCoverageReason::UnsupportedFact);
                        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                    }
                    let record_id = codex_native_record_id(&meta.session_id, &native)
                        .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                    let envelope = normalize_codex_observation_with_location(
                        &native,
                        &meta.session_id,
                        native_thread_id.as_deref(),
                        record_id.clone(),
                        range,
                        CodexObservationLocation {
                            project_path: admission_scope
                                .projection_project_path(context_state.cwd.as_deref()),
                            location_path: context_state.cwd.as_deref(),
                            transcript_path: path,
                        },
                    )?;
                    stable_record_id = Some(record_id);
                    Ok(envelope)
                },
            );
            match parsed {
                Ok(parsed) => Ok(JsonlFrameAdmission::durable(
                    parsed,
                    stable_record_id
                        .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?,
                )),
                Err(_) => Ok(JsonlFrameAdmission::non_durable(
                    non_durable_reason.unwrap_or(ObservationCoverageReason::MalformedFrame),
                )),
            }
        },
    )
    .await?;
    Ok(CodexJsonlAdmissionProgress {
        bytes_consumed: progress.bytes_consumed,
        source_deferred: progress.source_deferred,
    })
}

fn codex_observation_record_supported(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            "session_meta"
                | "turn_context"
                | "event_msg"
                | "response_item"
                | "compacted"
                | "inter_agent_communication"
        )
    )
}

#[cfg(test)]
pub(crate) fn normalize_codex_observation(
    native: &Value,
    session_id: &str,
    native_thread_id: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    normalize_codex_observation_inner(
        native,
        session_id,
        native_thread_id,
        stable_record_id,
        range,
        None,
    )
}

struct CodexObservationLocation<'a> {
    project_path: Option<&'a Path>,
    location_path: Option<&'a Path>,
    transcript_path: &'a Path,
}

fn normalize_codex_observation_with_location(
    native: &Value,
    session_id: &str,
    native_thread_id: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    location: CodexObservationLocation<'_>,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    normalize_codex_observation_inner(
        native,
        session_id,
        native_thread_id,
        stable_record_id,
        range,
        Some(location),
    )
}

fn normalize_codex_observation_inner(
    native: &Value,
    session_id: &str,
    native_thread_id: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    location: Option<CodexObservationLocation<'_>>,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let native_kind = native
        .get("type")
        .and_then(Value::as_str)
        .ok_or(ObservationRecordParseErrorV1::NormalizationFailed)?;
    let payload = native.get("payload").unwrap_or(native);
    let timestamp = timestamp_from_record(native);
    let mut relations = CanonicalObservationRelationsV1::new(
        SessionId::new(session_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    );
    // `session_meta.payload.id` is the native thread id. The session parser may
    // fall back to a filename for legacy source identity, which is not enough
    // evidence to populate this relation.
    if let Some(thread_id) = native_thread_id.and_then(observation_id_from_native) {
        relations = relations.with_thread_id(thread_id);
    }
    if let Some(turn_id) = codex_native_turn_id(payload) {
        relations = relations.with_turn_id(turn_id);
    }
    if matches!(
        native_kind,
        "event_msg" | "response_item" | "compacted" | "inter_agent_communication"
    ) {
        relations = relations.with_message_id(stable_record_id.clone());
    }
    if native_kind == "session_meta" {
        relations = append_codex_session_meta_agent_relations(relations, payload, native_thread_id);
    }

    let mut facts = Vec::new();
    if let Some(location) = location {
        facts.push(CanonicalObservationFactV1::Session {
            project_path: location
                .project_path
                .map(|path| path.to_string_lossy().into_owned()),
            location_path: location
                .location_path
                .map(|path| path.to_string_lossy().into_owned()),
            transcript_path: Some(location.transcript_path.to_string_lossy().into_owned()),
            title: None,
            started_at: None,
            ended_at: None,
            source: Some("codex_rollout".to_string()),
            native_source: Some("codex".to_string()),
            profile: None,
            location_provenance: Some("rollout_context".to_string()),
        });
    }
    match native_kind {
        "session_meta" => {
            facts.push(CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::SessionStart,
            });
            append_codex_git_facts(payload, &mut facts);
            if payload.pointer("/source/subagent").is_some()
                || payload.get("thread_source").and_then(Value::as_str) == Some("subagent")
            {
                facts.push(CanonicalObservationFactV1::Workflow {
                    evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                    reference: None,
                    content: None,
                });
            }
        }
        "turn_context" => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "turn_context".to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
        "event_msg" => append_codex_event_facts(payload, timestamp, &mut facts),
        "response_item" => {
            append_codex_response_item_facts(payload, timestamp, &stable_record_id, &mut facts);
        }
        "compacted" => {
            facts.push(CanonicalObservationFactV1::Compaction {
                summary: payload.get("message").cloned(),
                input_tokens: canonical_u64(payload.get("input_tokens")),
                output_tokens: canonical_u64(payload.get("output_tokens")),
            });
            facts.push(CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
            });
        }
        "inter_agent_communication" => {
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                reference: None,
                content: payload
                    .get("message")
                    .or_else(|| payload.get("content"))
                    .cloned(),
            });
        }
        _ => {}
    }
    if facts.is_empty() {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: native_kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        native_kind,
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn observation_id_from_native(value: &str) -> Option<ObservationId> {
    ObservationId::new(value).ok()
}

fn codex_native_turn_id(payload: &Value) -> Option<ObservationId> {
    string_field(payload, "turn_id")
        .or_else(|| {
            nested_string_field(
                payload,
                "/internal_chat_message_metadata_passthrough/turn_id",
            )
        })
        .and_then(|turn_id| observation_id_from_native(&turn_id))
}

fn append_codex_session_meta_agent_relations(
    mut relations: CanonicalObservationRelationsV1,
    payload: &Value,
    native_thread_id: Option<&str>,
) -> CanonicalObservationRelationsV1 {
    let parent_session_id = string_field(payload, "forked_from_id")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/parent_thread_id"));
    let thread_source = string_field(payload, "thread_source");
    let is_subagent = thread_source.as_deref() == Some("subagent")
        || parent_session_id.is_some()
        || payload.pointer("/source/subagent").is_some();
    if !is_subagent {
        return relations;
    }
    // Codex does not expose a separate stable agent id in session_meta. The
    // subagent's native session/thread id is stable; nickname and role are
    // mutable descriptive labels and must never participate in identity.
    if let Some(agent_id) = native_thread_id.and_then(observation_id_from_native) {
        relations = relations.with_agent_id(agent_id);
    }
    if let Some(parent_agent_id) = parent_session_id.and_then(|id| observation_id_from_native(&id))
    {
        relations = relations.with_parent_agent_id(parent_agent_id);
    }
    relations
}

fn append_codex_event_facts(
    payload: &Value,
    timestamp: Option<i64>,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    match payload.get("type").and_then(Value::as_str) {
        Some("user_message" | "agent_message") => {
            let role = if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                CanonicalMessageRoleV1::User
            } else {
                CanonicalMessageRoleV1::Assistant
            };
            if let Some(content) = payload.get("message").cloned() {
                facts.push(CanonicalObservationFactV1::Message {
                    role,
                    content,
                    model: payload
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp,
                });
            }
        }
        Some("token_count") => {
            let usage = payload
                .get("info")
                .and_then(|info| {
                    info.get("last_token_usage")
                        .or_else(|| info.get("total_token_usage"))
                })
                .unwrap_or(payload);
            let input = canonical_u64(usage.get("input_tokens"));
            let cache_read = canonical_u64(
                usage
                    .get("cached_input_tokens")
                    .or_else(|| usage.get("cache_read_input_tokens")),
            );
            facts.push(CanonicalObservationFactV1::Usage {
                input_tokens: input.map(|input| input.saturating_sub(cache_read.unwrap_or(0))),
                output_tokens: canonical_u64(
                    usage
                        .get("output_tokens")
                        .or_else(|| usage.get("completion_tokens")),
                ),
                cache_read_tokens: cache_read,
                cache_write_tokens: canonical_u64(usage.get("cache_write_input_tokens")),
                reasoning_tokens: canonical_u64(
                    usage
                        .get("reasoning_output_tokens")
                        .or_else(|| usage.get("reasoning_tokens")),
                ),
            });
        }
        Some("thread_goal_updated") => {
            if !append_codex_thread_goal_lifecycle_fact(payload, facts) {
                facts.push(CanonicalObservationFactV1::Unknown {
                    native_kind: "thread_goal_updated".to_string(),
                    state: CanonicalUnknownStateV1::Malformed,
                });
            }
        }
        Some(event @ ("task_started" | "task_complete" | "turn_aborted")) => {
            append_codex_turn_lifecycle_fact(payload, event, facts);
        }
        Some(kind) => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
        None => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "event_msg".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        }),
    }
}

fn append_codex_thread_goal_lifecycle_fact(
    payload: &Value,
    facts: &mut Vec<CanonicalObservationFactV1>,
) -> bool {
    // Real Codex shape (see goal_event_line / write_codex_rollout_with_goal_events):
    // payload.goal.{objective,status,threadId,tokensUsed,timeUsedSeconds,createdAt,updatedAt}
    // plus optional payload.threadId. Never invent fields absent from that nest.
    let Some(goal) = payload.get("goal") else {
        return false;
    };
    let Some(objective) = goal
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
    else {
        return false;
    };
    let _ = objective;
    let status = goal
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(str::to_string);
    let provider_reference = goal
        .get("threadId")
        .and_then(Value::as_str)
        .or_else(|| payload.get("threadId").and_then(Value::as_str))
        .filter(|thread_id| !thread_id.is_empty())
        .map(str::to_string);
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(goal.clone()),
    });
    true
}

fn append_codex_turn_lifecycle_fact(
    payload: &Value,
    event: &str,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    // Exact singular task_complete / task_started / turn_aborted only
    // (write_codex_rollout_with_structured_events, task_events_become_turn_boundary_rows).
    // Do not index last_agent_message as content — classic turn rows exclude it.
    let provider_reference = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .filter(|turn_id| !turn_id.is_empty())
        .map(str::to_string);
    // Keep native `reason` in content only — do not promote it to status
    // (no fixture evidence that abort reason is a workflow status vocabulary).
    let mut content = serde_json::Map::new();
    content.insert("type".to_string(), Value::String(event.to_string()));
    for key in [
        "turn_id",
        "started_at",
        "completed_at",
        "duration_ms",
        "time_to_first_token_ms",
        "model_context_window",
        "reason",
    ] {
        if let Some(value) = payload.get(key) {
            content.insert(key.to_string(), value.clone());
        }
    }
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: Some(event.to_string()),
        status: None,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(Value::Object(content)),
    });
}

fn append_codex_update_plan_lifecycle_fact(
    payload: &Value,
    facts: &mut Vec<CanonicalObservationFactV1>,
) -> bool {
    // Real Codex shape: response_item function_call name=update_plan with
    // arguments JSON string/object {explanation?, plan:[{step,status}]} + call_id.
    let Some(arguments) = events::parse_arguments(payload.get("arguments")) else {
        return false;
    };
    if arguments.get("plan").and_then(Value::as_array).is_none() {
        return false;
    }
    let provider_reference = payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .map(str::to_string);
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Plan,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status: None,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(arguments),
    });
    true
}

fn append_codex_response_item_facts(
    payload: &Value,
    timestamp: Option<i64>,
    stable_record_id: &ObservationId,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let Some(item_kind) = payload.get("type").and_then(Value::as_str) else {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "response_item".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        });
        return;
    };
    match item_kind {
        "message" => {
            if let Some(content) = payload.get("content").cloned() {
                facts.push(CanonicalObservationFactV1::Message {
                    role: canonical_message_role(payload.get("role").and_then(Value::as_str)),
                    content,
                    model: payload
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp,
                });
                // Goal-context response_item messages stay Message-only.
                // WorkflowLifecycle Goal is reserved for nested thread_goal_updated
                // (write_codex_rollout_with_goal_events), not synthetic goal-context text.
            }
        }
        "function_call" | "custom_tool_call" | "tool_search_call" | "web_search_call" => {
            let invocation_id = canonical_native_observation_id(
                payload
                    .get("call_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str),
                stable_record_id,
            );
            let name = response_item_tool_name(payload, item_kind)
                .unwrap_or_else(|| item_kind.to_string());
            facts.push(CanonicalObservationFactV1::ToolInvocation {
                invocation_id,
                name: name.clone(),
                arguments: Value::Null,
            });
            if item_kind == "function_call" && name == "update_plan" {
                let _ = append_codex_update_plan_lifecycle_fact(payload, facts);
            }
        }
        "function_call_output" | "custom_tool_call_output" => {
            facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id: Some(canonical_native_observation_id(
                    payload.get("call_id").and_then(Value::as_str),
                    stable_record_id,
                )),
                content: Value::Null,
                success: payload
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|status| matches!(status, "completed" | "success" | "succeeded")),
            });
        }
        "reasoning" => {
            let summary = payload.get("summary").filter(|summary| !summary.is_null());
            let encrypted = payload
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.is_empty());
            let (visibility, content) = if let Some(summary) = summary {
                (
                    CanonicalReasoningVisibilityV1::Visible,
                    Some(summary.clone()),
                )
            } else if encrypted {
                (CanonicalReasoningVisibilityV1::Redacted, None)
            } else {
                (CanonicalReasoningVisibilityV1::Unavailable, None)
            };
            facts.push(CanonicalObservationFactV1::Reasoning {
                visibility,
                content,
            });
        }
        kind => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
    }
}

fn append_codex_git_facts(payload: &Value, facts: &mut Vec<CanonicalObservationFactV1>) {
    let Some(git) = payload.get("git") else {
        return;
    };
    if let Some(branch) = git
        .get("branch")
        .or_else(|| git.get("current_branch"))
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Branch,
            reference: Some(branch.to_string()),
            content: None,
        });
    }
    if let Some(commit) = git
        .get("commit_hash")
        .or_else(|| git.get("commit"))
        .or_else(|| git.get("head"))
        .and_then(Value::as_str)
        .filter(|commit| !commit.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Commit,
            reference: Some(commit.to_string()),
            content: None,
        });
    }
}

pub(crate) fn codex_native_record_id(
    session_id: &str,
    value: &Value,
) -> TranscriptIngestResult<ObservationId> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.provider-native-record.v1\0codex\0");
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    hasher.update(
        serde_json::to_vec(value)
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?,
    );
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    }
    Ok(ObservationId::new(format!(
        "codex.native.sha256:{encoded}"
    ))?)
}
