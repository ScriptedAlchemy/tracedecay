use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use md5::{Digest, Md5};
use serde::Deserialize;
use tracedecay_capture::kimi as kimi_capture;
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProviderId,
    RetentionClass, SessionId,
};
use tracedecay_runtime_core::privacy::{
    ObservationRecordParseErrorV1, parse_normalized_observation_record_v1,
};
use tracedecay_store::{ParseOffset, observation::ObservationCoverageReason};

use crate::admission::HostAdmission;
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
    FileDiscoveryReport, HostProviderCoverage, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, bound_path_list, canonical_framed_sha256, jsonl_file_identity,
    persist_host_provider_coverage,
};

const PROVIDER: &str = "kimi";
const MAX_SESSION_FILES: usize = 512;
const MAX_DISCOVERY_CANDIDATES: usize = 4_096;
const MAX_DISCOVERY_FAILURE_EVIDENCE: usize = 16;
const MAX_DISCOVERY_INPUT_BYTES: u64 =
    MAX_SNAPSHOT_METADATA_BYTES + ((MAX_DISCOVERY_CANDIDATES as u64 + 1) * 4 * 1024);
const MAX_DISCOVERY_UNITS: usize = MAX_DISCOVERY_CANDIDATES * 2;
const KIMI_DISCOVERY_FRONTIER_KEY: &str = "host-frontier://kimi/discovery/v1";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KimiDiscoveryFailureKind {
    InvalidProviderPartition,
    DirectoryUnavailable,
    DirectoryEntryUnavailable,
    EntryTypeUnavailable,
    ContextMetadataUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KimiDiscoveryFailure {
    kind: KimiDiscoveryFailureKind,
    source_digest: String,
    error_kind: io::ErrorKind,
}

struct KimiDiscoveryReport {
    files: FileDiscoveryReport,
    path_offsets: Vec<(PathBuf, u64)>,
    failures: Vec<KimiDiscoveryFailure>,
    failure_count: u64,
    witness: u64,
    start_offset: u64,
    reached_end: bool,
}

impl KimiDiscoveryReport {
    fn record_failure(
        &mut self,
        kind: KimiDiscoveryFailureKind,
        path: &Path,
        error: &io::Error,
        budget: &mut HostScanBudget,
    ) {
        self.failure_count = self.failure_count.saturating_add(1);
        budget.mark_unavailable();
        if self.failures.len() < MAX_DISCOVERY_FAILURE_EVIDENCE {
            self.failures.push(KimiDiscoveryFailure {
                kind,
                source_digest: canonical_framed_sha256(
                    b"tracedecay.kimi.discovery-source.v1",
                    &[path.as_os_str().as_encoded_bytes()],
                ),
                error_kind: error.kind(),
            });
        }
    }
}

#[derive(Deserialize)]
struct KimiMetadata {
    #[serde(default)]
    work_dirs: Vec<KimiWorkDir>,
}

#[derive(Deserialize)]
struct KimiWorkDir {
    path: PathBuf,
    #[serde(default = "local_kaos")]
    kaos: String,
}

fn local_kaos() -> String {
    "local".to_owned()
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

fn discovery_witness(
    session_dirs: impl IntoIterator<Item = PathBuf>,
) -> TranscriptIngestResult<u64> {
    let mut witness = canonical_framed_sha256(b"tracedecay.kimi.discovery-witness.v1", &[b"start"]);
    for path in session_dirs {
        let canonical = match std::fs::canonicalize(&path) {
            Ok(canonical) => canonical,
            Err(error) => {
                let error_kind = format!("{:?}", error.kind());
                let next = canonical_framed_sha256(
                    b"tracedecay.kimi.discovery-witness.v1",
                    &[
                        witness.as_bytes(),
                        path.as_os_str().as_encoded_bytes(),
                        b"canonical-unavailable",
                        error_kind.as_bytes(),
                    ],
                );
                witness = next;
                continue;
            }
        };
        let metadata =
            std::fs::metadata(&canonical).map_err(|source| TranscriptIngestError::ScanIo {
                operation: "stat Kimi sessions directory witness",
                path: canonical.clone(),
                source,
            })?;
        let modified = metadata
            .modified()
            .map_err(|source| TranscriptIngestError::ScanIo {
                operation: "read Kimi sessions directory generation",
                path: canonical.clone(),
                source,
            })?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| TranscriptIngestError::NonDurableRecord {
                provider: PROVIDER,
                offset: 0,
                end_offset: 0,
                reason: "Kimi sessions directory generation predates Unix epoch",
            })?
            .as_nanos()
            .to_be_bytes();
        witness = canonical_framed_sha256(
            b"tracedecay.kimi.discovery-witness.v1",
            &[
                witness.as_bytes(),
                canonical.as_os_str().as_encoded_bytes(),
                &metadata.len().to_be_bytes(),
                &modified,
            ],
        );
    }
    let prefix = witness.get(..16).ok_or_else(invalid_frame)?;
    u64::from_str_radix(prefix, 16).map_err(|_| invalid_frame())
}

impl KimiSource {
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_share_dir(&home.join(".kimi")))
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
        frontier: ParseOffset,
        mut budget: HostScanBudget,
    ) -> TranscriptIngestResult<(KimiDiscoveryReport, HostScanBudget)> {
        let mut discovery = KimiDiscoveryReport {
            files: bound_path_list(Vec::new(), bounds),
            path_offsets: Vec::new(),
            failures: Vec::new(),
            failure_count: 0,
            witness: 0,
            start_offset: 0,
            reached_end: true,
        };
        let Some(metadata) = self.metadata(&mut budget)? else {
            return Ok((discovery, budget));
        };
        let matcher =
            TranscriptScopeMatcher::for_scope(project_root, self.user_registered_roots.as_deref());
        let work_dirs = metadata
            .work_dirs
            .into_iter()
            .filter(|work_dir| matcher.accepts(Some(&work_dir.path)))
            .collect::<Vec<_>>();
        let mut session_dirs = Vec::with_capacity(work_dirs.len());
        for work_dir in work_dirs {
            match self.sessions_dir(&work_dir) {
                Some(path) => session_dirs.push(path),
                None => discovery.record_failure(
                    KimiDiscoveryFailureKind::InvalidProviderPartition,
                    &self.share_dir.join("sessions"),
                    &io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Kimi metadata contains an unsafe provider partition",
                    ),
                    &mut budget,
                ),
            }
        }
        if !session_dirs.is_empty() {
            let sessions_root = self.share_dir.join("sessions");
            let sessions_root_metadata =
                std::fs::symlink_metadata(&sessions_root).map_err(|source| {
                    TranscriptIngestError::ScanIo {
                        operation: "stat Kimi sessions root",
                        path: sessions_root.clone(),
                        source,
                    }
                })?;
            if sessions_root_metadata.file_type().is_symlink() || !sessions_root_metadata.is_dir() {
                return Err(TranscriptIngestError::ScanIo {
                    operation: "stat Kimi sessions root",
                    path: sessions_root,
                    source: io::Error::other(
                        "Kimi sessions root must be a real directory, not a link",
                    ),
                });
            }
        }
        session_dirs.sort();
        discovery.witness = discovery_witness(session_dirs.iter().cloned())?;
        discovery.start_offset = if frontier.file_id == discovery.witness {
            frontier.byte_offset
        } else {
            0
        };
        let limit = bounds.max_files.min(MAX_DISCOVERY_CANDIDATES);
        let mut paths = Vec::with_capacity(limit.saturating_add(1));
        let mut raw_offset = 0_u64;
        'session_dirs: for sessions_dir in session_dirs {
            if !budget.try_charge_unit() {
                discovery.reached_end = false;
                break;
            }
            match std::fs::symlink_metadata(&sessions_dir) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    discovery.record_failure(
                        KimiDiscoveryFailureKind::DirectoryUnavailable,
                        &sessions_dir,
                        &io::Error::other(
                            "Kimi session directory must be a real directory, not a link",
                        ),
                        &mut budget,
                    );
                    continue;
                }
                Err(error) => {
                    discovery.record_failure(
                        KimiDiscoveryFailureKind::DirectoryUnavailable,
                        &sessions_dir,
                        &error,
                        &mut budget,
                    );
                    continue;
                }
            }
            let entries = match std::fs::read_dir(&sessions_dir) {
                Ok(entries) => entries,
                Err(error) => {
                    discovery.record_failure(
                        KimiDiscoveryFailureKind::DirectoryUnavailable,
                        &sessions_dir,
                        &error,
                        &mut budget,
                    );
                    continue;
                }
            };
            for entry in entries {
                if paths.len() > limit || !budget.checkpoint() {
                    discovery.reached_end = false;
                    break 'session_dirs;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        discovery.record_failure(
                            KimiDiscoveryFailureKind::DirectoryEntryUnavailable,
                            &sessions_dir,
                            &error,
                            &mut budget,
                        );
                        continue;
                    }
                };
                raw_offset = raw_offset.saturating_add(1);
                if raw_offset <= discovery.start_offset {
                    continue;
                }
                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(error) => {
                        discovery.record_failure(
                            KimiDiscoveryFailureKind::EntryTypeUnavailable,
                            &path,
                            &error,
                            &mut budget,
                        );
                        continue;
                    }
                };
                let candidate = if file_type.is_symlink() {
                    None
                } else if file_type.is_file()
                    && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                {
                    Some(path)
                } else if file_type.is_dir() {
                    let context = path.join("context.jsonl");
                    match std::fs::symlink_metadata(&context) {
                        Ok(metadata) if metadata.is_file() => Some(context),
                        Ok(_) => None,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                        Err(error) => {
                            discovery.record_failure(
                                KimiDiscoveryFailureKind::ContextMetadataUnavailable,
                                &context,
                                &error,
                                &mut budget,
                            );
                            None
                        }
                    }
                } else {
                    None
                };
                let Some(candidate) = candidate else {
                    continue;
                };
                if !budget.try_charge_unit() || !charge_discovered_path(&mut budget, &candidate)? {
                    discovery.reached_end = false;
                    break 'session_dirs;
                }
                discovery.path_offsets.push((candidate.clone(), raw_offset));
                paths.push(candidate);
            }
        }
        discovery.files = bound_path_list(
            paths,
            TranscriptDiscoveryBounds {
                max_files: limit,
                ..bounds
            },
        );
        discovery.path_offsets.truncate(limit);
        Ok((discovery, budget))
    }

    fn metadata(
        &self,
        budget: &mut HostScanBudget,
    ) -> TranscriptIngestResult<Option<KimiMetadata>> {
        let path = self.share_dir.join("kimi.json");
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(TranscriptIngestError::ScanIo {
                    operation: "stat Kimi metadata",
                    path,
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(TranscriptIngestError::ScanIo {
                operation: "stat Kimi metadata",
                path,
                source: io::Error::other("Kimi metadata must be a regular file, not a link"),
            });
        }
        if metadata.len() > MAX_SNAPSHOT_METADATA_BYTES {
            return Err(TranscriptIngestError::NonDurableRecord {
                provider: PROVIDER,
                offset: 0,
                end_offset: metadata.len(),
                reason: "Kimi metadata exceeds provider byte bound",
            });
        }
        if !budget.try_charge_input(metadata.len()) {
            return Ok(None);
        }
        let Some(text) = read_snapshot_text_bounded(PROVIDER, &path, MAX_SNAPSHOT_METADATA_BYTES)?
        else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| TranscriptIngestError::NonDurableRecord {
                provider: PROVIDER,
                offset: 0,
                end_offset: text.len() as u64,
                reason: "malformed Kimi metadata JSON",
            })
    }

    fn sessions_dir(&self, work_dir: &KimiWorkDir) -> Option<PathBuf> {
        let digest = Md5::digest(work_dir.path.to_string_lossy().as_bytes());
        let hash = format!("{digest:x}");
        let directory = if matches!(work_dir.kaos.as_str(), "" | "local") {
            hash
        } else {
            let mut components = Path::new(&work_dir.kaos).components();
            if !matches!(components.next(), Some(std::path::Component::Normal(_)))
                || components.next().is_some()
            {
                return None;
            }
            format!("{}_{hash}", work_dir.kaos)
        };
        Some(self.share_dir.join("sessions").join(directory))
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
    let frontier = facade
        .get_parse_offset(&scope, KIMI_DISCOVERY_FRONTIER_KEY)
        .await
        .map_err(|outcome| {
            crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
        })?
        .unwrap_or_default();
    let scan_budget = HostScanBudget::new(
        MAX_DISCOVERY_INPUT_BYTES,
        MAX_DISCOVERY_UNITS,
        Instant::now() + HOST_SCAN_WINDOW,
        cancellation.clone(),
    );
    let owned_source = source.clone();
    let owned_project_root = project_root.to_path_buf();
    let discovered = tokio::task::spawn_blocking(move || {
        owned_source.discover(
            &owned_project_root,
            TranscriptDiscoveryBounds::from_discovered_units(MAX_DISCOVERY_CANDIDATES),
            frontier,
            scan_budget,
        )
    })
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
    let mut scheduled_paths = discovery.path_offsets;
    let unscheduled_files = scheduled_paths.len().saturating_sub(MAX_SESSION_FILES);
    scheduled_paths.truncate(MAX_SESSION_FILES);
    let scheduled_count = scheduled_paths.len();
    let mut outcome = KimiCaptureOutcome {
        deferred: discovery_truncated
            || unscheduled_files > 0
            || discovery.failure_count > 0
            || scan_budget.evidence().is_deferred(),
        discovery_failures: discovery.failure_count,
        ..KimiCaptureOutcome::default()
    };
    let mut remaining = max_new_bytes.unwrap_or(u64::MAX);
    let mut processed = 0_usize;
    let mut processed_offset = discovery.start_offset;
    for (path, next_offset) in scheduled_paths {
        if cancellation.is_cancelled() || remaining == 0 {
            outcome.deferred = true;
            break;
        }
        let session_id = match kimi_session_id(&path) {
            Ok(session_id) => session_id,
            Err(_) => {
                warn_isolated_source(&path, "invalid_source_identity");
                outcome.discovery_failures = outcome.discovery_failures.saturating_add(1);
                outcome.deferred = true;
                processed = processed.saturating_add(1);
                processed_offset = next_offset;
                continue;
            }
        };
        let provider = ProviderId::new(PROVIDER).map_err(|_| invalid_frame())?;
        let session = SessionId::new(&session_id).map_err(|_| invalid_frame())?;
        let file_identity = match jsonl_file_identity(&path) {
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
                processed = processed.saturating_add(1);
                processed_offset = next_offset;
                continue;
            }
        };
        let source_key = SessionId::new(format!("kimi-file-{file_identity:016x}"))
            .map_err(|_| invalid_frame())?;
        let source_identity =
            ObservationSourceIdentityV1::for_provider_source(provider, session, source_key)
                .map_err(|_| invalid_frame())?;
        let retention = RetentionClass::new("transcript.kimi.v1").map_err(|_| invalid_frame())?;
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
            move |(), bytes, range, _| {
                let native_id = kimi_capture::native_record_id(&session_id, range)
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
                    Err(ObservationRecordParseErrorV1::Empty) => Ok(
                        JsonlFrameAdmission::non_durable(ObservationCoverageReason::BlankFrame),
                    ),
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
                processed = processed.saturating_add(1);
                processed_offset = next_offset;
                continue;
            }
            Err(error) => return Err(error),
        };
        outcome.bytes_consumed = outcome
            .bytes_consumed
            .saturating_add(progress.bytes_consumed);
        outcome.deferred |= progress.source_deferred;
        remaining = remaining.saturating_sub(progress.bytes_consumed);
        processed = processed.saturating_add(1);
        processed_offset = next_offset;
    }
    if (processed > 0 || discovery.reached_end) && !scan_budget.evidence().cancelled {
        let next_offset = if discovery.reached_end
            && unscheduled_files == 0
            && !discovery_truncated
            && processed == scheduled_count
        {
            0
        } else {
            processed_offset
        };
        facade
            .advance_parse_offset(
                &scope,
                KIMI_DISCOVERY_FRONTIER_KEY,
                ParseOffset {
                    byte_offset: next_offset,
                    mtime: frontier.mtime.saturating_add(1),
                    file_id: discovery.witness,
                },
            )
            .await
            .map_err(|outcome| {
                crate::runtime::snapshot_observation::host_admission_error(PROVIDER, outcome)
            })?;
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

fn kimi_session_id(path: &Path) -> TranscriptIngestResult<String> {
    let session_id = if path.file_name().and_then(|name| name.to_str()) == Some("context.jsonl") {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
    } else {
        path.file_stem().and_then(|name| name.to_str())
    };
    session_id
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        })
}

const fn invalid_frame() -> TranscriptIngestError {
    TranscriptIngestError::InvalidFrameState { provider: PROVIDER }
}

#[cfg(test)]
#[path = "kimi_frontier_tests.rs"]
mod frontier_tests;

#[cfg(test)]
mod tests {
    use md5::{Digest, Md5};
    use serde_json::json;
    use std::time::Instant;
    use tracedecay_domain::ObservationScopeV1;
    use tracedecay_store::ParseOffset;

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
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let share = temp.path().join("isolated-kimi");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::write(
            share.join("kimi.json"),
            json!({"work_dirs": [{"path": project}]}).to_string(),
        )
        .unwrap();
        let hash = format!("{:x}", Md5::digest(project.to_string_lossy().as_bytes()));
        let transcript = share
            .join("sessions")
            .join(hash)
            .join("session-a")
            .join("context.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        (
            temp,
            project,
            transcript,
            KimiSource::with_share_dir(&share),
        )
    }

    #[tokio::test]
    async fn isolated_source_is_bounded_resumable_and_keeps_partial_prefix() {
        let (_temp, project, path, source) = fixture();
        let first = json!({"role": "user", "content": "first"}).to_string() + "\n";
        let second = json!({"role": "assistant", "content": "second"}).to_string() + "\n";
        std::fs::write(&path, format!("{first}{second}")).unwrap();
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();

        let partial = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            Some(first.len() as u64),
            &cancellation,
        )
        .await
        .unwrap();
        assert!(partial.deferred);
        assert_eq!(admission.observations().len(), 1);
        let partial_coverage = admission
            .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://kimi/v1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            partial_coverage.file_id,
            HostProviderCoverage::Partial as u64
        );
        assert!(partial_coverage.byte_offset > 0);

        let resumed = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &cancellation,
        )
        .await
        .unwrap();
        assert!(!resumed.deferred);
        assert_eq!(admission.observations().len(), 2);
        let complete_coverage = admission
            .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://kimi/v1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            complete_coverage.file_id,
            HostProviderCoverage::Complete as u64
        );
        assert_eq!(complete_coverage.byte_offset, 0);
    }

    #[tokio::test]
    async fn compaction_summary_flows_through_canonical_redaction_authority() {
        let (_temp, project, path, source) = fixture();
        std::fs::write(
            path,
            json!({
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": "Previous context has been compacted. Here is the compaction output: summary",
                    "secret_key": "never-persist-kimi-secret"
                }]
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        let admission = MemoryHostAdmission::default();

        capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();

        let stored = admission
            .observations()
            .iter()
            .map(|observation| observation.observation().payload().to_string())
            .collect::<String>();
        assert!(stored.contains("compaction"));
        assert!(!stored.contains("never-persist-kimi-secret"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn linked_work_dir_preserves_admitted_prefix_and_defers_coverage() {
        use std::os::unix::fs::symlink;

        let (_temp, project, path, source) = fixture();
        std::fs::write(
            &path,
            json!({"role": "user", "content": "available prefix"}).to_string() + "\n",
        )
        .unwrap();
        std::fs::write(
            source.share_dir.join("kimi.json"),
            json!({
                "work_dirs": [
                    {"path": project},
                    {"path": project, "kaos": "remote"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let unavailable_hash = format!("{:x}", Md5::digest(project.to_string_lossy().as_bytes()));
        let unavailable_sessions = source
            .share_dir
            .join("sessions")
            .join(format!("remote_{unavailable_hash}"));
        let outside = source.share_dir.join("outside-session-directory");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("context.jsonl"),
            json!({"role": "user", "content": "outside provider root"}).to_string() + "\n",
        )
        .unwrap();
        symlink(outside, unavailable_sessions).unwrap();
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

        assert_eq!(admission.observations().len(), 1);
        assert!(outcome.deferred);
        assert_eq!(outcome.discovery_failures, 1);
        assert!(!admission.observations().iter().any(|stored| {
            stored
                .observation()
                .payload()
                .to_string()
                .contains("outside provider root")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn linked_provider_metadata_is_rejected_before_discovery() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let share = temp.path().join("kimi");
        std::fs::create_dir_all(&share).unwrap();
        let outside = temp.path().join("outside-kimi.json");
        std::fs::write(&outside, r#"{"work_dirs":[]}"#).unwrap();
        symlink(&outside, share.join("kimi.json")).unwrap();
        let source = KimiSource::with_share_dir(&share);
        let error = match source.discover(
            temp.path(),
            TranscriptDiscoveryBounds::default_walk(),
            ParseOffset::default(),
            discovery_budget(),
        ) {
            Ok(_) => panic!("linked Kimi metadata must not be discovered"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            TranscriptIngestError::ScanIo {
                operation: "stat Kimi metadata",
                path,
                ..
            } if path == share.join("kimi.json")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn linked_sessions_root_is_rejected_before_discovery() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        let share = temp.path().join("kimi");
        let outside = temp.path().join("outside-sessions");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&share).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            share.join("kimi.json"),
            json!({"work_dirs": [{"path": project}]}).to_string(),
        )
        .unwrap();
        symlink(&outside, share.join("sessions")).unwrap();
        let source = KimiSource::with_share_dir(&share);
        let error = match source.discover(
            &project,
            TranscriptDiscoveryBounds::default_walk(),
            ParseOffset::default(),
            discovery_budget(),
        ) {
            Ok(_) => panic!("linked Kimi sessions root must not be discovered"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            TranscriptIngestError::ScanIo {
                operation: "stat Kimi sessions root",
                path,
                ..
            } if path == share.join("sessions")
        ));
    }

    #[tokio::test]
    async fn provider_partition_cannot_escape_the_kimi_sessions_root() {
        let (_temp, project, _path, source) = fixture();
        let hash = format!("{:x}", Md5::digest(project.to_string_lossy().as_bytes()));
        let escaped = source
            .share_dir
            .join("escape")
            .join(format!("session-{hash}"))
            .join("context.jsonl");
        std::fs::create_dir_all(escaped.parent().unwrap()).unwrap();
        std::fs::write(
            &escaped,
            json!({"role": "user", "content": "outside provider root"}).to_string() + "\n",
        )
        .unwrap();
        std::fs::write(
            source.share_dir.join("kimi.json"),
            json!({
                "work_dirs": [{
                    "path": project,
                    "kaos": "../escape/session"
                }]
            })
            .to_string(),
        )
        .unwrap();
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

    #[test]
    fn discovery_is_scoped_and_reports_file_count_backpressure() {
        let (_temp, project, first, source) = fixture();
        std::fs::write(&first, "{}\n").unwrap();
        let second = first
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("session-b/context.jsonl");
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(second, "{}\n").unwrap();

        let report = source
            .discover(
                &project,
                TranscriptDiscoveryBounds {
                    max_files: 1,
                    ..TranscriptDiscoveryBounds::default_walk()
                },
                ParseOffset::default(),
                discovery_budget(),
            )
            .unwrap()
            .0;
        assert_eq!(report.files.paths.len(), 1);
        assert!(report.files.is_truncated());
        assert!(
            source
                .discover(
                    &project.join("unregistered"),
                    TranscriptDiscoveryBounds::default_walk(),
                    ParseOffset::default(),
                    discovery_budget(),
                )
                .unwrap()
                .0
                .files
                .paths
                .is_empty()
        );
    }

    #[tokio::test]
    async fn durable_discovery_frontier_survives_restart_beyond_candidate_window() {
        let (_temp, project, first, source) = fixture();
        if first.exists() {
            std::fs::remove_file(first).unwrap();
        }
        let sessions = source.share_dir.join("sessions").join(format!(
            "{:x}",
            Md5::digest(project.to_string_lossy().as_bytes())
        ));
        for ordinal in 0..=super::MAX_DISCOVERY_CANDIDATES {
            let transcript = sessions
                .join(format!("session-{ordinal:04}"))
                .join("context.jsonl");
            std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
            std::fs::write(
                transcript,
                json!({"role": "user", "content": format!("message-{ordinal:04}")}).to_string()
                    + "\n",
            )
            .unwrap();
        }
        let admission = MemoryHostAdmission::default();

        let first = capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
        assert!(first.deferred);
        assert_eq!(admission.observations().len(), super::MAX_SESSION_FILES);

        let mut previous = admission.observations().len();
        for _ in 0..10 {
            let restarted = KimiSource::with_share_dir(&source.share_dir);
            capture_kimi_observations(
                &admission,
                &restarted,
                &project,
                ObservationScopeV1::Profile,
                None,
                &ObservationCancellation::default(),
            )
            .await
            .unwrap();
            let current = admission.observations().len();
            assert!(
                current >= previous,
                "restart must not lose durable progress"
            );
            previous = current;
            if current == super::MAX_DISCOVERY_CANDIDATES + 1 {
                break;
            }
        }
        assert_eq!(
            admission.observations().len(),
            super::MAX_DISCOVERY_CANDIDATES + 1
        );
        assert!(admission.observations().iter().any(|stored| {
            stored
                .observation()
                .payload()
                .to_string()
                .contains("message-4096")
        }));
    }
}
