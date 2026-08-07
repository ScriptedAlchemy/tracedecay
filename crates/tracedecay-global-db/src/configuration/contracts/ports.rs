//! Narrow ports consumed by configuration operations.

use std::future::Future;
use std::pin::Pin;

use tracedecay_domain::configuration::{
    ConfigurationMutationEffectV1, ConfigurationMutationGrantReceiptV1,
    ConfigurationMutationOperationV1, ConfigurationMutationSinkV1, ConfigurationSnapshotV1,
    CredentialReferenceMetadataV1, ProtectedApplyRequest, ProtectedChange, ProtectedChangePlan,
};
use tracedecay_domain::{AccessPolicyDigest, ManifestDigest, UtcMicros};

use super::types::{
    AuthorizedActor, ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationError, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    ConfigurationRollbackRequest, DirectConfigurationMutation, WriteOnlyCredentialMutation,
};

/// Async result used by configuration control-plane ports. Configuration
/// persistence is always owned by the daemon's authoritative database lane;
/// application orchestration must not block an executor to impersonate a
/// synchronous connection owner.
pub type ConfigurationOperationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ConfigurationError>> + Send + 'a>>;

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
/// CWD, source locator, collection label, or host profile. Resolution is
/// asynchronous so a durable authority implementation never has to block a
/// Tokio worker to satisfy this application boundary.
pub trait ScopeResolutionPort: Sync {
    fn resolve_protected_change<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        change: &'a ProtectedChange,
    ) -> ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1>;

    fn revalidate_plan<'a>(
        &'a self,
        actor: &'a AuthorizedActor,
        plan: &'a ProtectedChangePlan,
    ) -> ConfigurationOperationFuture<'a, ScopeRevalidationEvidenceV1>;
}

pub trait ConfigurationClock: Sync {
    fn now(&self) -> UtcMicros;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentConfigurationMutationAuthorizationV1 {
    pub grant_revision: u64,
    pub grant_digest: ManifestDigest,
    pub scope_digest: ManifestDigest,
    pub policy_epoch: u64,
    pub policy_digest: AccessPolicyDigest,
}

/// Current policy/grant recheck. Implementations consume the immutable policy
/// decision/grant state; the configuration layer cannot mint or refresh a
/// receipt and cannot infer authority from transport origin. The future keeps
/// database-backed rechecks on the owning async lane.
pub trait ConfigurationMutationAuthorizationPort: Sync {
    fn recheck<'a>(
        &'a self,
        receipt: &'a ConfigurationMutationGrantReceiptV1,
        operation: ConfigurationMutationOperationV1,
        expected_revision: &'a tracedecay_domain::configuration::ConfigurationRevisionId,
        sink: ConfigurationMutationSinkV1,
        effect: ConfigurationMutationEffectV1,
        now: UtcMicros,
    ) -> ConfigurationOperationFuture<'a, CurrentConfigurationMutationAuthorizationV1>;
}

/// Transactional persistence boundary. Each `commit_*` method must atomically
/// commit the new revision, receipt, audit event, and plan terminal state.
pub trait ConfigurationControlStore: Sync {
    fn current(&self) -> ConfigurationOperationFuture<'_, ConfigurationCurrentStateV1>;

    fn save_plan(
        &self,
        plan: &ProtectedChangePlan,
        operation: &ProtectedChange,
    ) -> ConfigurationOperationFuture<'_, ()>;

    fn load_plan(
        &self,
        plan_id: &tracedecay_domain::configuration::ChangePlanId,
    ) -> ConfigurationOperationFuture<'_, Option<ProtectedChangePlan>>;

    /// Returns the durable receipt for an exact protected/rollback apply
    /// replay before any ambient plan or configuration state is consulted.
    fn replay_apply(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &ProtectedApplyRequest,
        operation: ConfigurationMutationOperationV1,
    ) -> ConfigurationOperationFuture<'_, Option<ConfigurationMutationReceipt>>;

    fn commit_direct(
        &self,
        authority: &ConfigurationMutationAuthority,
        mutation: &DirectConfigurationMutation,
        expected_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt>;

    fn commit_protected(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt>;

    fn dry_run_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        rollback: &ConfigurationRollbackRequest,
        now: UtcMicros,
    ) -> ConfigurationOperationFuture<'_, ProtectedChangePlan>;

    fn apply_rollback(
        &self,
        authority: &ConfigurationMutationAuthority,
        request: &ProtectedApplyRequest,
        plan: &ProtectedChangePlan,
        evidence: &ScopeRevalidationEvidenceV1,
    ) -> ConfigurationOperationFuture<'_, ConfigurationMutationReceipt>;

    fn audit(
        &self,
        actor: &AuthorizedActor,
        query: &ConfigurationAuditQuery,
    ) -> ConfigurationOperationFuture<'_, ConfigurationAuditPage>;

    fn observed_state(
        &self,
        actor: &AuthorizedActor,
    ) -> ConfigurationOperationFuture<'_, Vec<ComponentConfigurationState>>;
}

/// Secure credential sink boundary. The material is resolved by the secure
/// adapter using an opaque handle and never crosses into the application DTO.
pub trait CredentialWritePort: Sync {
    fn write_reference(
        &self,
        authority: &ConfigurationMutationAuthority,
        write: &WriteOnlyCredentialMutation,
        expected_revision: &tracedecay_domain::configuration::ConfigurationRevisionId,
    ) -> ConfigurationOperationFuture<'_, CredentialReferenceMetadataV1>;
}
