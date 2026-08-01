use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    StoredCursor, TranscriptIngestError, TranscriptIngestResult, file_mtime_secs,
    log_jsonl_decode_skip, log_jsonl_oversized_skip, log_source_skip, should_resume_jsonl,
    stable_jsonl_file_id,
};

/// One newly-read JSONL line: its exact byte range and decoded value.
pub struct JsonlLine {
    pub offset: i64,
    pub value: Value,
}

/// New JSONL content read from a file, plus the advanced cursor.
pub struct NewJsonl {
    pub lines: Vec<JsonlLine>,
    pub new_cursor: StoredCursor,
}

/// Why strict JSONL framing stopped before consuming the next record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFrameDeferral {
    Partial {
        offset: u64,
    },
    Malformed {
        offset: u64,
    },
    Backlog {
        offset: u64,
        unread_bytes: u64,
        max_new_bytes: u64,
    },
}

impl JsonlFrameDeferral {
    pub fn offset(self) -> u64 {
        match self {
            Self::Partial { offset }
            | Self::Malformed { offset }
            | Self::Backlog { offset, .. } => offset,
        }
    }

    pub fn reason_code(self) -> &'static str {
        match self {
            Self::Partial { .. } => "partial_jsonl_frame",
            Self::Malformed { .. } => "malformed_jsonl_frame",
            Self::Backlog { .. } => "jsonl_backlog_limit",
        }
    }
}

pub const MAX_JSONL_RECORD_BYTES: usize = 16 * 1024 * 1024;
/// Default strict-scan budget keeps recovery bounded even without a hook cap.
pub const STRICT_JSONL_BATCH_BYTES: u64 = 2 * 1024 * 1024;
pub(super) const MAX_JSONL_FRAMES_PER_BATCH: usize = 4096;
const JSONL_RESUME_FINGERPRINT_BYTES: usize = 4 * 1024;
const JSONL_SNAPSHOT_SAMPLE_COUNT: u64 = 8;
const JSONL_RESUME_HASH_BASE: u64 = 0x9e37_79b1_85eb_ca87;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonlResumeState {
    pub generation: u64,
    pub file_identity: u64,
    pub fingerprint: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawJsonlFrame {
    Eof,
    Complete { byte_len: u64 },
    Partial { byte_len: u64 },
    Oversized { byte_len: u64, terminated: bool },
    BudgetExhausted { byte_len: u64, oversized: bool },
}

impl RawJsonlFrame {
    fn byte_len(self) -> u64 {
        match self {
            Self::Eof => 0,
            Self::Complete { byte_len }
            | Self::Partial { byte_len }
            | Self::Oversized { byte_len, .. }
            | Self::BudgetExhausted { byte_len, .. } => byte_len,
        }
    }
}

struct ResumeTail {
    bytes: VecDeque<u8>,
    rolling_hash: u64,
    oldest_weight: u64,
}

impl ResumeTail {
    fn new() -> Self {
        let mut oldest_weight = 1_u64;
        for _ in 1..JSONL_RESUME_FINGERPRINT_BYTES {
            oldest_weight = oldest_weight.wrapping_mul(JSONL_RESUME_HASH_BASE);
        }
        Self {
            bytes: VecDeque::with_capacity(JSONL_RESUME_FINGERPRINT_BYTES),
            rolling_hash: 0,
            oldest_weight,
        }
    }

    fn from_bytes(bytes: &[u8]) -> Self {
        let mut tail = Self::new();
        tail.extend(bytes);
        tail
    }

    fn extend(&mut self, bytes: &[u8]) {
        let bytes = if bytes.len() > JSONL_RESUME_FINGERPRINT_BYTES {
            self.bytes.clear();
            self.rolling_hash = 0;
            &bytes[bytes.len() - JSONL_RESUME_FINGERPRINT_BYTES..]
        } else {
            bytes
        };
        for &byte in bytes {
            if let Some(oldest) = (self.bytes.len() == JSONL_RESUME_FINGERPRINT_BYTES)
                .then(|| self.bytes.pop_front())
                .flatten()
            {
                self.rolling_hash = self.rolling_hash.wrapping_sub(
                    u64::from(oldest)
                        .wrapping_add(1)
                        .wrapping_mul(self.oldest_weight),
                );
            }
            self.rolling_hash = self
                .rolling_hash
                .wrapping_mul(JSONL_RESUME_HASH_BASE)
                .wrapping_add(u64::from(byte).wrapping_add(1));
            self.bytes.push_back(byte);
        }
    }

    fn fingerprint(&self, position: u64) -> u64 {
        finish_resume_fingerprint(position, self.bytes.len(), self.rolling_hash)
    }
}

fn finish_resume_fingerprint(position: u64, len: usize, rolling_hash: u64) -> u64 {
    let mut value = rolling_hash
        ^ position.wrapping_mul(0xd6e8_feb8_6659_fd93)
        ^ (len as u64).wrapping_mul(0xa076_1d64_78bd_642f);
    value ^= value >> 32;
    value = value.wrapping_mul(0xe703_7ed1_a0b4_28db);
    value ^ (value >> 32)
}

fn resume_tail_fingerprint(position: u64, tail: &[u8]) -> u64 {
    ResumeTail::from_bytes(tail).fingerprint(position)
}

fn read_jsonl_resume_tail(file: &mut std::fs::File, position: u64) -> std::io::Result<Vec<u8>> {
    let start = position.saturating_sub(JSONL_RESUME_FINGERPRINT_BYTES as u64);
    let len = usize::try_from(position.saturating_sub(start)).unwrap_or(usize::MAX);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = vec![0_u8; len];
    file.read_exact(&mut tail)?;
    Ok(tail)
}

fn bounded_jsonl_snapshot_fingerprint(
    file: &mut std::fs::File,
    extent: u64,
) -> std::io::Result<u64> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-snapshot-v1");
    hasher.update(extent.to_le_bytes());
    for sample in 1..=JSONL_SNAPSHOT_SAMPLE_COUNT {
        let position = extent.saturating_mul(sample) / JSONL_SNAPSHOT_SAMPLE_COUNT;
        let tail = read_jsonl_resume_tail(file, position)?;
        hasher.update(position.to_le_bytes());
        hasher.update((tail.len() as u64).to_le_bytes());
        hasher.update(tail);
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok(u64::from_be_bytes(bytes))
}

fn rewritten_jsonl_generation(
    previous: JsonlResumeState,
    file_identity: u64,
    snapshot_fingerprint: u64,
    file_size: u64,
    mtime: u64,
) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-rewrite-generation-v1");
    hasher.update(previous.generation.to_le_bytes());
    hasher.update(file_identity.to_le_bytes());
    hasher.update(snapshot_fingerprint.to_le_bytes());
    hasher.update(file_size.to_le_bytes());
    hasher.update(mtime.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes).max(1)
}

/// Bounded raw JSONL framing shared by skip and defer policies.
///
/// The retained record buffer never exceeds `max_record_bytes`. Oversized
/// frames are drained in-place so legacy callers can skip complete records and
/// continue without allocating or parsing them.
pub struct RawJsonlFrameReader<R> {
    reader: R,
    record: Vec<u8>,
    resume_tail: ResumeTail,
    max_record_bytes: usize,
}

impl<R: BufRead> RawJsonlFrameReader<R> {
    pub fn new(reader: R, max_record_bytes: usize) -> Self {
        Self {
            reader,
            record: Vec::new(),
            resume_tail: ResumeTail::new(),
            max_record_bytes,
        }
    }

    fn seed_resume_tail(&mut self, tail: &[u8]) {
        self.resume_tail = ResumeTail::from_bytes(tail);
    }

    fn resume_fingerprint(&self, position: u64) -> u64 {
        self.resume_tail.fingerprint(position)
    }

    pub fn record(&self) -> &[u8] {
        &self.record
    }

    pub fn set_max_record_bytes(&mut self, max_record_bytes: usize) {
        self.max_record_bytes = max_record_bytes;
    }

    pub fn into_inner(self) -> R {
        self.reader
    }

    pub fn next_frame(&mut self) -> std::io::Result<RawJsonlFrame> {
        self.next_frame_with_budget(
            u64::try_from(self.max_record_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
    }

    pub fn next_frame_with_budget(
        &mut self,
        read_budget: u64,
    ) -> std::io::Result<RawJsonlFrame> {
        self.record.clear();
        let mut byte_len = 0_u64;
        let mut oversized = false;

        loop {
            if byte_len >= read_budget {
                return Ok(RawJsonlFrame::BudgetExhausted {
                    byte_len,
                    oversized,
                });
            }
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Ok(if byte_len == 0 {
                    RawJsonlFrame::Eof
                } else if oversized {
                    RawJsonlFrame::Oversized {
                        byte_len,
                        terminated: false,
                    }
                } else {
                    RawJsonlFrame::Partial { byte_len }
                });
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let available_record = newline.map_or(available.len(), |index| index + 1);
            let remaining =
                usize::try_from(read_budget.saturating_sub(byte_len)).unwrap_or(usize::MAX);
            let consumed = available_record.min(remaining);
            if !oversized {
                let retained =
                    consumed.min(self.max_record_bytes.saturating_sub(self.record.len()));
                self.record.extend_from_slice(&available[..retained]);
                oversized = retained < consumed;
            }
            self.resume_tail.extend(&available[..consumed]);
            self.reader.consume(consumed);
            byte_len = byte_len.saturating_add(consumed as u64);

            if newline.is_some_and(|index| index < consumed) {
                return Ok(if oversized {
                    RawJsonlFrame::Oversized {
                        byte_len,
                        terminated: true,
                    }
                } else {
                    RawJsonlFrame::Complete { byte_len }
                });
            }
            if consumed < available_record {
                return Ok(RawJsonlFrame::BudgetExhausted {
                    byte_len,
                    oversized,
                });
            }
        }
    }
}

/// Strict framing result used by providers that must retry invalid records.
#[cfg(test)]
pub enum StrictJsonlOutcome {
    Complete(NewJsonl),
    Deferred {
        parsed: NewJsonl,
        reason: JsonlFrameDeferral,
    },
}

#[derive(Clone, Copy)]
pub(super) enum MalformedJsonlPolicy {
    Skip,
    Defer,
}

#[derive(Clone, Copy)]
struct RawJsonlScanRequest {
    previous: StoredCursor,
    max_new_bytes: Option<u64>,
    oversized_policy: MalformedJsonlPolicy,
    max_record_bytes: usize,
    resume_state: Option<JsonlResumeState>,
}

/// **`ByteOffset`** reader for append-only JSONL.
///
/// Seeks to `prev.position` (when the file has only grown and its mtime has not
/// regressed) and streams complete, newline-terminated lines, decoding each as
/// JSON. Blank and undecodable lines still advance the offset (so they are not
/// re-read) but are omitted from `lines`. A trailing line without a newline is a
/// partial write and is left unconsumed for the next call.
///
/// Returns `None` when the file cannot be stat-ed/opened. `max_new_bytes` is a
/// nominal batch cap: a capped read finishes at most one bounded complete record
/// that crosses the cap, then leaves the remaining backlog for a later call.
/// This guarantees cursor progress without allowing a second record past the cap.
pub fn stream_new_jsonl(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
) -> Option<NewJsonl> {
    let (parsed, deferred, start_offset) = stream_new_jsonl_with_policy(
        path,
        prev,
        max_new_bytes,
        MalformedJsonlPolicy::Skip,
        MAX_JSONL_RECORD_BYTES,
    )?;
    if matches!(deferred, Some(JsonlFrameDeferral::Backlog { .. }))
        && parsed.new_cursor.position == start_offset
    {
        None
    } else {
        Some(parsed)
    }
}

/// Reads complete Claude-style JSONL frames with strict malformed-frame deferral.
///
/// Malformed JSON stops at that frame's start. Bounded oversized raw frames
/// advance without payload but stop the batch before exposing their suffix.
/// `max_record_bytes` includes the terminating newline. Other providers retain
/// [`stream_new_jsonl`]'s skip-and-advance behavior.
#[cfg(test)]
pub fn stream_new_jsonl_strict(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
) -> Option<StrictJsonlOutcome> {
    let (parsed, reason, _) = stream_new_jsonl_with_policy(
        path,
        prev,
        max_new_bytes,
        MalformedJsonlPolicy::Defer,
        max_record_bytes,
    )?;
    Some(match reason {
        Some(reason) => StrictJsonlOutcome::Deferred { parsed, reason },
        None => StrictJsonlOutcome::Complete(parsed),
    })
}

pub(super) fn stream_new_jsonl_with_policy(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    malformed_policy: MalformedJsonlPolicy,
    max_record_bytes: usize,
) -> Option<(NewJsonl, Option<JsonlFrameDeferral>, u64)> {
    let mut raw = match try_stream_new_jsonl_raw_with_policy(
        path,
        prev,
        max_new_bytes,
        malformed_policy,
        max_record_bytes,
        None,
    ) {
        Ok(raw) => raw,
        Err(error) => {
            log_source_skip(path, "scan jsonl transcript", &error);
            return None;
        }
    };
    let mut lines = Vec::new();
    let mut covered_through = raw.new_cursor.position;

    for frame in raw.frames.drain(..) {
        if frame.bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice::<Value>(&frame.bytes) {
            Ok(value) => lines.push(JsonlLine {
                offset: frame.offset as i64,
                value,
            }),
            Err(error) => match malformed_policy {
                MalformedJsonlPolicy::Skip => {
                    log_jsonl_decode_skip(path, frame.offset, &error);
                }
                MalformedJsonlPolicy::Defer => {
                    covered_through = frame.offset;
                    raw.deferred = Some(JsonlFrameDeferral::Malformed {
                        offset: frame.offset,
                    });
                    break;
                }
            },
        }
    }
    raw.new_cursor.position = covered_through;

    Some((
        NewJsonl {
            lines,
            new_cursor: raw.new_cursor,
        },
        raw.deferred,
        raw.start_offset,
    ))
}

/// One bounded, complete raw JSONL frame with its exact source byte range.
pub struct RawJsonlRecord {
    pub offset: u64,
    pub end_offset: u64,
    pub resume_fingerprint: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawJsonlSkippedReason {
    Whitespace,
    Oversized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawJsonlSkippedRange {
    pub offset: u64,
    pub end_offset: u64,
    pub resume_fingerprint: u64,
    pub reason: RawJsonlSkippedReason,
}

/// Raw framing result. No JSON parser has inspected these bytes.
pub struct RawNewJsonl {
    pub frames: Vec<RawJsonlRecord>,
    pub skipped: Vec<RawJsonlSkippedRange>,
    pub start_offset: u64,
    /// Furthest absolute source position inspected, including a partial frame
    /// that cannot advance the durable cursor.
    pub read_through: u64,
    pub file_identity: u64,
    pub new_cursor: StoredCursor,
    pub deferred: Option<JsonlFrameDeferral>,
}

/// Strict bounded framing used by Claude's single-parse privacy boundary.
#[cfg(test)]
pub fn stream_new_jsonl_raw_strict(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
) -> Option<RawNewJsonl> {
    match try_stream_new_jsonl_raw_strict(path, prev, max_new_bytes, max_record_bytes) {
        Ok(raw) => Some(raw),
        Err(error) => {
            log_source_skip(path, "scan strict jsonl transcript", &error);
            None
        }
    }
}

#[cfg(test)]
pub fn try_stream_new_jsonl_raw_strict(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
) -> TranscriptIngestResult<RawNewJsonl> {
    try_stream_new_jsonl_raw_strict_with_resume(path, prev, max_new_bytes, max_record_bytes, None)
}

pub fn try_stream_new_jsonl_raw_strict_with_resume(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
    resume_state: Option<JsonlResumeState>,
) -> TranscriptIngestResult<RawNewJsonl> {
    let one_record_bytes = u64::try_from(max_record_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let recovery_batch_bytes = STRICT_JSONL_BATCH_BYTES.max(one_record_bytes);
    try_stream_new_jsonl_raw_with_policy(
        path,
        prev,
        Some(max_new_bytes.unwrap_or(recovery_batch_bytes)),
        MalformedJsonlPolicy::Defer,
        max_record_bytes,
        resume_state,
    )
}

fn try_stream_new_jsonl_raw_with_policy(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    oversized_policy: MalformedJsonlPolicy,
    max_record_bytes: usize,
    resume_state: Option<JsonlResumeState>,
) -> TranscriptIngestResult<RawNewJsonl> {
    let file = std::fs::File::open(path)
        .map_err(|error| TranscriptIngestError::scan_io("open", path, error))?;
    try_stream_new_jsonl_raw_from_file(
        path,
        file,
        RawJsonlScanRequest {
            previous: prev,
            max_new_bytes,
            oversized_policy,
            max_record_bytes,
            resume_state,
        },
        || {},
    )
}

#[derive(Clone, Copy)]
struct JsonlScanGeneration {
    file_size: u64,
    mtime: u64,
    file_id: u64,
    file_identity: u64,
    snapshot_fingerprint: u64,
    seek_to: u64,
}

struct PreparedJsonlScan {
    file: std::fs::File,
    generation: JsonlScanGeneration,
}

impl PreparedJsonlScan {
    fn capture(
        path: &Path,
        mut file: std::fs::File,
        previous: StoredCursor,
        resume_state: Option<JsonlResumeState>,
        after_generation_capture: impl FnOnce(),
    ) -> TranscriptIngestResult<Self> {
        let metadata = file
            .metadata()
            .map_err(|error| TranscriptIngestError::scan_io("fstat", path, error))?;
        let file_size = metadata.len();
        let mtime = file_mtime_secs(&metadata);
        let file_identity = stable_jsonl_file_id(&mut file, &metadata)
            .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        let snapshot_fingerprint = bounded_jsonl_snapshot_fingerprint(&mut file, file_size)
            .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        after_generation_capture();
        let (seek_to, file_id) = if let Some(resume_state) = resume_state {
            let resume_matches = previous.position > 0
                && previous.file_id == resume_state.generation
                && file_size >= previous.position
                && file_identity == resume_state.file_identity
                && read_jsonl_resume_tail(&mut file, previous.position).is_ok_and(|tail| {
                    resume_tail_fingerprint(previous.position, &tail) == resume_state.fingerprint
                });
            if resume_matches {
                (previous.position, resume_state.generation)
            } else {
                (
                    0,
                    if file_identity == resume_state.file_identity {
                        rewritten_jsonl_generation(
                            resume_state,
                            file_identity,
                            snapshot_fingerprint,
                            file_size,
                            mtime,
                        )
                    } else {
                        file_identity
                    },
                )
            }
        } else if should_resume_jsonl(previous, file_size, mtime, file_identity) {
            (previous.position, file_identity)
        } else {
            (0, file_identity)
        };
        Ok(Self {
            file,
            generation: JsonlScanGeneration {
                file_size,
                mtime,
                file_id,
                file_identity,
                snapshot_fingerprint,
                seek_to,
            },
        })
    }

    fn is_complete(&self) -> bool {
        self.generation.seek_to >= self.generation.file_size
    }

    fn into_empty_outcome(self) -> RawNewJsonl {
        let generation = self.generation;
        RawNewJsonl {
            frames: Vec::new(),
            skipped: Vec::new(),
            start_offset: generation.seek_to,
            read_through: generation.seek_to,
            file_identity: generation.file_identity,
            new_cursor: StoredCursor {
                position: generation.seek_to,
                mtime: generation.mtime,
                file_id: generation.file_id,
            },
            deferred: None,
        }
    }
}

enum JsonlScanStep {
    Continue,
    Stop(Option<JsonlFrameDeferral>),
}

struct RawJsonlBatchScanner {
    reader: RawJsonlFrameReader<BufReader<std::fs::File>>,
    generation: JsonlScanGeneration,
    max_new_bytes: Option<u64>,
    scan_end: Option<u64>,
    one_record_budget: u64,
    max_record_bytes: usize,
    frames: Vec<RawJsonlRecord>,
    skipped: Vec<RawJsonlSkippedRange>,
    offset: u64,
    read_through: u64,
    continuing_oversized: bool,
    frame_count: usize,
    deferred: Option<JsonlFrameDeferral>,
}

impl RawJsonlBatchScanner {
    fn start(
        path: &Path,
        prepared: PreparedJsonlScan,
        max_new_bytes: Option<u64>,
        max_record_bytes: usize,
    ) -> TranscriptIngestResult<Self> {
        let generation = prepared.generation;
        let mut file = prepared.file;
        let resume_tail = read_jsonl_resume_tail(&mut file, generation.seek_to)
            .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        let mut reader = BufReader::new(file);
        reader
            .seek(SeekFrom::Start(generation.seek_to))
            .map_err(|error| TranscriptIngestError::scan_io("seek", path, error))?;
        let continuing_oversized =
            Self::starts_inside_record(path, &mut reader, generation.seek_to)?;
        let frame_limit = if continuing_oversized {
            0
        } else {
            max_record_bytes
        };
        let mut reader = RawJsonlFrameReader::new(reader, frame_limit);
        reader.seed_resume_tail(&resume_tail);
        Ok(Self {
            reader,
            generation,
            max_new_bytes,
            scan_end: max_new_bytes.map(|cap| {
                generation
                    .seek_to
                    .saturating_add(cap)
                    .min(generation.file_size)
            }),
            one_record_budget: u64::try_from(max_record_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
            max_record_bytes,
            frames: Vec::new(),
            skipped: Vec::new(),
            offset: generation.seek_to,
            read_through: generation.seek_to,
            continuing_oversized,
            frame_count: 0,
            deferred: None,
        })
    }

    fn starts_inside_record(
        path: &Path,
        reader: &mut BufReader<std::fs::File>,
        seek_to: u64,
    ) -> TranscriptIngestResult<bool> {
        if seek_to == 0 {
            return Ok(false);
        }
        let mut previous = [0_u8; 1];
        reader
            .seek(SeekFrom::Start(seek_to - 1))
            .map_err(|error| TranscriptIngestError::scan_io("seek", path, error))?;
        reader
            .read_exact(&mut previous)
            .map_err(|error| TranscriptIngestError::scan_io("read", path, error))?;
        reader
            .seek(SeekFrom::Start(seek_to))
            .map_err(|error| TranscriptIngestError::scan_io("seek", path, error))?;
        Ok(previous[0] != b'\n')
    }

    fn scan(
        mut self,
        path: &Path,
        oversized_policy: MalformedJsonlPolicy,
    ) -> TranscriptIngestResult<Self> {
        loop {
            if let Some(step) = self.boundary_step() {
                self.apply_step(&step);
                return Ok(self);
            }
            let read_budget = self.read_budget();
            let frame = self
                .reader
                .next_frame_with_budget(read_budget)
                .map_err(|error| TranscriptIngestError::scan_io("read", path, error))?;
            self.read_through = self
                .read_through
                .max(self.offset.saturating_add(frame.byte_len()));
            self.frame_count = self.frame_count.saturating_add(1);
            let resume_fingerprint = self
                .reader
                .resume_fingerprint(self.offset.saturating_add(frame.byte_len()));
            let step = match frame {
                RawJsonlFrame::Eof => JsonlScanStep::Stop(None),
                RawJsonlFrame::Partial { .. } => {
                    JsonlScanStep::Stop(Some(JsonlFrameDeferral::Partial {
                        offset: self.offset,
                    }))
                }
                RawJsonlFrame::Oversized {
                    byte_len,
                    terminated,
                } => self.handle_oversized(
                    path,
                    oversized_policy,
                    byte_len,
                    terminated,
                    resume_fingerprint,
                ),
                RawJsonlFrame::BudgetExhausted {
                    byte_len,
                    oversized,
                } => self.handle_budget_exhausted(byte_len, oversized, resume_fingerprint),
                RawJsonlFrame::Complete { byte_len } => {
                    self.handle_complete(byte_len, resume_fingerprint)
                }
            };
            if self.apply_step(&step) {
                return Ok(self);
            }
        }
    }

    fn boundary_step(&self) -> Option<JsonlScanStep> {
        if self.offset >= self.generation.file_size {
            return Some(JsonlScanStep::Stop(None));
        }
        let budget_exhausted = self
            .scan_end
            .is_some_and(|end| self.offset >= end && self.offset < self.generation.file_size);
        if budget_exhausted || self.frame_count >= MAX_JSONL_FRAMES_PER_BATCH {
            return Some(JsonlScanStep::Stop(self.backlog_at(self.offset)));
        }
        None
    }

    fn read_budget(&self) -> u64 {
        let nominal = self
            .scan_end
            .map_or(u64::MAX, |end| end.saturating_sub(self.offset));
        // Finish at most the current valid record past the nominal cap. Chunks
        // already known to be oversized retain the nominal budget.
        if self.continuing_oversized {
            nominal
        } else {
            nominal.max(self.one_record_budget)
        }
    }

    fn handle_oversized(
        &mut self,
        path: &Path,
        policy: MalformedJsonlPolicy,
        byte_len: u64,
        terminated: bool,
        resume_fingerprint: u64,
    ) -> JsonlScanStep {
        let next_offset = self.offset.saturating_add(byte_len);
        match (policy, terminated) {
            (MalformedJsonlPolicy::Skip, true) => {
                if self.scan_end.is_some_and(|end| next_offset > end) {
                    return JsonlScanStep::Stop(self.backlog_at(self.offset));
                }
                log_jsonl_oversized_skip(path, self.offset, byte_len);
                self.offset = next_offset;
                // Having consumed the tail of the record this scan resumed
                // inside, restore the real record budget. Without this the
                // reader keeps the zero limit it was resumed with and reports
                // every subsequent valid record as oversized, skipping them and
                // advancing the durable cursor past them for good.
                if terminated && self.continuing_oversized {
                    self.reader.set_max_record_bytes(self.max_record_bytes);
                    self.continuing_oversized = false;
                }
                JsonlScanStep::Continue
            }
            (MalformedJsonlPolicy::Skip, false) => {
                JsonlScanStep::Stop(Some(JsonlFrameDeferral::Partial {
                    offset: self.offset,
                }))
            }
            (MalformedJsonlPolicy::Defer, _) => {
                self.push_skipped(
                    next_offset,
                    RawJsonlSkippedReason::Oversized,
                    resume_fingerprint,
                );
                self.offset = next_offset;
                if terminated && self.continuing_oversized {
                    self.reader.set_max_record_bytes(self.max_record_bytes);
                    self.continuing_oversized = false;
                }
                if self.offset < self.generation.file_size {
                    JsonlScanStep::Stop(self.backlog_at(self.offset))
                } else {
                    JsonlScanStep::Continue
                }
            }
        }
    }

    fn handle_budget_exhausted(
        &mut self,
        byte_len: u64,
        oversized: bool,
        resume_fingerprint: u64,
    ) -> JsonlScanStep {
        if oversized && byte_len > 0 {
            let next_offset = self.offset.saturating_add(byte_len);
            self.push_skipped(
                next_offset,
                RawJsonlSkippedReason::Oversized,
                resume_fingerprint,
            );
            self.offset = next_offset;
        }
        JsonlScanStep::Stop(self.backlog_at(self.offset))
    }

    fn handle_complete(&mut self, byte_len: u64, resume_fingerprint: u64) -> JsonlScanStep {
        let next_offset = self.offset.saturating_add(byte_len);
        if self.offset > self.generation.seek_to
            && self.scan_end.is_some_and(|end| next_offset > end)
        {
            return JsonlScanStep::Stop(self.backlog_at(self.offset));
        }
        if self.reader.record().iter().all(u8::is_ascii_whitespace) {
            self.push_skipped(
                next_offset,
                RawJsonlSkippedReason::Whitespace,
                resume_fingerprint,
            );
        } else {
            self.frames.push(RawJsonlRecord {
                offset: self.offset,
                end_offset: next_offset,
                resume_fingerprint,
                bytes: self.reader.record().to_vec(),
            });
        }
        self.offset = next_offset;
        JsonlScanStep::Continue
    }

    fn push_skipped(
        &mut self,
        end_offset: u64,
        reason: RawJsonlSkippedReason,
        resume_fingerprint: u64,
    ) {
        if let Some(last) = self.skipped.last_mut()
            && last.reason == reason
            && last.end_offset == self.offset
        {
            last.end_offset = end_offset;
            last.resume_fingerprint = resume_fingerprint;
        } else {
            self.skipped.push(RawJsonlSkippedRange {
                offset: self.offset,
                end_offset,
                resume_fingerprint,
                reason,
            });
        }
    }

    fn backlog_at(&self, offset: u64) -> Option<JsonlFrameDeferral> {
        self.max_new_bytes
            .map(|max_new_bytes| JsonlFrameDeferral::Backlog {
                offset,
                unread_bytes: self.generation.file_size.saturating_sub(offset),
                max_new_bytes,
            })
    }

    fn apply_step(&mut self, step: &JsonlScanStep) -> bool {
        match step {
            JsonlScanStep::Continue => false,
            JsonlScanStep::Stop(deferred) => {
                self.deferred = *deferred;
                true
            }
        }
    }

    fn revalidate(self, path: &Path) -> TranscriptIngestResult<RawNewJsonl> {
        let mut file = self.reader.into_inner().into_inner();
        let final_metadata = file
            .metadata()
            .map_err(|error| TranscriptIngestError::scan_io("fstat", path, error))?;
        let final_file_id = stable_jsonl_file_id(&mut file, &final_metadata)
            .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        let final_snapshot =
            bounded_jsonl_snapshot_fingerprint(&mut file, self.generation.file_size)
                .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        if final_file_id != self.generation.file_identity
            || final_snapshot != self.generation.snapshot_fingerprint
            || final_metadata.len() < self.read_through
        {
            return Err(TranscriptIngestError::ScanGenerationChanged {
                path: path.to_path_buf(),
            });
        }
        Ok(RawNewJsonl {
            frames: self.frames,
            skipped: self.skipped,
            start_offset: self.generation.seek_to,
            read_through: self.read_through,
            file_identity: self.generation.file_identity,
            new_cursor: StoredCursor {
                position: self.offset,
                mtime: file_mtime_secs(&final_metadata),
                file_id: self.generation.file_id,
            },
            deferred: self.deferred,
        })
    }
}

fn try_stream_new_jsonl_raw_from_file(
    path: &Path,
    file: std::fs::File,
    request: RawJsonlScanRequest,
    after_generation_capture: impl FnOnce(),
) -> TranscriptIngestResult<RawNewJsonl> {
    let RawJsonlScanRequest {
        previous,
        max_new_bytes,
        oversized_policy,
        max_record_bytes,
        resume_state,
    } = request;
    let prepared =
        PreparedJsonlScan::capture(path, file, previous, resume_state, after_generation_capture)?;
    if prepared.is_complete() {
        return Ok(prepared.into_empty_outcome());
    }
    RawJsonlBatchScanner::start(path, prepared, max_new_bytes, max_record_bytes)?
        .scan(path, oversized_policy)?
        .revalidate(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_after_generation_capture_scans_one_handle_then_resets_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("active.jsonl");
        let moved = dir.path().join("moved.jsonl");
        let original = b"{\"id\":\"original\"}\n";
        let replacement = b"{\"id\":\"replaced\"}\n";
        std::fs::write(&path, original).unwrap();
        let handle = std::fs::File::open(&path).unwrap();

        let first = try_stream_new_jsonl_raw_from_file(
            &path,
            handle,
            RawJsonlScanRequest {
                previous: StoredCursor::default(),
                max_new_bytes: None,
                oversized_policy: MalformedJsonlPolicy::Defer,
                max_record_bytes: MAX_JSONL_RECORD_BYTES,
                resume_state: None,
            },
            || {
                std::fs::rename(&path, &moved).unwrap();
                std::fs::write(&path, replacement).unwrap();
            },
        )
        .unwrap();

        assert_eq!(first.frames.len(), 1);
        assert_eq!(first.frames[0].bytes, original);
        assert_eq!(first.new_cursor.position, original.len() as u64);

        let second =
            try_stream_new_jsonl_raw_strict(&path, first.new_cursor, None, MAX_JSONL_RECORD_BYTES)
                .unwrap();
        assert_eq!(second.start_offset, 0);
        assert_eq!(second.frames.len(), 1);
        assert_eq!(second.frames[0].bytes, replacement);
        assert_eq!(second.new_cursor.position, replacement.len() as u64);
    }

    #[test]
    fn same_handle_mutation_after_generation_capture_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mutated.jsonl");
        let original = b"{\"id\":\"original\"}\n";
        let replacement = b"{\"id\":\"replaced\"}\n";
        assert_eq!(original.len(), replacement.len());
        std::fs::write(&path, original).unwrap();
        let handle = std::fs::File::open(&path).unwrap();

        let error = try_stream_new_jsonl_raw_from_file(
            &path,
            handle,
            RawJsonlScanRequest {
                previous: StoredCursor::default(),
                max_new_bytes: None,
                oversized_policy: MalformedJsonlPolicy::Defer,
                max_record_bytes: MAX_JSONL_RECORD_BYTES,
                resume_state: None,
            },
            || std::fs::write(&path, replacement).unwrap(),
        )
        .err()
        .expect("same-handle mutation must invalidate the scan generation");

        assert!(matches!(
            error,
            TranscriptIngestError::ScanGenerationChanged { path: error_path }
                if error_path == path
        ));
    }
}
