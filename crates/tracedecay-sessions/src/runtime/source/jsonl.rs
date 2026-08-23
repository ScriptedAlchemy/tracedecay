use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    StoredCursor, TranscriptIngestError, TranscriptIngestResult, file_mtime_secs,
    log_jsonl_decode_skip, log_jsonl_oversized_skip, log_source_skip, should_resume_jsonl,
    stable_jsonl_file_id,
};

pub struct JsonlLine {
    pub offset: i64,
    pub value: Value,
}

pub struct NewJsonl {
    pub lines: Vec<JsonlLine>,
    pub new_cursor: StoredCursor,
    /// Absolute source offset this batch resumed from.
    pub start_offset: u64,
    /// Whether `new_cursor.file_id` names a replacement (truncate-and-rewrite)
    /// generation instead of the file's append-only identity. It is a property
    /// of the stored cursor, so every batch of one replacement generation
    /// reports it -- not just the batch that starts at offset zero.
    pub replacement_generation: bool,
}

pub use crate::runtime::hotpath::{JsonlChangeKind, JsonlIoAccounting};

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
const JSONL_HASH_CHUNK_BYTES: usize = 64 * 1024;
const UNCHANGED_PREFIX_MEMO_CAP: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct UnchangedPrefixMemoKey {
    file_identity: u64,
    size: u64,
    change: JsonlFileChangeToken,
    fingerprint: u64,
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct JsonlFileChangeToken {
    mtime_seconds: i64,
    mtime_nanos: i64,
    ctime_seconds: i64,
    ctime_nanos: i64,
}

#[cfg(unix)]
fn jsonl_file_change_token(metadata: &std::fs::Metadata) -> JsonlFileChangeToken {
    JsonlFileChangeToken {
        mtime_seconds: metadata.mtime(),
        mtime_nanos: metadata.mtime_nsec(),
        ctime_seconds: metadata.ctime(),
        ctime_nanos: metadata.ctime_nsec(),
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct JsonlFileChangeToken {
    last_write_time: u64,
}

#[cfg(windows)]
fn jsonl_file_change_token(metadata: &std::fs::Metadata) -> JsonlFileChangeToken {
    JsonlFileChangeToken {
        last_write_time: metadata.last_write_time(),
    }
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct JsonlFileChangeToken {
    modified_nanos: Option<u128>,
}

#[cfg(not(any(unix, windows)))]
fn jsonl_file_change_token(metadata: &std::fs::Metadata) -> JsonlFileChangeToken {
    JsonlFileChangeToken {
        modified_nanos: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos()),
    }
}

fn unchanged_prefix_memo() -> &'static Mutex<HashMap<UnchangedPrefixMemoKey, ()>> {
    static MEMO: std::sync::OnceLock<Mutex<HashMap<UnchangedPrefixMemoKey, ()>>> =
        std::sync::OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

fn unchanged_prefix_memo_hit(key: UnchangedPrefixMemoKey) -> bool {
    unchanged_prefix_memo()
        .lock()
        .map(|memo| memo.contains_key(&key))
        .unwrap_or(false)
}

fn remember_unchanged_prefix(key: UnchangedPrefixMemoKey) {
    let Ok(mut memo) = unchanged_prefix_memo().lock() else {
        return;
    };
    if memo.len() >= UNCHANGED_PREFIX_MEMO_CAP {
        memo.clear();
    }
    memo.insert(key, ());
}

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

#[derive(Clone)]
struct ResumeDigest {
    hasher: Sha256,
}

impl ResumeDigest {
    fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"tracedecay-jsonl-resume-prefix-v2");
        Self { hasher }
    }

    fn extend(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    /// 64-bit check of the hashed prefix plus `position`. This is not SHA-256
    /// mid-state: the domain cursor (`StoredCursor` + optional resume
    /// fingerprint) has no field that can carry hasher bytes without
    /// displacing `file_id` / generation, which would collapse rewrite and
    /// file-identity detection. A first append after a durable resume must
    /// therefore re-walk `[0, cursor)` to rebuild this digest.
    fn fingerprint(&self, position: u64) -> u64 {
        let mut hasher = self.hasher.clone();
        hasher.update(position.to_le_bytes());
        digest_prefix_u64(hasher.finalize())
    }
}

fn digest_prefix_u64(digest: sha2::digest::Output<Sha256>) -> u64 {
    let [
        first,
        second,
        third,
        fourth,
        fifth,
        sixth,
        seventh,
        eighth,
        ..,
    ] = <[u8; 32]>::from(digest);
    u64::from_be_bytes([first, second, third, fourth, fifth, sixth, seventh, eighth])
}

fn jsonl_prefix_digest(
    file: &mut std::fs::File,
    extent: u64,
) -> std::io::Result<(ResumeDigest, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = extent;
    let mut hashed = 0_u64;
    let mut buffer = vec![0_u8; JSONL_HASH_CHUNK_BYTES];
    let mut digest = ResumeDigest::new();
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "JSONL prefix ended during generation hashing",
            ));
        }
        digest.extend(&buffer[..read]);
        hashed = hashed.saturating_add(read as u64);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok((digest, hashed))
}

/// Memoized [`bounded_jsonl_snapshot_fingerprint`]: the hash walks the whole
/// extent, so callers compute it at most once per scan and only on paths that
/// actually consume it.
fn memoized_jsonl_snapshot_fingerprint(
    cache: &mut Option<u64>,
    hashed_bytes: &mut u64,
    file: &mut std::fs::File,
    extent: u64,
) -> std::io::Result<u64> {
    if let Some(fingerprint) = *cache {
        return Ok(fingerprint);
    }
    let (fingerprint, hashed) = bounded_jsonl_snapshot_fingerprint(file, extent)?;
    *cache = Some(fingerprint);
    *hashed_bytes = hashed_bytes.saturating_add(hashed);
    Ok(fingerprint)
}

fn bounded_jsonl_snapshot_fingerprint(
    file: &mut std::fs::File,
    extent: u64,
) -> std::io::Result<(u64, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-snapshot-v2");
    hasher.update(extent.to_le_bytes());
    let mut remaining = extent;
    let mut hashed = 0_u64;
    let mut buffer = vec![0_u8; JSONL_HASH_CHUNK_BYTES];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "JSONL snapshot ended during generation hashing",
            ));
        }
        hasher.update(&buffer[..read]);
        hashed = hashed.saturating_add(read as u64);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok((digest_prefix_u64(hasher.finalize()), hashed))
}

/// Chain depth for replacement markers derived from one file identity.
///
/// The marker for a rewritten file must be recoverable from the persisted
/// cursor alone (there is no room for a separate generation column), so it is
/// re-derived by searching this bounded counter space. Successive rewrites of a
/// file whose identity never changes therefore stay distinct until the counter
/// wraps.
const MAX_JSONL_REPLACEMENT_GENERATIONS: u32 = 64;

/// Deterministic replacement marker for `counter`-th rewrite of `file_identity`.
///
/// Never zero: the resume check reads zero as "identity unknown".
fn replacement_jsonl_generation(file_identity: u64, counter: u32) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-replacement-generation-v1");
    hasher.update(file_identity.to_le_bytes());
    hasher.update(counter.to_le_bytes());
    digest_prefix_u64(hasher.finalize()).max(1)
}

fn replacement_generation_counter(file_identity: u64, generation: u64) -> Option<u32> {
    if generation == 0 || generation == file_identity {
        return None;
    }
    (1..=MAX_JSONL_REPLACEMENT_GENERATIONS)
        .find(|counter| replacement_jsonl_generation(file_identity, *counter) == generation)
}

/// Whether `generation` was minted for a rewritten generation of this file.
pub(super) fn is_replacement_jsonl_generation(file_identity: u64, generation: u64) -> bool {
    replacement_generation_counter(file_identity, generation).is_some()
}

/// Marker for the generation that replaces the one `previous_generation` names.
fn next_replacement_jsonl_generation(file_identity: u64, previous_generation: u64) -> u64 {
    let counter = replacement_generation_counter(file_identity, previous_generation)
        .map_or(1, |counter| counter % MAX_JSONL_REPLACEMENT_GENERATIONS + 1);
    replacement_jsonl_generation(file_identity, counter)
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
    digest_prefix_u64(hasher.finalize()).max(1)
}

/// Bounded raw JSONL framing shared by skip and defer policies.
///
/// The retained record buffer never exceeds `max_record_bytes`. Oversized
/// frames are drained in-place so legacy callers can skip complete records and
/// continue without allocating or parsing them.
pub struct RawJsonlFrameReader<R> {
    reader: R,
    record: Vec<u8>,
    resume_digest: ResumeDigest,
    max_record_bytes: usize,
}

impl<R: BufRead> RawJsonlFrameReader<R> {
    pub fn new(reader: R, max_record_bytes: usize) -> Self {
        Self {
            reader,
            record: Vec::new(),
            resume_digest: ResumeDigest::new(),
            max_record_bytes,
        }
    }

    fn seed_resume_digest(&mut self, digest: ResumeDigest) {
        self.resume_digest = digest;
    }

    fn resume_fingerprint(&self, position: u64) -> u64 {
        self.resume_digest.fingerprint(position)
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

    pub fn next_frame_with_budget(&mut self, read_budget: u64) -> std::io::Result<RawJsonlFrame> {
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
            self.resume_digest.extend(&available[..consumed]);
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
#[hotpath::measure]
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
#[hotpath::measure]
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

#[hotpath::measure]
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
            start_offset: raw.start_offset,
            replacement_generation: raw.replacement_generation,
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
    /// Whether `new_cursor.file_id` names a replacement generation rather than
    /// the append-only identity of the scanned file.
    pub replacement_generation: bool,
    pub deferred: Option<JsonlFrameDeferral>,
    pub io: JsonlIoAccounting,
}

/// Strict bounded framing used by Claude's single-parse privacy boundary.
#[cfg(test)]
#[hotpath::measure]
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
#[hotpath::measure]
pub fn try_stream_new_jsonl_raw_strict(
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    max_record_bytes: usize,
) -> TranscriptIngestResult<RawNewJsonl> {
    try_stream_new_jsonl_raw_strict_with_resume(path, prev, max_new_bytes, max_record_bytes, None)
}

#[hotpath::measure]
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
    /// `None` only when the scan proved it would read nothing, so no batch —
    /// and therefore no revalidation — consumes it.
    snapshot_fingerprint: Option<u64>,
    seek_to: u64,
    replacement: bool,
}

struct PreparedJsonlScan {
    file: std::fs::File,
    generation: JsonlScanGeneration,
    io: JsonlIoAccounting,
    /// Prefix digest already computed while validating the resume checkpoint,
    /// with the exact extent it covers. `RawJsonlBatchScanner::start` needs the
    /// same digest to seed the reader, so carrying it forward keeps one scan to
    /// one pass over the prefix instead of hashing those bytes a second time.
    validated_prefix: Option<(u64, ResumeDigest)>,
}

impl PreparedJsonlScan {
    #[hotpath::measure]
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
        let (file_identity, identity_window_bytes) = stable_jsonl_file_id(&mut file, &metadata)
            .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        crate::runtime::hotpath::record_file_opened();
        let mut io = JsonlIoAccounting {
            identity_window_bytes,
            ..JsonlIoAccounting::default()
        };
        // The snapshot fingerprint hashes the whole extent, so it is captured
        // lazily: only rewrite-marker minting and scans that will actually
        // read bytes pay for it. A no-change poll whose cursor already sits
        // at end-of-file skips the hash entirely.
        let mut snapshot_fingerprint = None;
        // Retains the digest computed below so the scanner can seed its reader
        // from it instead of walking the same prefix a second time.
        let mut validated_prefix: Option<(u64, ResumeDigest)> = None;
        let (seek_to, file_id) = if let Some(resume_state) = resume_state {
            let identity_matches = previous.position > 0
                && previous.file_id == resume_state.generation
                && file_size >= previous.position
                && file_identity == resume_state.file_identity;
            let memo_key = UnchangedPrefixMemoKey {
                file_identity,
                size: file_size,
                change: jsonl_file_change_token(&metadata),
                fingerprint: resume_state.fingerprint,
            };
            // A later unchanged poll with the same identity, size, and
            // high-resolution change token can skip the prefix walk. Unix
            // ctime additionally detects attempts to restore mtime after an
            // in-place rewrite.
            let resume_matches = identity_matches
                && ((file_size == previous.position && unchanged_prefix_memo_hit(memo_key))
                    || match jsonl_prefix_digest(&mut file, previous.position) {
                        Ok((digest, hashed)) => {
                            io.prefix_validation_bytes =
                                io.prefix_validation_bytes.saturating_add(hashed);
                            let matched =
                                digest.fingerprint(previous.position) == resume_state.fingerprint;
                            if matched {
                                validated_prefix = Some((previous.position, digest));
                                if file_size == previous.position {
                                    remember_unchanged_prefix(memo_key);
                                }
                            }
                            matched
                        }
                        Err(_) => false,
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
                            memoized_jsonl_snapshot_fingerprint(
                                &mut snapshot_fingerprint,
                                &mut io.snapshot_hash_bytes,
                                &mut file,
                                file_size,
                            )
                            .map_err(|error| {
                                TranscriptIngestError::scan_io("fingerprint", path, error)
                            })?,
                            file_size,
                            mtime,
                        )
                    } else {
                        file_identity
                    },
                )
            }
        } else if should_resume_jsonl(previous, file_size, mtime, file_identity) {
            // Carry the stored generation forward: a replacement marker minted
            // when this file was rewritten must survive every later batch of
            // that generation, otherwise batch two would re-mint ids that
            // collide with the retained pre-rewrite rows.
            (
                previous.position,
                if previous.file_id == 0 {
                    file_identity
                } else {
                    previous.file_id
                },
            )
        } else if previous.position > 0 {
            // The cursor is being rewound to the head of a file it had already
            // read past: this generation replaces the recorded one.
            (
                0,
                next_replacement_jsonl_generation(file_identity, previous.file_id),
            )
        } else if is_replacement_jsonl_generation(file_identity, previous.file_id) {
            // A replacement that was truncated to nothing keeps its marker so
            // the records written into it stay namespaced.
            (0, previous.file_id)
        } else {
            (0, file_identity)
        };
        // The fingerprint must be captured before `after_generation_capture`
        // (the seam that simulates concurrent mutation) so revalidation still
        // rejects same-handle rewrites that race the scan. Append-only resumes
        // keep identity + size; hashing the whole file again would re-read the
        // already-validated prefix.
        if seek_to == 0 && seek_to < file_size {
            memoized_jsonl_snapshot_fingerprint(
                &mut snapshot_fingerprint,
                &mut io.snapshot_hash_bytes,
                &mut file,
                file_size,
            )
            .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        }
        after_generation_capture();
        // A generation that is not the file's own identity was minted for a
        // rewrite, so it stays flagged for every batch it covers. The rewind
        // clause additionally covers the first batch after a file was replaced
        // by a different identity, whose generation is that new identity.
        let replacement = file_id != file_identity || (seek_to == 0 && previous.position > 0);
        io.change = if replacement {
            JsonlChangeKind::Rewritten
        } else if seek_to >= file_size {
            JsonlChangeKind::Unchanged
        } else {
            JsonlChangeKind::Appended
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
                replacement,
            },
            io,
            validated_prefix,
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
            replacement_generation: generation.replacement,
            deferred: None,
            io: self.io,
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
    io: JsonlIoAccounting,
}

impl RawJsonlBatchScanner {
    #[hotpath::measure]
    fn start(
        path: &Path,
        prepared: PreparedJsonlScan,
        max_new_bytes: Option<u64>,
        max_record_bytes: usize,
    ) -> TranscriptIngestResult<Self> {
        let generation = prepared.generation;
        let mut file = prepared.file;
        let mut io = prepared.io;
        let resume_digest = match prepared.validated_prefix {
            // `capture` already walked exactly this prefix to check the resume
            // checkpoint, and the digest it produced is the one this reader
            // needs. Re-deriving it would read the same bytes a second time
            // for an identical result.
            Some((extent, digest)) if extent == generation.seek_to => digest,
            _ => {
                let (digest, hashed) = jsonl_prefix_digest(&mut file, generation.seek_to)
                    .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
                io.prefix_validation_bytes = io.prefix_validation_bytes.saturating_add(hashed);
                digest
            }
        };
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
        reader.seed_resume_digest(resume_digest);
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
            io,
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
        let (final_file_id, identity_window_bytes) =
            stable_jsonl_file_id(&mut file, &final_metadata)
                .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
        let mut io = self.io;
        io.identity_window_bytes = io
            .identity_window_bytes
            .saturating_add(identity_window_bytes);
        let snapshot_changed = if let Some(expected_snapshot) = self.generation.snapshot_fingerprint
        {
            let (final_snapshot, snapshot_hashed) =
                bounded_jsonl_snapshot_fingerprint(&mut file, self.generation.file_size)
                    .map_err(|error| TranscriptIngestError::scan_io("fingerprint", path, error))?;
            io.snapshot_hash_bytes = io.snapshot_hash_bytes.saturating_add(snapshot_hashed);
            final_snapshot != expected_snapshot
        } else {
            false
        };
        io.content_bytes = self.read_through.saturating_sub(self.generation.seek_to);
        // Full-file scans compare the snapshot they captured. Append-only
        // resumes compare identity and size so a same-handle rewrite of the
        // head still fails closed without hashing the validated prefix again.
        if final_file_id != self.generation.file_identity
            || snapshot_changed
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
            replacement_generation: self.generation.replacement,
            deferred: self.deferred,
            io,
        })
    }
}

#[hotpath::measure]
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
    let raw = if prepared.is_complete() {
        prepared.into_empty_outcome()
    } else {
        RawJsonlBatchScanner::start(path, prepared, max_new_bytes, max_record_bytes)?
            .scan(path, oversized_policy)?
            .revalidate(path)?
    };
    crate::runtime::hotpath::record_jsonl_io(&raw.io);
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

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

    #[test]
    fn unchanged_resume_hashes_the_full_prefix_and_reads_zero_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unchanged.jsonl");
        std::fs::write(&path, b"{\"v\":0}\n{\"v\":1}\n").unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        let prefix = first.new_cursor.position;
        let second = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(second.io.change, JsonlChangeKind::Unchanged);
        assert_eq!(
            second.io.content_bytes, 0,
            "an unchanged poll must not consume frame bytes past the cursor"
        );
        assert_eq!(
            second.io.snapshot_hash_bytes, 0,
            "EOF resume skips the whole-file snapshot hash"
        );
        assert_eq!(
            second.io.prefix_validation_bytes, prefix,
            "the first unchanged poll still verifies the stored prefix"
        );
        assert!(
            second.io.identity_window_bytes > 0,
            "file identity still reads the head window"
        );
        let third = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            second.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(third.io.change, JsonlChangeKind::Unchanged);
        assert_eq!(third.io.content_bytes, 0);
        assert_eq!(
            third.io.prefix_validation_bytes, 0,
            "a later unchanged poll with the same identity/size/mtime skips the prefix walk"
        );
    }

    #[test]
    fn memoized_unchanged_prefix_revalidates_an_in_place_tail_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memo-rewrite.jsonl");
        let original = b"{\"v\":0}\n".repeat(3_000);
        std::fs::write(&path, &original).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };

        let unchanged = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(unchanged.start_offset, first.new_cursor.position);

        let mut rewritten = original;
        let tail = rewritten.len() - b"{\"v\":0}\n".len();
        rewritten[tail..].copy_from_slice(b"{\"v\":1}\n");
        std::fs::write(&path, rewritten).unwrap();

        let rescanned = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            unchanged.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        assert_eq!(
            rescanned.start_offset, 0,
            "a memoized prefix must be invalidated by a same-size in-place rewrite"
        );
        assert_ne!(rescanned.new_cursor.file_id, checkpoint.generation);
    }

    #[test]
    fn one_append_hashes_prefix_once_and_reads_appended_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("append.jsonl");
        let first_line = b"{\"v\":0}\n";
        std::fs::write(&path, first_line).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        let appended = b"{\"v\":1}\n";
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(appended)
            .unwrap();
        let second = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        let prefix = u64::try_from(first_line.len()).unwrap();
        let appended_len = u64::try_from(appended.len()).unwrap();
        assert_eq!(second.io.change, JsonlChangeKind::Appended);
        assert_eq!(second.io.content_bytes, appended_len);
        assert_eq!(
            second.io.prefix_validation_bytes, prefix,
            "one append verifies the stored prefix once and reuses that digest"
        );
        assert_eq!(
            second.io.snapshot_hash_bytes, 0,
            "append-only resume must not snapshot-hash the already-validated prefix"
        );
    }

    /// RED leftover: a durable resume is `(position, generation, file_identity,
    /// fingerprint)`. The fingerprint cannot seed `Sha256`, so the first
    /// append after a process-local-memo miss still walks `[0, cursor)`.
    /// Stashing hasher bytes in `StoredCursor::file_id` would weaken rewrite
    /// and file-identity detection. This test locks that invariant.
    #[test]
    fn first_append_after_durable_cursor_rewalks_prefix_to_rebuild_hasher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("durable-append.jsonl");
        let first_line = b"{\"v\":0}\n";
        std::fs::write(&path, first_line).unwrap();
        let first = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            StoredCursor::default(),
            None,
            MAX_JSONL_RECORD_BYTES,
            None,
        )
        .unwrap();
        let checkpoint = JsonlResumeState {
            generation: first.new_cursor.file_id,
            file_identity: first.file_identity,
            fingerprint: first.frames.last().unwrap().resume_fingerprint,
        };
        // Drop process-local unchanged memo by using a distinct path identity
        // window is still required; append changes size so the memo cannot
        // apply anyway. The durable cursor is only StoredCursor + checkpoint.
        let appended = b"{\"v\":1}\n";
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(appended)
            .unwrap();
        let second = try_stream_new_jsonl_raw_strict_with_resume(
            &path,
            first.new_cursor,
            None,
            MAX_JSONL_RECORD_BYTES,
            Some(checkpoint),
        )
        .unwrap();
        let prefix = u64::try_from(first_line.len()).unwrap();
        assert_eq!(second.io.change, JsonlChangeKind::Appended);
        assert_eq!(
            second.io.prefix_validation_bytes, prefix,
            "hasher mid-state is not in the domain cursor; first append re-walks [0, cursor)"
        );
        assert_eq!(
            second.io.content_bytes,
            u64::try_from(appended.len()).unwrap()
        );
        assert_eq!(second.new_cursor.file_id, checkpoint.generation);
    }
}
