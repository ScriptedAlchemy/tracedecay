use thiserror::Error;

/// Validation failures for transport-neutral application contracts.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ApplicationContractError {
    #[error("{field} must be non-empty, trimmed, bounded, and control-character free")]
    InvalidIdentifier { field: &'static str },
    #[error("{field} must be greater than zero")]
    ZeroValue { field: &'static str },
    #[error("{field} has an invalid range")]
    InvalidRange { field: &'static str },
    #[error("{field} is inconsistent with the application contract")]
    Inconsistent { field: &'static str },
    #[error("{field} contains a duplicate value")]
    Duplicate { field: &'static str },
    #[error("domain contract rejected application input: {0}")]
    Domain(String),
    #[error("catalog contract rejected application input: {0}")]
    Catalog(String),
}

impl From<tracedecay_domain::DomainError> for ApplicationContractError {
    fn from(error: tracedecay_domain::DomainError) -> Self {
        Self::Domain(error.to_string())
    }
}

impl From<tracedecay_tool_catalog::CatalogValidationError> for ApplicationContractError {
    fn from(error: tracedecay_tool_catalog::CatalogValidationError) -> Self {
        Self::Catalog(error.to_string())
    }
}

impl From<tracedecay_tool_catalog::IdentifierError> for ApplicationContractError {
    fn from(error: tracedecay_tool_catalog::IdentifierError) -> Self {
        Self::Catalog(error.to_string())
    }
}
