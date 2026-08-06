use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{
    FileDiscoveryReport, TranscriptIngestError, TranscriptIngestResult, canonical_framed_sha256,
};

use super::{MAX_DISCOVERY_FAILURE_EVIDENCE, PROVIDER, invalid_frame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KimiDiscoveryFailureKind {
    InvalidProviderPartition,
    DirectoryUnavailable,
    DirectoryEntryUnavailable,
    EntryTypeUnavailable,
    ContextMetadataUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KimiDiscoveryFailure {
    pub(super) kind: KimiDiscoveryFailureKind,
    pub(super) source_digest: String,
    pub(super) error_kind: io::ErrorKind,
}

pub(super) struct KimiDiscoveryReport {
    pub(super) files: FileDiscoveryReport,
    pub(super) path_offsets: Vec<(PathBuf, u64)>,
    pub(super) failures: Vec<KimiDiscoveryFailure>,
    pub(super) failure_count: u64,
    pub(super) witness: u64,
    pub(super) start_offset: u64,
    pub(super) reached_end: bool,
}

impl KimiDiscoveryReport {
    pub(super) fn record_failure(
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
pub(super) struct KimiMetadata {
    #[serde(default)]
    pub(super) work_dirs: Vec<KimiWorkDir>,
}

#[derive(Deserialize)]
pub(super) struct KimiWorkDir {
    pub(super) path: PathBuf,
    #[serde(default = "local_kaos")]
    pub(super) kaos: String,
}

fn local_kaos() -> String {
    "local".to_owned()
}

pub(super) fn charge_discovered_path(
    budget: &mut HostScanBudget,
    path: &Path,
) -> TranscriptIngestResult<bool> {
    let bytes = u64::try_from(path.as_os_str().as_encoded_bytes().len())
        .map_err(|_| invalid_frame())?
        .max(1);
    Ok(budget.try_charge_input(bytes))
}

pub(super) fn discovery_witness(
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
