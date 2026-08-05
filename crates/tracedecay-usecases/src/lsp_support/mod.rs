//! Store-free LSP runtime composition.

mod factory;
mod runtime_adapters;

pub use factory::{
    DaemonLspSessionFactory, FederatedLspProviderAuthority, PreparedFederatedLspProviderRoutes,
};
pub use runtime_adapters::{
    BrokerDiagnosticSnapshotAuthority, DaemonSemanticProviderAdapter, LspDiagnosticDocumentPort,
    LspSemanticRequestAuthority, LspWorkspaceDocumentIndexPort,
};
pub(crate) use runtime_adapters::{
    managed_diagnostic_authority_digest, validate_managed_diagnostic_scope,
};
