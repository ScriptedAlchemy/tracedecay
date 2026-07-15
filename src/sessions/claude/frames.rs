use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::ClaudeByteRangeV1;

use crate::privacy::{
    MAX_OBSERVATION_RECORD_BYTES, ParsedClaudeRecordV1, SanitizedClaudeRecordV1,
    parse_claude_record_v1,
};
use crate::sessions::shared::StoredCursor;
use crate::sessions::source::{
    JsonlFrameDeferral, JsonlResumeState, RawJsonlSkippedReason, TranscriptCursorCheckpoint,
    TranscriptCursorKey, TranscriptIngestResult, try_stream_new_jsonl_raw_strict_with_resume,
};

use super::PROVIDER;
use super::cursor::{claude_cursor_key, claude_observation_source_id, claude_source_id};

/// Stable identity available before the durable cursor lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeSourceScanIdentity {
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
pub(crate) enum ClaudeFrameCoverage {
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
pub(crate) enum ClaudeSkippedFrameReason {
    Whitespace,
    OutOfScope,
    Malformed,
    Oversized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaudeSkippedFrame {
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
pub(crate) struct ClaudeSourceFrame {
    pub offset: u64,
    pub end_offset: u64,
    pub resume_fingerprint: u64,
    raw_message_id: Option<String>,
    raw_tool_event_ids: Vec<String>,
    raw_hook_tool_use_id: Option<String>,
    raw_logical_parent_uuid: Option<String>,
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

    pub(super) fn scope_value(&self) -> Option<&Value> {
        match &self.payload {
            ClaudeFramePayload::Parsed(record) => Some(record.value()),
            ClaudeFramePayload::Sanitized(value) => Some(value.payload()),
            ClaudeFramePayload::Consumed => None,
        }
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
pub(crate) struct ClaudeSourceFrameScan {
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
pub(crate) fn identify_claude_source(path: &Path) -> Option<ClaudeSourceScanIdentity> {
    let session_id = claude_source_id(path)?;
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
pub(crate) fn scan_claude_source_frames(
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

pub(crate) fn try_scan_claude_source_frames(
    identity: ClaudeSourceScanIdentity,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<Option<ClaudeSourceFrameScan>> {
    try_scan_claude_source_frames_with_resume(identity, previous, max_new_bytes, None)
}

pub(crate) fn try_scan_claude_source_frames_with_resume(
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
        let Ok(record) = parse_claude_record_v1(&frame.bytes, range) else {
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
            raw_message_id: record
                .value()
                .pointer("/message/id")
                .and_then(Value::as_str)
                .or_else(|| record.value().get("uuid").and_then(Value::as_str))
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            raw_tool_event_ids: record
                .value()
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    item.get("id")
                        .or_else(|| item.get("tool_use_id"))
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                })
                .collect(),
            raw_hook_tool_use_id: record
                .value()
                .get("toolUseID")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            raw_logical_parent_uuid: record
                .value()
                .get("logicalParentUuid")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
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
