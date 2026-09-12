use std::collections::BinaryHeap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use tracedecay_capture::kimi as kimi_capture;
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProviderId,
    RetentionClass, SessionId,
};
use tracedecay_privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay_store::{ParseOffset, observation::ObservationCoverageReason};

use crate::admission::{HostAdmission, HostDiscoveryQueueEntry};
use crate::observation::ObservationCancellation;
use crate::runtime::host_scan::{HOST_SCAN_WINDOW, HostScanBudget};
use crate::runtime::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionRequest, admit_jsonl_observations,
};
use crate::runtime::shared::TranscriptScopeMatcher;
use crate::runtime::snapshot_observation::{
    MAX_SNAPSHOT_METADATA_BYTES, read_snapshot_text_bounded,
};
use crate::runtime::source::{
    FileDiscoveryLimit, HostProviderCoverage, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, bound_path_list, canonical_framed_sha256, jsonl_file_identity,
    persist_host_provider_coverage, run_blocking_transcript_section,
};

mod discovery;
use discovery::{
    KimiDiscoveryFailureKind, KimiDiscoveryReport, KimiSessionState, charge_discovered_path,
};

const PROVIDER: &str = "kimi";
const MAX_SESSION_FILES: usize = 512;
const MAX_DISCOVERY_CANDIDATES: usize = 4_096;
const MAX_DISCOVERY_FAILURE_EVIDENCE: usize = 16;
const MAX_DISCOVERY_INPUT_BYTES: u64 =
    MAX_SNAPSHOT_METADATA_BYTES + ((MAX_DISCOVERY_CANDIDATES as u64 + 1) * 4 * 1024);
const MAX_DISCOVERY_UNITS: usize = MAX_DISCOVERY_CANDIDATES * 2;
const KIMI_DISCOVERY_FRONTIER_KEY: &str = "host-frontier://kimi/discovery/v1";
const KIMI_QUEUE_FRONTIER_KEY: &str = "host-frontier://kimi/queue/v1";
const KIMI_FRONTIER_VERSION: u64 = 1;
const KIMI_CODE_HOME_ENV: &str = "KIMI_CODE_HOME";

#[derive(Clone)]
pub struct KimiSource {
    share_dir: PathBuf,
    user_registered_roots: Option<Vec<PathBuf>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KimiCaptureOutcome {
    pub bytes_consumed: u64,
    pub deferred: bool,
    pub discovery_failures: u64,
}

impl KimiSource {
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        let share_dir = std::env::var_os(KIMI_CODE_HOME_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|override_home| override_home.starts_with(&home))
            .unwrap_or_else(|| home.join(".kimi-code"));
        Some(Self::with_share_dir(&share_dir))
    }

    pub fn with_share_dir(share_dir: &Path) -> Self {
        Self {
            share_dir: share_dir.to_path_buf(),
            user_registered_roots: None,
        }
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: Vec<PathBuf>) -> Self {
        self.user_registered_roots = Some(registered_roots);
        self
    }

    fn discover(
        &self,
        project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
        frontier_path: Option<PathBuf>,
        mut budget: HostScanBudget,
    ) -> TranscriptIngestResult<(KimiDiscoveryReport, HostScanBudget)> {
        hotpath::measure_block!("sessions.hosts.kimi.discover", {
            let mut discovery = KimiDiscoveryReport {
                files: bound_path_list(Vec::new(), bounds),
                failures: Vec::new(),
                failure_count: 0,
                scan_complete: true,
                reached_end: true,
            };
            let matcher = TranscriptScopeMatcher::for_scope(
                project_root,
                self.user_registered_roots.as_deref(),
            );
            let sessions_root = self.share_dir.join("sessions");
            let root_metadata = match std::fs::symlink_metadata(&sessions_root) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok((discovery, budget));
                }
                Err(source) => {
                    return Err(TranscriptIngestError::ScanIo {
                        operation: "stat Kimi sessions root",
                        path: sessions_root,
                        source,
                    });
                }
            };
            if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
                return Err(TranscriptIngestError::ScanIo {
                    operation: "stat Kimi sessions root",
                    path: sessions_root,
                    source: io::Error::other(
                        "Kimi sessions root must be a real directory, not a link",
                    ),
                });
            }
            let limit = bounds.max_files.min(MAX_DISCOVERY_CANDIDATES);
            let mut paths = BinaryHeap::with_capacity(limit);
            let mut has_more = false;
            let work_dirs = read_real_directories(
                &sessions_root,
                KimiDiscoveryFailureKind::DirectoryUnavailable,
                &mut discovery,
                &mut budget,
            )?;
            'session_dirs: for work_dir in work_dirs {
                if !budget.try_charge_unit() {
                    discovery.scan_complete = false;
                    discovery.reached_end = false;
                    break;
                }
                let session_dirs = read_real_directories(
                    &work_dir,
                    KimiDiscoveryFailureKind::DirectoryUnavailable,
                    &mut discovery,
                    &mut budget,
                )?;
                for session_dir in session_dirs {
                    if !budget.checkpoint() {
                        discovery.scan_complete = false;
                        discovery.reached_end = false;
                        break 'session_dirs;
                    }
                    let Some(state) =
                        read_session_state(&session_dir, &mut discovery, &mut budget)?
                    else {
                        continue;
                    };
                    if state.id
                        != session_dir
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("")
                        || !matcher.accepts(Some(&state.cwd))
                    {
                        continue;
                    }
                    let agents_dir = session_dir.join("agents");
                    if !validate_real_directory(
                        &agents_dir,
                        KimiDiscoveryFailureKind::InvalidAgentPartition,
                        &mut discovery,
                        &mut budget,
                    ) {
                        continue;
                    }
                    for (agent_id, agent) in state.agents {
                        if !matches!(agent.kind.as_str(), "main" | "sub")
                            || !safe_component(&agent_id)
                        {
                            if matches!(agent.kind.as_str(), "main" | "sub") {
                                discovery.record_failure(
                                    KimiDiscoveryFailureKind::InvalidAgentPartition,
                                    &session_dir,
                                    &io::Error::new(
                                        io::ErrorKind::InvalidInput,
                                        "unsafe Kimi agent id",
                                    ),
                                    &mut budget,
                                );
                            }
                            continue;
                        }
                        let agent_dir = agents_dir.join(agent_id);
                        if !validate_real_directory(
                            &agent_dir,
                            KimiDiscoveryFailureKind::InvalidAgentPartition,
                            &mut discovery,
                            &mut budget,
                        ) {
                            continue;
                        }
                        let candidate = agent_dir.join("wire.jsonl");
                        match std::fs::symlink_metadata(&candidate) {
                            Ok(metadata)
                                if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                            Ok(_) => continue,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                            Err(error) => {
                                discovery.record_failure(
                                    KimiDiscoveryFailureKind::SessionMetadataUnavailable,
                                    &candidate,
                                    &error,
                                    &mut budget,
                                );
                                continue;
                            }
                        }
                        if frontier_path
                            .as_ref()
                            .is_some_and(|frontier| candidate <= *frontier)
                        {
                            continue;
                        }
                        if !budget.try_charge_unit()
                            || !charge_discovered_path(&mut budget, &candidate)?
                        {
                            discovery.scan_complete = false;
                            discovery.reached_end = false;
                            break 'session_dirs;
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
            }
            let paths = paths.into_sorted_vec();
            discovery.files = bound_path_list(
                paths,
                TranscriptDiscoveryBounds {
                    max_files: limit,
                    ..bounds
                },
            );
            if has_more {
                discovery.files.truncated = Some(FileDiscoveryLimit::FileCount);
                discovery.reached_end = false;
            }
            if discovery.files.is_truncated() {
                discovery.reached_end = false;
            }
            Ok((discovery, budget))
        })
    }
}

fn safe_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

fn validate_real_directory(
    path: &Path,
    failure_kind: KimiDiscoveryFailureKind,
    discovery: &mut KimiDiscoveryReport,
    budget: &mut HostScanBudget,
) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            discovery.record_failure(
                failure_kind,
                path,
                &io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Kimi session directory must be a real directory, not a link",
                ),
                budget,
            );
            false
        }
        Err(error) => {
            discovery.record_failure(failure_kind, path, &error, budget);
            false
        }
    }
}

fn read_real_directories(
    parent: &Path,
    failure_kind: KimiDiscoveryFailureKind,
    discovery: &mut KimiDiscoveryReport,
    budget: &mut HostScanBudget,
) -> TranscriptIngestResult<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            discovery.record_failure(failure_kind, parent, &error, budget);
            return Ok(Vec::new());
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                discovery.record_failure(
                    KimiDiscoveryFailureKind::DirectoryEntryUnavailable,
                    parent,
                    &error,
                    budget,
                );
                continue;
            }
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && !kind.is_symlink() => paths.push(path),
            Ok(_) => {}
            Err(error) => discovery.record_failure(
                KimiDiscoveryFailureKind::EntryTypeUnavailable,
                &path,
                &error,
                budget,
            ),
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_session_state(
    session_dir: &Path,
    discovery: &mut KimiDiscoveryReport,
    budget: &mut HostScanBudget,
) -> TranscriptIngestResult<Option<KimiSessionState>> {
    let path = session_dir.join("state.json");
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            discovery.record_failure(
                KimiDiscoveryFailureKind::SessionMetadataUnavailable,
                &path,
                &error,
                budget,
            );
            return Ok(None);
        }
    };
    if metadata.len() > MAX_SNAPSHOT_METADATA_BYTES {
        discovery.record_failure(
            KimiDiscoveryFailureKind::InvalidSessionMetadata,
            &path,
            &io::Error::new(
                io::ErrorKind::InvalidData,
                "Kimi state exceeds metadata bound",
            ),
            budget,
        );
        return Ok(None);
    }
    if !budget.try_charge_input(metadata.len()) {
        discovery.scan_complete = false;
        discovery.reached_end = false;
        return Ok(None);
    }
    let Some(text) = read_snapshot_text_bounded(PROVIDER, &path, MAX_SNAPSHOT_METADATA_BYTES)?
    else {
        return Ok(None);
    };
    match serde_json::from_str(&text) {
        Ok(state) => Ok(Some(state)),
        Err(_) => {
            discovery.record_failure(
                KimiDiscoveryFailureKind::InvalidSessionMetadata,
                &path,
                &io::Error::new(io::ErrorKind::InvalidData, "malformed Kimi state JSON"),
                budget,
            );
            Ok(None)
        }
    }
}

pub async fn capture_kimi_observations(
    facade: &dyn HostAdmission,
    source: &KimiSource,
    project_root: &Path,
    scope: ObservationScopeV1,
    max_new_bytes: Option<u64>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<KimiCaptureOutcome> {
    hotpath::future!(
        async {
            let discovery_frontier = facade
                .get_parse_offset(&scope, KIMI_DISCOVERY_FRONTIER_KEY)
                .await
                .map_err(|outcome| {
                    crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
                })?
                .unwrap_or_default();
            let frontier_path = if discovery_frontier.file_id == 0 {
                None
            } else {
                Some(
                    facade
                        .discovery_path(&scope, PROVIDER, discovery_frontier.file_id)
                        .await
                        .map_err(|outcome| {
                            crate::runtime::snapshot_observation::host_admission_error(
                                PROVIDER, outcome,
                            )
                        })?
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
            let discovered = hotpath::future!(
                tokio::task::spawn_blocking(move || {
                    owned_source.discover(
                        &owned_project_root,
                        TranscriptDiscoveryBounds::from_discovered_units(MAX_DISCOVERY_CANDIDATES),
                        frontier_path,
                        scan_budget,
                    )
                }),
                label = "sessions.hosts.kimi.discover_task"
            )
            .await
            .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
            let (discovery, scan_budget) = discovered;
            for failure in &discovery.failures {
                tracing::warn!(
                    provider = PROVIDER,
                    failure_kind = ?failure.kind,
                    error_kind = ?failure.error_kind,
                    source_digest = failure.source_digest,
                    "Kimi session discovery is incomplete"
                );
            }
            if discovery.failure_count > discovery.failures.len() as u64 {
                tracing::warn!(
                    provider = PROVIDER,
                    failure_count = discovery.failure_count,
                    reported_failures = discovery.failures.len(),
                    "additional Kimi discovery failures were bounded"
                );
            }
            let discovery_truncated = discovery.files.is_truncated();
            let discovery_skipped = discovery.files.skipped_oversized_entries;
            let discovered_paths = discovery.files.paths;
            let last_discovered_entry = if cancellation.is_cancelled() {
                None
            } else {
                facade
                    .enqueue_discovery_paths(&scope, PROVIDER, discovered_paths)
                    .await
                    .map_err(|outcome| {
                        crate::runtime::snapshot_observation::host_admission_error(
                            PROVIDER, outcome,
                        )
                    })?
            };
            let queue_frontier = facade
                .get_parse_offset(&scope, KIMI_QUEUE_FRONTIER_KEY)
                .await
                .map_err(|outcome| {
                    crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
                })?
                .unwrap_or_default();
            let mut scheduled_paths = if cancellation.is_cancelled() {
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
                    .map_err(|outcome| {
                        crate::runtime::snapshot_observation::host_admission_error(
                            PROVIDER, outcome,
                        )
                    })?
            };
            if scheduled_paths.is_empty()
                && queue_frontier.byte_offset > 0
                && !cancellation.is_cancelled()
            {
                scheduled_paths = facade
                    .discovery_paths_after(&scope, PROVIDER, 0, MAX_SESSION_FILES.saturating_add(1))
                    .await
                    .map_err(|outcome| {
                        crate::runtime::snapshot_observation::host_admission_error(
                            PROVIDER, outcome,
                        )
                    })?;
            }
            let queue_has_more = scheduled_paths.len() > MAX_SESSION_FILES;
            scheduled_paths.truncate(MAX_SESSION_FILES);
            let mut outcome = KimiCaptureOutcome {
                deferred: discovery_truncated
                    || queue_has_more
                    || discovery_skipped > 0
                    || discovery.failure_count > 0
                    || scan_budget.evidence().is_deferred()
                    || cancellation.is_cancelled(),
                discovery_failures: discovery.failure_count,
                ..KimiCaptureOutcome::default()
            };
            let mut remaining = max_new_bytes.unwrap_or(u64::MAX);
            let mut processed_sequence = None;
            for HostDiscoveryQueueEntry { sequence, path } in scheduled_paths {
                if cancellation.is_cancelled() || remaining == 0 {
                    outcome.deferred = true;
                    break;
                }
                let (session_id, agent_id) = match kimi_session_identity(&path) {
                    Ok(identity) => identity,
                    Err(_) => {
                        warn_isolated_source(&path, "invalid_source_identity");
                        outcome.discovery_failures = outcome.discovery_failures.saturating_add(1);
                        outcome.deferred = true;
                        processed_sequence = Some(sequence);
                        continue;
                    }
                };
                let provider = ProviderId::new(PROVIDER).map_err(|_| invalid_frame())?;
                let session = SessionId::new(&session_id).map_err(|_| invalid_frame())?;
                let file_identity = match hotpath::measure_block!(
                    "sessions.hosts.kimi.identity_blocking",
                    run_blocking_transcript_section(|| jsonl_file_identity(&path))
                ) {
                    Ok(file_identity) => file_identity,
                    Err(error) => {
                        warn_isolated_source(&path, "source_identity_unavailable");
                        tracing::debug!(
                            provider = PROVIDER,
                            source = %path.display(),
                            error = %error,
                            "Kimi source identity read failed"
                        );
                        outcome.discovery_failures = outcome.discovery_failures.saturating_add(1);
                        outcome.deferred = true;
                        processed_sequence = Some(sequence);
                        continue;
                    }
                };
                let source_key = SessionId::new(format!("kimi-file-{file_identity:016x}"))
                    .map_err(|_| invalid_frame())?;
                let source_identity =
                    ObservationSourceIdentityV1::for_provider_source(provider, session, source_key)
                        .map_err(|_| invalid_frame())?;
                let retention =
                    RetentionClass::new("transcript.kimi.v1").map_err(|_| invalid_frame())?;
                let native_record_prefix = format!("{session_id}:{agent_id}");
                let request = JsonlObservationAdmissionRequest::new(
                    PROVIDER,
                    &path,
                    facade,
                    source_identity,
                    scope.clone(),
                    retention,
                )
                .with_max_new_bytes(max_new_bytes.map(|_| remaining))
                .with_cancellation(cancellation.clone());
                let progress = admit_jsonl_observations(
                    request,
                    |_| (),
                    move |(), bytes, range, _, _prepared, _hints| {
                        let native_id =
                            kimi_capture::native_record_id(&native_record_prefix, range)
                                .map_err(|_| invalid_frame())?;
                        match parse_normalized_observation_record_v1(
                            bytes,
                            range,
                            ObservationOrderingDomainV1::FileBytes,
                            |native| {
                                kimi_capture::normalize_observation(
                                    &native,
                                    &session_id,
                                    native_id.clone(),
                                    range,
                                )
                            },
                        ) {
                            Ok(parsed) => Ok(JsonlFrameAdmission::durable(parsed, native_id)),
                            Err(ObservationRecordParseErrorV1::Empty) => {
                                Ok(JsonlFrameAdmission::non_durable(
                                    ObservationCoverageReason::BlankFrame,
                                ))
                            }
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
                .await;
                let progress = match progress {
                    Ok(progress) => progress,
                    Err(error) if isolatable_source_error(&error) => {
                        warn_isolated_source(&path, "source_unavailable");
                        outcome.discovery_failures = outcome.discovery_failures.saturating_add(1);
                        outcome.deferred = true;
                        processed_sequence = Some(sequence);
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                outcome.bytes_consumed = outcome
                    .bytes_consumed
                    .saturating_add(progress.bytes_consumed);
                outcome.deferred |= progress.source_deferred;
                remaining = remaining.saturating_sub(progress.bytes_consumed);
                processed_sequence = Some(sequence);
            }
            if let Some(sequence) = processed_sequence
                && !cancellation.is_cancelled()
            {
                facade
                    .advance_parse_offset(
                        &scope,
                        KIMI_QUEUE_FRONTIER_KEY,
                        ParseOffset {
                            byte_offset: sequence,
                            mtime: queue_frontier.mtime.saturating_add(1),
                            file_id: KIMI_FRONTIER_VERSION,
                        },
                    )
                    .await
                    .map_err(|outcome| {
                        crate::runtime::snapshot_observation::host_admission_error(
                            PROVIDER, outcome,
                        )
                    })?;
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
                if let Some(next_frontier) = next_frontier
                    && !cancellation.is_cancelled()
                {
                    facade
                        .advance_parse_offset(&scope, KIMI_DISCOVERY_FRONTIER_KEY, next_frontier)
                        .await
                        .map_err(|outcome| {
                            crate::runtime::snapshot_observation::host_admission_error(
                                PROVIDER, outcome,
                            )
                        })?;
                }
            }
            let deferred_units = outcome
                .discovery_failures
                .saturating_add(u64::from(outcome.deferred));
            persist_host_provider_coverage(
                facade,
                &scope,
                PROVIDER,
                if outcome.deferred {
                    HostProviderCoverage::Partial
                } else {
                    HostProviderCoverage::Complete
                },
                deferred_units,
            )
            .await?;
            Ok(outcome)
        },
        label = "sessions.hosts.kimi.capture"
    )
    .await
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

fn warn_isolated_source(path: &Path, failure_kind: &'static str) {
    tracing::warn!(
        provider = PROVIDER,
        failure_kind,
        source_digest = canonical_framed_sha256(
            b"tracedecay.kimi.session-source.v1",
            &[path.as_os_str().as_encoded_bytes()],
        ),
        "Kimi session source was isolated"
    );
}

fn kimi_session_identity(path: &Path) -> TranscriptIngestResult<(String, String)> {
    let session_dir = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|_| path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl"))
        .ok_or_else(|| TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        })?;
    let state_path = session_dir.join("state.json");
    let state_metadata =
        std::fs::symlink_metadata(&state_path).map_err(|source| TranscriptIngestError::ScanIo {
            operation: "stat Kimi session state",
            path: state_path.clone(),
            source,
        })?;
    if state_metadata.file_type().is_symlink()
        || !state_metadata.is_file()
        || state_metadata.len() > MAX_SNAPSHOT_METADATA_BYTES
    {
        return Err(TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        });
    }
    let state = read_snapshot_text_bounded(PROVIDER, &state_path, MAX_SNAPSHOT_METADATA_BYTES)?
        .ok_or_else(|| TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        })?;
    let state: KimiSessionState =
        serde_json::from_str(&state).map_err(|_| TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        })?;
    let directory_id = session_dir.file_name().and_then(|name| name.to_str());
    let agent_id = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|agent_id| safe_component(agent_id))
        .ok_or_else(|| TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        })?;
    if directory_id != Some(state.id.as_str()) || !state.agents.contains_key(agent_id) {
        return Err(TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        });
    }
    Ok((state.id, agent_id.to_owned()))
}

const fn invalid_frame() -> TranscriptIngestError {
    TranscriptIngestError::InvalidFrameState { provider: PROVIDER }
}

#[cfg(test)]
#[path = "kimi_frontier_tests.rs"]
mod frontier_tests;

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::time::Instant;
    use tracedecay_domain::ObservationScopeV1;

    use crate::admission::{HostAdmission, test_support::MemoryHostAdmission};
    use crate::observation::ObservationCancellation;
    use crate::runtime::host_scan::{HOST_SCAN_WINDOW, HostScanBudget};
    use crate::runtime::source::{HostProviderCoverage, TranscriptDiscoveryBounds};

    use super::{KimiSource, capture_kimi_observations};

    fn discovery_budget() -> HostScanBudget {
        HostScanBudget::new(
            super::MAX_DISCOVERY_INPUT_BYTES,
            super::MAX_DISCOVERY_UNITS,
            Instant::now() + HOST_SCAN_WINDOW,
            ObservationCancellation::default(),
        )
    }

    fn fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        KimiSource,
    ) {
        crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let share = temp.path().join(".kimi-code");
        let session = share.join("sessions/wd_project/session-current");
        let transcript = session.join("agents/main/wire.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            session.join("state.json"),
            json!({
                "id": "session-current",
                "version": 2,
                "cwd": project,
                "agents": {"main": {"type": "main"}}
            })
            .to_string(),
        )
        .unwrap();
        (
            temp,
            project,
            transcript,
            KimiSource::with_share_dir(&share),
        )
    }

    fn wire_message(role: &str, text: &str, time: u64) -> String {
        json!({
            "type": "context.append_message",
            "agentId": "main",
            "message": {
                "role": role,
                "content": [{"type": "text", "text": text}],
                "toolCalls": []
            },
            "time": time
        })
        .to_string()
            + "\n"
    }

    #[tokio::test]
    async fn current_native_wire_is_bounded_resumable_and_keeps_exact_visible_text() {
        let (_temp, project, path, source) = fixture();
        let first = wire_message("user", "first", 1_789_228_081_157);
        let second = json!({
            "type": "context.append_loop_event",
            "agentId": "main",
            "event": {
                "type": "content.part",
                "uuid": "event-2",
                "turnId": "0",
                "step": 2,
                "stepUuid": "step-2",
                "part": {"type": "text", "text": "TDKIMI-CURRENT-NONCE"}
            },
            "time": 1_789_228_156_434_u64
        })
        .to_string()
            + "\n";
        std::fs::write(&path, format!("{first}{second}")).unwrap();
        let admission = MemoryHostAdmission::default();

        let partial = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(first.len() as u64),
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
        assert!(partial.deferred);
        assert_eq!(admission.observations().len(), 1);

        let resumed = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
        assert!(!resumed.deferred);
        let payloads = admission
            .observations()
            .iter()
            .map(|stored| stored.observation().payload().to_string())
            .collect::<String>();
        assert!(payloads.contains("TDKIMI-CURRENT-NONCE"));
        assert!(payloads.contains("session-current"));
        let coverage = admission
            .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://kimi/v1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(coverage.file_id, HostProviderCoverage::Complete as u64);
    }

    #[test]
    fn discovery_uses_state_cwd_as_scope_authority() {
        let (_temp, project, path, source) = fixture();
        std::fs::write(path, wire_message("user", "visible", 1)).unwrap();
        let in_scope = source
            .discover(
                &project,
                TranscriptDiscoveryBounds::default_walk(),
                None,
                discovery_budget(),
            )
            .unwrap()
            .0;
        assert_eq!(in_scope.files.paths.len(), 1);
        let out_of_scope = source
            .discover(
                &project.join("other"),
                TranscriptDiscoveryBounds::default_walk(),
                None,
                discovery_budget(),
            )
            .unwrap()
            .0;
        assert!(out_of_scope.files.paths.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn linked_sessions_root_is_rejected_before_discovery() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let share = temp.path().join(".kimi-code");
        let outside = temp.path().join("outside-sessions");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, share.join("sessions")).unwrap();
        let error = match KimiSource::with_share_dir(&share).discover(
            temp.path(),
            TranscriptDiscoveryBounds::default_walk(),
            None,
            discovery_budget(),
        ) {
            Ok(_) => panic!("linked Kimi sessions root must not be discovered"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            crate::runtime::source::TranscriptIngestError::ScanIo {
                operation: "stat Kimi sessions root",
                ..
            }
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linked_agents_directory_cannot_escape_the_kimi_session() {
        use std::os::unix::fs::symlink;

        let (temp, project, path, source) = fixture();
        let session_dir = path
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .unwrap();
        let outside = temp.path().join("outside-agents");
        std::fs::create_dir_all(outside.join("main")).unwrap();
        std::fs::write(
            outside.join("main/wire.jsonl"),
            wire_message("user", "outside provider root", 1),
        )
        .unwrap();
        std::fs::remove_dir_all(session_dir.join("agents")).unwrap();
        symlink(&outside, session_dir.join("agents")).unwrap();
        let admission = MemoryHostAdmission::default();

        let outcome = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();

        assert!(outcome.deferred);
        assert_eq!(outcome.discovery_failures, 1);
        assert!(admission.observations().is_empty());
    }
}
