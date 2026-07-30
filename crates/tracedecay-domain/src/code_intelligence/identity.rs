//! Occurrence identity, source spans, and revision/digest primitives for the
//! PR9 code-intelligence model (Plan 25, "Identity and lineage" and
//! "Code-search chunk and projection contract").
//!
//! Generation-local occurrence identity is exact. Logical identity remains
//! stable only while its declared repository, language, qualified-structure,
//! and source-evidence tuple is unchanged. Extractor enumeration order and
//! mutable line numbers never affect identity.

use std::fmt;
use std::fmt::Write as _;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::research::DomainError;

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

macro_rules! code_id {
    ($($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                if value.is_empty()
                    || value.trim() != value
                    || value.len() > 512
                    || value.chars().any(char::is_control)
                {
                    return Err(DomainError::NonCanonical {
                        field: stringify!($name),
                    });
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                Self::new(self.0.clone()).map(|_| ())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

fn validate_code_digest(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    let valid = value
        .split_once(':')
        .and_then(|(algorithm, encoded)| {
            let expected_len = match algorithm {
                "sha256" | "blake3" => 64,
                "sha512" => 128,
                _ => return None,
            };
            Some(
                encoded.len() == expected_len
                    && encoded
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            )
        })
        .unwrap_or(false);
    if !valid {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

macro_rules! code_digest_id {
    ($($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed algorithm-tagged integrity digest: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                validate_code_digest(&value, stringify!($name))?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                validate_code_digest(&self.0, stringify!($name))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

code_id!(
    CodeGenerationId,
    FileOccurrenceId,
    SymbolOccurrenceId,
    CodeSearchChunkId,
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

code_digest_id!(ContentDigest, FileIdentityDigest, SymbolIdentityDigest,);

impl ContentDigest {
    /// Canonical content identity over byte-exact source.
    ///
    /// This is the single algorithm for content identity. Adapters that need a
    /// content digest without depending on the code-index crate call it
    /// directly rather than re-deriving the encoding.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut encoded = String::with_capacity("sha256:".len() + 64);
        encoded.push_str("sha256:");
        for byte in digest {
            write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
        }
        Self::new(encoded).expect("sha256 hex is a valid content digest")
    }
}

/// Byte range inside one sanitized source file. Mutable line numbers are
/// never part of identity (Plan 25).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// The exact occurrence key for a chunk within one code generation
/// (Plan 25: `(CodeGenerationId, CodeSearchChunkId)` is the exact occurrence
/// key; a digest change classifies an upsert and a move/rename or
/// structural-boundary change classifies delete-plus-add).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ChunkOccurrenceKeyV1 {
    pub generation_id: CodeGenerationId,
    pub chunk_id: CodeSearchChunkId,
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
