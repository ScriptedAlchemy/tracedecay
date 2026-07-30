//! Daemon-owned LSP 3.17 gateway.
//!
//! The store-free session-facing contracts — protocol actor, session
//! lifecycle, capability negotiation, overlays, diagnostic merge, context
//! projection, and the analyzer broker ports — live in [`tracedecay_lsp`].
//!
//! What stays daemon-owned is exactly what needs daemon authority:
//! analyzer/store composition ([`factory`]) plus Tokio and `lsp-types`
//! bindings ([`runtime_adapters`]). The small re-export set below is retained
//! only for existing root callers that have not cut over yet.

mod factory;
mod runtime_adapters;

pub use factory::DaemonLspSessionFactory;
pub use runtime_adapters::{
    BrokerDiagnosticSnapshotAuthority, DaemonSemanticProviderAdapter, LspDiagnosticDocumentPort,
    LspSemanticRequestAuthority,
};
