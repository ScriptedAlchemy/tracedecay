use tracedecay_domain::{ContentDigest, ManifestDigest};

use crate::gateway::AdmittedRoot;
use crate::overlay::OverlaySnapshot;
use crate::provider::{DiagnosticRefreshIdentity, GenerationDiagnostics};
use crate::session::AuthorizedLspWorkspace;

pub const MAX_WORKSPACE_DIAGNOSTIC_FANOUT: usize = 4;
pub const MAX_WORKSPACE_DIAGNOSTIC_RESULTS: usize = 128;
pub const MAX_WORKSPACE_DIAGNOSTIC_BYTES: usize = 1024 * 1024;
pub const MAX_WORKSPACE_DIAGNOSTIC_RESULT_ID_BYTES: usize = 1024;

#[derive(Clone, Debug)]
pub struct CanonicalWorkspaceDiagnosticRefreshRequest {
    pub workspace: AuthorizedLspWorkspace,
    pub root: AdmittedRoot,
    pub overlays: Vec<OverlaySnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedWorkspaceDocument {
    pub uri: String,
    pub content_digest: ContentDigest,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedWorkspaceDocuments {
    pub code_generation_id: String,
    pub snapshot_digest: ManifestDigest,
    pub documents: Vec<IndexedWorkspaceDocument>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceDocumentDiagnostics {
    pub uri: String,
    pub version: Option<i64>,
    pub content_digest: ContentDigest,
    pub diagnostics: GenerationDiagnostics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceGenerationDiagnostics {
    pub code_generation_id: String,
    pub snapshot_digest: ManifestDigest,
    pub documents: Vec<WorkspaceDocumentDiagnostics>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceDiagnosticSnapshotOutcome {
    Ready {
        diagnostics: WorkspaceGenerationDiagnostics,
        completed_operation_id: Option<String>,
    },
    Refreshing(DiagnosticRefreshIdentity),
    Partial {
        code_generation_id: Option<String>,
        coverage: String,
    },
    Failed {
        code_generation_id: Option<String>,
        failure_class: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceDiagnosticRootFailure {
    pub root_uri: String,
    pub scope_digest: Option<ManifestDigest>,
    pub failure_class: String,
}
