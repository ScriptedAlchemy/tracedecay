use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{
    FileDiscoveryReport, TranscriptIngestResult, canonical_framed_sha256,
};

use super::{MAX_DISCOVERY_FAILURE_EVIDENCE, invalid_frame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KimiDiscoveryFailureKind {
    DirectoryUnavailable,
    DirectoryEntryUnavailable,
    EntryTypeUnavailable,
    SessionMetadataUnavailable,
    InvalidSessionMetadata,
    InvalidAgentPartition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KimiDiscoveryFailure {
    pub(super) kind: KimiDiscoveryFailureKind,
    pub(super) source_digest: String,
    pub(super) error_kind: io::ErrorKind,
}

pub(super) struct KimiDiscoveryReport {
    pub(super) files: FileDiscoveryReport,
    pub(super) failures: Vec<KimiDiscoveryFailure>,
    pub(super) failure_count: u64,
    /// Whether the sweep observed every candidate it was asked to observe.
    /// A partial sweep must not advance the durable discovery frontier.
    pub(super) scan_complete: bool,
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
        self.scan_complete = false;
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
pub(super) struct KimiSessionState {
    #[serde(default)]
    pub(super) id: Option<String>,
    #[serde(default)]
    pub(super) cwd: Option<PathBuf>,
    #[serde(default, rename = "workDir")]
    pub(super) work_dir: Option<PathBuf>,
    #[serde(default)]
    pub(super) agents: BTreeMap<String, KimiAgentState>,
}

#[derive(Deserialize)]
pub(super) struct KimiAgentState {
    #[serde(rename = "type")]
    pub(super) kind: String,
    #[serde(default)]
    pub(super) homedir: Option<PathBuf>,
}

impl KimiSessionState {
    pub(super) fn working_directory(&self) -> Option<&Path> {
        match (&self.cwd, &self.work_dir) {
            (Some(cwd), Some(work_dir)) if cwd == work_dir => Some(cwd),
            (Some(cwd), None) => Some(cwd),
            (None, Some(work_dir)) => Some(work_dir),
            (Some(_), Some(_)) | (None, None) => None,
        }
    }

    pub(super) fn session_id(&self, session_dir: &Path) -> Option<String> {
        let directory_id = session_dir.file_name()?.to_str()?;
        match self.id.as_deref() {
            Some(id) if id == directory_id => Some(id.to_owned()),
            Some(_) => None,
            None if !self.agents.is_empty()
                && self.agents.iter().all(|(agent_id, agent)| {
                    agent.homedir.as_deref()
                        == Some(session_dir.join("agents").join(agent_id).as_path())
                }) =>
            {
                Some(directory_id.to_owned())
            }
            None => None,
        }
    }
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
