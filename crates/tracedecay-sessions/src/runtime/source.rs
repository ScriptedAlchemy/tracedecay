//! Provider-neutral transcript discovery and source readers.
//!
//! Each host implements [`TranscriptSource`] to locate its transcripts and
//! name their durable cursor keys; its observation admission then reads them
//! with the readers here and admits canonical observations:
//!
//! * the strict raw JSONL frame readers (`try_stream_new_jsonl_raw_strict*`)
//!   for append-only transcripts (Cursor, Claude, Codex, …), resuming at a
//!   byte offset under a file-identity and prefix-fingerprint check;
//! * [`read_snapshot_file_bounded`] for complete snapshot documents (Cline,
//!   Roo Code, Kilo, Kiro), whose [`content_hash64`] is the source generation.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::ObservationScopeV1;
use tracedecay_store::{ParseOffset, TranscriptStoreError};

/// The identity primitive lives in the domain crate so the capture kernels,
/// which sit below this crate, share the exact framing. Re-exported here
/// because this module is where the session runtime's callers already import
/// it from.
pub use tracedecay_domain::canonical_text::canonical_framed_sha256;

pub use super::host_coverage::HostCoverageReason;
use crate::admission::{HostAdmission, HostAdmissionOutcome};
pub use crate::runtime::shared::{NewRows, StoredCursor, TranscriptIngestStats};
use tracedecay_framing::{WireReadOutcome, read_bounded_to_string};

pub type TranscriptIngestResult<T> = Result<T, TranscriptIngestError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostProviderCoverage {
    Complete,
    Partial,
    Unavailable,
}

/// Closed `host-coverage://` `file_id` code table. Codes 1-3 are the
/// reason-free states released before refusal reasons were recorded; each
/// unavailable refusal owns one further code. A code outside this table
/// decodes to no coverage, so the next sweep rewrites it.
impl HostProviderCoverage {
    pub(super) const fn file_id(self, reason: Option<HostCoverageReason>) -> u64 {
        match (self, reason) {
            (Self::Complete, _) => 1,
            (Self::Partial, _) => 2,
            (Self::Unavailable, None) => 3,
            (Self::Unavailable, Some(HostCoverageReason::DatabaseMissing)) => 4,
            (Self::Unavailable, Some(HostCoverageReason::DatabaseNotAFile)) => 5,
            (Self::Unavailable, Some(HostCoverageReason::DatabaseUnreadable)) => 6,
            (Self::Unavailable, Some(HostCoverageReason::SourceIdentityUnavailable)) => 7,
        }
    }

    pub(super) const fn from_file_id(file_id: u64) -> Option<(Self, Option<HostCoverageReason>)> {
        match file_id {
            1 => Some((Self::Complete, None)),
            2 => Some((Self::Partial, None)),
            3 => Some((Self::Unavailable, None)),
            4 => Some((Self::Unavailable, Some(HostCoverageReason::DatabaseMissing))),
            5 => Some((
                Self::Unavailable,
                Some(HostCoverageReason::DatabaseNotAFile),
            )),
            6 => Some((
                Self::Unavailable,
                Some(HostCoverageReason::DatabaseUnreadable),
            )),
            7 => Some((
                Self::Unavailable,
                Some(HostCoverageReason::SourceIdentityUnavailable),
            )),
            _ => None,
        }
    }
}

/// Durable typed Codex discovery frontier for project-scoped admission.
pub(super) const CODEX_HISTORY_FRONTIER_KEY: &str = "tracedecay-internal:codex-history-frontier:v2";
pub(super) const CODEX_HISTORY_EPOCH_KEY: &str = "tracedecay-internal:codex-history-epoch:v2";

pub(super) async fn read_host_provider_coverage(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    provider: &'static str,
) -> TranscriptIngestResult<Option<HostProviderCoverage>> {
    let key = format!("host-coverage://{provider}/v1");
    let offset = admission
        .get_parse_offset(scope, &key)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error(provider, outcome)
        })?;
    Ok(offset
        .and_then(|stored| HostProviderCoverage::from_file_id(stored.file_id))
        .map(|(coverage, _)| coverage))
}

pub(super) async fn read_codex_history_frontier(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
) -> TranscriptIngestResult<crate::runtime::hosts::codex::CodexDiscoveryFrontier> {
    let stored_frontier = admission
        .get_parse_offset(scope, CODEX_HISTORY_FRONTIER_KEY)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error("codex", outcome)
        })?
        .unwrap_or_default();
    let stored_epoch = admission
        .get_parse_offset(scope, CODEX_HISTORY_EPOCH_KEY)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error("codex", outcome)
        })?
        .unwrap_or_default();
    crate::runtime::hosts::codex::CodexDiscoveryFrontier::from_parse_offsets(
        stored_frontier,
        stored_epoch,
    )
}

pub(super) async fn persist_codex_history_frontier(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    expected: crate::runtime::hosts::codex::CodexDiscoveryFrontier,
    frontier: crate::runtime::hosts::codex::CodexDiscoveryFrontier,
) -> TranscriptIngestResult<()> {
    let (frontier_offset, epoch_offset) = frontier.into_parse_offsets();
    let (expected_frontier, expected_epoch) = expected.into_parse_offsets();
    admission
        .replace_parse_offset_pair(
            scope,
            (
                CODEX_HISTORY_FRONTIER_KEY,
                expected_frontier,
                frontier_offset,
            ),
            (CODEX_HISTORY_EPOCH_KEY, expected_epoch, epoch_offset),
        )
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error("codex", outcome)
        })
}

pub(super) async fn persist_host_provider_coverage(
    admission: &(impl HostAdmission + ?Sized),
    scope: &ObservationScopeV1,
    provider: &'static str,
    coverage: HostProviderCoverage,
    deferred_units: u64,
    reason: Option<HostCoverageReason>,
) -> TranscriptIngestResult<()> {
    let key = format!("host-coverage://{provider}/v1");
    let current = admission
        .get_parse_offset(scope, &key)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error(provider, outcome)
        })?;
    revise_host_record(
        admission,
        scope,
        &key,
        current,
        deferred_units,
        coverage.file_id(reason),
    )
    .await
    .map_err(|outcome| {
        crate::runtime::snapshot_observation::host_admission_error(provider, outcome)
    })
}

/// Writes a host bookkeeping record whose readers consult only its
/// `byte_offset` and `file_id`. Its `mtime` is a revision that lets a changed
/// record move `byte_offset` backwards past the monotonic cursor guard. An
/// unchanged record stays as stored, so a sweep that finds nothing new
/// commits nothing. An absent record is always written, even with default
/// values, or an empty host would never be observed.
pub(super) async fn revise_host_record(
    admission: &(impl HostAdmission + ?Sized),
    scope: &ObservationScopeV1,
    key: &str,
    stored: Option<ParseOffset>,
    byte_offset: u64,
    file_id: u64,
) -> Result<(), HostAdmissionOutcome> {
    if stored.is_some_and(|stored| (stored.byte_offset, stored.file_id) == (byte_offset, file_id)) {
        return Ok(());
    }
    let current = stored.unwrap_or_default();
    admission
        .advance_parse_offset(
            scope,
            key,
            ParseOffset {
                byte_offset,
                mtime: current.mtime.saturating_add(1).max(1),
                file_id,
            },
        )
        .await
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TranscriptIngestError {
    #[error("{provider} transcript ingestion was cancelled")]
    Cancelled { provider: &'static str },
    #[error(transparent)]
    Store(#[from] TranscriptStoreError),
    #[error("transcript scan failed to {operation} {path}")]
    ScanIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("transcript changed generation while scanning {path}")]
    ScanGenerationChanged { path: PathBuf },
    #[error(transparent)]
    Privacy(#[from] tracedecay_privacy::PrivacySanitizerError),
    #[error(transparent)]
    Domain(#[from] tracedecay_domain::DomainError),
    #[error(transparent)]
    ObservationContract(#[from] tracedecay_domain::ObservationContractError),
    #[error("{provider} record at {offset}..{end_offset} is non-durable: {reason}")]
    NonDurableRecord {
        provider: &'static str,
        offset: u64,
        end_offset: u64,
        reason: &'static str,
    },
    /// Host admission refused or failed the observation write. Unlike
    /// [`Self::NonDurableRecord`] this says nothing about the record itself,
    /// so the admission authority's own retryability verdict must survive to
    /// classification: a still-mounting write authority is a transient race,
    /// not a permanently blocked projection.
    #[error("{provider} observation admission failed: {reason}")]
    HostAdmission {
        provider: &'static str,
        reason: &'static str,
        retryable: bool,
        detail: Option<String>,
    },
    #[error("{provider} frame state is invalid")]
    InvalidFrameState { provider: &'static str },
    #[error("{provider} blocking source scan did not join successfully")]
    BlockingScanTaskFailed { provider: &'static str },
    #[error("{provider} background resource is unavailable: {resource}")]
    BackgroundResourceUnavailable {
        provider: &'static str,
        resource: &'static str,
    },
    #[error("Codex discovery frontier is invalid: {detail}")]
    InvalidCodexDiscoveryFrontier { detail: &'static str },
    #[error("{provider} transcript has no injective source identity: {path}")]
    InvalidSourceIdentity {
        provider: &'static str,
        path: PathBuf,
    },
}

impl TranscriptIngestError {
    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }

    fn scan_io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::ScanIo {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

fn log_source_skip(path: &Path, action: &'static str, error: &impl std::fmt::Display) {
    tracing::debug!(
        transcript_path = %path.display(),
        action,
        error = %error,
        "skipping transcript source input"
    );
}

/// Durable identity for one transcript cursor.
///
/// Physical paths retain their native identity. Opaque keys let providers keep
/// an injective identity when a native path cannot be represented as Unicode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCursorKey {
    durable: DurableTranscriptCursorKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DurableTranscriptCursorKey {
    Path(PathBuf),
    Opaque(String),
}

impl TranscriptCursorKey {
    pub fn for_path(path: &Path) -> Self {
        Self {
            durable: DurableTranscriptCursorKey::Path(path.to_path_buf()),
        }
    }

    pub fn opaque(key: impl Into<String>) -> Self {
        Self {
            durable: DurableTranscriptCursorKey::Opaque(key.into()),
        }
    }

    /// Exact durable text used by opaque keys or Unicode-compatible paths.
    pub fn durable_text(&self) -> String {
        match &self.durable {
            DurableTranscriptCursorKey::Path(path) => path.to_string_lossy().into_owned(),
            DurableTranscriptCursorKey::Opaque(key) => key.clone(),
        }
    }

    pub fn store_path(&self) -> PathBuf {
        match &self.durable {
            DurableTranscriptCursorKey::Path(path) => path.clone(),
            DurableTranscriptCursorKey::Opaque(key) => PathBuf::from(key),
        }
    }
}

/// One typed cursor checkpoint for a transcript source.
///
/// Keeping the durable key attached to both ends of a scan prevents consumers
/// from advancing a cursor reconstructed from an ambient or lossy path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptCursorCheckpoint {
    pub key: TranscriptCursorKey,
    pub state: StoredCursor,
}

/// A pluggable transcript provider's discovery half.
///
/// Implementors locate their transcript files for a project and name the
/// durable cursor key of each; each host's observation admission reads and
/// admits the files.
///
/// `Send + Sync` is required so boxed sources can be driven from detached
/// background tasks (e.g. the serve-side startup sweep).
pub trait TranscriptSource: Send + Sync {
    /// Stable provider id stored on every session/message row (e.g. `"claude"`).
    fn provider(&self) -> &'static str;

    /// Candidate transcript files to consider for `project_root`. May scan
    /// per-project and/or OS-specific global directories. Non-existent paths
    /// are tolerated by admission.
    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf>;

    /// Bounded discovery used by multi-source ingest admission.
    ///
    /// Default applies [`TranscriptDiscoveryBounds`] after `transcript_paths`.
    /// Providers that enumerate via [`collect_files_with_ext_bounded`] should
    /// override so limits are enforced before materialization.
    fn discover_transcript_paths(
        &self,
        project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        bound_path_list(self.transcript_paths(project_root), bounds)
    }

    /// Discover one deterministic page beginning at `start_offset`.
    ///
    /// The omitted count covers paths before and after the returned page. It
    /// lets the scheduler report backpressure without retaining the whole
    /// source corpus. Providers with a streaming enumerator may override this
    /// method to apply the offset before materializing paths.
    fn discover_transcript_paths_page(
        &self,
        project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
        start_offset: usize,
    ) -> (FileDiscoveryReport, usize) {
        let mut paths = self.transcript_paths(project_root);
        paths.sort();
        paths.dedup();
        let total_paths = paths.len();
        let report = bound_path_list(paths.into_iter().skip(start_offset), bounds);
        let omitted_paths = total_paths.saturating_sub(report.paths.len());
        (report, omitted_paths)
    }
}

/// Runs one synchronous transcript discovery/parse section on Tokio's
/// blocking pool so historical ingest cannot pin a request-runtime worker.
///
/// [`TranscriptSource`] is deliberately synchronous: adapters walk host
/// directories and read transcript files with plain `std` IO. Ingest is an
/// async pipeline whose CPU/IO slices sit between store awaits. Those
/// slices must own their inputs so the worker can return from `poll` at
/// the `.await` point. `block_in_place` only hands the run queue away; the
/// original worker thread still executes the section, so daemon requests
/// share cores with ingest and a narrow pool loses its parked-worker
/// headroom.
#[tracing::instrument(
    name = "sessions.blocking_transcript_section",
    level = "trace",
    skip_all
)]
pub(crate) async fn run_blocking_transcript_section<T, F>(work: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| match error.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(error) => {
                panic!("blocking transcript section worker stopped: {error}")
            }
        })
}

/// Spawns a task on `handle` and waits for it from a blocking section.
///
/// On a one-worker multi-thread runtime this only succeeds when the caller
/// is inside [`run_blocking_transcript_section`]: `spawn_blocking` frees
/// the worker so the spawned task can run. An inline filesystem/JSONL
/// section deadlocks until the receive timeout fails.
#[cfg(test)]
pub(crate) fn require_blocking_section_releases_worker(handle: tokio::runtime::Handle) {
    let (sender, receiver) = std::sync::mpsc::channel();
    handle.spawn(async move {
        let _ = sender.send(());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("host transcript filesystem work must release the only Tokio worker");
}

mod discovery;
mod jsonl;

pub use discovery::{FileDiscoveryLimit, FileDiscoveryReport, TranscriptDiscoveryBounds};
pub use discovery::{
    bound_path_list, collect_files_with_ext_bounded, os_str_byte_len, path_byte_len,
};

pub use crate::runtime::jsonl_io::{JsonlChangeKind, JsonlIoAccounting};
#[cfg(test)]
use jsonl::stream_new_jsonl_raw_strict;
#[cfg(test)]
pub(in crate::runtime) use jsonl::{HoldUnchangedGenerationCache, spin_until_jsonl_change_settled};
pub(in crate::runtime) use jsonl::{
    JsonlFileChangeToken, JsonlNativeFileIdentity, ResumeDigest, jsonl_change_token_settled,
    jsonl_file_change_token_under, jsonl_native_file_identity, jsonl_prefix_digest,
};
pub use jsonl::{
    JsonlFrameDeferral, JsonlPrefixCheckpoint, JsonlPrefixRecovery, JsonlResumeState,
    MAX_JSONL_RECORD_BYTES, RawJsonlFrame, RawJsonlFrameReader, RawJsonlRecord,
    RawJsonlSkippedRange, RawJsonlSkippedReason, STRICT_JSONL_BATCH_BYTES,
};
pub(in crate::runtime) use jsonl::{
    MAX_JSONL_FRAMES_PER_BATCH, try_stream_new_jsonl_raw_strict_with_resume_and_frame_limit,
};
#[cfg(test)]
pub use jsonl::{try_stream_new_jsonl_raw_strict, try_stream_new_jsonl_raw_strict_with_resume};

/// Reads one complete snapshot document, or `None` (logged) when it cannot be
/// opened or read or exceeds `max_bytes`.
pub fn read_snapshot_file_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            log_source_skip(path, "open transcript file", &error);
            return None;
        }
    };
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    match read_bounded_to_string(&mut file, max_bytes) {
        Ok(WireReadOutcome::Ready(contents)) => Some(contents),
        Ok(WireReadOutcome::Oversized) => None,
        Err(error) => {
            log_source_skip(path, "read transcript file", &error);
            None
        }
    }
}

/// Recursively collect files with the given extension under `dir`, bounded by
/// `max_depth` and [`TranscriptDiscoveryBounds::default_walk`] (file count,
/// path bytes, metadata charge, cumulative discovery bytes). Directory
/// symlinks are not followed. Returns an empty vec when `dir` is missing or
/// unreadable. Used by global-store adapters (Claude, Codex) whose transcripts
/// live in nested date/slug directories.
#[cfg(test)]
pub fn collect_files_with_ext(dir: &Path, ext: &str, max_depth: u8) -> Vec<PathBuf> {
    collect_files_with_ext_bounded(
        dir,
        ext,
        max_depth,
        TranscriptDiscoveryBounds::default_walk(),
    )
    .paths
}

/// File modification time in epoch seconds, or 0 when unavailable.
fn file_mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

const JSONL_HEAD_FINGERPRINT_BYTES: usize = 1024;

fn should_resume_jsonl(prev: StoredCursor, file_size: u64, mtime: u64, file_id: u64) -> bool {
    if prev.position == 0 || file_size < prev.position {
        return false;
    }
    if prev.file_id != 0 && file_id != 0 {
        // A stored replacement marker still names this file: it is the
        // generation minted when the file was last rewritten, so resuming past
        // it keeps one namespace across every batch of that generation.
        return prev.file_id == file_id
            || jsonl::is_replacement_jsonl_generation(file_id, prev.file_id);
    }
    mtime >= prev.mtime
}

fn stable_jsonl_file_id(
    file: &mut std::fs::File,
    meta: &std::fs::Metadata,
) -> std::io::Result<(u64, u64)> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-file-id-v1");
    #[cfg(unix)]
    {
        hasher.update(meta.dev().to_le_bytes());
        hasher.update(meta.ino().to_le_bytes());
    }
    #[cfg(windows)]
    {
        // Match `jsonl_native_file_identity`: a native-handle miss is typed,
        // never a fabricated 0/0 identity that would collide across files.
        let information = tracedecay_private_fs::windows_file::information(file)?;
        hasher.update(information.volume_serial_number.to_le_bytes());
        hasher.update(information.file_index.to_le_bytes());
        hasher.update(meta.creation_time().to_le_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Creation time is stable across appends and changes when a transcript
        // is replaced on platforms without a native file-id implementation.
        if let Ok(created) = meta.created() {
            if let Ok(created) = created.duration_since(std::time::UNIX_EPOCH) {
                hasher.update(created.as_nanos().to_le_bytes());
            }
        }
    }
    let (head, identity_window_bytes) = jsonl_head_fingerprint(file)?;
    hasher.update(head.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok((u64::from_be_bytes(bytes), identity_window_bytes))
}

pub(super) fn jsonl_file_identity(path: &Path) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    Ok(stable_jsonl_file_id(&mut file, &metadata)?.0)
}

fn jsonl_head_fingerprint(file: &mut std::fs::File) -> std::io::Result<(u64, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::new(file);
    let mut buf = Vec::new();
    // Hash only the first logical line prefix so append-only writes keep a
    // stable identity even for initially tiny files.
    let _ = reader
        .by_ref()
        .take(JSONL_HEAD_FINGERPRINT_BYTES as u64)
        .read_until(b'\n', &mut buf)?;
    let window = u64::try_from(buf.len()).unwrap_or(u64::MAX);
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay-jsonl-head-v1");
    hasher.update(&buf);
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Ok((u64::from_be_bytes(bytes), window))
}

/// Stable 64-bit content hash prefix suitable for the existing integer
/// `parse_offsets.byte_offset` column.
pub fn content_hash64(contents: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(contents.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    // The hash rides in `StoredCursor::position`, whose persisted column is a
    // typed non-negative i64 (strict encode and decode). Masking to 63 bits
    // keeps every hash inside that domain; the entropy loss is immaterial for
    // change detection.
    u64::from_be_bytes(bytes) & (u64::MAX >> 1)
}

#[cfg(test)]
mod tests;
