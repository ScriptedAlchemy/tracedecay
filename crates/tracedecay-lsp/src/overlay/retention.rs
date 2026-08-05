use crate::gateway::AdmittedRoot;

use super::{OverlayDiagnosticDebouncer, OverlaySnapshot, OverlayStore, snapshot};

impl OverlayStore {
    pub(crate) fn retain_documents(&mut self, mut retain: impl FnMut(&str) -> bool) {
        self.documents.retain(|uri, _| retain(uri));
    }

    pub(crate) fn snapshots_for_root(&self, root: &AdmittedRoot) -> Vec<OverlaySnapshot> {
        self.documents
            .iter()
            .filter(|(uri, _)| root.contains_document(uri))
            .map(|(uri, document)| snapshot(uri, document))
            .collect()
    }
}

impl OverlayDiagnosticDebouncer {
    pub(crate) fn retain_documents(&mut self, mut retain: impl FnMut(&str) -> bool) {
        self.pending.retain(|uri, _| retain(uri));
    }
}
