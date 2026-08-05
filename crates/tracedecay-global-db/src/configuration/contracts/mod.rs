//! Transport-neutral configuration control-plane contract.
//!
//! Moved down from root `src/application/configuration/{types,ports}.rs`. The
//! only durable implementation of [`ports::ConfigurationControlStore`] is
//! [`super::store::GlobalDbConfigurationControlStore`], and root
//! `src/application/` is staying at the top of the stack (see
//! `tracedecay-application/SEAMS.md`) while already depending on `global_db` —
//! so the contract had to land beside its implementer or stay a cycle.
//!
//! Neither file carried a composition-root dependency: both reach only
//! `tracedecay-domain`, `serde`, `thiserror`, and `zeroize`.

pub mod ports;
pub mod types;

pub use ports::{
    ConfigurationClock, ConfigurationControlStore, ConfigurationCurrentStateV1,
    ConfigurationMutationAuthorizationPort, ConfigurationOperationFuture, CredentialWritePort,
    CurrentConfigurationMutationAuthorizationV1, ScopeResolutionPort, ScopeRevalidationEvidenceV1,
};
pub use types::{
    ActivationDriftV1, AuthorizedActor, CONFIGURATION_AUDIT_PAGE_LIMIT,
    ComponentConfigurationState, ConfigurationAuditPage, ConfigurationAuditQuery,
    ConfigurationError, ConfigurationMutationAuthority, ConfigurationMutationReceipt,
    ConfigurationPlanContext, ConfigurationRollbackRequest, CredentialWriteHandleV1,
    DirectConfigurationMutation, ResolvedSetting, SettingSummary, WriteOnlyCredentialMutation,
    configuration_layer_scope_digest,
};
