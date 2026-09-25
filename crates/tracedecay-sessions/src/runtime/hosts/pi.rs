//! Pi coding-agent transcript source.
//!
//! Pi writes one JSONL file per session at
//! `<agent dir>/sessions/--<encoded cwd>--/<timestamp>_<session id>.jsonl`.
//! The agent directory is `~/.pi/agent` unless `PI_CODING_AGENT_DIR`
//! relocates it. Each file opens with a `session` header naming the session id
//! and working directory; that header, checked against the file name, is the
//! identity and scope authority for every entry after it.

use std::collections::BinaryHeap;
use std::ffi::OsStr;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::Instant;

use tracedecay_capture::pi::{self as pi_capture, PiSessionHeader};
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProviderId,
    RetentionClass, SessionId,
};
use tracedecay_privacy::{
    ObservationRecordParseErrorV1, parse_normalized_observation_record_v1,
    protect_sensitive_structural_id,
};
use tracedecay_runtime_core::logging::StateChangeLogGate;
use tracedecay_store::{ParseOffset, observation::ObservationCoverageReason};

use crate::admission::{HostAdmission, HostDiscoveryQueueEntry};
use crate::observation::ObservationCancellation;
use crate::runtime::host_scan::{HOST_SCAN_WINDOW, HostScanBudget};
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionProgress, JsonlObservationAdmissionRequest,
    admit_jsonl_observations,
};
use crate::runtime::shared::TranscriptScopeMatcher;
use crate::runtime::snapshot_observation::MAX_SNAPSHOT_METADATA_BYTES;
use crate::runtime::source::{
    FileDiscoveryLimit, FileDiscoveryReport, HostProviderCoverage, TranscriptDiscoveryBounds,
    TranscriptIngestError, TranscriptIngestResult, bound_path_list, canonical_framed_sha256,
    jsonl_file_identity, persist_host_provider_coverage, run_blocking_transcript_section,
};

/// Environment override Pi reads for its agent directory.
pub const PI_AGENT_DIR_ENV: &str = "PI_CODING_AGENT_DIR";
/// Home-relative agent directory Pi loads by default.
pub const PI_AGENT_RELATIVE: &str = ".pi/agent";

const PROVIDER: &str = "pi";
const MAX_SESSION_FILES: usize = 512;
const MAX_DISCOVERY_CANDIDATES: usize = 4_096;
const MAX_DISCOVERY_FAILURE_EVIDENCE: usize = 16;
/// A header is one short JSON object; a longer first line is not a header.
const MAX_HEADER_BYTES: u64 = 64 * 1024;
const MAX_DISCOVERY_INPUT_BYTES: u64 =
    MAX_SNAPSHOT_METADATA_BYTES + ((MAX_DISCOVERY_CANDIDATES as u64 + 1) * 4 * 1024);
const MAX_DISCOVERY_UNITS: usize = MAX_DISCOVERY_CANDIDATES * 2;
const PI_DISCOVERY_FRONTIER_KEY: &str = "host-frontier://pi/discovery/v1";
const PI_QUEUE_FRONTIER_KEY: &str = "host-frontier://pi/queue/v1";
const PI_FRONTIER_VERSION: u64 = 1;

/// Discovery failures already reported, keyed by the failing file's digest, so
/// a malformed file re-scanned every pass logs once per state change.
static PI_DISCOVERY_FAILURE_GATE: StateChangeLogGate<
    String,
    (PiDiscoveryFailureKind, io::ErrorKind),
> = StateChangeLogGate::new();

/// The agent directory Pi loads for `home`. `PI_CODING_AGENT_DIR` names only
/// the running process user's directory, so it answers only for that home,
/// and only an absolute value relocates it: a sandbox or tempdir home never
/// resolves outside itself, and a relative value cannot point at whatever the
/// working directory happens to be.
pub fn pi_agent_dir(home: &Path) -> PathBuf {
    let ambient = is_process_home(home)
        .then(|| std::env::var_os(PI_AGENT_DIR_ENV))
        .flatten();
    pi_agent_dir_for(home, ambient.as_deref())
}

fn pi_agent_dir_for(home: &Path, ambient: Option<&OsStr>) -> PathBuf {
    ambient
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(PI_AGENT_RELATIVE))
}

fn is_process_home(home: &Path) -> bool {
    let Some(own) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return false;
    };
    own == home
        || matches!(
            (std::fs::canonicalize(&own), std::fs::canonicalize(home)),
            (Ok(own), Ok(home)) if own == home
        )
}

/// Pi's per-cwd session directory name: the leading separator dropped and
/// every `/`, `\`, and `:` replaced by `-`, wrapped in `--`.
fn session_dir_name(cwd: &Path) -> String {
    let text = cwd.to_string_lossy();
    let trimmed = text
        .strip_prefix('/')
        .or_else(|| text.strip_prefix('\\'))
        .unwrap_or(&text);
    format!("--{}--", trimmed.replace(['/', '\\', ':'], "-"))
}

#[derive(Clone)]
pub struct PiSource {
    agent_dir: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PiCaptureOutcome {
    pub bytes_consumed: u64,
    pub deferred: bool,
    pub discovery_failures: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PiDiscoveryFailureKind {
    DirectoryUnavailable,
    DirectoryEntryUnavailable,
    EntryTypeUnavailable,
    SessionHeaderUnavailable,
    InvalidSessionHeader,
}

struct PiDiscoveryFailure {
    kind: PiDiscoveryFailureKind,
    source_digest: String,
    error_kind: io::ErrorKind,
}

struct PiDiscoveryReport {
    files: FileDiscoveryReport,
    failures: Vec<PiDiscoveryFailure>,
    failure_count: u64,
    /// A partial sweep must not advance the durable discovery frontier.
    scan_complete: bool,
    reached_end: bool,
}

impl PiDiscoveryReport {
    fn record_failure(
        &mut self,
        kind: PiDiscoveryFailureKind,
        path: &Path,
        error: &io::Error,
        budget: &mut HostScanBudget,
    ) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.scan_complete = false;
        budget.mark_unavailable();
        if self.failures.len() < MAX_DISCOVERY_FAILURE_EVIDENCE {
            self.failures.push(PiDiscoveryFailure {
                kind,
                source_digest: source_digest(path),
                error_kind: error.kind(),
            });
        }
    }
}

enum HeaderRead {
    Header(PiSessionHeader),
    /// The first line is not finished yet: Pi is mid-write or the file is
    /// empty. Not malformed, just not ready.
    Incomplete,
}

impl PiSource {
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_agent_dir(&pi_agent_dir(&home)))
    }

    pub fn with_agent_dir(agent_dir: &Path) -> Self {
        Self {
            agent_dir: agent_dir.to_path_buf(),
            user_registered_roots: None,
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }

    fn sessions_root(&self) -> PathBuf {
        self.agent_dir.join("sessions")
    }

    fn matcher(&self, project_root: &Path) -> TranscriptScopeMatcher {
        TranscriptScopeMatcher::for_scope(project_root, self.user_registered_roots.as_deref())
    }

    /// `None` when Pi has not created its sessions root; an error when the
    /// root exists but is not a real directory.
    fn existing_sessions_root(&self) -> TranscriptIngestResult<Option<PathBuf>> {
        let root = self.sessions_root();
        match std::fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                Ok(Some(root))
            }
            Ok(_) => Err(TranscriptIngestError::ScanIo {
                operation: "stat Pi sessions root",
                path: root,
                source: io::Error::other("Pi sessions root must be a real directory, not a link"),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(TranscriptIngestError::ScanIo {
                operation: "stat Pi sessions root",
                path: root,
                source,
            }),
        }
    }

    fn discover(
        &self,
        project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
        frontier_path: Option<PathBuf>,
        mut budget: HostScanBudget,
    ) -> TranscriptIngestResult<(PiDiscoveryReport, HostScanBudget)> {
        hotpath::measure_block!("sessions.hosts.pi.discover", {
            let mut discovery = PiDiscoveryReport {
                files: bound_path_list(Vec::new(), bounds),
                failures: Vec::new(),
                failure_count: 0,
                scan_complete: true,
                reached_end: true,
            };
            let Some(sessions_root) = self.existing_sessions_root()? else {
                return Ok((discovery, budget));
            };
            let matcher = self.matcher(project_root);
            let limit = bounds.max_files.min(MAX_DISCOVERY_CANDIDATES);
            let mut paths = BinaryHeap::with_capacity(limit);
            let mut has_more = false;
            let cwd_dirs = read_entries(
                &sessions_root,
                EntryKind::Directory,
                &mut discovery,
                &mut budget,
            )?;
            'cwd_dirs: for cwd_dir in cwd_dirs {
                for candidate in read_entries(
                    &cwd_dir,
                    EntryKind::SessionFile,
                    &mut discovery,
                    &mut budget,
                )? {
                    if !budget.checkpoint() {
                        discovery.scan_complete = false;
                        discovery.reached_end = false;
                        break 'cwd_dirs;
                    }
                    if frontier_path
                        .as_ref()
                        .is_some_and(|frontier| candidate <= *frontier)
                    {
                        continue;
                    }
                    let header = match read_session_header(&candidate) {
                        Ok(HeaderRead::Header(header)) => header,
                        Ok(HeaderRead::Incomplete) => {
                            discovery.scan_complete = false;
                            continue;
                        }
                        Err((kind, error)) => {
                            discovery.record_failure(kind, &candidate, &error, &mut budget);
                            continue;
                        }
                    };
                    if !budget.try_charge_input(header_charge(&header)) {
                        discovery.scan_complete = false;
                        discovery.reached_end = false;
                        break 'cwd_dirs;
                    }
                    if !matcher.accepts(Some(Path::new(&header.cwd))) {
                        continue;
                    }
                    if paths.len() < limit {
                        paths.push(candidate);
                    } else {
                        has_more = true;
                        if paths.peek().is_some_and(|largest| candidate < *largest) {
                            let _ = paths.pop();
                            paths.push(candidate);
                        }
                    }
                }
            }
            discovery.files = bound_path_list(
                paths.into_sorted_vec(),
                TranscriptDiscoveryBounds {
                    max_files: limit,
                    ..bounds
                },
            );
            if has_more {
                discovery.files.truncated = Some(FileDiscoveryLimit::FileCount);
            }
            if discovery.files.is_truncated() {
                discovery.reached_end = false;
            }
            Ok((discovery, budget))
        })
    }

    /// The session files for one Pi session, located through Pi's own
    /// per-cwd directory rather than a sweep of every session.
    fn session_files(&self, cwd: &Path, session_id: &str) -> TranscriptIngestResult<Vec<PathBuf>> {
        let Some(sessions_root) = self.existing_sessions_root()? else {
            return Ok(Vec::new());
        };
        let dir = sessions_root.join(session_dir_name(cwd));
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(TranscriptIngestError::ScanIo {
                    operation: "read Pi session directory",
                    path: dir,
                    source,
                });
            }
        };
        let mut files = Vec::new();
        for entry in entries.take(MAX_DISCOVERY_CANDIDATES) {
            let entry = entry.map_err(|source| TranscriptIngestError::ScanIo {
                operation: "read Pi session directory entry",
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if is_session_file_name(&path)
                && entry
                    .file_type()
                    .is_ok_and(|kind| kind.is_file() && !kind.is_symlink())
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .and_then(pi_capture::session_id_from_file_name)
                    .as_deref()
                    == Some(session_id)
            {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }
}

#[derive(Clone, Copy)]
enum EntryKind {
    Directory,
    SessionFile,
}

fn is_session_file_name(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str) == Some("jsonl")
}

/// Real directories or real `.jsonl` files directly under `parent`, in path
/// order. Links are never followed out of the Pi agent directory.
fn read_entries(
    parent: &Path,
    kind: EntryKind,
    discovery: &mut PiDiscoveryReport,
    budget: &mut HostScanBudget,
) -> TranscriptIngestResult<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            discovery.record_failure(
                PiDiscoveryFailureKind::DirectoryUnavailable,
                parent,
                &error,
                budget,
            );
            return Ok(Vec::new());
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                discovery.record_failure(
                    PiDiscoveryFailureKind::DirectoryEntryUnavailable,
                    parent,
                    &error,
                    budget,
                );
                continue;
            }
        };
        if !budget.try_charge_unit() {
            discovery.scan_complete = false;
            discovery.reached_end = false;
            break;
        }
        let path = entry.path();
        if !charge_discovered_path(budget, &path)? {
            discovery.scan_complete = false;
            discovery.reached_end = false;
            break;
        }
        match (kind, entry.file_type()) {
            (EntryKind::Directory, Ok(file_type))
                if file_type.is_dir() && !file_type.is_symlink() =>
            {
                paths.push(path);
            }
            (EntryKind::SessionFile, Ok(file_type))
                if file_type.is_file()
                    && !file_type.is_symlink()
                    && is_session_file_name(&path) =>
            {
                paths.push(path);
            }
            (_, Ok(_)) => {}
            (_, Err(error)) => discovery.record_failure(
                PiDiscoveryFailureKind::EntryTypeUnavailable,
                &path,
                &error,
                budget,
            ),
        }
    }
    paths.sort();
    Ok(paths)
}

fn charge_discovered_path(
    budget: &mut HostScanBudget,
    path: &Path,
) -> TranscriptIngestResult<bool> {
    let bytes = u64::try_from(path.as_os_str().as_encoded_bytes().len())
        .map_err(|_| invalid_frame())?
        .max(1);
    Ok(budget.try_charge_input(bytes))
}

fn header_charge(header: &PiSessionHeader) -> u64 {
    u64::try_from(header.session_id.len() + header.cwd.len()).unwrap_or(u64::MAX)
}

/// Read and validate the header line of one session file: a `session`
/// object whose id is the one the file name carries.
fn read_session_header(path: &Path) -> Result<HeaderRead, (PiDiscoveryFailureKind, io::Error)> {
    let unavailable = |error| (PiDiscoveryFailureKind::SessionHeaderUnavailable, error);
    let invalid = |reason: &'static str| {
        (
            PiDiscoveryFailureKind::InvalidSessionHeader,
            io::Error::new(io::ErrorKind::InvalidData, reason),
        )
    };
    let file = std::fs::File::open(path).map_err(unavailable)?;
    let mut line = Vec::new();
    io::BufReader::new(file)
        .take(MAX_HEADER_BYTES + 1)
        .read_until(b'\n', &mut line)
        .map_err(unavailable)?;
    if line.last() != Some(&b'\n') {
        return if u64::try_from(line.len()).unwrap_or(u64::MAX) > MAX_HEADER_BYTES {
            Err(invalid("Pi session header exceeds its bound"))
        } else {
            Ok(HeaderRead::Incomplete)
        };
    }
    let header = pi_capture::parse_session_header(line.trim_ascii())
        .ok_or_else(|| invalid("first line is not a Pi session header"))?;
    let named = path
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(pi_capture::session_id_from_file_name);
    if named.as_deref() != Some(header.session_id.as_str()) {
        return Err(invalid("Pi session header id does not match its file name"));
    }
    Ok(HeaderRead::Header(header))
}

/// Sweep every in-scope Pi session through the durable discovery queue.
pub async fn capture_pi_observations(
    facade: &dyn HostAdmission,
    source: &PiSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<PiCaptureOutcome> {
    hotpath::future!(
        async {
            let discovery_frontier = facade
                .get_parse_offset(&scope, PI_DISCOVERY_FRONTIER_KEY)
                .await
                .map_err(admission_error)?
                .unwrap_or_default();
            let frontier_path = if discovery_frontier.file_id == 0 {
                None
            } else {
                Some(
                    facade
                        .discovery_path(&scope, PROVIDER, discovery_frontier.file_id)
                        .await
                        .map_err(admission_error)?
                        .map(|entry| entry.path)
                        .ok_or_else(invalid_frame)?,
                )
            };
            let scan_budget = HostScanBudget::new(
                MAX_DISCOVERY_INPUT_BYTES,
                MAX_DISCOVERY_UNITS,
                Instant::now() + HOST_SCAN_WINDOW,
                cancellation.clone(),
            );
            let owned_source = source.clone();
            let owned_project_root = project_root.to_path_buf();
            let (discovery, scan_budget) = hotpath::future!(
                tokio::task::spawn_blocking(move || {
                    owned_source.discover(
                        &owned_project_root,
                        TranscriptDiscoveryBounds::from_discovered_units(MAX_DISCOVERY_CANDIDATES),
                        frontier_path,
                        scan_budget,
                    )
                }),
                label = "sessions.hosts.pi.discover_task"
            )
            .await
            .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
            warn_discovery_failures(&discovery);
            let discovery_truncated = discovery.files.is_truncated();
            let discovery_skipped = discovery.files.skipped_oversized_entries;
            let last_discovered_entry = if cancellation.is_cancelled() {
                None
            } else {
                facade
                    .enqueue_discovery_paths(&scope, PROVIDER, discovery.files.paths)
                    .await
                    .map_err(admission_error)?
            };
            let queue_frontier = facade
                .get_parse_offset(&scope, PI_QUEUE_FRONTIER_KEY)
                .await
                .map_err(admission_error)?
                .unwrap_or_default();
            let mut scheduled = if cancellation.is_cancelled() {
                Vec::new()
            } else {
                facade
                    .discovery_paths_after(
                        &scope,
                        PROVIDER,
                        queue_frontier.byte_offset,
                        MAX_SESSION_FILES.saturating_add(1),
                    )
                    .await
                    .map_err(admission_error)?
            };
            if scheduled.is_empty()
                && queue_frontier.byte_offset > 0
                && !cancellation.is_cancelled()
            {
                scheduled = facade
                    .discovery_paths_after(&scope, PROVIDER, 0, MAX_SESSION_FILES.saturating_add(1))
                    .await
                    .map_err(admission_error)?;
            }
            let queue_has_more = scheduled.len() > MAX_SESSION_FILES;
            scheduled.truncate(MAX_SESSION_FILES);
            let mut outcome = PiCaptureOutcome {
                deferred: discovery_truncated
                    || queue_has_more
                    || discovery_skipped > 0
                    || discovery.failure_count > 0
                    || !discovery.scan_complete
                    || scan_budget.evidence().is_deferred()
                    || cancellation.is_cancelled(),
                discovery_failures: discovery.failure_count,
                ..PiCaptureOutcome::default()
            };
            let mut remaining = max_new_bytes.unwrap_or(u64::MAX);
            let mut processed_sequence = None;
            for HostDiscoveryQueueEntry { sequence, path } in scheduled {
                if cancellation.is_cancelled() || remaining == 0 {
                    outcome.deferred = true;
                    break;
                }
                processed_sequence = Some(sequence);
                admit_scheduled_file(
                    facade,
                    &path,
                    None,
                    &scope,
                    max_new_bytes.map(|_| remaining),
                    cancellation,
                    &mut outcome,
                )
                .await?;
                remaining = max_new_bytes.map_or(u64::MAX, |budget| {
                    budget.saturating_sub(outcome.bytes_consumed)
                });
            }
            if let Some(sequence) = processed_sequence
                && !cancellation.is_cancelled()
            {
                facade
                    .advance_parse_offset(
                        &scope,
                        PI_QUEUE_FRONTIER_KEY,
                        ParseOffset {
                            byte_offset: sequence,
                            mtime: queue_frontier.mtime.saturating_add(1),
                            file_id: PI_FRONTIER_VERSION,
                        },
                    )
                    .await
                    .map_err(admission_error)?;
            }
            if discovery.scan_complete
                && !scan_budget.evidence().cancelled
                && !cancellation.is_cancelled()
            {
                let next_frontier = if discovery.reached_end {
                    Some(ParseOffset {
                        byte_offset: 0,
                        mtime: discovery_frontier.mtime.saturating_add(1),
                        file_id: 0,
                    })
                } else {
                    last_discovered_entry.map(|entry| ParseOffset {
                        byte_offset: entry.sequence,
                        mtime: discovery_frontier.mtime.saturating_add(1),
                        file_id: entry.sequence,
                    })
                };
                if let Some(next_frontier) = next_frontier {
                    facade
                        .advance_parse_offset(&scope, PI_DISCOVERY_FRONTIER_KEY, next_frontier)
                        .await
                        .map_err(admission_error)?;
                }
            }
            persist_coverage(facade, &scope, &outcome).await?;
            Ok(outcome)
        },
        label = "sessions.hosts.pi.capture"
    )
    .await
}

/// Land one named session, the one a Pi lifecycle event reports, without
/// sweeping the others. A session Pi has not written to disk yet is an empty
/// pass, not a failure.
#[expect(
    clippy::too_many_arguments,
    reason = "One bounded admission pass names its source, scope, session, and budget."
)]
pub async fn capture_pi_session(
    facade: &dyn HostAdmission,
    source: &PiSource,
    project_root: &Path,
    cwd: &Path,
    session_id: &str,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<PiCaptureOutcome> {
    let files = run_blocking_transcript_section(|| source.session_files(cwd, session_id))?;
    let matcher = source.matcher(project_root);
    let mut outcome = PiCaptureOutcome::default();
    for path in files {
        if cancellation.is_cancelled() {
            outcome.deferred = true;
            break;
        }
        let remaining = max_new_bytes.map(|budget| budget.saturating_sub(outcome.bytes_consumed));
        if remaining == Some(0) {
            outcome.deferred = true;
            break;
        }
        admit_scheduled_file(
            facade,
            &path,
            Some(&matcher),
            &scope,
            remaining,
            cancellation,
            &mut outcome,
        )
        .await?;
    }
    Ok(outcome)
}

/// Admit one session file, isolating a file whose identity or bytes cannot be
/// read so the rest of the pass still lands.
async fn admit_scheduled_file(
    facade: &dyn HostAdmission,
    path: &Path,
    matcher: Option<&TranscriptScopeMatcher>,
    scope: &ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
    outcome: &mut PiCaptureOutcome,
) -> TranscriptIngestResult<()> {
    let header = match run_blocking_transcript_section(|| read_session_header(path)) {
        Ok(HeaderRead::Header(header)) => header,
        Ok(HeaderRead::Incomplete) => {
            outcome.deferred = true;
            return Ok(());
        }
        Err((kind, error)) => {
            warn_isolated_source(path, kind, error.kind());
            outcome.discovery_failures = outcome.discovery_failures.saturating_add(1);
            outcome.deferred = true;
            return Ok(());
        }
    };
    if matcher.is_some_and(|matcher| !matcher.accepts(Some(Path::new(&header.cwd)))) {
        return Ok(());
    }
    match admit_session_file(facade, path, &header, scope, max_new_bytes, cancellation).await {
        Ok(progress) => {
            outcome.bytes_consumed = outcome
                .bytes_consumed
                .saturating_add(progress.bytes_consumed);
            outcome.deferred |= progress.source_deferred;
            Ok(())
        }
        Err(error) if isolatable_source_error(&error) => {
            warn_isolated_source(
                path,
                PiDiscoveryFailureKind::SessionHeaderUnavailable,
                io::ErrorKind::Other,
            );
            outcome.discovery_failures = outcome.discovery_failures.saturating_add(1);
            outcome.deferred = true;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn admit_session_file(
    facade: &dyn HostAdmission,
    path: &Path,
    header: &PiSessionHeader,
    scope: &ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<JsonlObservationAdmissionProgress> {
    let provider = ProviderId::new(PROVIDER).map_err(|_| invalid_frame())?;
    let canonical_session_id =
        protect_sensitive_structural_id(&header.session_id).map_err(|_| invalid_frame())?;
    let session = SessionId::new(&canonical_session_id).map_err(|_| invalid_frame())?;
    let file_identity =
        run_blocking_transcript_section(|| jsonl_file_identity(path)).map_err(|source| {
            TranscriptIngestError::ScanIo {
                operation: "read Pi session identity",
                path: path.to_path_buf(),
                source,
            }
        })?;
    let source_key = protect_sensitive_structural_id(&format!("pi-file-{file_identity:016x}"))
        .map_err(|_| invalid_frame())?;
    let source_identity = ObservationSourceIdentityV1::for_provider_source(
        provider,
        session,
        SessionId::new(source_key).map_err(|_| invalid_frame())?,
    )
    .map_err(|_| invalid_frame())?;
    let retention = RetentionClass::new("transcript.pi.v1").map_err(|_| invalid_frame())?;
    let request = JsonlObservationAdmissionRequest::new(
        PROVIDER,
        path,
        facade,
        source_identity,
        scope.clone(),
        retention,
    )
    .with_max_new_bytes(max_new_bytes)
    .with_cancellation(cancellation.clone());
    admit_jsonl_observations(
        request,
        |_| (),
        move |(), bytes, range, _, _prepared, _hints| {
            let mut native_record_id = None;
            let parsed = parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    let envelope =
                        pi_capture::normalize_observation(&native, &canonical_session_id, range)?;
                    native_record_id = Some(envelope.stable_record_id().clone());
                    Ok(envelope)
                },
            );
            match parsed {
                Ok(parsed) => Ok(JsonlFrameAdmission::durable(
                    parsed,
                    native_record_id.ok_or_else(invalid_frame)?,
                )),
                Err(ObservationRecordParseErrorV1::Empty) => Ok(JsonlFrameAdmission::non_durable(
                    ObservationCoverageReason::BlankFrame,
                )),
                Err(
                    ObservationRecordParseErrorV1::TooLarge
                    | ObservationRecordParseErrorV1::CanonicalEnvelopeTooLarge,
                ) => Ok(JsonlFrameAdmission::non_durable(
                    ObservationCoverageReason::OversizedFrame,
                )),
                Err(_) => Ok(JsonlFrameAdmission::non_durable(
                    ObservationCoverageReason::MalformedFrame,
                )),
            }
        },
    )
    .await
}

async fn persist_coverage(
    facade: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    outcome: &PiCaptureOutcome,
) -> TranscriptIngestResult<()> {
    persist_host_provider_coverage(
        facade,
        scope,
        PROVIDER,
        if outcome.deferred {
            HostProviderCoverage::Partial
        } else {
            HostProviderCoverage::Complete
        },
        outcome
            .discovery_failures
            .saturating_add(u64::from(outcome.deferred)),
    )
    .await
}

fn warn_discovery_failures(discovery: &PiDiscoveryReport) {
    for failure in &discovery.failures {
        if !PI_DISCOVERY_FAILURE_GATE.admit(
            failure.source_digest.clone(),
            (failure.kind, failure.error_kind),
        ) {
            continue;
        }
        tracing::warn!(
            provider = PROVIDER,
            failure_kind = ?failure.kind,
            error_kind = ?failure.error_kind,
            source_digest = failure.source_digest,
            "Pi session discovery is incomplete"
        );
    }
    if discovery.failure_count > discovery.failures.len() as u64 {
        tracing::warn!(
            provider = PROVIDER,
            failure_count = discovery.failure_count,
            reported_failures = discovery.failures.len(),
            "additional Pi discovery failures were bounded"
        );
    }
}

fn isolatable_source_error(error: &TranscriptIngestError) -> bool {
    matches!(
        error,
        TranscriptIngestError::ScanIo { .. }
            | TranscriptIngestError::ScanGenerationChanged { .. }
            | TranscriptIngestError::NonDurableRecord { .. }
            | TranscriptIngestError::InvalidSourceIdentity { .. }
    )
}

fn warn_isolated_source(path: &Path, kind: PiDiscoveryFailureKind, error_kind: io::ErrorKind) {
    tracing::warn!(
        provider = PROVIDER,
        failure_kind = ?kind,
        error_kind = ?error_kind,
        source_digest = source_digest(path),
        "Pi session source was isolated"
    );
}

fn source_digest(path: &Path) -> String {
    canonical_framed_sha256(
        b"tracedecay.pi.session-source.v1",
        &[path.as_os_str().as_encoded_bytes()],
    )
}

fn admission_error(outcome: crate::admission::HostAdmissionOutcome) -> TranscriptIngestError {
    crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
}

const fn invalid_frame() -> TranscriptIngestError {
    TranscriptIngestError::InvalidFrameState { provider: PROVIDER }
}

#[cfg(test)]
#[path = "pi_tests.rs"]
mod tests;
