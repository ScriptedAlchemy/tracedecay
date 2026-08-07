//! Occurrence identity, source spans, and revision/digest primitives for the
//! query code-intelligence model (Plan 25, "Identity and lineage" and
//! "Code-search chunk and projection contract").
//!
//! Generation-local occurrence identity is exact. Logical identity remains
//! stable only while its declared repository, language, qualified-structure,
//! and source-evidence tuple is unchanged. Extractor enumeration order and
//! mutable line numbers never affect identity.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical_text::validated_string_newtype;
use crate::research::DomainError;
use crate::research::id::digest_id;

/// Whether a canonical repository-relative path is exactly the requested
/// scope or one of its descendants.
pub fn repository_path_matches_scope(path: &str, scope_prefix: Option<&str>) -> bool {
    scope_prefix.is_none_or(|prefix| {
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

/// Reject code identities that are empty, untrimmed, over 512 bytes, or carry
/// control characters.
use crate::canonical_text::validate_canonical_identity as validate_code_identity;

validated_string_newtype!(
    schema,
    DomainError,
    validate_code_identity;
    CodeGenerationId,
    FileOccurrenceId,
    SymbolOccurrenceId,
    CodeSearchChunkId,
);

validated_string_newtype!(
    plain,
    DomainError,
    validate_code_identity;
    LanguageId,
    LanguageDescriptorRevision,
    GrammarRevision,
    ExtractorRevision,
    ChunkerRevision,
    SanitizerRevision,
    QueryNormalizationRevision,
    LanguageRegistryRevision,
    PolicyRevisionId,
);

digest_id!(
    DomainError, std::convert::identity;
    ContentDigest,
    FileIdentityDigest,
    SymbolIdentityDigest,
);

impl ContentDigest {
    /// Canonical content identity over byte-exact source.
    ///
    /// This is the single algorithm for content identity. Adapters that need a
    /// content digest without depending on the code-index crate call it
    /// directly rather than re-deriving the encoding.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let encoded =
            crate::canonical_text::encode_tagged_lowercase_hex("sha256:", &Sha256::digest(bytes));
        Self::new(encoded).expect("sha256 hex is a valid content digest")
    }
}

/// Byte range inside one sanitized source file. Mutable line numbers are
/// never part of identity (Plan 25).
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    pub start_byte: u64,
    pub end_byte: u64,
}

impl SourceSpan {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.start_byte > self.end_byte {
            return Err(DomainError::NonCanonical {
                field: "source span byte range",
            });
        }
        Ok(())
    }

    pub const fn len(&self) -> u64 {
        self.end_byte.saturating_sub(self.start_byte)
    }

    pub const fn is_empty(&self) -> bool {
        self.start_byte == self.end_byte
    }
}

/// The logical inputs that define chunk identity. Two chunks share one
/// `CodeSearchChunkId` exactly when every field matches; content and
/// generation are deliberately absent (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChunkLogicalIdentityV1 {
    pub repository: crate::research::id::RepositoryId,
    pub file_identity: FileIdentityDigest,
    pub symbol_identity: Option<SymbolIdentityDigest>,
    pub grain: super::search::CodeSearchChunkGrainV1,
    /// Deterministic structural split path, or the pinned fallback window
    /// start/size when no structural boundary exists.
    pub split_path: Vec<u32>,
    pub chunker_revision: ChunkerRevision,
}
