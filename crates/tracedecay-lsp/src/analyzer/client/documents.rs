use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tracedecay_runtime_core::path_safety::canonicalize_existing_prefix;

use super::{DiagnosticDocumentCoverage, LspDocument};
use crate::LspSemanticDocumentSnapshot;
use crate::analyzer::broker::{CodeDiagnostic, DiagnosticEvidenceUnavailable};

pub(crate) struct AnalyzerDocuments {
    state: Mutex<DocumentState>,
}

pub(super) struct DocumentState {
    pub(super) attempt: Option<u32>,
    pub(super) documents: BTreeMap<PathBuf, SynchronizedDocument>,
}

pub(super) struct SynchronizedDocument {
    pub(super) uri: String,
    pub(super) wire_version: i32,
    pub(super) text: String,
    pub(super) snapshot: Option<LspSemanticDocumentSnapshot>,
    pub(super) accepted: Option<Arc<AcceptedDiagnosticPublication>>,
    pub(super) epoch: u64,
    pub(super) eligibility: PublicationEligibility,
}

pub(super) enum PublicationEligibility {
    Unverified,
    Refreshing(Arc<RefreshClaimIdentity>),
    Committed,
}

pub(crate) struct AcceptedDiagnosticPublication {
    pub(crate) diagnostics: Vec<CodeDiagnostic>,
    pub(crate) coverage: DiagnosticDocumentCoverage,
}

pub(super) struct RefreshClaimIdentity {
    path: PathBuf,
    epoch: u64,
}

pub(crate) struct DocumentRefreshClaim {
    owner: Arc<AnalyzerDocuments>,
    identity: Arc<RefreshClaimIdentity>,
}

pub(crate) struct CollectedDocumentPublication {
    claim: DocumentRefreshClaim,
    accepted: Arc<AcceptedDiagnosticPublication>,
    document: LspDocument,
    uri: String,
    wire_version: i32,
    attempt: u32,
}

impl AnalyzerDocuments {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(DocumentState {
                attempt: None,
                documents: BTreeMap::new(),
            }),
        })
    }

    pub(super) fn lock(
        &self,
    ) -> Result<MutexGuard<'_, DocumentState>, DiagnosticEvidenceUnavailable> {
        self.state
            .lock()
            .map_err(|_| DiagnosticEvidenceUnavailable::MetadataUnavailable)
    }

    pub(crate) fn activate(&self, attempt: u32) -> Result<(), DiagnosticEvidenceUnavailable> {
        let mut state = self.lock()?;
        state.documents.clear();
        state.attempt = Some(attempt);
        Ok(())
    }

    pub(crate) fn retire(&self, attempt: Option<u32>) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if attempt.is_none() || state.attempt == attempt {
            state.attempt = None;
            state.documents.clear();
        }
    }

    pub(crate) fn invalidate(
        &self,
        paths: Option<&[PathBuf]>,
        epoch: u64,
    ) -> Result<(), DiagnosticEvidenceUnavailable> {
        let mut state = self.lock()?;
        let invalidate = |document: &mut SynchronizedDocument| {
            document.epoch = epoch;
            document.eligibility = PublicationEligibility::Unverified;
        };
        match paths {
            Some(paths) => {
                for path in paths {
                    if let Some(document) = state.documents.get_mut(path) {
                        invalidate(document);
                    }
                }
            }
            None => state.documents.values_mut().for_each(invalidate),
        }
        Ok(())
    }

    pub(crate) fn claim(
        self: &Arc<Self>,
        path: PathBuf,
        epoch: u64,
    ) -> Result<DocumentRefreshClaim, DiagnosticEvidenceUnavailable> {
        let claim = DocumentRefreshClaim {
            owner: Arc::clone(self),
            identity: Arc::new(RefreshClaimIdentity { path, epoch }),
        };
        claim.bind()?;
        Ok(claim)
    }

    pub(crate) fn read_committed<T>(
        &self,
        path: &Path,
        uri: &str,
        document: &LspDocument,
        read: impl FnOnce(&DiagnosticDocumentCoverage) -> T,
    ) -> Result<T, DiagnosticEvidenceUnavailable> {
        let state = self.lock()?;
        if state.attempt.is_none() {
            return Err(DiagnosticEvidenceUnavailable::IncarnationRetired);
        }
        let current = state
            .documents
            .get(path)
            .ok_or(DiagnosticEvidenceUnavailable::SourceUnavailable)?;
        validate_document(current, uri, document)?;
        if !matches!(current.eligibility, PublicationEligibility::Committed) {
            return Err(DiagnosticEvidenceUnavailable::PublicationUncommitted);
        }
        let accepted = current
            .accepted
            .as_ref()
            .ok_or(DiagnosticEvidenceUnavailable::PublicationUncommitted)?;
        Ok(read(&accepted.coverage))
    }
}

impl DocumentRefreshClaim {
    pub(crate) fn path(&self) -> &Path {
        &self.identity.path
    }

    pub(crate) fn is_current(&self) -> Result<bool, DiagnosticEvidenceUnavailable> {
        let state = self.owner.lock()?;
        Ok(state.documents.get(self.path()).is_none_or(|document| {
            document.epoch < self.identity.epoch
                || matches!(&document.eligibility, PublicationEligibility::Refreshing(identity) if Arc::ptr_eq(identity, &self.identity))
        }))
    }

    pub(crate) fn bind(&self) -> Result<(), DiagnosticEvidenceUnavailable> {
        let mut state = self.owner.lock()?;
        if let Some(document) = state.documents.get_mut(self.path()) {
            if document.epoch > self.identity.epoch {
                return Err(DiagnosticEvidenceUnavailable::PublicationUncommitted);
            }
            document.epoch = self.identity.epoch;
            document.eligibility = PublicationEligibility::Refreshing(Arc::clone(&self.identity));
        }
        Ok(())
    }

    pub(crate) fn collected(
        self,
        accepted: Arc<AcceptedDiagnosticPublication>,
        document: LspDocument,
        uri: String,
        wire_version: i32,
        attempt: u32,
    ) -> CollectedDocumentPublication {
        CollectedDocumentPublication {
            claim: self,
            accepted,
            document,
            uri,
            wire_version,
            attempt,
        }
    }
}

impl Drop for DocumentRefreshClaim {
    fn drop(&mut self) {
        let mut state = match self.owner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(document) = state.documents.get_mut(self.path())
            && matches!(&document.eligibility, PublicationEligibility::Refreshing(identity) if Arc::ptr_eq(identity, &self.identity))
        {
            document.eligibility = PublicationEligibility::Unverified;
        }
    }
}

impl CollectedDocumentPublication {
    pub(crate) fn relative_path(&self) -> &str {
        &self.document.relative_path
    }

    pub(crate) fn commit(
        &self,
        install: impl FnOnce(&AcceptedDiagnosticPublication),
    ) -> Result<(), DiagnosticEvidenceUnavailable> {
        let mut state = self.claim.owner.lock()?;
        if state.attempt != Some(self.attempt) {
            return Err(DiagnosticEvidenceUnavailable::IncarnationRetired);
        }
        let current = state
            .documents
            .get_mut(self.claim.path())
            .ok_or(DiagnosticEvidenceUnavailable::SourceUnavailable)?;
        validate_document(current, &self.uri, &self.document)?;
        if current.wire_version != self.wire_version
            || !matches!(&current.eligibility, PublicationEligibility::Refreshing(identity) if Arc::ptr_eq(identity, &self.claim.identity))
            || !current
                .accepted
                .as_ref()
                .is_some_and(|accepted| Arc::ptr_eq(accepted, &self.accepted))
        {
            return Err(DiagnosticEvidenceUnavailable::PublicationUncommitted);
        }
        install(&self.accepted);
        current.eligibility = PublicationEligibility::Committed;
        Ok(())
    }
}

fn validate_document(
    current: &SynchronizedDocument,
    uri: &str,
    document: &LspDocument,
) -> Result<(), DiagnosticEvidenceUnavailable> {
    if current.uri != uri || current.text != document.text {
        return Err(DiagnosticEvidenceUnavailable::ContentChanged);
    }
    if current.snapshot != document.snapshot
        || current
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.lifetime.is_open())
    {
        return Err(DiagnosticEvidenceUnavailable::OverlayStale);
    }
    Ok(())
}

pub(crate) fn document_identity(path: &Path, uri: &str) -> Option<PathBuf> {
    let (_, uri_path) = crate::gateway::strict_file_uri_path(uri)?;
    let path = canonicalize_existing_prefix(path)?;
    let uri_path = canonicalize_existing_prefix(&uri_path)?;
    (path == uri_path).then_some(path)
}
