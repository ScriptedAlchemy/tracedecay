use super::{OverlayDiagnosticDebouncer, OverlayStore};

impl OverlayStore {
    pub(crate) fn retain_documents(&mut self, mut retain: impl FnMut(&str) -> bool) {
        self.documents.retain(|uri, _| retain(uri));
    }
}

impl OverlayDiagnosticDebouncer {
    pub(crate) fn retain_documents(&mut self, mut retain: impl FnMut(&str) -> bool) {
        self.pending.retain(|uri, _| retain(uri));
    }
}
