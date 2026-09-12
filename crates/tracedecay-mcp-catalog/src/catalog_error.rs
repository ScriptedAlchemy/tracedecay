use thiserror::Error;

/// Typed failure assembling or validating the portable MCP tool catalog.
#[derive(Debug, Error)]
pub enum McpCatalogError {
    #[error("MCP dispatch catalog is invalid: {0}")]
    Catalog(#[from] tracedecay_tool_catalog::McpDispatchCatalogError),
    #[error("MCP dispatch metadata is invalid: {0}")]
    CatalogValidation(#[from] tracedecay_tool_catalog::CatalogValidationError),
    #[error("MCP catalog initialization failed: {0}")]
    Initialization(String),
    #[error("advertised MCP tool '{0}' has no dispatch contract")]
    MissingContract(String),
}
