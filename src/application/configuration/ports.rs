//! Narrow ports consumed by configuration operations.

use tracedecay_domain::configuration::{
    ConfigurationSnapshotV1, CredentialReferenceMetadataV1, ProtectedApplyRequest, ProtectedChange,
    ProtectedChangePlan,
};
use tracedecay_domain::{AccessPolicyDigest, ManifestDigest, UtcMicros};

use super::types::{
    AuthorizedActor, ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationError, ConfigurationMutationReceipt, ConfigurationRollbackRequest,
    DirectConfigurationMutation, WriteOnlyCredentialMutation,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigurationCurrentStateV1 {
    pub revision_id: tracedecay_domain::configuration::ConfigurationRevisionId,
    pub snapshot: ConfigurationSnapshotV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRevalidationEvidenceV1 {
    pub resolved_scope_digest: ManifestDigest,
    pub membership_digest: Option<ManifestDigest>,
    pub authorization_policy_digest: AccessPolicyDigest,
    pub policy_epoch: u64,
}

/// Plan 16-backed authority resolver. This port owns re-resolution; adapters
/// and the configuration layer do not infer project authority from a path,
/// CWD, source locator, collection label, or host profile.
pub trait ScopeResolutionPort {
    fn resolve_protected_change(
        &self,
        actor: &AuthorizedActor,
        change: &ProtectedChange,
    ) -> Result<ScopeRevalidationEvidenceV1, ConfigurationError>;

    fn revalidate_plan(
        &self,
        actor: &AuthorizedActor,
        plan: &ProtectedChangePlan,
    ) -> Result<ScopeRevalidationEvidenceV1, ConfigurationError>;
}

pub trait ConfigurationClock {
    fn now(&self) -> UtcMicros;
}

/// Transactional persistence boundary. Each `commit_*` method must atomically
/// commit the new revision, receipt, audit event, and plan terminal state.
pub trait ConfigurationControlStore {
    fn current(&self) -> Result<ConfigurationCurrentStateV1, ConfigurationError>;

    fn save_plan(&self, plan: &ProtectedChangePlan) -> Result<(), ConfigurationError>;

    fn load_plan(
        &self,
        plan_id: &tracedecay_domain::configuration::ChangePlanId,
    ) -> Result<Option<ProtectedChangePlan>, ConfigurationError>;

    fn commit_direct(
        &self,
        actor: &AuthorizedActor,
        mutation: &DirectConfigurationMutation,
        expected_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;

    fn commit_protected(
        &self,
        actor: &AuthorizedActor,
        request: &ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;

    fn dry_run_rollback(
        &self,
        actor: &AuthorizedActor,
        rollback: &ConfigurationRollbackRequest,
    ) -> Result<ProtectedChangePlan, ConfigurationError>;

    fn apply_rollback(
        &self,
        actor: &AuthorizedActor,
        request: &ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;

    fn audit(
        &self,
        actor: &AuthorizedActor,
        query: &ConfigurationAuditQuery,
    ) -> Result<ConfigurationAuditPage, ConfigurationError>;

    fn observed_state(
        &self,
        actor: &AuthorizedActor,
    ) -> Result<Vec<ComponentConfigurationState>, ConfigurationError>;
}

/// Secure credential sink boundary. The material is resolved by the secure
/// adapter using an opaque handle and never crosses into the application DTO.
pub trait CredentialWritePort {
    fn write_reference(
        &self,
        actor: &AuthorizedActor,
        write: &WriteOnlyCredentialMutation,
        expected_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    ) -> Result<CredentialReferenceMetadataV1, ConfigurationError>;
}
