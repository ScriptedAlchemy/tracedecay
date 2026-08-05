use std::path::{Component, PathBuf};

use tracedecay_domain::SnapshotFileDispositionV1;
use tracedecay_lsp::{
    AdmittedRoot, IndexedWorkspaceDocument, IndexedWorkspaceDocuments, LspRuntimeFailure,
    LspRuntimeFuture, MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
};
use tracedecay_usecases::lsp_support::LspWorkspaceDocumentIndexPort;
use url::Url;

use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;

#[derive(Clone)]
pub(super) struct PublishedCodeIndexWorkspaceDocuments {
    registry: CodeIndexSchedulerRegistryV1,
}

impl PublishedCodeIndexWorkspaceDocuments {
    pub(super) fn new(registry: CodeIndexSchedulerRegistryV1) -> Self {
        Self { registry }
    }
}

impl LspWorkspaceDocumentIndexPort for PublishedCodeIndexWorkspaceDocuments {
    fn is_mounted(&self) -> bool {
        true
    }

    fn indexed_documents(
        &self,
        root: AdmittedRoot,
        maximum_documents: usize,
    ) -> LspRuntimeFuture<Result<IndexedWorkspaceDocuments, LspRuntimeFailure>> {
        let registry = self.registry.clone();
        Box::pin(async move {
            if maximum_documents == 0 || maximum_documents > MAX_WORKSPACE_DIAGNOSTIC_RESULTS {
                return Err(LspRuntimeFailure::new(
                    "workspace-diagnostic-document-bound-invalid",
                ));
            }
            let root_url = Url::parse(root.uri())
                .ok()
                .filter(|url| {
                    url.scheme() == "file" && url.query().is_none() && url.fragment().is_none()
                })
                .ok_or_else(|| LspRuntimeFailure::new("workspace-root-uri-invalid"))?;
            let root_path = root_url
                .to_file_path()
                .map_err(|()| LspRuntimeFailure::new("workspace-root-uri-invalid"))?
                .canonicalize()
                .map_err(|_| LspRuntimeFailure::new("workspace-root-unavailable"))?;
            let latest = registry
                .latest_complete_fresh(&root_path)
                .await
                .ok_or_else(|| LspRuntimeFailure::new("workspace-code-generation-unavailable"))?;
            let generation = latest.generation();
            let snapshot = generation.snapshot();
            let mut documents = Vec::new();
            for file in snapshot
                .files
                .iter()
                .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
            {
                if documents.len() == maximum_documents {
                    return Err(LspRuntimeFailure::new(
                        "workspace-diagnostic-document-capacity",
                    ));
                }
                let relative = PathBuf::from(&file.logical_path);
                if relative.as_os_str().is_empty()
                    || relative.is_absolute()
                    || relative
                        .components()
                        .any(|component| !matches!(component, Component::Normal(_)))
                {
                    return Err(LspRuntimeFailure::new("workspace-index-document-invalid"));
                }
                let uri = Url::from_file_path(root_path.join(relative))
                    .map_err(|()| LspRuntimeFailure::new("workspace-index-document-invalid"))?
                    .to_string();
                documents.push(IndexedWorkspaceDocument {
                    uri,
                    content_digest: file.content_digest.clone(),
                });
            }
            Ok(IndexedWorkspaceDocuments {
                code_generation_id: generation.manifest().generation_id.as_str().to_owned(),
                snapshot_digest: generation.manifest().snapshot_digest.clone(),
                documents,
            })
        })
    }
}
