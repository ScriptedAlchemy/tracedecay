//! Daemon façade for the store-free authenticated LSP session registry.

pub use tracedecay_lsp::{
    AuthorizedLspSession, DaemonLspSessionEndpoint, LSP_SESSION_TTL_MS, LspEndpointError,
    LspSessionAccess, LspSessionAdmissionPort, LspSessionCredential, LspSessionId,
    LspSessionOpenRequest, LspSessionRegistry, MAX_LSP_SESSIONS,
};
