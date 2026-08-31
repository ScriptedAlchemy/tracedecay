//! Store-free LSP runtime composition.

use tracedecay_domain::errors::TraceDecayError;
use tracedecay_lsp::analyzer::AnalyzerRuntimeError;

mod factory;
mod runtime_adapters;

/// Kernel config-error mapping for analyzer failures.
///
/// Lives here — not in `tracedecay-runtime-core` — so the kernel path does not
/// depend on the LSP crate or its lite grammars. A `From` impl is illegal here
/// (orphan rule: neither type is local).
pub fn analyzer_runtime_config_error(error: AnalyzerRuntimeError) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.message().to_string(),
    }
}

pub use factory::{DaemonLspSessionFactory, UpstreamCapabilityInitializationAuthority};
pub use runtime_adapters::{
    BrokerDiagnosticSnapshotAuthority, DaemonSemanticProviderAdapter, LspDiagnosticDocumentPort,
    LspSemanticRequestAuthority, LspWorkspaceDocumentIndexPort,
};
pub(crate) use runtime_adapters::{
    managed_diagnostic_authority_digest, validate_managed_diagnostic_scope,
};
