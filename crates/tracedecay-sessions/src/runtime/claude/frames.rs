use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_capture::claude::{normalize, stable_record_id};
use tracedecay_domain::{ClaudeByteRangeV1, ObservationOrderingDomainV1};

use crate::runtime::shared::StoredCursor;
use crate::runtime::source::{
    JsonlFrameDeferral, JsonlResumeState, RawJsonlSkippedReason, TranscriptCursorCheckpoint,
    TranscriptCursorKey, TranscriptIngestResult, try_stream_new_jsonl_raw_strict_with_resume,
};
use tracedecay_runtime_core::privacy::{
    MAX_OBSERVATION_RECORD_BYTES, ParsedClaudeRecordV1, SanitizedClaudeRecordV1,
    parse_normalized_observation_record_v1, protect_sensitive_structural_id,
};

use super::PROVIDER;
use super::cursor::{claude_cursor_key, claude_observation_source_id, claude_source_id};

/// Stable identity available before the durable cursor lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSourceScanIdentity {
    pub provider: &'static str,
    pub session_id: String,
    pub source_id: String,
    pub source_path: PathBuf,
    pub cursor_key: TranscriptCursorKey,
}

pub(super) struct ClaudeFrameScope {
    pub project_root: PathBuf,
}

/// Exact byte coverage achieved by one bounded scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeFrameCoverage {
    Complete {
        start_offset: u64,
        end_offset: u64,
    },
    Deferred {
        start_offset: u64,
        covered_through: u64,
        reason: JsonlFrameDeferral,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeSkippedFrameReason {
    Whitespace,
    OutOfScope,
    Malformed,
    Oversized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeSkippedFrame {
    pub offset: u64,
    pub end_offset: u64,
    pub resume_fingerprint: u64,
    pub reason: ClaudeSkippedFrameReason,
}

enum ClaudeFramePayload {
    Parsed(ParsedClaudeRecordV1),
    Sanitized(SanitizedClaudeRecordV1),
    Consumed,
}

/// One privacy-parsed Claude frame with its exact original source range.
pub struct ClaudeSourceFrame {
    pub offset: u64,
    pub end_offset: u64,
    pub resume_fingerprint: u64,
    raw_message_id: Option<String>,
    raw_tool_event_ids: Vec<String>,
    raw_hook_tool_use_id: Option<String>,
    raw_logical_parent_uuid: Option<String>,
    scope_record: Value,
    payload: ClaudeFramePayload,
}

impl ClaudeSourceFrame {
    pub fn take_parsed_record(&mut self) -> Option<ParsedClaudeRecordV1> {
        match std::mem::replace(&mut self.payload, ClaudeFramePayload::Consumed) {
            ClaudeFramePayload::Parsed(record) => Some(record),
            other => {
                self.payload = other;
                None
            }
        }
    }

    pub fn set_sanitized_record(&mut self, value: SanitizedClaudeRecordV1) -> bool {
        if !matches!(self.payload, ClaudeFramePayload::Consumed) {
            return false;
        }
        self.payload = ClaudeFramePayload::Sanitized(value);
        true
    }

    pub fn sanitized_record(&self) -> Option<&SanitizedClaudeRecordV1> {
        match &self.payload {
            ClaudeFramePayload::Sanitized(value) => Some(value),
            ClaudeFramePayload::Parsed(_) | ClaudeFramePayload::Consumed => None,
        }
    }

    pub(super) const fn scope_value(&self) -> &Value {
        &self.scope_record
    }

    pub(super) fn raw_message_id(&self) -> Option<&str> {
        self.raw_message_id.as_deref()
    }

    pub(super) fn raw_tool_event_ids(&self) -> &[String] {
        &self.raw_tool_event_ids
    }

    pub(super) fn raw_hook_tool_use_id(&self) -> Option<&str> {
        self.raw_hook_tool_use_id.as_deref()
    }

    pub(super) fn raw_logical_parent_uuid(&self) -> Option<&str> {
        self.raw_logical_parent_uuid.as_deref()
    }
}

/// Parsed Claude frames and the typed cursor transition they cover.
pub struct ClaudeSourceFrameScan {
    pub identity: ClaudeSourceScanIdentity,
    pub file_generation: u64,
    pub file_identity: u64,
    pub previous_cursor: TranscriptCursorCheckpoint,
    pub next_cursor: TranscriptCursorCheckpoint,
    /// Furthest absolute source position inspected by this scan. This may be
    /// beyond `next_cursor` when the final frame is incomplete.
    pub read_through: u64,
    pub frames: Vec<ClaudeSourceFrame>,
    pub skipped_frames: Vec<ClaudeSkippedFrame>,
    pub coverage: ClaudeFrameCoverage,
    pub(super) scope: Option<ClaudeFrameScope>,
}

/// Identify a Claude transcript before loading its durable cursor.
///
/// Session identities are privacy-protected here so scan, capture, and
/// source-cursor lookup/persistence all reuse the same durable key. Public
/// identifiers are preserved byte-for-byte; credential-shaped stems become
/// stable `privacy.structural-id.v1.*` digests. The observation source ID is
/// already an opaque path digest and remains unchanged.
pub fn identify_claude_source(path: &Path) -> Option<ClaudeSourceScanIdentity> {
    let session_id = protect_sensitive_structural_id(&claude_source_id(path)?).ok()?;
    Some(ClaudeSourceScanIdentity {
        provider: PROVIDER,
        source_id: claude_observation_source_id(path),
        session_id,
        source_path: path.to_path_buf(),
        cursor_key: claude_cursor_key(path),
    })
}

/// Frame and privacy-parse newly appended Claude records exactly once.
#[cfg(test)]
pub fn scan_claude_source_frames(
    identity: ClaudeSourceScanIdentity,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
) -> Option<ClaudeSourceFrameScan> {
    match try_scan_claude_source_frames(identity, previous, max_new_bytes) {
        Ok(scan) => scan,
        Err(error) => {
            tracing::debug!(error = %error, "skipping Claude transcript scan");
            None
        }
    }
}

pub fn try_scan_claude_source_frames(
    identity: ClaudeSourceScanIdentity,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<Option<ClaudeSourceFrameScan>> {
    try_scan_claude_source_frames_with_resume(identity, previous, max_new_bytes, None)
}

pub fn try_scan_claude_source_frames_with_resume(
    identity: ClaudeSourceScanIdentity,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
    resume_state: Option<JsonlResumeState>,
) -> TranscriptIngestResult<Option<ClaudeSourceFrameScan>> {
    let mut raw = try_stream_new_jsonl_raw_strict_with_resume(
        &identity.source_path,
        previous,
        max_new_bytes,
        MAX_OBSERVATION_RECORD_BYTES,
        resume_state,
    )?;
    let mut frames = Vec::new();
    let mut skipped_frames = raw
        .skipped
        .drain(..)
        .map(|range| ClaudeSkippedFrame {
            offset: range.offset,
            end_offset: range.end_offset,
            resume_fingerprint: range.resume_fingerprint,
            reason: match range.reason {
                RawJsonlSkippedReason::Whitespace => ClaudeSkippedFrameReason::Whitespace,
                RawJsonlSkippedReason::Oversized => ClaudeSkippedFrameReason::Oversized,
            },
        })
        .collect::<Vec<_>>();

    for frame in raw.frames.drain(..) {
        let Ok(range) = ClaudeByteRangeV1::new(frame.offset, frame.end_offset) else {
            return Ok(None);
        };
        let mut raw_message_id = None;
        let mut raw_tool_event_ids = Vec::new();
        let mut raw_hook_tool_use_id = None;
        let mut raw_logical_parent_uuid = None;
        let mut scope_record = None;
        let Ok(record) = parse_normalized_observation_record_v1(
            &frame.bytes,
            range,
            ObservationOrderingDomainV1::FileBytes,
            |native| {
                raw_message_id = native
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .or_else(|| native.get("uuid").and_then(Value::as_str))
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned);
                raw_tool_event_ids = native
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| {
                        item.get("id")
                            .or_else(|| item.get("tool_use_id"))
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                    })
                    .collect();
                raw_hook_tool_use_id = native
                    .get("toolUseID")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned);
                raw_logical_parent_uuid = native
                    .get("logicalParentUuid")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned);
                scope_record = Some(serde_json::json!({
                    "type": native.get("type").cloned().unwrap_or(Value::Null),
                    "cwd": native.get("cwd").cloned().unwrap_or(Value::Null),
                }));
                let stable_record_id =
                    stable_record_id(&native, &identity.session_id, frame.offset)?;
                normalize(&native, &identity.session_id, stable_record_id, range)
            },
        ) else {
            skipped_frames.push(ClaudeSkippedFrame {
                offset: frame.offset,
                end_offset: frame.end_offset,
                resume_fingerprint: frame.resume_fingerprint,
                reason: ClaudeSkippedFrameReason::Malformed,
            });
            continue;
        };
        frames.push(ClaudeSourceFrame {
            offset: frame.offset,
            end_offset: frame.end_offset,
            resume_fingerprint: frame.resume_fingerprint,
            raw_message_id,
            raw_tool_event_ids,
            raw_hook_tool_use_id,
            raw_logical_parent_uuid,
            scope_record: scope_record.unwrap_or(Value::Null),
            payload: ClaudeFramePayload::Parsed(record),
        });
    }

    let coverage = raw.deferred.map_or(
        ClaudeFrameCoverage::Complete {
            start_offset: raw.start_offset,
            end_offset: raw.new_cursor.position,
        },
        |reason| ClaudeFrameCoverage::Deferred {
            start_offset: raw.start_offset,
            covered_through: raw.new_cursor.position,
            reason,
        },
    );

    Ok(Some(ClaudeSourceFrameScan {
        file_generation: raw.new_cursor.file_id,
        file_identity: raw.file_identity,
        previous_cursor: TranscriptCursorCheckpoint {
            key: identity.cursor_key.clone(),
            state: previous,
        },
        next_cursor: TranscriptCursorCheckpoint {
            key: identity.cursor_key.clone(),
            state: raw.new_cursor,
        },
        read_through: raw.read_through,
        identity,
        frames,
        skipped_frames,
        coverage,
        scope: None,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;
    use tracedecay_domain::CanonicalObservationEnvelopeV1;

    use super::*;

    #[test]
    fn scanned_claude_frame_crosses_canonical_normalization_boundary() {
        let temp = TempDir::new().unwrap();
        let path = temp
            .path()
            .join(".claude/projects/fixture/session.fixture.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let native = json!({
            "type": "user",
            "sessionId": "session.fixture",
            "uuid": "message.fixture",
            "cwd": temp.path(),
            "message": {"role": "user", "content": "hello"}
        });
        std::fs::write(&path, format!("{native}\n")).unwrap();

        let identity = identify_claude_source(&path).unwrap();
        let mut scan = try_scan_claude_source_frames(identity, StoredCursor::default(), None)
            .unwrap()
            .unwrap();
        let parsed = scan.frames[0].take_parsed_record().unwrap();
        let envelope =
            serde_json::from_value::<CanonicalObservationEnvelopeV1>(parsed.value().clone())
                .unwrap();
        assert_eq!(envelope.provider().as_str(), "claude");
        assert_eq!(envelope.stable_record_id().as_str(), "message.fixture");
        assert_eq!(scan.frames[0].scope_value()["type"], "user");
    }
}
