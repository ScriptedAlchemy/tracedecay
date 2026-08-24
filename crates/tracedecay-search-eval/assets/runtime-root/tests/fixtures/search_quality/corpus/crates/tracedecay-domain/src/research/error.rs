use thiserror::Error;

/// Validation failures for pure research-domain values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("{field} is not canonical")]
    NonCanonical { field: &'static str },
    #[error("{field} contains a duplicate identity")]
    DuplicateId { field: &'static str },
    #[error("{field} references an unknown identity")]
    UnknownReference { field: &'static str },
    #[error("{field} is not pinned to the required snapshot")]
    SnapshotMismatch { field: &'static str },
    #[error("confidence must be finite and within [0.0, 1.0]")]
    InvalidConfidence,
    #[error("{field} violates the structural bounds for sanitized text")]
    UnsafeText { field: &'static str },
    #[error("an Activity primary subject cannot also have related_activity")]
    ActivityFacetOnActivitySubject,
    #[error("a manifest cannot supersede itself")]
    SelfSupersession,
    #[error("direct authorship requires provider-linked activity evidence")]
    AuthorshipWithoutProviderLinkage,
    #[error("time interval start must not be after its end")]
    InvalidTimeInterval,
    #[error("redacted and rejected counts cannot exceed scanned count")]
    InvalidRedactionCounts,
    #[error(
        "evidence declared by a user or provider, or directly observed, requires confidence 1.0"
    )]
    NonCertainDeclaration,
    #[error("manifest digest does not match its canonical domain-separated payload")]
    DigestMismatch,
    #[error("canonical serialization failed: {0}")]
    CanonicalSerialization(String),
}
