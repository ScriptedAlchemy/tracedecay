//! Registered per-project LSP authority: projection scope, documents, and snapshots.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use same_file::Handle;
use tokio::io::AsyncReadExt;
use tracedecay_domain::ContentDigest;
use tracedecay_lsp::analyzer::adapters::builtin_adapters;
use tracedecay_lsp::analyzer::client::LspDocument;
use tracedecay_lsp::{
    AdmittedRoot, CanonicalDiagnosticRefreshRequest, LspRuntimeFailure, LspRuntimeFuture,
    strict_file_uri_path,
};
use tracedecay_runtime_core::path_safety::canonical_root_identity;
use url::Url;

use super::LspFeedbackProjectionScope;
use super::diagnostic_projection::{LspFeedbackDocumentSnapshot, LspFeedbackDocumentSnapshotPort};
use super::document_paths::{adapter_for_path, open_project_file, validated_document_path};
use super::overlay_admission::admit_overlay;
use super::projection_identity::LspCodeIndexProjectionIdentityPort;
use crate::feedback::concrete::{FeedbackRuntime, ProjectFeedbackStore};
use crate::lsp_support::LspDiagnosticDocumentPort;

/// Resolves current scope through the existing admitted Git/graph owner.
pub trait LspFeedbackProjectionScopePort: Send + Sync {
    fn resolve(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<Result<LspFeedbackProjectionScope, LspRuntimeFailure>>;
}

/// Exact registered project/root authority used by production LSP sessions,
/// bound to the admitted feedback scope and code-index generation.
#[derive(Clone)]
pub struct RegisteredProjectLspAuthority {
    pub(super) feedback: Arc<FeedbackRuntime>,
    publications: ProjectFeedbackStore,
    pub(super) project_root: PathBuf,
    /// The admitted root's canonical identity, resolved once here rather than
    /// on every document request: `project_root` is already canonicalized at
    /// construction, so the only work left is the spelling normalization, and
    /// that answer cannot change for the authority's lifetime.
    pub(super) root_identity: PathBuf,
    pub(super) project_dir: Arc<Dir>,
    pub(super) root_uri: Url,
    pub(super) code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    pub(super) workspace_index: Arc<dyn crate::lsp_support::LspWorkspaceDocumentIndexPort>,
}

impl RegisteredProjectLspAuthority {
    pub fn new(
        feedback: Arc<FeedbackRuntime>,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
        workspace_index: Arc<dyn crate::lsp_support::LspWorkspaceDocumentIndexPort>,
    ) -> Result<Self, LspRuntimeFailure> {
        let project_root = feedback
            .project_root()
            .canonicalize()
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        let root_uri = Url::from_directory_path(&project_root)
            .map_err(|()| LspRuntimeFailure::new("registered-project-root-invalid"))?;
        let project_dir = Dir::open_ambient_dir(&project_root, ambient_authority())
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        let path_handle = Handle::from_path(&project_root)
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        let directory_handle = project_dir
            .try_clone()
            .map(Dir::into_std_file)
            .and_then(Handle::from_file)
            .map_err(|_| LspRuntimeFailure::new("registered-project-root-unavailable"))?;
        if path_handle != directory_handle {
            return Err(LspRuntimeFailure::new(
                "registered-project-root-unavailable",
            ));
        }
        let publications = feedback.publication_store();
        let root_identity = canonical_root_identity(&project_root);
        Ok(Self {
            feedback,
            publications,
            project_root,
            root_identity,
            project_dir: Arc::new(project_dir),
            root_uri,
            code_index,
            workspace_index,
        })
    }

    pub fn publication_store(&self) -> ProjectFeedbackStore {
        self.publications.clone()
    }

    pub(super) fn validate_root(&self, root: &AdmittedRoot) -> Result<(), LspRuntimeFailure> {
        if root.scope_digest() != Some(&self.feedback.scope().scope_digest) {
            return Err(LspRuntimeFailure::new("registered-project-root-mismatch"));
        }
        let (_, path) = strict_file_uri_path(root.uri())
            .ok_or_else(|| LspRuntimeFailure::new("registered-project-root-mismatch"))?;
        let same_root = same_file::is_same_file(path, &self.project_root).unwrap_or(false);
        same_root
            .then_some(())
            .ok_or_else(|| LspRuntimeFailure::new("registered-project-root-mismatch"))
    }

    pub(super) fn document_path(
        &self,
        document_uri: &str,
    ) -> Result<(PathBuf, String), LspRuntimeFailure> {
        let document = validated_document_path(
            &self.project_root,
            &self.root_identity,
            &self.root_uri,
            &self.project_dir,
            document_uri,
        )?;
        let relative_path = document
            .relative
            .to_str()
            .filter(|path| !path.is_empty())
            .map(|path| path.replace('\\', "/"))
            .ok_or_else(|| LspRuntimeFailure::new("document-path-invalid"))?;
        Ok((document.absolute, relative_path))
    }

    /// The URI retained evidence records for a saved document.
    ///
    /// A managed test run keys its document digests by the URI of the canonical
    /// project root joined with the project-relative path. A client may spell
    /// the same document through an OS or worktree alias of the root, so its
    /// URI is never the lookup key; the resolved scope's relative path is.
    pub(super) fn retained_document_uri(
        &self,
        scope: &LspFeedbackProjectionScope,
    ) -> Result<Option<String>, LspRuntimeFailure> {
        scope
            .document_relative_path
            .as_deref()
            .map(|relative| {
                Url::from_file_path(self.project_root.join(relative))
                    .map(|url| url.to_string())
                    .map_err(|()| LspRuntimeFailure::new("document-uri-invalid"))
            })
            .transpose()
    }

    #[hotpath::measure(label = "usecases.lsp.document.read", future = true)]
    pub(super) async fn read_disk_document(
        &self,
        relative: &Path,
    ) -> Result<String, LspRuntimeFailure> {
        let (_canonical, file) = open_project_file(&self.project_dir, relative)?;
        let mut file = tokio::fs::File::from_std(file.into_std());
        let mut text = String::new();
        file.read_to_string(&mut text)
            .await
            .map_err(|_| LspRuntimeFailure::new("document-unavailable"))?;
        Ok(text)
    }

    #[hotpath::measure(label = "usecases.lsp.scope.current", future = true)]
    pub(super) async fn current_scope(
        &self,
        document_relative_path: Option<String>,
    ) -> Result<LspFeedbackProjectionScope, LspRuntimeFailure> {
        let scope = self.feedback.scope();
        scope
            .validate()
            .map_err(|_| LspRuntimeFailure::new("registered-project-scope-invalid"))?;
        let identity = self
            .code_index
            .current_identity(self.project_root.clone(), document_relative_path.clone())
            .await?;
        let mut projection = identity.admit_commit_scope(scope)?;
        projection.document_relative_path = document_relative_path;
        Ok(projection)
    }
}

impl LspFeedbackProjectionScopePort for RegisteredProjectLspAuthority {
    fn resolve(
        &self,
        root: AdmittedRoot,
        document_uri: Option<String>,
    ) -> LspRuntimeFuture<Result<LspFeedbackProjectionScope, LspRuntimeFailure>> {
        let authority = self.clone();
        Box::pin(hotpath::future!(
            async move {
                authority.validate_root(&root)?;
                let document_relative_path = document_uri
                    .as_deref()
                    .map(|uri| authority.document_path(uri).map(|(_, relative)| relative))
                    .transpose()?;
                authority.current_scope(document_relative_path).await
            },
            label = "usecases.lsp.scope.resolve"
        ))
    }
}

impl LspDiagnosticDocumentPort for RegisteredProjectLspAuthority {
    fn load_document(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<LspDocument, LspRuntimeFailure>> {
        let authority = self.clone();
        Box::pin(hotpath::future!(
            async move {
                authority.validate_root(&request.root)?;
                let (path, relative_path) = authority.document_path(&request.document_uri)?;
                let relative = Path::new(&relative_path);
                let (language, language_id, text) = match request.overlay {
                    Some(overlay) => {
                        let overlay = admit_overlay(overlay, &request.document_uri)?;
                        let adapter = builtin_adapters()
                            .into_iter()
                            .find(|adapter| adapter.language_id == overlay.language_id)
                            .ok_or_else(|| {
                                LspRuntimeFailure::new("document-language-not-registered")
                            })?;
                        (
                            adapter.language,
                            adapter.language_id,
                            overlay.text.to_string(),
                        )
                    }
                    None => {
                        let adapter = adapter_for_path(&path).ok_or_else(|| {
                            LspRuntimeFailure::new("document-language-not-registered")
                        })?;
                        let text = authority.read_disk_document(relative).await?;
                        (adapter.language, adapter.language_id, text)
                    }
                };
                let document = LspDocument {
                    language,
                    language_id,
                    relative_path,
                    text,
                };
                let observed_content_digest = ContentDigest::of_bytes(document.text.as_bytes());
                if request
                    .expected_content_digest
                    .as_ref()
                    .is_some_and(|expected| expected != &observed_content_digest)
                {
                    return Err(LspRuntimeFailure::new("document-content-stale"));
                }
                Ok(document)
            },
            label = "usecases.lsp.document.load"
        ))
    }
}

impl crate::lsp_support::LspWorkspaceDocumentIndexPort for RegisteredProjectLspAuthority {
    fn is_mounted(&self) -> bool {
        self.workspace_index.is_mounted()
    }

    fn indexed_documents(
        &self,
        root: AdmittedRoot,
        maximum_documents: usize,
    ) -> LspRuntimeFuture<Result<tracedecay_lsp::IndexedWorkspaceDocuments, LspRuntimeFailure>>
    {
        if let Err(error) = self.validate_root(&root) {
            return Box::pin(async move { Err(error) });
        }
        self.workspace_index
            .indexed_documents(root, maximum_documents)
    }
}

impl LspFeedbackDocumentSnapshotPort for RegisteredProjectLspAuthority {
    fn snapshot(
        &self,
        root: AdmittedRoot,
        document_uri: String,
    ) -> LspRuntimeFuture<Result<LspFeedbackDocumentSnapshot, LspRuntimeFailure>> {
        let authority = self.clone();
        Box::pin(hotpath::future!(
            async move {
                authority.validate_root(&root)?;
                let (_, relative_path) = authority.document_path(&document_uri)?;
                let text = authority
                    .read_disk_document(Path::new(&relative_path))
                    .await?;
                Ok(LspFeedbackDocumentSnapshot { text })
            },
            label = "usecases.lsp.document.snapshot"
        ))
    }
}
