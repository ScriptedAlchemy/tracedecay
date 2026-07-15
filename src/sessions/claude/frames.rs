use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::ClaudeByteRangeV1;

use crate::privacy::{PR5_MAX_CLAUDE_RECORD_BYTES, ParsedClaudeRecordV1, parse_claude_record_v1};
use crate::sessions::shared::StoredCursor;
use crate::sessions::source::{
    JsonlFrameDeferral, TranscriptCursorCheckpoint, TranscriptCursorKey,
    stream_new_jsonl_raw_strict,
};

use super::PROVIDER;
use super::cursor::{claude_cursor_key, claude_source_id};

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
    pub session_cwd: Option<PathBuf>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaudeSkippedFrame {
    pub offset: u64,
    pub end_offset: u64,
    pub reason: ClaudeSkippedFrameReason,
}

enum ClaudeFramePayload {
    Parsed(ParsedClaudeRecordV1),
    Sanitized(Value),
    Consumed,
}

/// One privacy-parsed Claude frame with its exact original source range.
pub(crate) struct ClaudeSourceFrame {
    pub offset: u64,
    pub end_offset: u64,
    payload: ClaudeFramePayload,
}

impl ClaudeSourceFrame {
    pub fn parsed_record(&self) -> Option<&ParsedClaudeRecordV1> {
        match &self.payload {
            ClaudeFramePayload::Parsed(record) => Some(record),
            ClaudeFramePayload::Sanitized(_) | ClaudeFramePayload::Consumed => None,
        }
    }

    pub fn take_parsed_record(&mut self) -> Option<ParsedClaudeRecordV1> {
        match std::mem::replace(&mut self.payload, ClaudeFramePayload::Consumed) {
            ClaudeFramePayload::Parsed(record) => Some(record),
            other => {
                self.payload = other;
                None
            }
        }
    }

    pub fn set_sanitized_record(&mut self, value: Value) -> bool {
        if !matches!(self.payload, ClaudeFramePayload::Consumed) {
            return false;
        }
        self.payload = ClaudeFramePayload::Sanitized(value);
        true
    }

    pub fn sanitized_record(&self) -> Option<&Value> {
        match &self.payload {
            ClaudeFramePayload::Sanitized(value) => Some(value),
            ClaudeFramePayload::Parsed(_) | ClaudeFramePayload::Consumed => None,
        }
    }

    pub(super) fn scope_value(&self) -> Option<&Value> {
        match &self.payload {
            ClaudeFramePayload::Parsed(record) => Some(record.value()),
            ClaudeFramePayload::Sanitized(value) => Some(value),
            ClaudeFramePayload::Consumed => None,
        }
    }
}

/// Parsed Claude frames and the typed cursor transition they cover.
pub(crate) struct ClaudeSourceFrameScan {
    pub identity: ClaudeSourceScanIdentity,
    pub file_generation: u64,
    pub previous_cursor: TranscriptCursorCheckpoint,
    pub next_cursor: TranscriptCursorCheckpoint,
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
        source_id: format!("{PROVIDER}:{session_id}"),
        session_id,
        source_path: path.to_path_buf(),
        cursor_key: claude_cursor_key(path),
    })
}

/// Frame and privacy-parse newly appended Claude records exactly once.
pub(crate) fn scan_claude_source_frames(
    identity: ClaudeSourceScanIdentity,
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
) -> Option<ClaudeSourceFrameScan> {
    let mut raw = stream_new_jsonl_raw_strict(
        &identity.source_path,
        previous,
        max_new_bytes,
        PR5_MAX_CLAUDE_RECORD_BYTES,
    )?;
    let mut frames = Vec::new();
    let mut skipped_frames = Vec::new();

    for frame in raw.frames.drain(..) {
        if frame.bytes.iter().all(u8::is_ascii_whitespace) {
            skipped_frames.push(ClaudeSkippedFrame {
                offset: frame.offset,
                end_offset: frame.end_offset,
                reason: ClaudeSkippedFrameReason::Whitespace,
            });
            continue;
        }
        let range = ClaudeByteRangeV1::new(frame.offset, frame.end_offset).ok()?;
        let Ok(record) = parse_claude_record_v1(&frame.bytes, range) else {
            raw.new_cursor.position = frame.offset;
            raw.deferred = Some(JsonlFrameDeferral::Malformed {
                offset: frame.offset,
            });
            break;
        };
        frames.push(ClaudeSourceFrame {
            offset: frame.offset,
            end_offset: frame.end_offset,
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

    Some(ClaudeSourceFrameScan {
        file_generation: raw.new_cursor.file_id,
        previous_cursor: TranscriptCursorCheckpoint {
            key: identity.cursor_key.clone(),
            state: previous,
        },
        next_cursor: TranscriptCursorCheckpoint {
            key: identity.cursor_key.clone(),
            state: raw.new_cursor,
        },
        identity,
        frames,
        skipped_frames,
        coverage,
        scope: None,
    })
}
