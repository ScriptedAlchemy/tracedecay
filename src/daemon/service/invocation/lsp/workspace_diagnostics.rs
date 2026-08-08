use std::path::{Component, PathBuf};

use tracedecay_application::ResolvedScope;
use tracedecay_domain::SnapshotFileDispositionV1;
use tracedecay_lsp::{
    AdmittedRoot, IndexedWorkspaceDocument, IndexedWorkspaceDocuments, LspRuntimeFailure,
    LspRuntimeFuture, MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
};
use url::Url;

use crate::daemon::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use tracedecay_usecases::lsp_runtime::LspWorkspaceDocumentIndexPort;

#[derive(Clone)]
pub(in crate::daemon::service::invocation) struct PublishedCodeIndexWorkspaceDocuments {
    registry: CodeIndexSchedulerRegistryV1,
    scope: ResolvedScope,
    project_root: Option<PathBuf>,
}

impl PublishedCodeIndexWorkspaceDocuments {
    pub(in crate::daemon::service::invocation) fn new(
        registry: CodeIndexSchedulerRegistryV1,
        scope: ResolvedScope,
        project_root: PathBuf,
    ) -> Self {
        let project_root = project_root.canonicalize().ok();
        Self {
            registry,
            scope,
            project_root,
        }
    }
}

impl LspWorkspaceDocumentIndexPort for PublishedCodeIndexWorkspaceDocuments {
    fn is_mounted(&self) -> bool {
        self.project_root.as_ref().is_some_and(|project_root| {
            self.registry
                .has_current_ready_decoded_for_root_scope(project_root, &self.scope)
        })
    }

    fn indexed_documents(
        &self,
        root: AdmittedRoot,
        maximum_documents: usize,
    ) -> LspRuntimeFuture<Result<IndexedWorkspaceDocuments, LspRuntimeFailure>> {
        let registry = self.registry.clone();
        let scope = self.scope.clone();
        let project_root = self.project_root.clone();
        Box::pin(async move {
            let Some(project_root) = project_root else {
                return Err(LspRuntimeFailure::new(
                    "workspace-code-generation-unavailable",
                ));
            };
            if root.scope_digest() != Some(&scope.scope_digest) {
                return Err(LspRuntimeFailure::new("workspace-root-scope-mismatch"));
            }
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
            if project_root != root_path {
                return Err(LspRuntimeFailure::new("workspace-root-scope-mismatch"));
            }
            let latest = registry
                .latest_complete_ready_decoded_for_root_scope(&root_path, &scope)
                .await
                .ok_or_else(|| LspRuntimeFailure::new("workspace-code-generation-warming"))?;
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
