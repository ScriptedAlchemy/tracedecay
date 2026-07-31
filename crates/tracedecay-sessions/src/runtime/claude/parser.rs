use std::borrow::Cow;
use std::path::Path;

use serde_json::Value;
use tracedecay_domain::{
    ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1, ClaudeSourceIdentityV1,
    ObservationScopeV1, RetentionClass, SessionId,
};

use crate::privacy::{ClaudeRecordSanitizerV1, ClaudeSanitizationOutcomeV1};
use crate::runtime::claude_observation::CLAUDE_TRANSCRIPT_RETENTION_CLASS;
use crate::runtime::shared::{StoredCursor, title_from_messages};
use crate::runtime::source::{
    JsonlFrameDeferral, ParsedTranscript, SessionDraft, TranscriptIngestError,
    TranscriptIngestResult,
};

use super::cursor::claude_cursor_key;
use super::frames::{
    ClaudeFrameCoverage, ClaudeSkippedFrameReason, ClaudeSourceFrame, ClaudeSourceFrameScan,
    identify_claude_source, try_scan_claude_source_frames,
};
use super::record_metadata::{SessionAccumulator, accumulate_session_facts, session_metadata};
use super::source_records::{
    ClaudeRecordContext, ClaudeRecordDisposition, map_sanitized_claude_record, reasoning_from_line,
    record_cwd, structured_marker_from_line, system_hook_message_from_line,
};
use super::{ClaudeSource, PROVIDER, claude_subagent_identity};

pub(super) fn fold_scanned_frames(
    source: &ClaudeSource,
    scan: &ClaudeSourceFrameScan,
    project_root: &Path,
) -> Option<ParsedTranscript> {
    scan.scope
        .as_ref()
        .filter(|scope| scope.project_root == project_root)?;
    let subagent = claude_subagent_identity(&scan.identity.source_path);
    let session_id = scan.identity.session_id.clone();
    let source_path = Path::new(&scan.identity.source_id);
    let project = source.user_scope.as_ref().map_or_else(
        || project_root.to_string_lossy().to_string(),
        |_| "user".to_string(),
    );
    let sanitized_session_cwd = scan
        .frames
        .iter()
        .filter_map(ClaudeSourceFrame::sanitized_record)
        .find_map(|record| record_cwd(record.payload()));
    let mut accumulator = SessionAccumulator::default();
    let mut messages = Vec::new();

    for frame in &scan.frames {
        let record = frame.sanitized_record()?.payload();
        accumulate_session_facts(record, &mut accumulator);
        let context = ClaudeRecordContext {
            session_id: &session_id,
            project_key: &project,
            project_path: &project,
            file_generation: scan.file_generation,
            offset: frame.offset,
            session_cwd: sanitized_session_cwd.as_deref(),
            source_path: Some(scan.identity.source_id.as_str()),
            raw_message_id: frame.raw_message_id(),
            raw_tool_event_ids: frame.raw_tool_event_ids(),
            raw_hook_tool_use_id: frame.raw_hook_tool_use_id(),
        };
        let mut message = match map_sanitized_claude_record(record, &context) {
            ClaudeRecordDisposition::Message { message, .. } => Some(*message),
            ClaudeRecordDisposition::NonConversational => {
                let owned_native = envelope_native_content(record);
                let native = owned_native.as_ref().unwrap_or(record);
                system_hook_message_from_line(
                    native,
                    source_path,
                    &context,
                    frame.raw_hook_tool_use_id().filter(|raw| {
                        native.get("toolUseID").and_then(Value::as_str) == Some(*raw)
                    }),
                )
            }
        };
        if message.is_none() {
            let owned_native = envelope_native_content(record);
            let marker_source = owned_native.as_ref().unwrap_or(record);
            let marker_record = if frame.raw_logical_parent_uuid()
                != marker_source
                    .get("logicalParentUuid")
                    .and_then(Value::as_str)
                || marker_source
                    .get("logicalParentUuid")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with("[TraceDecay redacted:"))
            {
                let mut record = marker_source.clone();
                if let Some(record) = record.as_object_mut() {
                    record.remove("logicalParentUuid");
                }
                Cow::Owned(record)
            } else {
                Cow::Borrowed(marker_source)
            };
            message = structured_marker_from_line(
                marker_record.as_ref(),
                source_path,
                &context,
                &mut accumulator,
            );
        }
        if let Some(reasoning) = reasoning_from_line(
            record,
            source_path,
            &context,
            message.as_ref().map(|message| message.message_id.as_str()),
        ) {
            messages.push(reasoning);
        }
        if let Some(message) = message {
            messages.push(message);
        }
    }

    let draft = SessionDraft {
        session_id,
        project_key: project.clone(),
        project_path: project,
        title: title_from_messages(&messages),
        metadata_json: serde_json::to_string(&session_metadata(
            sanitized_session_cwd.as_deref(),
            subagent.as_ref(),
            &accumulator,
        ))
        .ok(),
        parent_session_id: subagent.as_ref().map(|info| info.parent_session_id.clone()),
        is_subagent: subagent.is_some(),
        agent_id: subagent.as_ref().map(|info| info.agent_id.clone()),
        parent_tool_use_id: subagent
            .as_ref()
            .and_then(|info| info.parent_tool_use_id.clone()),
    };
    Some(ParsedTranscript {
        draft,
        messages,
        new_cursor: scan.next_cursor.state,
    })
}

fn envelope_native_content(record: &Value) -> Option<Value> {
    let envelope =
        serde_json::from_value::<tracedecay_domain::CanonicalObservationEnvelopeV1>(record.clone())
            .ok()?;
    envelope.facts().iter().find_map(|fact| {
        let (tracedecay_domain::CanonicalObservationFactV1::Git {
            content: Some(content),
            ..
        }
        | tracedecay_domain::CanonicalObservationFactV1::Workflow {
            content: Some(content),
            ..
        }
        | tracedecay_domain::CanonicalObservationFactV1::WorkflowLifecycle {
            content: Some(content),
            ..
        }
        | tracedecay_domain::CanonicalObservationFactV1::Compaction {
            summary: Some(content),
            ..
        }
        | tracedecay_domain::CanonicalObservationFactV1::Reasoning {
            content: Some(content),
            ..
        }
        | tracedecay_domain::CanonicalObservationFactV1::Message { content, .. }
        | tracedecay_domain::CanonicalObservationFactV1::ToolResult { content, .. }
        | tracedecay_domain::CanonicalObservationFactV1::ToolInvocation {
            arguments: content,
            ..
        }) = fact
        else {
            return None;
        };
        content.get("type").is_some().then(|| content.clone())
    })
}

pub(super) fn try_parse_claude_transcript(
    source_adapter: &ClaudeSource,
    path: &Path,
    prev: StoredCursor,
    project_root: &Path,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<Option<ParsedTranscript>> {
    let identity = identify_claude_source(path).ok_or_else(|| {
        TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        }
    })?;
    let Some(mut scan) = try_scan_claude_source_frames(identity, prev, max_new_bytes)? else {
        return Ok(None);
    };
    if scan.previous_cursor.state != prev || scan.previous_cursor.key != claude_cursor_key(path) {
        return Ok(None);
    }
    if let ClaudeFrameCoverage::Deferred { reason, .. } = scan.coverage {
        tracing::debug!(
            provider = PROVIDER,
            line_offset = reason.offset(),
            reason = reason.reason_code(),
            "deferring transcript input at strict JSONL frame"
        );
    }
    let coverage = scan.coverage;
    if let ClaudeFrameCoverage::Deferred {
        start_offset,
        covered_through,
        reason: JsonlFrameDeferral::Backlog { .. },
    } = coverage
    {
        if start_offset == covered_through {
            return Ok(None);
        }
        scan.coverage = ClaudeFrameCoverage::Complete {
            start_offset,
            end_offset: covered_through,
        };
    }
    let retained = source_adapter.retain_scoped_frames(&mut scan, project_root);
    scan.coverage = coverage;
    if retained.is_none() {
        return Ok(None);
    }

    if let Some(skipped) = scan.skipped_frames.iter().find(|frame| {
        matches!(
            frame.reason,
            ClaudeSkippedFrameReason::Malformed | ClaudeSkippedFrameReason::Oversized
        )
    }) {
        return Err(TranscriptIngestError::NonDurableRecord {
            provider: PROVIDER,
            offset: skipped.offset,
            end_offset: skipped.end_offset,
            reason: match skipped.reason {
                ClaudeSkippedFrameReason::Malformed => "malformed",
                ClaudeSkippedFrameReason::Oversized => "oversized",
                ClaudeSkippedFrameReason::Whitespace | ClaudeSkippedFrameReason::OutOfScope => {
                    unreachable!()
                }
            },
        });
    }

    // Legacy callers still use this trait path. They must receive the same
    // sanitizer-issued payload as observation-first ingestion, never the
    // parser's raw `Value` relabelled as sanitized.
    let sanitizer = ClaudeRecordSanitizerV1::claude_v1()?;
    let source = ClaudeSourceIdentityV1::for_source(
        SessionId::new(scan.identity.session_id.clone())?,
        SessionId::new(scan.identity.source_id.clone())?,
    )?;
    let generation = ClaudeFileGenerationV1::new(scan.file_generation)?;
    let retention_class = RetentionClass::new(CLAUDE_TRANSCRIPT_RETENTION_CLASS)?;
    for frame in &mut scan.frames {
        let parsed = frame
            .take_parsed_record()
            .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
        let range = *parsed.source_range();
        let identity = ClaudeObservationIdentityMaterialV1::new(
            source.clone(),
            ObservationScopeV1::Profile,
            generation,
            range,
        )?;
        let sanitized =
            match sanitizer.sanitize_parsed(parsed, identity, retention_class.clone())? {
                ClaudeSanitizationOutcomeV1::Durable {
                    sanitized_record, ..
                } => sanitized_record,
                ClaudeSanitizationOutcomeV1::Rejected { .. } => {
                    return Err(TranscriptIngestError::NonDurableRecord {
                        provider: PROVIDER,
                        offset: range.start(),
                        end_offset: range.end(),
                        reason: "sanitizer_rejected",
                    });
                }
                ClaudeSanitizationOutcomeV1::Quarantined { .. } => {
                    return Err(TranscriptIngestError::NonDurableRecord {
                        provider: PROVIDER,
                        offset: range.start(),
                        end_offset: range.end(),
                        reason: "sanitizer_quarantined",
                    });
                }
            };
        if !frame.set_sanitized_record(sanitized) {
            return Err(TranscriptIngestError::InvalidFrameState { provider: PROVIDER });
        }
    }
    fold_scanned_frames(source_adapter, &scan, project_root)
        .map(Some)
        .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })
}
