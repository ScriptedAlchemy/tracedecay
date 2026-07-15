use std::fmt::Write as _;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1, CanonicalWorkflowEvidenceKindV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::global_db::GlobalDb;
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::sessions::SessionMessageRecord;
use crate::sessions::shared::{
    StoredCursor, TranscriptLocation, TranscriptLocationMetadataKeys, append_location_metadata,
    append_tool_calls_metadata, append_tool_event_metadata, append_usage_metadata,
    content_storage_text_and_tools, paths_equal, title_from_messages,
};
use crate::sessions::source::{
    MAX_JSONL_RECORD_BYTES, ParsedTranscript, SessionDraft, TranscriptIngestError,
    TranscriptIngestResult, TranscriptSource, collect_files_with_ext, stream_new_jsonl,
    try_stream_new_jsonl_raw_strict_with_resume,
};
use crate::storage::{
    SESSIONS_DB_FILENAME, default_profile_project_id, default_profile_root,
    profile_sharded_data_root, resolve_layout_for_current_profile,
};
const PROJECT_SESSION_DB_FILENAME: &str = SESSIONS_DB_FILENAME;
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
}

pub fn project_session_db_path(project_root: &Path) -> PathBuf {
    resolve_layout_for_current_profile(project_root).map_or_else(
        |_| {
            let profile_root = default_profile_root()
                .unwrap_or_else(|_| PathBuf::from(crate::config::TRACEDECAY_DIR));
            profile_sharded_data_root(&profile_root, &default_profile_project_id(project_root))
                .join(PROJECT_SESSION_DB_FILENAME)
        },
        |layout| layout.sessions_db_path,
    )
}

pub async fn open_project_session_db(project_root: &Path) -> Option<GlobalDb> {
    let db_path = resolved_project_session_db_path(project_root).await?;
    GlobalDb::open_at(&db_path).await
}

pub async fn resolved_project_session_db_path(project_root: &Path) -> Option<PathBuf> {
    match crate::storage::read_enrollment_marker(project_root) {
        Ok(Some(_)) => {
            return resolve_layout_for_current_profile(project_root)
                .ok()
                .map(|layout| layout.sessions_db_path);
        }
        Ok(None) => {}
        Err(_) => return None,
    }
    if let Some(db_path) = registry_profile_session_db_path(project_root).await {
        return Some(db_path);
    }
    resolve_layout_for_current_profile(project_root)
        .ok()
        .map(|layout| layout.sessions_db_path)
}

async fn registry_profile_session_db_path(project_root: &Path) -> Option<PathBuf> {
    let profile_root = crate::storage::default_profile_root().ok()?;
    let global = GlobalDb::open().await?;
    let git_common_dir = (!crate::worktree::is_detached_linked_worktree(project_root))
        .then(|| crate::worktree::git_common_dir(project_root))
        .flatten();
    // Mirror the graph store's identity-then-unique-remote fallback
    // (`resolve_store_layout_for_project` in tracedecay/lifecycle.rs) so a
    // renamed/moved checkout keeps routing its session history to the store it
    // was originally registered under, instead of silently forking a fresh
    // session DB at a new default path.
    let resolution = if let Some(resolution) = global
        .resolve_project_store_by_identity(project_root, git_common_dir.as_deref())
        .await
    {
        resolution
    } else {
        let remote = crate::tracedecay::git_remote_url(project_root)?;
        let resolution = global
            .resolve_unique_project_store_by_git_remote(&remote)
            .await?;
        // Remote uniqueness alone cannot tell a renamed checkout (whose
        // original registered location no longer exists on disk) apart from
        // a second, still-present clone of the same remote. Only borrow the
        // registered store when the original checkout is gone, so a live
        // clone never inherits another checkout's session history.
        if registered_checkout_present(&resolution.project) {
            return None;
        }
        resolution
    };
    if resolution.store.storage_mode != "profile_sharded" {
        return None;
    }
    Some(
        profile_root
            .join(resolution.store.store_relpath)
            .join(PROJECT_SESSION_DB_FILENAME),
    )
}

/// Returns `true` when the checkout a registered project was recorded at still
/// exists on disk. A renamed/moved checkout leaves neither its canonical root
/// nor its git common dir behind, whereas a separate clone of the same remote
/// leaves the original checkout in place.
fn registered_checkout_present(project: &crate::global_db::CodeProjectRecord) -> bool {
    let roots = [
        Some(project.canonical_root.as_str()),
        Some(project.display_root.as_str()),
        project.git_common_dir.as_deref(),
    ];
    roots
        .into_iter()
        .flatten()
        .filter(|root| !root.is_empty())
        .any(|root| Path::new(root).exists())
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
        preflight_cursor_jsonl(path, prev, max_new_bytes)?;
        Ok(self.parse_new(path, prev, project_root, max_new_bytes))
    }
}

fn preflight_cursor_jsonl(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<()> {
    let frames = try_stream_new_jsonl_raw_strict_with_resume(
        path,
        prev,
        max_new_bytes,
        MAX_JSONL_RECORD_BYTES,
        None,
    )?;
    if let Some(crate::sessions::source::JsonlFrameDeferral::Malformed { offset }) = frames.deferred
    {
        return Err(TranscriptIngestError::NonDurableRecord {
            provider: "cursor",
            offset,
            end_offset: frames.read_through.max(offset),
            reason: "malformed_jsonl_frame",
        });
    }
    for frame in frames.frames {
        if serde_json::from_slice::<Value>(&frame.bytes).is_err() {
            return Err(TranscriptIngestError::NonDurableRecord {
                provider: "cursor",
                offset: frame.offset,
                end_offset: frame.end_offset,
                reason: "malformed_jsonl_frame",
            });
        }
    }
    Ok(())
}

const CURSOR_OBSERVATION_RETENTION: &str = "retention.provider-observation";

async fn admit_cursor_jsonl_observations(
    event: &Value,
    parent_session_id: &str,
    path: &Path,
    db: &GlobalDb,
    user_scope: bool,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<()> {
    let subagent = cursor_subagent_identity(path, parent_session_id);
    let session_id = subagent
        .as_ref()
        .map_or(parent_session_id, |(session_id, _)| session_id.as_str());
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("cursor")?,
        SessionId::new(session_id)?,
    )?;
    let scope = cursor_observation_scope(event, user_scope)?;
    let authorities = if user_scope {
        HostAdmissionAuthorities::new(None, Some(db))
    } else {
        HostAdmissionAuthorities::new(Some(db), None)
    };
    let admission = HostAdmissionFacade::new(authorities);
    let mut expected_cursor = admission
        .get_source_cursor(&source, &scope)
        .await
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "cursor" })?;
    let previous = expected_cursor.as_ref().map_or(
        StoredCursor {
            position: 0,
            mtime: 0,
            file_id: 0,
        },
        |cursor| StoredCursor {
            position: cursor.position(),
            mtime: 0,
            file_id: cursor.generation().generation_id(),
        },
    );
    let resume_state = expected_cursor.as_ref().and_then(|cursor| {
        Some(crate::sessions::source::JsonlResumeState {
            generation: cursor.generation().generation_id(),
            file_identity: cursor.file_identity()?,
            fingerprint: cursor.resume_fingerprint()?,
        })
    });
    let raw = try_stream_new_jsonl_raw_strict_with_resume(
        path,
        previous,
        max_new_bytes,
        MAX_JSONL_RECORD_BYTES,
        resume_state,
    )?;
    let generation = ObservationSourceGenerationV1::new(raw.new_cursor.file_id)?;

    for frame in raw.frames {
        let range =
            tracedecay_domain::ObservationSourceRangeV1::new(frame.offset, frame.end_offset)?;
        let mut stable_record_id = None;
        let mut unsupported_record = false;
        let parsed = parse_normalized_observation_record_v1(
            &frame.bytes,
            range,
            ObservationOrderingDomainV1::FileBytes,
            |native| {
                if native.get("role").and_then(Value::as_str).is_none() {
                    unsupported_record = true;
                    return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                }
                let record_id = observation_native_record_id("cursor", session_id, &native)
                    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                let envelope =
                    normalize_cursor_observation(&native, session_id, record_id.clone(), range)?;
                stable_record_id = Some(record_id);
                Ok(envelope)
            },
        )
        .map_err(|_| TranscriptIngestError::NonDurableRecord {
            provider: "cursor",
            offset: frame.offset,
            end_offset: frame.end_offset,
            reason: if unsupported_record {
                "unknown_cursor_record_type"
            } else {
                "invalid_cursor_record"
            },
        })?;
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            scope.clone(),
            generation,
            range,
            ObservationOrderingDomainV1::FileBytes,
            stable_record_id
                .ok_or(TranscriptIngestError::InvalidFrameState { provider: "cursor" })?,
        )?;
        let request = CaptureObservationRequest::new(
            parsed,
            identity,
            expected_cursor.clone(),
            RetentionClass::new(CURSOR_OBSERVATION_RETENTION)?,
            ObservationCancellation::default(),
        )
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "cursor" })?
        .with_resume_checkpoint(raw.file_identity, frame.resume_fingerprint);
        let non_durable = match admission.capture_observation(request).await {
            Ok(CaptureObservationOutcome::Persisted { outcome, .. }) => {
                match outcome {
                    ObservationPersistOutcome::Committed(_)
                    | ObservationPersistOutcome::ExactDuplicate(_)
                    | ObservationPersistOutcome::CoveredDuplicate(_) => {
                        if expected_cursor.as_ref().is_none_or(|cursor| {
                            cursor.generation() != generation
                                || cursor.position() < frame.end_offset
                        }) {
                            expected_cursor = Some(
                                ObservationSourceCursorV1::for_ordering(
                                    source.clone(),
                                    scope.clone(),
                                    generation,
                                    ObservationOrderingDomainV1::FileBytes,
                                    frame.end_offset,
                                )?
                                .with_resume_checkpoint(
                                    raw.file_identity,
                                    frame.resume_fingerprint,
                                ),
                            );
                        }
                    }
                }
                None
            }
            Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => Some((
                receipt,
                ObservationCoverageReason::SanitizerRejected,
                "privacy_rejected_cursor_record",
            )),
            Ok(CaptureObservationOutcome::Quarantined { receipt, .. }) => Some((
                receipt,
                ObservationCoverageReason::SanitizerQuarantined,
                "privacy_quarantined_cursor_record",
            )),
            Err(outcome) => {
                return Err(TranscriptIngestError::NonDurableRecord {
                    provider: "cursor",
                    offset: frame.offset,
                    end_offset: frame.end_offset,
                    reason: outcome.reason_code.unwrap_or("host_admission_incomplete"),
                });
            }
        };
        if let Some((receipt, coverage_reason, error_reason)) = non_durable {
            let advance = ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
                source.clone(),
                scope.clone(),
                generation,
                ObservationOrderingDomainV1::FileBytes,
                expected_cursor.clone(),
                range,
                coverage_reason,
                receipt,
            )
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "cursor" })?
            .with_resume_checkpoint(raw.file_identity, frame.resume_fingerprint);
            admission
                .advance_non_durable_source_cursor(advance, ObservationCancellation::default())
                .await
                .map_err(|outcome| TranscriptIngestError::NonDurableRecord {
                    provider: "cursor",
                    offset: frame.offset,
                    end_offset: frame.end_offset,
                    reason: outcome
                        .reason_code
                        .unwrap_or("non_durable_cursor_advance_failed"),
                })?;
            return Err(TranscriptIngestError::NonDurableRecord {
                provider: "cursor",
                offset: frame.offset,
                end_offset: frame.end_offset,
                reason: error_reason,
            });
        }
    }
    Ok(())
}

fn cursor_observation_scope(
    event: &Value,
    user_scope: bool,
) -> TranscriptIngestResult<ObservationScopeV1> {
    if user_scope {
        return Ok(ObservationScopeV1::Profile);
    }
    let (_, project_path) = event_project(event);
    Ok(ObservationScopeV1::Project {
        project_id: ProjectId::new(default_profile_project_id(Path::new(&project_path)))?,
    })
}

fn normalize_cursor_observation(
    native: &Value,
    session_id: &str,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let native_kind = native
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or("message");
    let message = native.get("message").filter(|message| message.is_object());
    let content = message
        .and_then(|message| message.get("content"))
        .or_else(|| native.get("content"))
        .or_else(|| native.get("message").filter(|message| !message.is_object()));
    let timestamp = record_timestamp(native).or_else(|| timestamp_tag_from_record(native));
    let relations = CanonicalObservationRelationsV1::new(
        SessionId::new(session_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    )
    .with_message_id(stable_record_id.clone());
    let mut facts = Vec::new();

    if let Some(content) = content {
        if let Some(message_content) = canonical_cursor_message_content(content) {
            facts.push(CanonicalObservationFactV1::Message {
                role: canonical_cursor_role(native.get("role").and_then(Value::as_str)),
                content: message_content,
                model: cursor_record_message_model(native, message.unwrap_or(native)),
                timestamp,
            });
        }
        append_cursor_content_facts(content, &stable_record_id, &mut facts);
    }
    append_cursor_usage_fact(native, message, &mut facts);
    append_cursor_git_facts(native, &mut facts);

    if native_kind.to_ascii_lowercase().contains("compact") {
        facts.push(CanonicalObservationFactV1::Compaction {
            summary: content.and_then(canonical_cursor_message_content),
            input_tokens: None,
            output_tokens: None,
        });
    }
    if facts.is_empty() {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: native_kind.to_string(),
            state: CanonicalUnknownStateV1::Absent,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new("cursor")
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        native_kind,
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn canonical_cursor_message_content(content: &Value) -> Option<Value> {
    match content {
        Value::String(text) if !text.trim().is_empty() => Some(Value::String(text.clone())),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter(|item| {
                    !matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("tool_use" | "tool_result" | "thinking" | "reasoning")
                    )
                })
                .filter_map(|item| {
                    item.get("text")
                        .or_else(|| item.get("content"))
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            (!text.is_empty()).then(|| Value::Array(text.into_iter().map(Value::String).collect()))
        }
        Value::Object(map) => map
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(|text| Value::String(text.to_string())),
        _ => None,
    }
}

fn append_cursor_content_facts(
    content: &Value,
    stable_record_id: &ObservationId,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let Some(items) = content.as_array() else {
        return;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let invocation_id = canonical_cursor_observation_id(
                    item.get("id").and_then(Value::as_str),
                    stable_record_id,
                );
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or("tool")
                    .to_string();
                facts.push(CanonicalObservationFactV1::ToolInvocation {
                    invocation_id,
                    name: name.clone(),
                    arguments: Value::Null,
                });
                if is_subagent_dispatch_tool(&name) {
                    facts.push(CanonicalObservationFactV1::Workflow {
                        evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                        reference: item.get("id").and_then(Value::as_str).map(str::to_string),
                        content: None,
                    });
                }
            }
            Some("tool_result") => {
                facts.push(CanonicalObservationFactV1::ToolResult {
                    invocation_id: item
                        .get("tool_use_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .map(|id| canonical_cursor_observation_id(Some(id), stable_record_id)),
                    content: Value::Null,
                    success: item
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .map(|error| !error),
                });
            }
            Some("thinking" | "reasoning") => {
                let content = item
                    .get("text")
                    .or_else(|| item.get("thinking"))
                    .filter(|content| !content.is_null())
                    .cloned();
                facts.push(CanonicalObservationFactV1::Reasoning {
                    visibility: if content.is_some() {
                        CanonicalReasoningVisibilityV1::Visible
                    } else {
                        CanonicalReasoningVisibilityV1::Unavailable
                    },
                    content,
                });
            }
            _ => {}
        }
    }
}

fn append_cursor_usage_fact(
    native: &Value,
    message: Option<&Value>,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let usage = native
        .get("usage")
        .or_else(|| native.get("tokenCount"))
        .or_else(|| message.and_then(|message| message.get("usage")))
        .or_else(|| message.and_then(|message| message.get("tokenCount")));
    let Some(usage) = usage else {
        return;
    };
    let input_tokens = cursor_canonical_u64(
        usage
            .get("input_tokens")
            .or_else(|| usage.get("inputTokens")),
    );
    let output_tokens = cursor_canonical_u64(
        usage
            .get("output_tokens")
            .or_else(|| usage.get("outputTokens")),
    );
    let cache_read_tokens = cursor_canonical_u64(
        usage
            .get("cache_read_tokens")
            .or_else(|| usage.get("cacheReadTokens")),
    );
    let cache_write_tokens = cursor_canonical_u64(
        usage
            .get("cache_write_tokens")
            .or_else(|| usage.get("cacheWriteTokens")),
    );
    let reasoning_tokens = cursor_canonical_u64(
        usage
            .get("reasoning_tokens")
            .or_else(|| usage.get("reasoningTokens")),
    );
    if input_tokens.is_some()
        || output_tokens.is_some()
        || cache_read_tokens.is_some()
        || cache_write_tokens.is_some()
        || reasoning_tokens.is_some()
    {
        facts.push(CanonicalObservationFactV1::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        });
    }
}

fn append_cursor_git_facts(native: &Value, facts: &mut Vec<CanonicalObservationFactV1>) {
    if let Some(branch) = native
        .get("branch")
        .or_else(|| native.pointer("/git/branch"))
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Branch,
            reference: Some(branch.to_string()),
            content: None,
        });
    }
    if let Some(commit) = native
        .get("commit")
        .or_else(|| native.get("commit_hash"))
        .or_else(|| native.pointer("/git/commit"))
        .and_then(Value::as_str)
        .filter(|commit| !commit.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Commit,
            reference: Some(commit.to_string()),
            content: None,
        });
    }
    if native
        .get("gitDiffs")
        .and_then(Value::as_array)
        .is_some_and(|diffs| !diffs.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Diff,
            reference: None,
            content: None,
        });
    }
    if let Some(pull_requests) = native.get("pullRequests").and_then(Value::as_array) {
        for pull_request in pull_requests {
            let reference = ["url", "htmlUrl", "html_url", "id"]
                .into_iter()
                .find_map(|key| pull_request.get(key).and_then(Value::as_str))
                .map(str::to_string);
            facts.push(CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
                reference: reference.clone(),
                content: None,
            });
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::PullRequest,
                reference,
                content: None,
            });
        }
    }
}

fn canonical_cursor_role(role: Option<&str>) -> CanonicalMessageRoleV1 {
    match role {
        Some("user") => CanonicalMessageRoleV1::User,
        Some("assistant") => CanonicalMessageRoleV1::Assistant,
        Some("system" | "developer") => CanonicalMessageRoleV1::System,
        Some("tool") => CanonicalMessageRoleV1::Tool,
        _ => CanonicalMessageRoleV1::Unknown,
    }
}

fn canonical_cursor_observation_id(
    native_id: Option<&str>,
    fallback: &ObservationId,
) -> ObservationId {
    native_id
        .and_then(|native_id| ObservationId::new(native_id).ok())
        .unwrap_or_else(|| fallback.clone())
}

fn cursor_canonical_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

fn observation_native_record_id(
    provider: &str,
    session_id: &str,
    value: &Value,
) -> TranscriptIngestResult<ObservationId> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.provider-native-record.v1\0");
    hasher.update(provider.as_bytes());
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    hasher.update(
        serde_json::to_vec(value)
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "cursor" })?,
    );
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: "cursor" })?;
    }
    Ok(ObservationId::new(format!(
        "{provider}.native.sha256:{encoded}"
    ))?)
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
/// hooks should pass the resolved project DB from [`open_project_session_db`].
///
/// Ingestion is **incremental**: it resumes from the byte offset recorded in the
/// DB's `parse_offsets` table (via the shared [`crate::sessions::source`]
/// driver), so each call only parses and upserts transcript lines appended since
/// the last run rather than re-reading the whole file. Repeated calls on an
/// unchanged file are a no-op.
pub async fn ingest_cursor_transcript_event(
    event_json: &str,
    db: &GlobalDb,
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(&try_ingest_cursor_transcript_event(event_json, db).await)
}

pub async fn try_ingest_cursor_transcript_event(
    event_json: &str,
    db: &GlobalDb,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_transcript_event_capped(event_json, db, None).await
}

/// Like [`ingest_cursor_transcript_event`], but bounds how many newly-appended
/// bytes a single call will read. Cursor hooks pass byte caps to stay within hook
/// budgets; capped reads still discover subagent transcript files, with each file
/// independently subject to the same cap.
pub async fn ingest_cursor_transcript_event_capped(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(
        &try_ingest_cursor_transcript_event_capped(event_json, db, max_new_bytes).await,
    )
}

pub async fn try_ingest_cursor_transcript_event_capped(
    event_json: &str,
    db: &GlobalDb,
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
    let parent_session_id = event_session_id(&source.event, &source.transcript_path);
    for path in source.transcript_paths(&project_root) {
        admit_cursor_jsonl_observations(
            &source.event,
            &parent_session_id,
            &path,
            db,
            false,
            max_new_bytes,
        )
        .await?;
    }
    drain_cursor_observation_projections(db).await
}

pub async fn ingest_cursor_user_transcript_event_capped(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(
        &try_ingest_cursor_user_transcript_event_capped(event_json, db, max_new_bytes).await,
    )
}

pub async fn try_ingest_cursor_user_transcript_event_capped(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_user_transcript_event_capped_with_registered_roots(
        event_json,
        db,
        max_new_bytes,
        &[],
    )
    .await
}

/// User-scope live ingest guarded by a registry snapshot. The unguarded
/// wrapper remains useful for isolated parsing without a profile registry.
pub async fn ingest_cursor_user_transcript_event_capped_with_registered_roots(
    event_json: &str,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
    registered_roots: &[PathBuf],
) -> CursorTranscriptIngestStats {
    cursor_ingest_or_default(
        &try_ingest_cursor_user_transcript_event_capped_with_registered_roots(
            event_json,
            db,
            max_new_bytes,
            registered_roots,
        )
        .await,
    )
}

pub async fn try_ingest_cursor_user_transcript_event_capped_with_registered_roots(
    event_json: &str,
    db: &GlobalDb,
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
    let parent_session_id = event_session_id(&source.event, &source.transcript_path);
    for path in source.transcript_paths(&placeholder) {
        admit_cursor_jsonl_observations(
            &source.event,
            &parent_session_id,
            &path,
            db,
            true,
            max_new_bytes,
        )
        .await?;
    }
    drain_cursor_observation_projections(db).await
}

/// Canonically admit Cursor JSONL transcripts discovered during a project startup
/// sweep. Composer-owned session ids are skipped before discovery results reach
/// observation admission.
pub async fn try_ingest_cursor_project_sweep_capped<S: BuildHasher>(
    project_root: &Path,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
    skip_session_ids: std::collections::HashSet<String, S>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    let Some(source) = CursorSweepSource::new() else {
        return Ok(CursorTranscriptIngestStats::default());
    };
    admit_cursor_sweep_observations(
        &source.with_skip_session_ids(skip_session_ids.into_iter().collect()),
        project_root,
        db,
        max_new_bytes,
        false,
    )
    .await
}

/// Canonically admit Cursor JSONL transcripts discovered during a profile startup
/// sweep. Registered project slugs and composer-owned session ids are excluded
/// before observation admission.
pub async fn try_ingest_cursor_user_sweep_capped<S: BuildHasher>(
    db: &GlobalDb,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
    skip_session_ids: std::collections::HashSet<String, S>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    let Some(source) = CursorSweepSource::new() else {
        return Ok(CursorTranscriptIngestStats::default());
    };
    admit_cursor_sweep_observations(
        &source
            .with_skip_session_ids(skip_session_ids.into_iter().collect())
            .for_user_scope(registered_roots),
        Path::new(""),
        db,
        max_new_bytes,
        true,
    )
    .await
}

async fn admit_cursor_sweep_observations(
    source: &CursorSweepSource,
    project_root: &Path,
    db: &GlobalDb,
    max_new_bytes: Option<u64>,
    user_scope: bool,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    for path in source.transcript_paths(project_root) {
        let Some(parent_session_id) = sweep_parent_session_id(&path) else {
            continue;
        };
        let event = cursor_sweep_event(&parent_session_id, project_root, user_scope);
        admit_cursor_jsonl_observations(
            &event,
            &parent_session_id,
            &path,
            db,
            user_scope,
            max_new_bytes,
        )
        .await?;
    }
    drain_cursor_observation_projections(db).await
}

async fn drain_cursor_observation_projections(
    db: &GlobalDb,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    let stats = crate::sessions::claude_observation::drain_projection_queue(
        db,
        &ObservationCancellation::default(),
    )
    .await
    .map_err(|error| match error {
        crate::sessions::claude_observation::ClaudeObservationIngestError::Transcript(error) => {
            error
        }
        _ => TranscriptIngestError::InvalidFrameState { provider: "cursor" },
    })?;
    Ok(CursorTranscriptIngestStats {
        sessions_upserted: stats.transcript.sessions_upserted,
        messages_upserted: stats.transcript.messages_upserted,
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
        let root = crate::config::discover_project_root(&candidate).unwrap_or(candidate);
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
    /// ([`crate::sessions::cursor_composer`]). Transcript files whose stem is
    /// one of these are skipped so the two Cursor sources never double-ingest.
    skip_session_ids: std::collections::HashSet<String>,
    user_registered_slugs: Option<std::collections::HashSet<String>>,
}

impl CursorSweepSource {
    /// Source rooted at the real `~/.cursor/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
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
            return entries
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|slug| !registered_slugs.contains(slug))
                })
                .flat_map(|entry| {
                    collect_files_with_ext(
                        &entry.path().join("agent-transcripts"),
                        "jsonl",
                        MAX_SWEEP_SCAN_DEPTH,
                    )
                })
                .collect();
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
                eprintln!(
                    "Skipping Cursor transcript sweep for {}: project slug '{slug}' is ambiguous \
                     (another existing directory also encodes to it).",
                    project_root.display()
                );
                return Vec::new();
            }
        }
        let files = collect_files_with_ext(&transcripts_dir, "jsonl", MAX_SWEEP_SCAN_DEPTH);
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
        preflight_cursor_jsonl(path, prev, max_new_bytes)?;
        Ok(self.parse_new(path, prev, project_root, max_new_bytes))
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
    for dir in candidates {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                paths.push(path);
            }
        }
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
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
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
    None
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
pub(crate) struct TimestampCarry {
    carried: Option<i64>,
    fallback: Option<i64>,
}

impl TimestampCarry {
    pub(crate) fn new(fallback_mtime: Option<i64>) -> Self {
        Self {
            carried: None,
            fallback: fallback_mtime.filter(|mtime| *mtime > 0),
        }
    }

    /// Folds one transcript line into the carry and returns the timestamp to
    /// use for messages derived from that line.
    pub(crate) fn observe(&mut self, record: &Value) -> Option<i64> {
        if let Some(tag) = timestamp_tag_from_record(record) {
            self.carried = Some(tag);
        }
        self.carried.or(self.fallback)
    }
}

/// Extracts and parses the first `<timestamp>…</timestamp>` tag found in a
/// transcript line's text content.
fn timestamp_tag_from_record(record: &Value) -> Option<i64> {
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    match content {
        Value::String(text) => timestamp_tag_from_text(text),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .find_map(timestamp_tag_from_text),
        _ => None,
    }
}

fn timestamp_tag_from_text(text: &str) -> Option<i64> {
    let start = text.find("<timestamp>")? + "<timestamp>".len();
    let end = start + text[start..].find("</timestamp>")?;
    crate::timeutil::parse_cursor_human_timestamp(text[start..end].trim())
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

fn cursor_model_string(value: &Value) -> Option<String> {
    [
        "model",
        "model_id",
        "modelId",
        "model_name",
        "modelName",
        "model_slug",
        "modelSlug",
        "model_display_name",
        "modelDisplayName",
        "display_model",
        "displayModel",
        "display_model_name",
        "displayModelName",
    ]
    .into_iter()
    .find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string)
    })
}

fn cursor_record_message_model(record: &Value, message: &Value) -> Option<String> {
    cursor_model_string(record).or_else(|| cursor_model_string(message))
}

fn cursor_dispatch_model(item: &Value) -> Option<String> {
    item.get("input")
        .and_then(cursor_model_string)
        .or_else(|| cursor_model_string(item))
}

fn is_subagent_dispatch_tool(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "task" | "subagent")
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

fn dispatch_text(item: &Value) -> Option<String> {
    let input = item.get("input").unwrap_or(item);
    let mut parts = Vec::new();
    for key in ["description", "prompt", "subagent_type"] {
        if let Some(value) = input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(value.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
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
    let cwd_root = event_cwd(event).and_then(|cwd| crate::config::discover_project_root(&cwd));
    let candidates = event_project_candidates(event);
    let resolved = candidates
        .iter()
        .find_map(|candidate| crate::config::discover_project_root(candidate))
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
    use serde_json::json;

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
    fn canonical_cursor_record_keeps_typed_tools_and_excludes_hook_paths() {
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
        let envelope =
            normalize_cursor_observation(&native, "session-redacted", record_id.clone(), range)
                .unwrap();
        let rendered = format!("{envelope:?}");
        assert!(rendered.contains("Message"));
        assert!(rendered.contains("ToolInvocation"));
        assert!(rendered.contains("Reasoning"));
        assert!(rendered.contains("FileBytes"));
        assert!(rendered.contains(record_id.as_str()));
        assert!(!rendered.contains("/secret/worktree"));
        assert!(!rendered.contains("credential-redacted"));
    }
}
