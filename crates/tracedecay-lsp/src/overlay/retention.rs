use crate::gateway::AdmittedRoot;
use crate::session::AuthorizedLspWorkspace;

use super::{OverlayDiagnosticDebouncer, OverlaySnapshot, OverlayStore, snapshot};

impl OverlayStore {
    pub(crate) fn retain_documents(&mut self, mut retain: impl FnMut(&str) -> bool) {
        self.documents.retain(|uri, _| retain(uri));
    }

    pub(crate) fn snapshots_for_root(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
    ) -> Vec<OverlaySnapshot> {
        self.documents
            .iter()
            .filter(|(uri, _)| {
                workspace
                    .resolve_document(uri)
                    .is_ok_and(|owner| owner == root)
            })
            .map(|(uri, document)| snapshot(uri, document))
            .collect()
    }
}

impl OverlayDiagnosticDebouncer {
    pub(crate) fn retain_documents(&mut self, mut retain: impl FnMut(&str) -> bool) {
        self.pending.retain(|uri, _| retain(uri));
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::ManifestDigest;

    use super::*;
    use crate::session::AuthorizedLspWorkspace;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    #[test]
    fn nested_workspace_overlay_belongs_only_to_the_deepest_root() {
        let parent = AdmittedRoot::authorized("file:///workspace", digest('a'));
        let nested = AdmittedRoot::authorized("file:///workspace/nested", digest('b'));
        let workspace =
            AuthorizedLspWorkspace::new(Some(digest('c')), vec![parent.clone(), nested.clone()])
                .unwrap();
        let mut overlays = OverlayStore::default();
        overlays
            .open(
                &nested,
                "file:///workspace/nested/src/lib.rs",
                "rust",
                1,
                "fn nested() {}",
            )
            .unwrap();

        assert!(overlays.snapshots_for_root(&workspace, &parent).is_empty());
        assert_eq!(overlays.snapshots_for_root(&workspace, &nested).len(), 1);
    }
}
