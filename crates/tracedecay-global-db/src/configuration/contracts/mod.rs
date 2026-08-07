//! Store-facing configuration control-plane contract.
//!
//! The concrete mutation/query ports remain beside their sole durable
//! implementation. Public request/result DTOs are owned by
//! `tracedecay-application::configuration` and re-exported from `types` only
//! so the store implementation consumes that same wire authority.

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
    ConfigurationPlanContext, ConfigurationRollbackRequest, ConfigurationSettlementAuthorityV1,
    CredentialWriteHandleV1,
    DirectConfigurationMutation, ResolvedSetting, SettingSummary, WriteOnlyCredentialMutation,
    configuration_layer_scope_digest,
};
