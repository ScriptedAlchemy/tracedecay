use tracedecay_domain::{CodeGenerationId, ContentDigest, ManifestDigest};

use crate::diagnostics::GatewayDiagnostic;
use crate::gateway::{AdmittedRoot, LspRuntimeFailure, LspRuntimeFuture};
use crate::provider::GenerationDiagnostics;
use crate::workspace_diagnostics::{
    CanonicalWorkspaceDiagnosticRefreshRequest, WorkspaceGenerationDiagnostics,
};

use super::OverlaySnapshot;

#[derive(Clone, Debug)]
pub struct CanonicalDiagnosticRefreshRequest {
    pub root: AdmittedRoot,
    pub document_uri: String,
    pub overlay: Option<OverlaySnapshot>,
    pub source_generation: Option<u64>,
    pub expected_content_digest: Option<ContentDigest>,
    pub expected_code_generation_id: Option<CodeGenerationId>,
    pub expected_snapshot_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedDiagnosticSnapshot {
    pub generation: u64,
    pub code_generation_id: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    pub authority_digest: ManifestDigest,
    pub diagnostics: Vec<GatewayDiagnostic>,
}

pub trait ManagedDiagnosticSnapshotPort: Send + Sync {
    fn snapshot(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>>;
}

pub trait CanonicalDiagnosticSnapshotAuthority: Send + Sync {
    fn refresh(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>>;

    fn supports_workspace_diagnostics(&self) -> bool {
        false
    }

    fn refresh_workspace(
        &self,
        _request: CanonicalWorkspaceDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<WorkspaceGenerationDiagnostics, LspRuntimeFailure>> {
        Box::pin(async { Err(LspRuntimeFailure::new("workspace-diagnostics-unsupported")) })
    }
}
