//! Compatibility facade for LSP diagnostics owned by `tracedecay-lsp`.

pub use tracedecay_lsp::{LspError, activity, adapters, broker, settings};

pub mod client {
    pub use tracedecay_lsp::client::{
        LspDocument, LspRefreshTimeouts, StdioLspClient, collect_document_diagnostics,
        collect_document_diagnostics_with_timeouts,
    };
}
