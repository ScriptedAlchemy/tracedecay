//! Exact one-use approval for native branch integration.

use serde::{Deserialize, Serialize};

use crate::{
    DomainError, ManifestDigest, NativeIntegrationApprovalId, NativeIntegrationDelegatedAgentId,
    NativeIntegrationMechanicalModeV1, NativeIntegrationPreviewId, NativeIntegrationPreviewV1,
    NativeIntegrationPrincipalId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationCapabilityV1 {
    NativeIntegrationApply,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationApprovalV1 {
    pub approval_id: NativeIntegrationApprovalId,
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub repository_id: RepositoryId,
    pub source_worktree_id: WorktreeId,
    pub destination_worktree_id: WorktreeId,
    pub mode: NativeIntegrationMechanicalModeV1,
    pub selection_digest: ManifestDigest,
    pub scope_digest: ManifestDigest,
    pub analysis_digest: ManifestDigest,
    pub principal_id: NativeIntegrationPrincipalId,
    pub delegated_agent_id: Option<NativeIntegrationDelegatedAgentId>,
    pub capability: NativeIntegrationCapabilityV1,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub approval_digest: ManifestDigest,
}

#[derive(Serialize)]
struct ApprovalDigestMaterial<'a> {
    domain: &'static str,
    approval_id: &'a NativeIntegrationApprovalId,
    preview_id: &'a NativeIntegrationPreviewId,
    preview_digest: &'a ManifestDigest,
    repository_id: &'a RepositoryId,
    source_worktree_id: &'a WorktreeId,
    destination_worktree_id: &'a WorktreeId,
    mode: NativeIntegrationMechanicalModeV1,
    selection_digest: &'a ManifestDigest,
    scope_digest: &'a ManifestDigest,
    analysis_digest: &'a ManifestDigest,
    principal_id: &'a NativeIntegrationPrincipalId,
    delegated_agent_id: Option<&'a NativeIntegrationDelegatedAgentId>,
    capability: NativeIntegrationCapabilityV1,
    issued_at: UtcMicros,
    expires_at: UtcMicros,
}

impl NativeIntegrationApprovalV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.approval_digest = self.compute_approval_digest()?;
        Ok(self)
    }

    pub fn compute_approval_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate_fields()?;
        canonical_sha256(&ApprovalDigestMaterial {
            domain: "tracedecay.native-integration.approval.v1",
            approval_id: &self.approval_id,
            preview_id: &self.preview_id,
            preview_digest: &self.preview_digest,
            repository_id: &self.repository_id,
            source_worktree_id: &self.source_worktree_id,
            destination_worktree_id: &self.destination_worktree_id,
            mode: self.mode,
            selection_digest: &self.selection_digest,
            scope_digest: &self.scope_digest,
            analysis_digest: &self.analysis_digest,
            principal_id: &self.principal_id,
            delegated_agent_id: self.delegated_agent_id.as_ref(),
            capability: self.capability,
            issued_at: self.issued_at,
            expires_at: self.expires_at,
        })
    }

    pub fn validate_against(
        &self,
        preview: &NativeIntegrationPreviewV1,
        observed_at: UtcMicros,
    ) -> Result<(), DomainError> {
        preview.validate()?;
        self.validate_fields()?;
        if self.approval_digest != self.compute_approval_digest()?
            || self.preview_id != preview.preview_id
            || self.preview_digest != preview.preview_digest
            || self.repository_id != preview.repository_id
            || self.source_worktree_id != preview.source_worktree_id
            || self.destination_worktree_id != preview.destination_worktree_id
            || self.mode != preview.mode
            || self.selection_digest != preview.selection_digest
            || self.issued_at < preview.created_at
            || self.expires_at > preview.expires_at
            || observed_at < self.issued_at
            || observed_at > self.expires_at
            || !preview.disposition.is_eligible()
        {
            return Err(noncanonical("native integration approval binding"));
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.approval_id.validate()?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.repository_id.validate()?;
        self.source_worktree_id.validate()?;
        self.destination_worktree_id.validate()?;
        self.selection_digest.validate()?;
        self.scope_digest.validate()?;
        self.analysis_digest.validate()?;
        self.principal_id.validate()?;
        if let Some(agent) = &self.delegated_agent_id {
            agent.validate()?;
        }
        if self.issued_at >= self.expires_at {
            return Err(noncanonical("native integration approval expiry"));
        }
        Ok(())
    }
}

fn noncanonical(field: &'static str) -> DomainError {
    DomainError::NonCanonical { field }
}
