use std::fs::File;
use std::future::Future;
use std::hash::BuildHasher;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value;
#[cfg(test)]
pub use tracedecay_capture::cursor::normalize_cursor_observation;
use tracedecay_capture::cursor::{
    cursor_projected_message_id, normalize_cursor_observation_with_message_id,
    observation_native_record_id, timestamp_tag_from_record,
};
#[cfg(test)]
use tracedecay_domain::CanonicalObservationFactV1;
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProjectId,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::cursor_dispatch::{
    cursor_dispatch_model, cursor_model_string, dispatch_text, is_subagent_dispatch_tool,
};
use tracedecay_store::observation::ObservationCoverageReason;

use crate::admission::HostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::SessionMessageRecord;
use crate::runtime::ingest_byte_budget::IngestByteBudget;
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionProgress, JsonlObservationAdmissionRequest,
    admit_jsonl_observations, namespace_replacement_message_ids, preflight_and_parse_new,
};
use crate::runtime::shared::{
    StoredCursor, TranscriptLocation, TranscriptLocationMetadataKeys, append_location_metadata,
    append_tool_calls_metadata, append_tool_event_metadata, append_usage_metadata,
    content_storage_text_and_tools, paths_equal, title_from_messages,
};
use crate::runtime::source::{
    MAX_JSONL_RECORD_BYTES, ParsedTranscript, RawJsonlFrame, RawJsonlFrameReader, SessionDraft,
    TranscriptDiscoveryBounds, TranscriptIngestError, TranscriptIngestResult, TranscriptSource,
    collect_files_with_ext_bounded, stream_new_jsonl,
};
use tracedecay_runtime_core::privacy::{
    ObservationRecordParseErrorV1, parse_normalized_observation_record_v1,
};
const CURSOR_EVENT_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "cursor_event_cwd",
        "cursor_event_worktree",
        "cursor_event_location_provenance",
    );

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CursorTranscriptIngestStats {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
    pub bytes_consumed: u64,
    pub source_deferred: bool,
}

#[derive(Clone)]
struct CursorObservationContext {
    project_path: Option<String>,
    location_path: Option<String>,
    transcript_path: String,
    model: Option<String>,
    thread_id: Option<String>,
    location_provenance: Option<String>,
}

fn cursor_observation_context(
    event: &Value,
    transcript_path: &Path,
    user_scope: bool,
) -> CursorObservationContext {
    let (project_path, _) = event_project(event);
    let project_path = (project_path != "unknown" && !user_scope).then_some(project_path);
    // Canonical Session facts must be invariant to whether a hook or startup
    // sweep delivered the record. Route-specific cwd/provenance belongs to
    // admission evidence, while the selected workspace root is the stable
    // session location for project-scoped Cursor history.
    let location_path = project_path.clone();
    let location_provenance = project_path.as_ref().map(|_| "workspace_root".to_string());
    CursorObservationContext {
        project_path,
        location_path,
        transcript_path: transcript_path.to_string_lossy().into_owned(),
        model: cursor_model_string(event),
        thread_id: event
            .get("conversation_id")
            .or_else(|| event.get("chat_id"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        location_provenance,
    }
}

/// A Cursor hook event scoped to one transcript file.
struct CursorEventSource {
    event: Value,
    transcript_path: PathBuf,
    include_subagents: bool,
    user_scope: bool,
}

impl TranscriptSource for CursorEventSource {
    fn provider(&self) -> &'static str {
        "cursor"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        let mut paths = vec![self.transcript_path.clone()];
        if self.include_subagents {
            let parent_session_id = event_session_id(&self.event, &self.transcript_path);
            paths.extend(cursor_subagent_paths(
                &self.transcript_path,
                &parent_session_id,
            ));
        }
        paths
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        _project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let parent_session_id = event_session_id(&self.event, &self.transcript_path);
        parse_cursor_jsonl(
            &self.event,
            &parent_session_id,
            path,
            prev,
            max_new_bytes,
            self.user_scope,
        )
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        preflight_and_parse_new("cursor", path, prev, max_new_bytes, || {
            self.parse_new(path, prev, project_root, max_new_bytes)
        })
    }
}

const CURSOR_OBSERVATION_RETENTION: &str = "retention.provider-observation";

struct CursorJsonlAdmitState {
    timestamps: TimestampCarry,
    generation: u64,
    namespace_replacement: bool,
}

// Cursor JSONL admission chokepoint: the whole per-file admission future is
// boxed here so the per-file sweep loop no longer pins each call, keeping the
// debug poll frame bounded through the deep ingest recursion chain.
fn admit_cursor_jsonl_observations<'a>(
    parent_session_id: &'a str,
    path: &'a Path,
    context: &'a CursorObservationContext,
    admission: &'a dyn HostAdmission,
    scope: &'a ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &'a ObservationCancellation,
) -> Pin<
    Box<dyn Future<Output = TranscriptIngestResult<JsonlObservationAdmissionProgress>> + Send + 'a>,
> {
    Box::pin(async move {
        let subagent = cursor_subagent_identity(path, parent_session_id);
        let native_session_id = subagent
            .as_ref()
            .map_or(parent_session_id, |(session_id, _)| session_id.as_str());
        let source = ObservationSourceIdentityV1::for_provider(
            ProviderId::new("cursor")?,
            SessionId::new(native_session_id.to_owned())?,
        )?;
        let mut context = context.clone();
        if let Some((_, agent_id)) = subagent.as_ref() {
            context.model = parent_dispatch_model_for_subagent(path, parent_session_id, agent_id)
                .or(context.model);
        }
        let request = JsonlObservationAdmissionRequest::new(
            "cursor",
            path,
            admission,
            source,
            scope.clone(),
            RetentionClass::new(CURSOR_OBSERVATION_RETENTION)?,
        )
        .with_max_new_bytes(max_new_bytes)
        .with_cancellation(cancellation.clone());
        let progress = admit_jsonl_observations(
            request,
            |scan| CursorJsonlAdmitState {
                timestamps: TimestampCarry::new(i64::try_from(scan.source_mtime).ok()),
                generation: scan.generation,
                namespace_replacement: scan.replacement_rescan,
            },
            |state, bytes, range, source_offset| {
                let mut stable_record_id = None;
                let mut unsupported_record = false;
                let parsed = parse_normalized_observation_record_v1(
                    bytes,
                    range,
                    ObservationOrderingDomainV1::FileBytes,
                    |native| {
                        if native.get("role").and_then(Value::as_str).is_none() {
                            unsupported_record = true;
                            return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                        }
                        let record_id =
                            observation_native_record_id("cursor", native_session_id, &native)
                                .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                        let message_id = cursor_projected_message_id(
                            &native,
                            native_session_id,
                            source_offset,
                            state.generation,
                            state.namespace_replacement,
                        )?;
                        let timestamp = state.timestamps.observe(&native);
                        let native = cursor_native_with_context(native, &context, timestamp);
                        let (agent_id, parent_agent_id) = match subagent.as_ref() {
                            Some((child_id, _)) => {
                                (Some(child_id.as_str()), Some(parent_session_id))
                            }
                            None => (None, None),
                        };
                        let envelope = normalize_cursor_observation_with_message_id(
                            &native,
                            native_session_id,
                            record_id.clone(),
                            message_id.clone(),
                            range,
                            agent_id,
                            parent_agent_id,
                        )?;
                        stable_record_id = Some(record_id);
                        Ok(envelope)
                    },
                );
                match parsed {
                    Ok(parsed) => Ok(JsonlFrameAdmission::durable(
                        parsed,
                        stable_record_id.ok_or(TranscriptIngestError::InvalidFrameState {
                            provider: "cursor",
                        })?,
                    )),
                    Err(_) => Ok(JsonlFrameAdmission::non_durable(if unsupported_record {
                        ObservationCoverageReason::UnsupportedFact
                    } else {
                        ObservationCoverageReason::MalformedFrame
                    })),
                }
            },
        )
        .await?;
        Ok(progress)
    })
}

fn cursor_native_with_context(
    mut native: Value,
    context: &CursorObservationContext,
    derived_timestamp: Option<i64>,
) -> Value {
    let Some(object) = native.as_object_mut() else {
        return native;
    };
    for (key, value) in [
        ("tracedecayProjectPath", context.project_path.as_ref()),
        ("tracedecayLocationPath", context.location_path.as_ref()),
        (
            "tracedecayLocationProvenance",
            context.location_provenance.as_ref(),
        ),
        ("tracedecayModel", context.model.as_ref()),
        ("tracedecayThreadId", context.thread_id.as_ref()),
    ] {
        if let Some(value) = value {
            object.insert(key.to_string(), Value::String(value.clone()));
        }
    }
    object.insert(
        "tracedecayTranscriptPath".to_string(),
        Value::String(context.transcript_path.clone()),
    );
    if let Some(timestamp) = derived_timestamp {
        object.insert(
            "tracedecayDerivedTimestamp".to_string(),
            Value::from(timestamp),
        );
    }
    native
}

/// Parse the newly-appended portion of one Cursor transcript file into a
/// provider-neutral [`ParsedTranscript`]. Shared by the hook path
/// ([`CursorEventSource`]) and the startup catch-up sweep
/// ([`CursorSweepSource`]); both derive identical session/message ids for the
/// same file (the hook event's `session_id` always equals the transcript file
/// stem), so whichever runs second is an idempotent no-op.
fn parse_cursor_jsonl(
    event: &Value,
    parent_session_id: &str,
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    user_scope: bool,
) -> Option<ParsedTranscript> {
    let new = stream_new_jsonl(path, prev, max_new_bytes)?;
    // A truncate-and-rewrite can reuse every byte offset from the previous
    // file generation. Legacy projection keys are offset-based, so keep
    // replacement rows distinct instead of overwriting retained history.
    let replayed_from_start =
        prev.position > 0 && new.lines.first().is_some_and(|line| line.offset == 0);
    let subagent = cursor_subagent_identity(path, parent_session_id);
    let session_id = subagent.as_ref().map_or_else(
        || parent_session_id.to_string(),
        |(session_id, _agent_id)| session_id.clone(),
    );
    let subagent_model = subagent.as_ref().and_then(|(_, agent_id)| {
        parent_dispatch_model_for_subagent(path, parent_session_id, agent_id)
    });
    let event_cwd = event_cwd(event);
    let event_location_provenance = event_location_provenance(event);
    let mut carry = TimestampCarry::new(i64::try_from(new.new_cursor.mtime).ok());
    let mut messages = Vec::new();
    for line in &new.lines {
        let derived_timestamp = carry.observe(&line.value);
        let context = CursorMessageContext {
            transcript_path: path,
            source_offset: line.offset,
            derived_timestamp,
            model_fallback: subagent_model.as_deref(),
            event_cwd: event_cwd.as_deref(),
            event_location_provenance,
        };
        // The byte offset doubles as the message ordinal and source_offset,
        // matching the original Cursor ingestion.
        if let Some(message) = event_message(&line.value, event, &session_id, line.offset, context)
        {
            messages.push(message);
        }
        messages.extend(event_dispatch_messages(
            &line.value,
            event,
            &session_id,
            context,
        ));
    }
    if replayed_from_start {
        namespace_replacement_message_ids(&mut messages, new.new_cursor.file_id);
    }

    // Defer the (filesystem-walking) project/title/metadata derivation until
    // we actually have new messages; the driver ignores the draft otherwise.
    let draft = if messages.is_empty() {
        SessionDraft {
            session_id,
            project_key: String::new(),
            project_path: String::new(),
            title: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    } else {
        let (project_key, project_path) = if user_scope {
            ("user".to_string(), "user".to_string())
        } else {
            event_project(event)
        };
        let (draft_parent_session_id, agent_id) = subagent
            .map_or((None, None), |(_session_id, agent_id)| {
                (Some(parent_session_id.to_string()), Some(agent_id))
            });
        let is_subagent = draft_parent_session_id.is_some();
        SessionDraft {
            session_id,
            project_key,
            project_path,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(
                event,
                event_cwd.as_deref(),
                event_location_provenance,
            ))
            .ok(),
            parent_session_id: draft_parent_session_id,
            is_subagent,
            agent_id,
            parent_tool_use_id: None,
        }
    };

    Some(ParsedTranscript {
        draft,
        messages,
        new_cursor: new.new_cursor,
    })
}

/// Ingest the Cursor transcript referenced by a hook payload into the
/// provider-neutral session/message tables for the provided database. Project
/// hooks pass both the daemon-resolved project DB and canonical project id.
///
/// Ingestion is **incremental**: it resumes from the byte offset recorded in the
/// DB's `parse_offsets` table (via the shared [`crate::runtime::source`]
/// driver), so each call only parses and upserts transcript lines appended since
/// the last run rather than re-reading the whole file. Repeated calls on an
/// unchanged file are a no-op.
pub async fn ingest_cursor_transcript_event(
    event_json: &str,
    admission: &dyn HostAdmission,
    project_id: ProjectId,
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(
        &try_ingest_cursor_transcript_event(event_json, admission, project_id).await,
    )
}

pub async fn try_ingest_cursor_transcript_event(
    event_json: &str,
    admission: &dyn HostAdmission,
    project_id: ProjectId,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_transcript_event_capped(event_json, admission, project_id, None).await
}

/// Like [`ingest_cursor_transcript_event`], but bounds how many newly-appended
/// bytes a single call will read. Cursor hooks pass byte caps to stay within hook
/// budgets. The cap is shared across the parent and every discovered subagent
/// transcript.
pub async fn ingest_cursor_transcript_event_capped(
    event_json: &str,
    admission: &dyn HostAdmission,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(
        &try_ingest_cursor_transcript_event_capped(
            event_json,
            admission,
            project_id,
            max_new_bytes,
        )
        .await,
    )
}

pub async fn try_ingest_cursor_transcript_event_capped(
    event_json: &str,
    admission: &dyn HostAdmission,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_transcript_event_capped_with_admission(
        event_json,
        project_id,
        admission,
        max_new_bytes,
    )
    .await
}

pub async fn try_ingest_cursor_transcript_event_capped_with_admission(
    event_json: &str,
    project_id: ProjectId,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    let Ok(event) = serde_json::from_str::<Value>(event_json) else {
        return Ok(CursorTranscriptIngestStats::default());
    };
    let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(CursorTranscriptIngestStats::default());
    };

    // Cursor derives its project from the event, so the driver's project_root
    // argument is unused by `CursorEventSource`; the transcript path's parent is
    // a cheap, side-effect-free placeholder.
    let project_root = transcript_path
        .parent()
        .map_or_else(|| transcript_path.clone(), Path::to_path_buf);
    let source = CursorEventSource {
        event,
        transcript_path,
        include_subagents: true,
        user_scope: false,
    };
    let scope = ObservationScopeV1::Project { project_id };
    let parent_session_id = event_session_id(&source.event, &source.transcript_path);
    let mut budget = match max_new_bytes {
        Some(limit) => IngestByteBudget::bounded(limit),
        None => IngestByteBudget::unbounded(),
    };
    for path in source.transcript_paths(&project_root) {
        let context = cursor_observation_context(&source.event, &path, false);
        let progress = admit_cursor_jsonl_observations(
            &parent_session_id,
            &path,
            &context,
            admission,
            &scope,
            budget.remaining(),
            &ObservationCancellation::default(),
        )
        .await?;
        budget.record_progress(progress.bytes_consumed, progress.source_deferred);
    }
    let mut stats = drain_cursor_observation_projections(
        admission,
        &scope,
        &ObservationCancellation::default(),
    )
    .await?;
    stats.bytes_consumed = budget.consumed();
    stats.source_deferred = budget.deferred();
    Ok(stats)
}

pub async fn ingest_cursor_user_transcript_event_capped(
    event_json: &str,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(
        &try_ingest_cursor_user_transcript_event_capped(event_json, admission, max_new_bytes).await,
    )
}

pub async fn try_ingest_cursor_user_transcript_event_capped(
    event_json: &str,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_user_transcript_event_capped_with_registered_roots(
        event_json,
        admission,
        max_new_bytes,
        &[],
    )
    .await
}

/// User-scope live ingest guarded by a registry snapshot. The unguarded
/// wrapper remains useful for isolated parsing without a profile registry.
pub async fn ingest_cursor_user_transcript_event_capped_with_registered_roots(
    event_json: &str,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    registered_roots: &[PathBuf],
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(
        &try_ingest_cursor_user_transcript_event_capped_with_registered_roots(
            event_json,
            admission,
            max_new_bytes,
            registered_roots,
        )
        .await,
    )
}

pub async fn try_ingest_cursor_user_transcript_event_capped_with_registered_roots(
    event_json: &str,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    registered_roots: &[PathBuf],
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_user_transcript_event_capped_with_admission(
        event_json,
        admission,
        max_new_bytes,
        registered_roots,
    )
    .await
}

pub async fn try_ingest_cursor_user_transcript_event_capped_with_admission(
    event_json: &str,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    registered_roots: &[PathBuf],
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    let Ok(event) = serde_json::from_str::<Value>(event_json) else {
        return Ok(CursorTranscriptIngestStats::default());
    };
    let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(CursorTranscriptIngestStats::default());
    };
    let event_workspaces = cursor_event_workspace_roots(&event);
    let belongs_to_registered_project = if event_workspaces.is_empty() {
        // Without event workspace identity, Cursor's transcript directory is
        // the only attribution available. Its slash-to-hyphen encoding is
        // lossy, so a registered-slug collision must fail closed rather than
        // risk copying project evidence into user memory.
        cursor_transcript_project_slug(&transcript_path).is_some_and(|slug| {
            registered_roots
                .iter()
                .filter_map(|root| cursor_project_slug(root))
                .any(|registered_slug| registered_slug == slug)
        })
    } else {
        // A hook-provided cwd/file/workspace root is stronger than the lossy
        // transcript slug. This keeps distinct slash-vs-hyphen workspaces,
        // linked worktrees, and renamed checkouts from excluding one another.
        event_workspaces.iter().any(|workspace| {
            registered_roots
                .iter()
                .any(|registered| paths_equal(workspace, registered))
        })
    };
    if belongs_to_registered_project {
        return Ok(CursorTranscriptIngestStats::default());
    }
    let placeholder = transcript_path
        .parent()
        .map_or_else(|| transcript_path.clone(), Path::to_path_buf);
    let source = CursorEventSource {
        event,
        transcript_path,
        include_subagents: true,
        user_scope: true,
    };
    let scope = ObservationScopeV1::Profile;
    let parent_session_id = event_session_id(&source.event, &source.transcript_path);
    let mut budget = match max_new_bytes {
        Some(limit) => IngestByteBudget::bounded(limit),
        None => IngestByteBudget::unbounded(),
    };
    for path in source.transcript_paths(&placeholder) {
        let context = cursor_observation_context(&source.event, &path, true);
        let progress = admit_cursor_jsonl_observations(
            &parent_session_id,
            &path,
            &context,
            admission,
            &scope,
            budget.remaining(),
            &ObservationCancellation::default(),
        )
        .await?;
        budget.record_progress(progress.bytes_consumed, progress.source_deferred);
    }
    let mut stats = drain_cursor_observation_projections(
        admission,
        &scope,
        &ObservationCancellation::default(),
    )
    .await?;
    stats.bytes_consumed = budget.consumed();
    stats.source_deferred = budget.deferred();
    Ok(stats)
}

/// Canonically admit Cursor JSONL transcripts discovered during a project startup
/// sweep. Composer-owned session ids are skipped before discovery results reach
/// observation admission.
pub async fn try_ingest_cursor_project_sweep_capped<S: BuildHasher>(
    project_root: &Path,
    admission: &dyn HostAdmission,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
    skip_session_ids: std::collections::HashSet<String, S>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_project_sweep_capped_with_admission(
        project_root,
        project_id,
        admission,
        max_new_bytes,
        skip_session_ids,
        &ObservationCancellation::default(),
    )
    .await
}

/// Project startup-sweep variant whose authority has already been prepared by
/// the caller from the authoritative project identity and privacy policy.
pub fn try_ingest_cursor_project_sweep_capped_with_admission<'a, S: BuildHasher>(
    project_root: &'a Path,
    project_id: ProjectId,
    admission: &'a dyn HostAdmission,
    max_new_bytes: Option<u64>,
    skip_session_ids: std::collections::HashSet<String, S>,
    cancellation: &'a ObservationCancellation,
) -> Pin<Box<dyn Future<Output = TranscriptIngestResult<CursorTranscriptIngestStats>> + Send + 'a>>
{
    // Rehash into the default (Send) hasher before boxing so the returned
    // future never captures the caller's `S` hasher and stays `Send`.
    let skip_session_ids: std::collections::HashSet<String> =
        skip_session_ids.into_iter().collect();
    Box::pin(async move {
        let Some(source) = CursorSweepSource::new() else {
            return Ok(CursorTranscriptIngestStats::default());
        };
        admit_cursor_sweep_observations_with_admission(
            &source.with_skip_session_ids(skip_session_ids),
            project_root,
            admission,
            max_new_bytes,
            ObservationScopeV1::Project { project_id },
            cancellation,
        )
        .await
    })
}

/// Canonically admit Cursor JSONL transcripts discovered during a profile startup
/// sweep. Registered project slugs and composer-owned session ids are excluded
/// before observation admission.
pub async fn try_ingest_cursor_user_sweep_capped<S: BuildHasher>(
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    skip_session_ids: std::collections::HashSet<String, S>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_user_sweep_capped_with_admission(
        registered_roots,
        admission,
        max_new_bytes,
        skip_session_ids,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_ingest_cursor_user_sweep_capped_with_admission<S: BuildHasher>(
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    skip_session_ids: std::collections::HashSet<String, S>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    let Some(source) = CursorSweepSource::new() else {
        return Ok(CursorTranscriptIngestStats::default());
    };
    admit_cursor_sweep_observations_with_admission(
        &source
            .with_skip_session_ids(skip_session_ids.into_iter().collect())
            .for_user_scope(registered_roots),
        Path::new(""),
        admission,
        max_new_bytes,
        ObservationScopeV1::Profile,
        cancellation,
    )
    .await
}

async fn admit_cursor_sweep_observations_with_admission(
    source: &CursorSweepSource,
    project_root: &Path,
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    scope: ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    if cancellation.is_cancelled() {
        return Ok(CursorTranscriptIngestStats {
            source_deferred: true,
            ..CursorTranscriptIngestStats::default()
        });
    }
    let mut budget = match max_new_bytes {
        Some(limit) => IngestByteBudget::bounded(limit),
        None => IngestByteBudget::unbounded(),
    };
    for path in source.transcript_paths(project_root) {
        if cancellation.is_cancelled() {
            budget.defer();
            break;
        }
        let Some(parent_session_id) = sweep_parent_session_id(&path) else {
            continue;
        };
        let event = cursor_sweep_event(
            &parent_session_id,
            project_root,
            matches!(&scope, ObservationScopeV1::Profile),
        );
        let context = cursor_observation_context(
            &event,
            &path,
            matches!(&scope, ObservationScopeV1::Profile),
        );
        let progress = admit_cursor_jsonl_observations(
            &parent_session_id,
            &path,
            &context,
            admission,
            &scope,
            budget.remaining(),
            cancellation,
        )
        .await?;
        budget.record_progress(progress.bytes_consumed, progress.source_deferred);
    }
    let mut stats = if cancellation.is_cancelled() {
        CursorTranscriptIngestStats::default()
    } else {
        drain_cursor_observation_projections(admission, &scope, cancellation).await?
    };
    stats.bytes_consumed = budget.consumed();
    stats.source_deferred = budget.deferred();
    Ok(stats)
}

async fn drain_cursor_observation_projections(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    let stats =
        crate::runtime::claude_observation::drain_projection_queue(admission, scope, cancellation)
            .await
            .map_err(|error| match error {
                crate::runtime::claude_observation::ClaudeObservationIngestError::Transcript(
                    error,
                ) => error,
                _ => TranscriptIngestError::InvalidFrameState { provider: "cursor" },
            })?;
    Ok(CursorTranscriptIngestStats {
        sessions_upserted: stats.transcript.sessions_upserted,
        messages_upserted: stats.projection_outputs,
        bytes_consumed: 0,
        source_deferred: false,
    })
}

fn cursor_ingest_or_default(
    result: &TranscriptIngestResult<CursorTranscriptIngestStats>,
) -> CursorTranscriptIngestStats {
    result.as_ref().map_or_else(
        |_| {
            tracing::error!(
                reason_code = "cursor_observation_ingest_failed",
                "Cursor transcript ingest failed"
            );
            CursorTranscriptIngestStats::default()
        },
        |stats| *stats,
    )
}

fn cursor_event_workspace_roots(event: &Value) -> Vec<PathBuf> {
    let candidates = if let Some(cwd) = event_cwd(event) {
        vec![cwd]
    } else if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(file_path);
        vec![path.parent().unwrap_or(path).to_path_buf()]
    } else {
        event
            .get("workspace_roots")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect()
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        let root =
            tracedecay_runtime_core::config::discover_project_root(&candidate).unwrap_or(candidate);
        if !roots.iter().any(|seen| paths_equal(seen, &root)) {
            roots.push(root);
        }
    }
    roots
}

fn cursor_transcript_project_slug(path: &Path) -> Option<&str> {
    let components = path.components().collect::<Vec<_>>();
    let transcripts = components
        .iter()
        .position(|component| component.as_os_str() == "agent-transcripts")?;
    components
        .get(transcripts.checked_sub(1)?)?
        .as_os_str()
        .to_str()
}

/// `agent-transcripts/<session>/subagents/<child>.jsonl` is the deepest layout
/// Cursor writes; a little headroom tolerates future nesting.
const MAX_SWEEP_SCAN_DEPTH: u8 = 4;
/// Upper bound on directory-existence probes while checking a slug for decode
/// ambiguity; exhausting it treats the slug as ambiguous (skip, never guess).
const SLUG_DECODE_PROBE_BUDGET: u32 = 4096;

/// Startup catch-up source for Cursor transcripts.
///
/// The live hook path ([`ingest_cursor_transcript_event`]) only sees turns
/// that fire while the tracedecay hooks are installed, so transcripts written
/// before a project was indexed could never ingest. This source sweeps
/// `~/.cursor/projects/<slug>/agent-transcripts/**.jsonl` for the slug that
/// encodes `project_root`, feeding every file through the same
/// [`parse_cursor_jsonl`] parser and (path-keyed) `parse_offsets` cursors as
/// the hook path — files either path has already ingested are byte-offset
/// no-ops for the other, so sweep and hooks never double-ingest.
pub struct CursorSweepSource {
    cursor_projects_dir: PathBuf,
    /// Session ids already owned by the richer composer store
    /// ([`crate::runtime::cursor_composer`]). Transcript files whose stem is
    /// one of these are skipped so the two Cursor sources never double-ingest.
    skip_session_ids: std::collections::HashSet<String>,
    user_registered_slugs: Option<std::collections::HashSet<String>>,
}

impl CursorSweepSource {
    /// Source rooted at the real `~/.cursor/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.cursor/projects` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        Self {
            cursor_projects_dir: home.join(".cursor").join("projects"),
            skip_session_ids: std::collections::HashSet::new(),
            user_registered_slugs: None,
        }
    }

    /// Skip transcript files whose stem (the Cursor session id) is owned by the
    /// composer store, so the composer rows win without duplication.
    #[must_use]
    pub fn with_skip_session_ids(mut self, ids: std::collections::HashSet<String>) -> Self {
        self.skip_session_ids = ids;
        self
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: &[PathBuf]) -> Self {
        self.user_registered_slugs = Some(
            registered_roots
                .iter()
                .filter_map(|root| cursor_project_slug(root))
                .collect(),
        );
        self
    }
}

impl TranscriptSource for CursorSweepSource {
    fn provider(&self) -> &'static str {
        "cursor"
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        if let Some(registered_slugs) = &self.user_registered_slugs {
            let Ok(entries) = std::fs::read_dir(&self.cursor_projects_dir) else {
                return Vec::new();
            };
            let default_bounds = TranscriptDiscoveryBounds::default_walk();
            let mut paths = Vec::new();
            let mut remaining_bytes = default_bounds.max_discovery_bytes;
            for entry in entries
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|slug| !registered_slugs.contains(slug))
                })
            {
                let remaining_files = default_bounds.max_files.saturating_sub(paths.len());
                if remaining_files == 0 || remaining_bytes == 0 {
                    break;
                }
                let bounds = TranscriptDiscoveryBounds {
                    max_files: remaining_files,
                    max_discovery_bytes: remaining_bytes,
                    ..default_bounds
                };
                let report = collect_files_with_ext_bounded(
                    &entry.path().join("agent-transcripts"),
                    "jsonl",
                    MAX_SWEEP_SCAN_DEPTH,
                    bounds,
                );
                remaining_bytes = remaining_bytes.saturating_sub(report.bytes_charged);
                paths.extend(report.paths);
            }
            return paths;
        }
        let Some(slug) = cursor_project_slug(project_root) else {
            return Vec::new();
        };
        let transcripts_dir = self
            .cursor_projects_dir
            .join(&slug)
            .join("agent-transcripts");
        if !transcripts_dir.is_dir() {
            return Vec::new();
        }
        // The slug encoding is lossy (`/` becomes `-`, and real directory
        // names may themselves contain `-`). When another *existing* directory
        // also encodes to this slug, the transcripts in it cannot be
        // attributed safely, so skip with a note rather than guess.
        match decode_slug_candidates(project_root, &slug) {
            Some(candidates)
                if candidates
                    .iter()
                    .all(|candidate| paths_equal(candidate, project_root)) => {}
            _ => {
                tracing::warn!(
                    project_root = %project_root.display(),
                    %slug,
                    "skipping Cursor transcript sweep because project slug is ambiguous"
                );
                return Vec::new();
            }
        }
        let files = collect_files_with_ext_bounded(
            &transcripts_dir,
            "jsonl",
            MAX_SWEEP_SCAN_DEPTH,
            TranscriptDiscoveryBounds::default_walk(),
        )
        .paths;
        // Cursor materializes some subagent sessions twice: under their
        // parent's `subagents/` dir and again as a top-level
        // `<id>/<id>.jsonl` copy whose content drifts slightly (so byte
        // offsets — and therefore message ids — diverge). Ingesting both
        // would duplicate messages and overwrite the parent linkage; keep
        // the subagent copy (it carries parentage, and it is the copy the
        // live hook path ingests) and skip the top-level duplicate.
        let subagent_stems: std::collections::HashSet<std::ffi::OsString> = files
            .iter()
            .filter(|path| is_subagent_transcript(path))
            .filter_map(|path| path.file_stem().map(std::ffi::OsStr::to_os_string))
            .collect();
        files
            .into_iter()
            .filter(|path| {
                is_subagent_transcript(path)
                    || path
                        .file_stem()
                        .is_none_or(|stem| !subagent_stems.contains(stem))
            })
            .filter(|path| {
                // Composer-owned sessions are ingested (richer) by the composer
                // sweep; skip the JSONL copy so neither path double-ingests.
                self.skip_session_ids.is_empty()
                    || path
                        .file_stem()
                        .and_then(std::ffi::OsStr::to_str)
                        .is_none_or(|stem| !self.skip_session_ids.contains(stem))
            })
            .collect()
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let parent_session_id = sweep_parent_session_id(path)?;
        // Synthesize the minimal hook-shaped event the shared parser expects:
        // the same session id a live hook would carry (Cursor names parent
        // transcripts `<session-id>.jsonl`) and the project root as `cwd` so
        // `event_project` scopes the session exactly like the hook path.
        let user_scope = self.user_registered_slugs.is_some();
        let event = cursor_sweep_event(&parent_session_id, project_root, user_scope);
        parse_cursor_jsonl(
            &event,
            &parent_session_id,
            path,
            prev,
            max_new_bytes,
            user_scope,
        )
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        preflight_and_parse_new("cursor", path, prev, max_new_bytes, || {
            self.parse_new(path, prev, project_root, max_new_bytes)
        })
    }
}

/// Synthesizes the minimal hook-shaped event used by startup sweeps so their
/// scope and location provenance match the legacy parser's behavior.
fn cursor_sweep_event(parent_session_id: &str, project_root: &Path, user_scope: bool) -> Value {
    if user_scope {
        serde_json::json!({
            "session_id": parent_session_id,
            "tracedecay_location_provenance": "user_sweep",
        })
    } else {
        serde_json::json!({
            "session_id": parent_session_id,
            "cwd": project_root.to_string_lossy(),
            "tracedecay_location_provenance": "sweep_project_root",
        })
    }
}

/// Compute the `~/.cursor/projects` directory slug Cursor derives from a
/// workspace path: every normal path component joined with `-`, case
/// preserved (verified against real `~/.cursor/projects` entries).
/// Returns `None` for non-UTF-8, relative, or traversal-containing paths.
pub fn cursor_project_slug(project_root: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in project_root.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
            std::path::Component::CurDir | std::path::Component::ParentDir => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("-"))
}

/// Enumerate every *existing* directory that [`cursor_project_slug`] would
/// encode to `slug`, by walking the filesystem from `project_root`'s root and
/// re-grouping dash-separated tokens into path components (pruned to
/// directories that actually exist). Returns `None` when the probe budget is
/// exhausted, which callers must treat as "ambiguous".
fn decode_slug_candidates(project_root: &Path, slug: &str) -> Option<Vec<PathBuf>> {
    let mut base = PathBuf::new();
    for component in project_root.components() {
        match component {
            std::path::Component::Normal(_) => break,
            other => base.push(other.as_os_str()),
        }
    }
    let tokens: Vec<&str> = slug.split('-').collect();
    let mut candidates = Vec::new();
    let mut budget = SLUG_DECODE_PROBE_BUDGET;
    let exhausted = decode_slug_inner(&base, &tokens, &mut candidates, &mut budget);
    (!exhausted).then_some(candidates)
}

/// Depth-first regrouping of `tokens` into existing directory components
/// under `base`. Returns `true` when the probe budget ran out (enumeration is
/// incomplete and the result must not be trusted).
fn decode_slug_inner(
    base: &Path,
    tokens: &[&str],
    candidates: &mut Vec<PathBuf>,
    budget: &mut u32,
) -> bool {
    if tokens.is_empty() {
        candidates.push(base.to_path_buf());
        return false;
    }
    for split in 1..=tokens.len() {
        if *budget == 0 {
            return true;
        }
        *budget -= 1;
        let candidate = base.join(tokens[..split].join("-"));
        if candidate.is_dir() && decode_slug_inner(&candidate, &tokens[split..], candidates, budget)
        {
            return true;
        }
    }
    false
}

/// Whether a transcript file lives in a `subagents/` directory.
fn is_subagent_transcript(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("subagents")
}

/// Derive the parent-session id for a swept transcript file from its location:
/// `…/<parent>/subagents/<child>.jsonl` belongs to `<parent>`; anything else
/// is a parent transcript whose file stem *is* the session id (which always
/// equals the `session_id` a live hook event would carry for that file).
fn sweep_parent_session_id(path: &Path) -> Option<String> {
    if is_subagent_transcript(path) {
        return path
            .parent()?
            .parent()?
            .file_name()?
            .to_str()
            .map(str::to_string);
    }
    path.file_stem()?.to_str().map(str::to_string)
}

fn cursor_subagent_paths(transcript_path: &Path, parent_session_id: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(parent_dir) = transcript_path.parent() {
        if transcript_path.file_stem().and_then(|stem| stem.to_str()) == Some(parent_session_id) {
            candidates.push(parent_dir.join(parent_session_id).join("subagents"));
        }
        if parent_dir.file_name().and_then(|name| name.to_str()) == Some(parent_session_id) {
            candidates.push(parent_dir.join("subagents"));
        }
    }

    let mut paths = Vec::new();
    let default_bounds = TranscriptDiscoveryBounds::default_walk();
    let mut remaining_bytes = default_bounds.max_discovery_bytes;
    for dir in candidates {
        let remaining_files = default_bounds.max_files.saturating_sub(paths.len());
        if remaining_files == 0 || remaining_bytes == 0 {
            break;
        }
        let bounds = TranscriptDiscoveryBounds {
            max_files: remaining_files,
            max_discovery_bytes: remaining_bytes,
            ..default_bounds
        };
        let report = collect_files_with_ext_bounded(&dir, "jsonl", 0, bounds);
        remaining_bytes = remaining_bytes.saturating_sub(report.bytes_charged);
        paths.extend(report.paths);
    }
    paths.sort();
    paths.dedup();
    paths
}

fn cursor_subagent_identity(path: &Path, parent_session_id: &str) -> Option<(String, String)> {
    let is_subagent_path = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("subagents");
    if !is_subagent_path {
        return None;
    }
    let parent_dir = path.parent()?.parent()?;
    if parent_dir.file_name().and_then(|name| name.to_str()) != Some(parent_session_id) {
        return None;
    }
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| !id.is_empty())?
        .to_string();
    Some((session_id.clone(), session_id))
}

fn parent_dispatch_model_for_subagent(
    path: &Path,
    parent_session_id: &str,
    agent_id: &str,
) -> Option<String> {
    let parent_dir = path.parent()?.parent()?;
    let candidates = [
        parent_dir.join(format!("{parent_session_id}.jsonl")),
        parent_dir.with_extension("jsonl"),
    ];
    for candidate in candidates {
        if let Some(model) = dispatch_model_for_agent(&candidate, agent_id) {
            return Some(model);
        }
    }
    None
}

fn dispatch_model_for_agent(path: &Path, agent_id: &str) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut frames = RawJsonlFrameReader::new(BufReader::new(file), MAX_JSONL_RECORD_BYTES);
    loop {
        let frame = frames.next_frame().ok()?;
        let record = match frame {
            RawJsonlFrame::Eof => return None,
            RawJsonlFrame::Complete { .. } | RawJsonlFrame::Partial { .. } => {
                let Ok(record) = serde_json::from_slice::<Value>(frames.record()) else {
                    continue;
                };
                record
            }
            RawJsonlFrame::Oversized { .. } | RawJsonlFrame::BudgetExhausted { .. } => {
                continue;
            }
        };
        if frames.record().is_empty() {
            continue;
        }
        let message = record.get("message").unwrap_or(&record);
        let content = message.get("content").unwrap_or(message);
        let Some(items) = content.as_array() else {
            continue;
        };
        for item in items {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            if is_subagent_dispatch_tool(name)
                && dispatch_targets_agent(item, agent_id)
                && let Some(model) = cursor_dispatch_model(item)
            {
                return Some(model);
            }
        }
    }
}

fn dispatch_targets_agent(item: &Value, agent_id: &str) -> bool {
    let input = item.get("input").unwrap_or(item);
    [
        "agent_id",
        "agentId",
        "subagent_id",
        "subagentId",
        "session_id",
        "sessionId",
        "id",
    ]
    .into_iter()
    .any(|key| {
        input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            == Some(agent_id)
    })
}

/// Per-line timestamp derivation for Cursor transcripts, which carry no
/// structured per-message timestamps. The injected `<timestamp>…</timestamp>`
/// tag in user prompts is parsed and carried forward across subsequent lines
/// (assistant turns happen after the prompt that started them); lines seen
/// before any tag fall back to the transcript file's mtime, which on the
/// incremental hook path approximates "now" for freshly appended lines.
pub struct TimestampCarry {
    carried: Option<i64>,
    fallback: Option<i64>,
}

impl TimestampCarry {
    pub fn new(fallback_mtime: Option<i64>) -> Self {
        Self {
            carried: None,
            fallback: fallback_mtime.filter(|mtime| *mtime > 0),
        }
    }

    /// Folds one transcript line into the carry and returns the timestamp to
    /// use for messages derived from that line.
    pub fn observe(&mut self, record: &Value) -> Option<i64> {
        if let Some(tag) = timestamp_tag_from_record(record) {
            self.carried = Some(tag);
        }
        self.carried.or(self.fallback)
    }
}

#[derive(Clone, Copy)]
struct CursorMessageContext<'a> {
    transcript_path: &'a Path,
    source_offset: i64,
    derived_timestamp: Option<i64>,
    model_fallback: Option<&'a str>,
    event_cwd: Option<&'a Path>,
    event_location_provenance: &'a str,
}

fn event_message(
    record: &Value,
    event: &Value,
    session_id: &str,
    ordinal: i64,
    context: CursorMessageContext<'_>,
) -> Option<SessionMessageRecord> {
    let role = record
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())?;
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    if content_is_only_subagent_dispatch(content) {
        return None;
    }
    let (text, tool_names) = content_storage_text_and_tools(
        content,
        message
            .get("tool_calls")
            .or_else(|| record.get("tool_calls")),
    );
    if text.trim().is_empty() {
        return None;
    }

    let message_id = record
        .get("id")
        .or_else(|| message.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || format!("{session_id}:{ordinal}"),
            std::string::ToString::to_string,
        );
    let model = cursor_record_message_model(record, message)
        .or_else(|| context.model_fallback.map(str::to_string))
        .or_else(|| cursor_model_string(event));

    Some(SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id,
        session_id: session_id.to_string(),
        role: role.to_string(),
        timestamp: record_timestamp(record)
            .or_else(|| record_timestamp(event))
            .or(context.derived_timestamp),
        ordinal,
        text,
        kind: content_kind(content).map(str::to_string),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(context.transcript_path.to_string_lossy().to_string()),
        source_offset: Some(context.source_offset),
        metadata_json: serde_json::to_string(&message_metadata(
            record,
            message,
            content,
            event,
            context.source_offset,
            context.event_cwd,
            context.event_location_provenance,
        ))
        .ok(),
    })
}

fn event_dispatch_messages(
    record: &Value,
    event: &Value,
    session_id: &str,
    context: CursorMessageContext<'_>,
) -> Vec<SessionMessageRecord> {
    let Some(role) = record
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
    else {
        return Vec::new();
    };
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    let Some(items) = content.as_array() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !is_subagent_dispatch_tool(name) {
            continue;
        }
        let Some(text) = dispatch_text(item) else {
            continue;
        };
        let tool_use_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let message_id = tool_use_id.map_or_else(
            || {
                format!(
                    "{}:tool_dispatch:{}:{index}",
                    session_id, context.source_offset
                )
            },
            |id| format!("{session_id}:tool_dispatch:{id}"),
        );
        out.push(SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id,
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp: record_timestamp(record)
                .or_else(|| record_timestamp(event))
                .or(context.derived_timestamp),
            ordinal: context.source_offset.saturating_add(index as i64),
            text,
            kind: Some("tool_dispatch".to_string()),
            model: cursor_dispatch_model(item)
                .or_else(|| cursor_record_message_model(record, message))
                .or_else(|| context.model_fallback.map(str::to_string))
                .or_else(|| cursor_model_string(event)),
            tool_names: Some(name.to_string()),
            source_path: Some(context.transcript_path.to_string_lossy().to_string()),
            source_offset: Some(context.source_offset),
            metadata_json: serde_json::to_string(&dispatch_message_metadata(
                record,
                event,
                context.source_offset,
                tool_use_id,
                context.event_cwd,
                context.event_location_provenance,
            ))
            .ok(),
        });
    }
    out
}

fn cursor_record_message_model(record: &Value, message: &Value) -> Option<String> {
    cursor_model_string(record).or_else(|| cursor_model_string(message))
}

fn content_is_only_subagent_dispatch(content: &Value) -> bool {
    let Some(items) = content.as_array() else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_use")
                && item
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(is_subagent_dispatch_tool)
        })
}

fn content_kind(content: &Value) -> Option<&'static str> {
    if content.is_array() {
        Some("message")
    } else if content.is_string() {
        Some("text")
    } else {
        None
    }
}

fn event_session_id(event: &Value, transcript_path: &Path) -> String {
    event
        .get("session_id")
        .or_else(|| event.get("conversation_id"))
        .or_else(|| event.get("chat_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || {
                transcript_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            },
            str::to_string,
        )
}

fn event_project(event: &Value) -> (String, String) {
    let cwd_root = event_cwd(event)
        .and_then(|cwd| tracedecay_runtime_core::config::discover_project_root(&cwd));
    let candidates = event_project_candidates(event);
    let resolved = candidates
        .iter()
        .find_map(|candidate| tracedecay_runtime_core::config::discover_project_root(candidate))
        .or_else(|| candidates.into_iter().next());
    let project_path = match (cwd_root, resolved) {
        (Some(cwd_root), Some(resolved)) if !paths_equal(&cwd_root, &resolved) => cwd_root,
        (Some(cwd_root), None) => cwd_root,
        (_, Some(resolved)) => resolved,
        _ => return ("unknown".to_string(), "unknown".to_string()),
    };
    let project = project_path.to_string_lossy().to_string();
    (project.clone(), project)
}

fn event_cwd(event: &Value) -> Option<PathBuf> {
    event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn event_project_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|seen| seen == &candidate) {
            candidates.push(candidate);
        }
    };
    if let Some(cwd) = event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        push_unique(PathBuf::from(cwd));
    }
    if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(file_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(transcript_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(roots) = event.get("workspace_roots").and_then(Value::as_array) {
        for root in roots {
            if let Some(path) = root.as_str().filter(|path| !path.is_empty()) {
                push_unique(PathBuf::from(path));
            }
        }
    }
    candidates
}

fn record_timestamp(value: &Value) -> Option<i64> {
    value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .and_then(|timestamp| {
            timestamp
                .as_i64()
                .or_else(|| timestamp.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
}

fn event_location_provenance(event: &Value) -> &str {
    event
        .get("tracedecay_location_provenance")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("hook_event")
}

fn session_metadata(event: &Value, event_cwd: Option<&Path>, location_provenance: &str) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("cursor_transcript".to_string()),
    );
    metadata.insert(
        "conversation_id".to_string(),
        event.get("conversation_id").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "hook_event_name".to_string(),
        event.get("hook_event_name").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "cursor_version".to_string(),
        event.get("cursor_version").cloned().unwrap_or(Value::Null),
    );
    if let Some(roots) = event.get("workspace_roots") {
        metadata.insert("workspace_roots".to_string(), roots.clone());
    }
    append_location_metadata(
        &mut metadata,
        CURSOR_EVENT_LOCATION_KEYS,
        TranscriptLocation::new(event_cwd, location_provenance),
    );
    Value::Object(metadata)
}

fn message_metadata(
    record: &Value,
    message: &Value,
    content: &Value,
    event: &Value,
    source_offset: i64,
    event_cwd: Option<&Path>,
    location_provenance: &str,
) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("cursor_transcript".to_string()),
    );
    metadata.insert(
        "raw_type".to_string(),
        record.get("type").cloned().unwrap_or(Value::Null),
    );
    append_host_event_ordering(&mut metadata, event, source_offset);
    append_location_metadata(
        &mut metadata,
        CURSOR_EVENT_LOCATION_KEYS,
        TranscriptLocation::new(event_cwd, location_provenance),
    );
    append_tool_calls_metadata(&mut metadata, message);
    append_tool_event_metadata(&mut metadata, content);
    // These JSONL agent-transcript lines carry no token counters (verified
    // across 100k+ real lines). Cursor *does* record per-turn token counts, but
    // only in the composer store (`state.vscdb` bubbles), which the richer
    // `cursor_composer` sweep reads and maps to `usage`. This probe stays as
    // future-proofing in case the JSONL format gains counters too.
    append_usage_metadata(&mut metadata, &[record, message]);
    Value::Object(metadata)
}

fn append_host_event_ordering(
    metadata: &mut serde_json::Map<String, Value>,
    event: &Value,
    transcript_offset: i64,
) {
    metadata.insert(
        "cursor_transcript_offset".to_string(),
        Value::from(transcript_offset),
    );
    if let Some(event_id) = ["event_id", "eventId"]
        .into_iter()
        .find_map(|key| event.get(key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
    {
        metadata.insert(
            "cursor_host_event_id".to_string(),
            Value::String(event_id.to_string()),
        );
    }
    if let Some(sequence) = ["event_sequence", "eventSequence", "sequence"]
        .into_iter()
        .find_map(|key| event.get(key))
        .filter(|value| value.is_i64() || value.is_u64() || value.is_string())
    {
        metadata.insert("cursor_host_event_sequence".to_string(), sequence.clone());
    }
    if let Some(timestamp) = record_timestamp(event) {
        metadata.insert(
            "cursor_host_event_timestamp".to_string(),
            Value::from(timestamp),
        );
    }
}

fn dispatch_message_metadata(
    record: &Value,
    event: &Value,
    source_offset: i64,
    tool_use_id: Option<&str>,
    event_cwd: Option<&Path>,
    location_provenance: &str,
) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("cursor_transcript".to_string()),
    );
    metadata.insert(
        "raw_type".to_string(),
        record.get("type").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "tool_use_id".to_string(),
        tool_use_id.map_or(Value::Null, |id| Value::String(id.to_string())),
    );
    append_host_event_ordering(&mut metadata, event, source_offset);
    append_location_metadata(
        &mut metadata,
        CURSOR_EVENT_LOCATION_KEYS,
        TranscriptLocation::new(event_cwd, location_provenance),
    );
    Value::Object(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::test_support::PanicHostAdmission;
    use serde_json::json;

    #[tokio::test]
    async fn cancelled_startup_sweep_defers_before_admitting_cursor_jsonl() {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let project_id = ProjectId::new("project.cursor-cancelled-startup").unwrap();
        let slug = cursor_project_slug(project.path()).unwrap();
        let transcript_dir = home
            .path()
            .join(".cursor")
            .join("projects")
            .join(slug)
            .join("agent-transcripts")
            .join("session-cancelled");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(
            transcript_dir.join("session-cancelled.jsonl"),
            r#"{"role":"user","message":{"content":"must not ingest"}}"#,
        )
        .unwrap();
        let source = CursorSweepSource::with_home(home.path());
        let cancellation = ObservationCancellation::default();
        cancellation.cancel();

        let outcome = admit_cursor_sweep_observations_with_admission(
            &source,
            project.path(),
            &PanicHostAdmission,
            None,
            ObservationScopeV1::Project { project_id },
            &cancellation,
        )
        .await
        .unwrap();

        assert_eq!(outcome.sessions_upserted, 0);
        assert_eq!(outcome.messages_upserted, 0);
        assert_eq!(outcome.bytes_consumed, 0);
        assert!(outcome.source_deferred);
    }

    #[test]
    fn host_event_ordering_is_kept_distinct_from_transcript_ordering() {
        let event = json!({
            "event_id": "evt-redacted",
            "event_sequence": 41,
            "timestamp": 1_783_500_600_i64,
        });
        let mut metadata = serde_json::Map::new();
        append_host_event_ordering(&mut metadata, &event, 128);
        assert_eq!(metadata["cursor_host_event_id"], "evt-redacted");
        assert_eq!(metadata["cursor_host_event_sequence"], 41);
        assert_eq!(metadata["cursor_host_event_timestamp"], 1_783_500_600_i64);
        assert_eq!(metadata["cursor_transcript_offset"], 128);
    }

    #[test]
    fn native_record_identity_is_stable_across_json_formatting() {
        let compact: Value = serde_json::from_str(
            r#"{"role":"assistant","message":{"content":"redacted fixture"}}"#,
        )
        .unwrap();
        let spaced: Value = serde_json::from_str(
            r#"{ "message": { "content": "redacted fixture" }, "role": "assistant" }"#,
        )
        .unwrap();
        assert_eq!(
            observation_native_record_id("cursor", "session-redacted", &compact)
                .unwrap()
                .as_str(),
            observation_native_record_id("cursor", "session-redacted", &spaced)
                .unwrap()
                .as_str()
        );
    }

    #[test]
    fn canonical_cursor_record_keeps_typed_tools_and_structured_content() {
        let native = json!({
            "role": "assistant",
            "cwd": "/secret/worktree",
            "workspace_roots": ["/secret/worktree"],
            "message": {
                "content": [
                    {"type": "text", "text": "redacted answer"},
                    {
                        "type": "tool_use",
                        "id": "tool-redacted",
                        "name": "Read",
                        "input": {"path": "/secret/worktree/file.rs", "token": "credential-redacted"}
                    },
                    {"type": "thinking", "thinking": "provider-visible summary"}
                ]
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(10, 90).unwrap();
        let record_id =
            observation_native_record_id("cursor", "session-redacted", &native).unwrap();
        let envelope = normalize_cursor_observation(
            &native,
            "session-redacted",
            record_id.clone(),
            range,
            None,
            None,
        )
        .unwrap();
        let rendered = format!("{envelope:?}");
        assert!(rendered.contains("Message"));
        assert!(rendered.contains("ToolInvocation"));
        assert!(rendered.contains("Reasoning"));
        assert!(rendered.contains("FileBytes"));
        assert!(rendered.contains(record_id.as_str()));
        assert!(rendered.contains("/secret/worktree/file.rs"));
        assert!(rendered.contains("credential-redacted"));
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert!(relations.get("thread_id").is_none());
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
    }

    #[test]
    fn cursor_conversation_id_sets_thread_relation_without_inventing_turn() {
        let native = json!({
            "role": "user",
            "conversation_id": "conversation-native",
            "message": {"content": [{"type": "text", "text": "hello"}]}
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 20).unwrap();
        let record_id =
            observation_native_record_id("cursor", "conversation-native", &native).unwrap();
        let envelope = normalize_cursor_observation(
            &native,
            "conversation-native",
            record_id.clone(),
            range,
            None,
            None,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["thread_id"], "conversation-native");
        assert_eq!(relations["message_id"], record_id.as_str());
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
    }

    #[test]
    fn cursor_subagent_lineage_sets_native_agent_relations() {
        let native = json!({
            "role": "assistant",
            "conversation_id": "child-agent",
            "message": {"content": [{"type": "text", "text": "subagent reply"}]}
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 20).unwrap();
        let record_id = observation_native_record_id("cursor", "child-agent", &native).unwrap();
        let envelope = normalize_cursor_observation(
            &native,
            "child-agent",
            record_id,
            range,
            Some("child-agent"),
            Some("parent-conversation"),
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["thread_id"], "child-agent");
        assert_eq!(relations["agent_id"], "child-agent");
        assert_eq!(relations["parent_agent_id"], "parent-conversation");
        assert!(relations.get("turn_id").is_none());
    }

    /// Exact assistant+`tool_use` JSONL shape from
    /// `tests/transcript_ingest_suite/cursor.rs`
    /// (`cursor_tool_use_blocks_populate_tool_event_metadata`). Provider-parser
    /// evidence is the native `role`/`message.content[]` Cursor transcript
    /// record; the expected output is the canonical envelope projection with
    /// explicit Cursor provider provenance — not a generic hand-built record.
    #[test]
    fn fixture_backed_cursor_jsonl_tool_use_reaches_canonical_envelope() {
        let native: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/cursor/tool_use.input.json"
        ))
        .expect("Cursor golden input");
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/cursor/tool_use.expected_envelope.json"
        ))
        .expect("Cursor golden expected envelope");
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 64).unwrap();
        let record_id =
            observation_native_record_id("cursor", "cursor-tool-fixture", &native).unwrap();
        let envelope = normalize_cursor_observation(
            &native,
            "cursor-tool-fixture",
            record_id.clone(),
            range,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            envelope.provider().as_str(),
            expected["provider"].as_str().unwrap()
        );
        assert_eq!(
            envelope.native_record_kind(),
            expected["native_record_kind"].as_str().unwrap()
        );
        assert_eq!(envelope.stable_record_id().as_str(), record_id.as_str());
        let actual = serde_json::to_value(&envelope).unwrap();
        assert_eq!(actual["version"], expected["version"]);
        assert_eq!(actual["evidence"], expected["evidence"]);
        let relations = actual["relations"].as_object().unwrap();
        assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
        assert_eq!(relations["message_id"], record_id.as_str());
        for absent in expected["relations"]["absent"].as_array().unwrap() {
            assert!(relations.get(absent.as_str().unwrap()).is_none());
        }
        let facts = actual["facts"].as_array().unwrap();
        assert!(facts.iter().any(|fact| fact["kind"] == "session"));
        assert!(facts.iter().any(|fact| {
            fact["kind"] == "message" && fact["content"] == native["message"]["content"]
        }));
        assert!(facts.iter().any(|fact| {
            fact["kind"] == "tool_invocation"
                && fact["arguments"] == native["message"]["content"][1]["input"]
        }));
        assert!(
            envelope.facts().iter().all(|fact| {
                !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. })
            }),
            "Cursor JSONL fixture must not emit WorkflowLifecycle without native lifecycle evidence"
        );
    }

    #[test]
    fn fixture_backed_cursor_workflow_lookalike_emits_no_workflow_lifecycle() {
        let native: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/cursor/workflow_lookalike.input.json"
        ))
        .expect("Cursor workflow lookalike input");
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/provider_normalization/cursor/workflow_lookalike.expected_envelope.json"
        ))
        .expect("Cursor workflow lookalike expected");
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 64).unwrap();
        let record_id =
            observation_native_record_id("cursor", "cursor-workflow-lookalike", &native).unwrap();
        let envelope = normalize_cursor_observation(
            &native,
            "cursor-workflow-lookalike",
            record_id,
            range,
            None,
            None,
        )
        .unwrap();
        let actual = serde_json::to_value(&envelope).unwrap();
        let facts = actual["facts"].as_array().unwrap();
        assert!(facts.iter().any(|fact| {
            fact["kind"] == "message"
                && fact["content"].as_str() == expected["expected_message"].as_str()
        }));
        for forbidden in expected["forbidden_fact_kinds"].as_array().unwrap() {
            assert!(
                facts.iter().all(|fact| fact["kind"] != *forbidden),
                "forbidden fact kind {forbidden} must remain absent"
            );
        }
        assert!(
            envelope.facts().iter().all(|fact| {
                !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. })
            }),
            "Cursor JSONL workflow lookalikes must not become WorkflowLifecycle"
        );
        let rendered = actual.to_string();
        for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
            assert!(
                !rendered.contains(rejected.as_str().unwrap()),
                "{rejected} must not survive Cursor JSONL normalization"
            );
        }
    }
}
