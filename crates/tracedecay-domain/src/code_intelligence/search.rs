//! Storage-neutral code-search chunk and projection contracts (Plan 25,
//! "Code-search chunk and projection contract").
//!
//! These values are immutable logical records, not rows coupled to a lexical
//! table, vector table, or vendor index. Chunks are the replayable source for
//! lexical and later model/version-specific projections; embeddings never
//! become source or symbol authority.
//!
//! Code search does not define parallel ranking, fusion-profile,
//! contribution, candidate, cursor, or hydration types here; Plan 15 owns
//! those in `crate::retrieval`.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::research::id::{ManifestDigest, PrivacyDomainId, SanitizationReceiptId};
use crate::research::{DomainError, canonical_sha256};

use super::identity::{
    ChunkerRevision, CodeGenerationId, CodeSearchChunkId, ContentDigest, FileOccurrenceId,
    LanguageDescriptorRevision, PolicyRevisionId, QueryNormalizationRevision, SanitizerRevision,
    SourceSpan, SymbolOccurrenceId,
};

/// Maximum canonical bytes of one chunk's sanitized text (contract bound;
/// oversized bodies split on deterministic structural boundaries or pinned
/// fallback windows before reaching this limit).
pub const MAX_CHUNK_TEXT_BYTES: usize = 64 * 1024;
/// Maximum sanitized query bytes held in one request-local query view.
pub const MAX_EPHEMERAL_QUERY_VIEW_BYTES: usize = 4 * 1024;

const CHANGED_CODE_CHUNK_SET_DIGEST_DOMAIN: &str = "tracedecay.changed-code-chunks.v1";
const CODE_INDEX_CAPABILITY_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.code-index-capability.v1";
const EMBEDDING_PROJECTION_KEY_DIGEST_DOMAIN: &str = "tracedecay.embedding-projection-key.v1";
const SEMANTIC_SEARCH_INDEX_KEY_DIGEST_DOMAIN: &str = "tracedecay.semantic-search-index-key.v1";

pub const EMBEDDING_PROJECTION_SCHEMA_V1: &str = "tracedecay.embedding-projection.v1";
pub const SEMANTIC_SEARCH_INDEX_SCHEMA_V1: &str = "tracedecay.semantic-search-index.v1";

fn validate_sorted_unique<T: Ord>(values: &[T], field: &'static str) -> Result<(), DomainError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

fn validate_revision(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if !crate::canonical_text::is_canonical_text(value) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// Bounded sanitized chunk text. Sanitization proof binds at the snapshot
/// level (`SanitizedCodeSnapshotV1` receipts), not per chunk; this newtype
/// enforces the size bound only.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct BoundedSanitizedText(String);

impl BoundedSanitizedText {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.len() > MAX_CHUNK_TEXT_BYTES {
            return Err(DomainError::UnsafeText {
                field: "bounded sanitized chunk text",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BoundedSanitizedText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Request-local sanitized query bytes used only while executing an
/// authorized retrieval. This value intentionally has no serialization or
/// cloning surface: durable state, telemetry, and cache keys carry only its
/// privacy-bound MAC identity.
#[derive(PartialEq, Eq)]
pub struct EphemeralSanitizedQueryViewV1 {
    text: String,
    sanitizer_revision: SanitizerRevision,
    normalization_revision: QueryNormalizationRevision,
}

impl fmt::Debug for EphemeralSanitizedQueryViewV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EphemeralSanitizedQueryViewV1")
            .field(
                "text",
                &format_args!("<{} bytes redacted>", self.text.len()),
            )
            .field("sanitizer_revision", &self.sanitizer_revision)
            .field("normalization_revision", &self.normalization_revision)
            .finish()
    }
}

impl EphemeralSanitizedQueryViewV1 {
    pub fn sanitize(
        raw_text: impl Into<String>,
        sanitizer_revision: SanitizerRevision,
        normalization_revision: QueryNormalizationRevision,
    ) -> Result<Self, DomainError> {
        let raw_text = raw_text.into();
        let text = raw_text.trim().to_owned();
        if text.is_empty() {
            return Err(DomainError::Empty {
                field: "ephemeral sanitized query view",
            });
        }
        if raw_text.len() > MAX_EPHEMERAL_QUERY_VIEW_BYTES
            || text.len() > MAX_EPHEMERAL_QUERY_VIEW_BYTES
            || text.chars().any(char::is_control)
        {
            return Err(DomainError::UnsafeText {
                field: "ephemeral sanitized query view",
            });
        }
        Ok(Self {
            text,
            sanitizer_revision,
            normalization_revision,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    pub fn sanitizer_revision(&self) -> &SanitizerRevision {
        &self.sanitizer_revision
    }

    pub fn normalization_revision(&self) -> &QueryNormalizationRevision {
        &self.normalization_revision
    }
}

/// The five deterministic chunk grains (Plan 25). Symbol signatures and
/// bodies are separate grains; members become child chunks only when the
/// language descriptor identifies stable member spans; file preambles cover
/// imports/module documentation; file windows cover otherwise unowned
/// sanitized ranges.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CodeSearchChunkGrainV1 {
    SymbolSignature,
    SymbolBody,
    SymbolMember,
    FilePreamble,
    FileWindow,
}

/// Eligibility of one generation-bound file document for chunk production.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "eligibility", content = "reason", rename_all = "snake_case")]
pub enum CodeSearchEligibilityV1 {
    Eligible,
    /// Explicitly excluded; every excluded byte range is declared.
    Excluded {
        reason: String,
    },
    /// Partially eligible; unsupported ranges are declared evidence.
    Partial {
        reason: String,
    },
    Unsupported {
        reason: String,
    },
}

/// One generation-bound file manifest — the scheduling/checkpoint unit.
/// Chunks are the projection and receipt unit (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSearchDocumentV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub eligibility: CodeSearchEligibilityV1,
    pub chunk_ids: Vec<CodeSearchChunkId>,
}

/// Where one chunk lives inside one generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSearchChunkAnchorV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
    pub parent_chunk_id: Option<CodeSearchChunkId>,
    pub source_span: SourceSpan,
    pub grain: CodeSearchChunkGrainV1,
    pub ordinal: u32,
}

impl CodeSearchChunkAnchorV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.source_span.validate()?;
        let symbol_grain = matches!(
            self.grain,
            CodeSearchChunkGrainV1::SymbolSignature
                | CodeSearchChunkGrainV1::SymbolBody
                | CodeSearchChunkGrainV1::SymbolMember
        );
        if symbol_grain && self.symbol_occurrence_id.is_none() {
            return Err(DomainError::UnknownReference {
                field: "symbol grain chunk without symbol occurrence",
            });
        }
        if !symbol_grain && self.symbol_occurrence_id.is_some() {
            return Err(DomainError::UnknownReference {
                field: "file grain chunk with symbol occurrence",
            });
        }
        Ok(())
    }
}

/// The classification of one whole exact technical term (Plan 25/Plan 15
/// exact tier). Whole exact terms and language-profiled subtokens are
/// distinct fields.
#[derive(
    Clone,
    Copy,
    Debug,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ExactTechnicalTermKindV1 {
    WholeSymbol,
    QualifiedName,
    Path,
    CompilerErrorCode,
    CompilerErrorText,
    RuntimeErrorCode,
    RuntimeErrorText,
    CliFlag,
    ToolName,
    ConfigurationKey,
    CommitIdentifier,
}

/// One whole exact technical term extracted as evidence (Plan 25: extraction
/// evidence only; Plan 05 applies Plan 15's protected lexical policy).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExactTechnicalTermV1 {
    kind: ExactTechnicalTermKindV1,
    original_bytes: Vec<u8>,
    canonical_bytes: Vec<u8>,
    span: SourceSpan,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_occurrence_id: Option<SymbolOccurrenceId>,
}

impl ExactTechnicalTermV1 {
    pub fn technical(
        kind: ExactTechnicalTermKindV1,
        original_bytes: Vec<u8>,
        span: SourceSpan,
    ) -> Result<Self, DomainError> {
        if matches!(
            kind,
            ExactTechnicalTermKindV1::WholeSymbol
                | ExactTechnicalTermKindV1::CompilerErrorText
                | ExactTechnicalTermKindV1::RuntimeErrorText
        ) {
            return Err(DomainError::NonCanonical {
                field: "contextual exact term authority",
            });
        }
        validate_self_authenticating_technical_term(kind, &original_bytes)?;
        Self::from_parts(kind, original_bytes, span, None)
    }

    /// Build an untrusted WholeSymbol candidate. This value cannot enter an
    /// exact projection until code-index extraction authority re-admits its
    /// containing chunk.
    pub fn untrusted_whole_symbol_candidate(
        original_bytes: Vec<u8>,
        span: SourceSpan,
        symbol_occurrence_id: SymbolOccurrenceId,
    ) -> Result<Self, DomainError> {
        symbol_occurrence_id.validate()?;
        Self::from_parts(
            ExactTechnicalTermKindV1::WholeSymbol,
            original_bytes,
            span,
            Some(symbol_occurrence_id),
        )
    }

    /// Build untrusted contextual error-text evidence recognized by the
    /// extractor. Like WholeSymbol, projection requires extraction admission.
    pub fn untrusted_contextual_text_candidate(
        kind: ExactTechnicalTermKindV1,
        original_bytes: Vec<u8>,
        span: SourceSpan,
    ) -> Result<Self, DomainError> {
        if !matches!(
            kind,
            ExactTechnicalTermKindV1::CompilerErrorText
                | ExactTechnicalTermKindV1::RuntimeErrorText
        ) {
            return Err(DomainError::NonCanonical {
                field: "contextual exact term kind",
            });
        }
        if original_bytes.iter().any(u8::is_ascii_control) {
            return Err(DomainError::NonCanonical {
                field: "contextual exact term bytes",
            });
        }
        Self::from_parts(kind, original_bytes, span, None)
    }

    fn from_parts(
        kind: ExactTechnicalTermKindV1,
        original_bytes: Vec<u8>,
        span: SourceSpan,
        symbol_occurrence_id: Option<SymbolOccurrenceId>,
    ) -> Result<Self, DomainError> {
        let canonical_bytes = match kind {
            ExactTechnicalTermKindV1::CliFlag
            | ExactTechnicalTermKindV1::ConfigurationKey
            | ExactTechnicalTermKindV1::ToolName
            | ExactTechnicalTermKindV1::CommitIdentifier => original_bytes.to_ascii_lowercase(),
            _ => original_bytes.clone(),
        };
        let term = Self {
            kind,
            original_bytes,
            canonical_bytes,
            span,
            symbol_occurrence_id,
        };
        term.validate_shape()?;
        Ok(term)
    }

    /// Rebind a WholeSymbol term's occurrence authority during chunk
    /// rematerialization for a new generation. Only WholeSymbol terms carry
    /// occurrence authority; rebinding any other kind is non-canonical.
    pub fn rebind_symbol_occurrence(
        &mut self,
        symbol_occurrence_id: SymbolOccurrenceId,
    ) -> Result<(), DomainError> {
        if self.kind != ExactTechnicalTermKindV1::WholeSymbol {
            return Err(DomainError::NonCanonical {
                field: "exact term occurrence rebind kind",
            });
        }
        symbol_occurrence_id.validate()?;
        self.symbol_occurrence_id = Some(symbol_occurrence_id);
        Ok(())
    }

    pub fn kind(&self) -> ExactTechnicalTermKindV1 {
        self.kind
    }

    pub fn original_bytes(&self) -> &[u8] {
        &self.original_bytes
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    pub fn span(&self) -> SourceSpan {
        self.span
    }

    pub fn symbol_occurrence_id(&self) -> Option<&SymbolOccurrenceId> {
        self.symbol_occurrence_id.as_ref()
    }

    pub fn requires_extraction_authority(&self) -> bool {
        matches!(
            self.kind,
            ExactTechnicalTermKindV1::WholeSymbol
                | ExactTechnicalTermKindV1::CompilerErrorText
                | ExactTechnicalTermKindV1::RuntimeErrorText
        )
    }

    fn validate_shape(&self) -> Result<(), DomainError> {
        self.span.validate()?;
        if self.span.is_empty() || self.original_bytes.is_empty() || self.canonical_bytes.is_empty()
        {
            return Err(DomainError::Empty {
                field: "exact technical term",
            });
        }
        match (self.kind, self.symbol_occurrence_id.as_ref()) {
            (ExactTechnicalTermKindV1::WholeSymbol, Some(symbol_occurrence_id)) => {
                symbol_occurrence_id.validate()?;
            }
            (ExactTechnicalTermKindV1::WholeSymbol, None) => {
                return Err(DomainError::NonCanonical {
                    field: "whole symbol exact term authority",
                });
            }
            (_, Some(_)) => {
                return Err(DomainError::NonCanonical {
                    field: "non-symbol exact term authority",
                });
            }
            (_, None) => {}
        }
        match self.kind {
            ExactTechnicalTermKindV1::WholeSymbol => {}
            ExactTechnicalTermKindV1::CompilerErrorText
            | ExactTechnicalTermKindV1::RuntimeErrorText => {
                if self.original_bytes.iter().any(u8::is_ascii_control) {
                    return Err(DomainError::NonCanonical {
                        field: "contextual exact term bytes",
                    });
                }
            }
            kind => validate_self_authenticating_technical_term(kind, &self.original_bytes)?,
        }
        let expected_canonical = match self.kind {
            ExactTechnicalTermKindV1::CliFlag
            | ExactTechnicalTermKindV1::ConfigurationKey
            | ExactTechnicalTermKindV1::ToolName
            | ExactTechnicalTermKindV1::CommitIdentifier => {
                self.original_bytes.to_ascii_lowercase()
            }
            _ => self.original_bytes.clone(),
        };
        if self.canonical_bytes != expected_canonical {
            return Err(DomainError::NonCanonical {
                field: "exact technical term canonical bytes",
            });
        }
        Ok(())
    }

    pub fn validate_within(&self, chunk_span: &SourceSpan) -> Result<(), DomainError> {
        self.validate_shape()?;
        if self.original_bytes.len() as u64 != self.span.len()
            || self.span.start_byte < chunk_span.start_byte
            || self.span.end_byte > chunk_span.end_byte
        {
            return Err(DomainError::NonCanonical {
                field: "exact technical term span",
            });
        }
        Ok(())
    }
}

fn validate_self_authenticating_technical_term(
    kind: ExactTechnicalTermKindV1,
    bytes: &[u8],
) -> Result<(), DomainError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DomainError::NonCanonical {
        field: "exact technical term UTF-8",
    })?;
    let is_ident = |segment: &str| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            && segment
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphabetic() || character == '_')
    };
    let valid = match kind {
        ExactTechnicalTermKindV1::QualifiedName => {
            text.contains("::") && text.split("::").all(is_ident)
        }
        ExactTechnicalTermKindV1::Path => {
            text.contains('/')
                && text.split('/').all(|segment| {
                    !segment.is_empty()
                        && segment.chars().all(|character| {
                            character.is_ascii_alphanumeric()
                                || matches!(character, '_' | '-' | '.')
                        })
                })
                && text
                    .rsplit('/')
                    .next()
                    .is_some_and(|filename| filename.contains('.'))
        }
        ExactTechnicalTermKindV1::CompilerErrorCode => {
            ["E", "TS", "CS"].into_iter().any(|prefix| {
                text.strip_prefix(prefix).is_some_and(|digits| {
                    digits.len() == 4 && digits.chars().all(|character| character.is_ascii_digit())
                })
            })
        }
        ExactTechnicalTermKindV1::RuntimeErrorCode => {
            text.strip_prefix("ERR_").is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix.chars().all(|character| {
                        character.is_ascii_uppercase()
                            || character.is_ascii_digit()
                            || character == '_'
                    })
            })
        }
        ExactTechnicalTermKindV1::CliFlag => text.strip_prefix("--").is_some_and(|flag| {
            !flag.is_empty()
                && !flag.ends_with('-')
                && flag
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphabetic())
                && flag.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
                })
        }),
        ExactTechnicalTermKindV1::ToolName => matches!(
            text.to_ascii_lowercase().as_str(),
            "cargo" | "rustc" | "tracedecay" | "pytest" | "kubectl" | "fastembed" | "ast-grep"
        ),
        ExactTechnicalTermKindV1::ConfigurationKey => {
            text.split('.').count() >= 3
                && text.split('.').all(|segment| {
                    !segment.is_empty()
                        && segment.chars().all(|character| {
                            character.is_ascii_lowercase()
                                || character.is_ascii_digit()
                                || character == '_'
                        })
                })
        }
        ExactTechnicalTermKindV1::CommitIdentifier => {
            text.strip_prefix("commit:").is_some_and(|identifier| {
                (7..=40).contains(&identifier.len())
                    && identifier
                        .chars()
                        .all(|character| character.is_ascii_hexdigit())
            })
        }
        ExactTechnicalTermKindV1::WholeSymbol
        | ExactTechnicalTermKindV1::CompilerErrorText
        | ExactTechnicalTermKindV1::RuntimeErrorText => false,
    };
    if valid {
        Ok(())
    } else {
        Err(DomainError::NonCanonical {
            field: "exact technical term kind",
        })
    }
}

impl<'de> Deserialize<'de> for ExactTechnicalTermV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            kind: ExactTechnicalTermKindV1,
            original_bytes: Vec<u8>,
            canonical_bytes: Vec<u8>,
            span: SourceSpan,
            #[serde(default)]
            symbol_occurrence_id: Option<SymbolOccurrenceId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let term = Self {
            kind: wire.kind,
            original_bytes: wire.original_bytes,
            canonical_bytes: wire.canonical_bytes,
            span: wire.span,
            symbol_occurrence_id: wire.symbol_occurrence_id,
        };
        term.validate_shape().map_err(serde::de::Error::custom)?;
        Ok(term)
    }
}

/// The sensitivity decision applied to one chunk by the privacy boundary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SensitivityDecision {
    pub level: SensitivityLevelV1,
    pub policy_revision: PolicyRevisionId,
}

/// Sensitivity levels; privacy-domain or key-epoch changes rebuild canonical
/// eligibility when policy output changes (Plan 25).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SensitivityLevelV1 {
    Public,
    Internal,
    Restricted,
    Redacted,
}

/// One deterministic, generation-bound code-search chunk.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeSearchChunkV1 {
    pub id: CodeSearchChunkId,
    pub anchor: CodeSearchChunkAnchorV1,
    pub content_digest: ContentDigest,
    pub language_descriptor_revision: LanguageDescriptorRevision,
    pub chunker_revision: ChunkerRevision,
    pub sanitizer_revision: SanitizerRevision,
    pub sensitivity: SensitivityDecision,
    /// Whole exact technical terms (distinct from subtokens).
    pub exact_terms: Vec<ExactTechnicalTermV1>,
    /// Language-profiled subtokens, in deterministic source order.
    pub subtokens: Vec<String>,
    pub sanitized_text: BoundedSanitizedText,
}

/// Type-state boundary for chunks re-admitted by parser-backed extraction.
///
/// Consumers may accept this contract without depending on the concrete
/// extraction engine. Implementations remain owned by that engine and return
/// the native domain chunk after their authority checks have succeeded.
///
/// # Safety
///
/// Implementors must only wrap chunks whose authority-sensitive exact terms
/// were produced or revalidated by parser-backed extraction. Implementing this
/// trait for untrusted chunks can admit forged exact-index evidence.
pub unsafe trait ExtractionAdmittedChunkV1 {
    fn into_admitted_chunk(self) -> CodeSearchChunkV1;
}

impl<'de> Deserialize<'de> for CodeSearchChunkV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: CodeSearchChunkId,
            anchor: CodeSearchChunkAnchorV1,
            content_digest: ContentDigest,
            language_descriptor_revision: LanguageDescriptorRevision,
            chunker_revision: ChunkerRevision,
            sanitizer_revision: SanitizerRevision,
            sensitivity: SensitivityDecision,
            exact_terms: Vec<ExactTechnicalTermV1>,
            subtokens: Vec<String>,
            sanitized_text: BoundedSanitizedText,
        }

        let wire = Wire::deserialize(deserializer)?;
        let chunk = Self {
            id: wire.id,
            anchor: wire.anchor,
            content_digest: wire.content_digest,
            language_descriptor_revision: wire.language_descriptor_revision,
            chunker_revision: wire.chunker_revision,
            sanitizer_revision: wire.sanitizer_revision,
            sensitivity: wire.sensitivity,
            exact_terms: wire.exact_terms,
            subtokens: wire.subtokens,
            sanitized_text: wire.sanitized_text,
        };
        chunk.validate().map_err(serde::de::Error::custom)?;
        Ok(chunk)
    }
}

impl CodeSearchChunkV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.id.validate()?;
        self.anchor.generation_id.validate()?;
        self.anchor.file_occurrence_id.validate()?;
        self.anchor.validate()?;
        self.content_digest.validate()?;
        self.language_descriptor_revision.validate()?;
        self.chunker_revision.validate()?;
        self.sanitizer_revision.validate()?;
        self.sensitivity.policy_revision.validate()?;
        if self.anchor.source_span.is_empty() || self.sanitized_text.as_str().is_empty() {
            return Err(DomainError::Empty {
                field: "code search chunk",
            });
        }
        if self.anchor.parent_chunk_id.as_ref() == Some(&self.id) {
            return Err(DomainError::SelfSupersession);
        }
        for term in &self.exact_terms {
            term.validate_within(&self.anchor.source_span)?;
            if term.kind() == ExactTechnicalTermKindV1::WholeSymbol
                && term.symbol_occurrence_id() != self.anchor.symbol_occurrence_id.as_ref()
            {
                return Err(DomainError::NonCanonical {
                    field: "whole symbol chunk authority",
                });
            }
            let start = term
                .span()
                .start_byte
                .checked_sub(self.anchor.source_span.start_byte)
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or(DomainError::NonCanonical {
                    field: "exact technical term source bytes",
                })?;
            let end = term
                .span()
                .end_byte
                .checked_sub(self.anchor.source_span.start_byte)
                .and_then(|offset| usize::try_from(offset).ok())
                .ok_or(DomainError::NonCanonical {
                    field: "exact technical term source bytes",
                })?;
            if self.sanitized_text.as_str().as_bytes().get(start..end)
                != Some(term.original_bytes())
            {
                return Err(DomainError::NonCanonical {
                    field: "exact technical term source bytes",
                });
            }
        }
        if self.exact_terms.windows(2).any(|terms| {
            (
                terms[0].span.start_byte,
                terms[0].span.end_byte,
                terms[0].kind,
                &terms[0].canonical_bytes,
                &terms[0].original_bytes,
            ) >= (
                terms[1].span.start_byte,
                terms[1].span.end_byte,
                terms[1].kind,
                &terms[1].canonical_bytes,
                &terms[1].original_bytes,
            )
        }) {
            return Err(DomainError::NonCanonical {
                field: "exact technical term order",
            });
        }
        if self
            .subtokens
            .iter()
            .any(|subtoken| subtoken.is_empty() || subtoken.chars().any(char::is_control))
        {
            return Err(DomainError::NonCanonical {
                field: "code search subtokens",
            });
        }
        Ok(())
    }
}

/// One chunk membership change between two generations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChangedCodeChunkV1 {
    pub chunk_id: CodeSearchChunkId,
    pub prior_digest: Option<ContentDigest>,
    pub current_digest: Option<ContentDigest>,
}

/// Ordered changed/reused/deleted chunk manifest between two generations
/// (Plan 25: lets downstream projectors prove exactly which generation-bound
/// chunks they consumed, skipped, replaced, or removed). A no-op generation
/// emits empty `added_or_changed` and `deleted` sets plus explicit `reused`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChangedCodeChunkSetV1 {
    pub from_generation: Option<CodeGenerationId>,
    pub to_generation: CodeGenerationId,
    pub manifest_digest: ManifestDigest,
    pub added_or_changed: Vec<ChangedCodeChunkV1>,
    pub deleted: Vec<ChangedCodeChunkV1>,
    pub reused: Vec<ChangedCodeChunkV1>,
}

#[derive(Serialize)]
struct ChangedCodeChunkSetDigestInput<'a> {
    domain: &'static str,
    from_generation: &'a Option<CodeGenerationId>,
    to_generation: &'a CodeGenerationId,
    added_or_changed: &'a [ChangedCodeChunkV1],
    deleted: &'a [ChangedCodeChunkV1],
    reused: &'a [ChangedCodeChunkV1],
}

impl ChangedCodeChunkSetV1 {
    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&ChangedCodeChunkSetDigestInput {
            domain: CHANGED_CODE_CHUNK_SET_DIGEST_DOMAIN,
            from_generation: &self.from_generation,
            to_generation: &self.to_generation,
            added_or_changed: &self.added_or_changed,
            deleted: &self.deleted,
            reused: &self.reused,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.to_generation.validate()?;
        if let Some(from_generation) = &self.from_generation {
            from_generation.validate()?;
            if from_generation == &self.to_generation {
                return Err(DomainError::SnapshotMismatch {
                    field: "changed chunk generations",
                });
            }
        }

        validate_changed_partition(
            &self.added_or_changed,
            "added or changed chunk order",
            |change| {
                change.current_digest.is_some()
                    && change.prior_digest.as_ref() != change.current_digest.as_ref()
            },
        )?;
        validate_changed_partition(&self.deleted, "deleted chunk order", |change| {
            change.prior_digest.is_some() && change.current_digest.is_none()
        })?;
        validate_changed_partition(&self.reused, "reused chunk order", |change| {
            change.prior_digest.is_some() && change.prior_digest == change.current_digest
        })?;

        let mut seen = BTreeSet::new();
        for change in self
            .added_or_changed
            .iter()
            .chain(&self.deleted)
            .chain(&self.reused)
        {
            if !seen.insert(&change.chunk_id) {
                return Err(DomainError::DuplicateId {
                    field: "changed chunk partitions",
                });
            }
        }
        self.manifest_digest.validate()?;
        if self.compute_digest()? != self.manifest_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

fn validate_changed_partition(
    changes: &[ChangedCodeChunkV1],
    field: &'static str,
    valid_shape: impl Fn(&ChangedCodeChunkV1) -> bool,
) -> Result<(), DomainError> {
    for change in changes {
        change.chunk_id.validate()?;
        if let Some(digest) = &change.prior_digest {
            digest.validate()?;
        }
        if let Some(digest) = &change.current_digest {
            digest.validate()?;
        }
        if !valid_shape(change) {
            return Err(DomainError::NonCanonical { field });
        }
    }
    if changes
        .windows(2)
        .any(|pair| pair[0].chunk_id >= pair[1].chunk_id)
    {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// Identity of one projection profile (Plan 25: projection kind, projection
/// schema revision, and a canonical profile digest). Plan 31's
/// `EmbeddingProjectionKeyV1` is the typed semantic profile whose canonical
/// digest occupies `profile_digest`; adapters cannot define a second
/// projection-key identity.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPoolingV1 {
    Mean,
    Cls,
    LastToken,
    MeanSqrtLength,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingTruncationSideV1 {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingDeviceClassV1 {
    Cpu,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingMetricV1 {
    Cosine,
    DotProduct,
    EuclideanL2,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingNormalizationV1 {
    None,
    L2,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPrecisionV1 {
    Fp32,
    Fp16,
    Bf16,
    Int8,
}

/// Immutable identity of one fully published semantic vector generation.
///
/// This identity is shared by projection stores and semantic retrieval
/// adapters; neither layer may define a lookalike generation key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct VectorGenerationIdV1(ManifestDigest);

impl VectorGenerationIdV1 {
    pub fn new(digest: ManifestDigest) -> Self {
        Self(digest)
    }

    pub fn as_digest(&self) -> &ManifestDigest {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }
}

/// Complete identity of one embedding projection. Every vector-affecting
/// input is pinned here; its canonical digest becomes the profile digest in
/// Plan 25's generic [`ProjectionKeyV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingProjectionKeyV1 {
    pub model_artifact_digest: ManifestDigest,
    pub tokenizer_digest: ManifestDigest,
    pub config_digest: ManifestDigest,
    pub query_instruction_digest: Option<ManifestDigest>,
    pub document_instruction_digest: Option<ManifestDigest>,
    pub pooling: EmbeddingPoolingV1,
    pub truncation_side: EmbeddingTruncationSideV1,
    pub truncation_length: u32,
    pub runtime_backend: String,
    pub runtime_build_revision: String,
    pub device_class: EmbeddingDeviceClassV1,
    pub dimensions: u32,
    pub metric: EmbeddingMetricV1,
    pub normalization: EmbeddingNormalizationV1,
    pub precision: EmbeddingPrecisionV1,
    pub chunk_schema_revision: String,
    pub chunker_revision: ChunkerRevision,
    pub privacy_domain: PrivacyDomainId,
    pub privacy_key_epoch: u64,
}

/// Validated projection/privacy authority shared by vector production and
/// bounded runtime session identity. Its fields are private so adapters cannot
/// reconstruct a compatible-looking identity from unconstrained strings.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AdmittedEmbeddingProjectionKeyV1 {
    embedding_key: EmbeddingProjectionKeyV1,
    projection_key: ProjectionKeyV1,
}

impl Serialize for AdmittedEmbeddingProjectionKeyV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct AdmittedProjectionRef<'a> {
            embedding_key: &'a EmbeddingProjectionKeyV1,
            projection_key: &'a ProjectionKeyV1,
        }

        AdmittedProjectionRef {
            embedding_key: &self.embedding_key,
            projection_key: &self.projection_key,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AdmittedEmbeddingProjectionKeyV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AdmittedProjectionRepr {
            embedding_key: EmbeddingProjectionKeyV1,
            projection_key: ProjectionKeyV1,
        }

        let repr = AdmittedProjectionRepr::deserialize(deserializer)?;
        let admitted = repr
            .embedding_key
            .admit()
            .map_err(serde::de::Error::custom)?;
        if admitted.projection_key != repr.projection_key {
            return Err(serde::de::Error::custom(
                "admitted embedding projection key digest mismatch",
            ));
        }
        Ok(admitted)
    }
}

impl EmbeddingProjectionKeyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.model_artifact_digest.validate()?;
        self.tokenizer_digest.validate()?;
        self.config_digest.validate()?;
        if let Some(digest) = &self.query_instruction_digest {
            digest.validate()?;
        }
        if let Some(digest) = &self.document_instruction_digest {
            digest.validate()?;
        }
        if self.truncation_length == 0 {
            return Err(DomainError::Empty {
                field: "embedding truncation length",
            });
        }
        if self.dimensions == 0 {
            return Err(DomainError::Empty {
                field: "embedding dimensions",
            });
        }
        validate_revision(&self.runtime_backend, "embedding runtime backend")?;
        validate_revision(
            &self.runtime_build_revision,
            "embedding runtime build revision",
        )?;
        validate_revision(
            &self.chunk_schema_revision,
            "embedding chunk schema revision",
        )?;
        self.chunker_revision.validate()?;
        self.privacy_domain.validate()?;
        Ok(())
    }

    pub fn admit(&self) -> Result<AdmittedEmbeddingProjectionKeyV1, DomainError> {
        Ok(AdmittedEmbeddingProjectionKeyV1 {
            embedding_key: self.clone(),
            projection_key: ProjectionKeyV1 {
                kind: ProjectionKindV1::Embedding,
                schema_revision: EMBEDDING_PROJECTION_SCHEMA_V1.to_string(),
                profile_digest: self.canonical_digest()?,
            },
        })
    }

    pub fn canonical_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&(EMBEDDING_PROJECTION_KEY_DIGEST_DOMAIN, self))
    }

    pub fn projection_key(&self) -> Result<ProjectionKeyV1, DomainError> {
        Ok(self.admit()?.projection_key)
    }
}

impl AdmittedEmbeddingProjectionKeyV1 {
    pub fn embedding_key(&self) -> &EmbeddingProjectionKeyV1 {
        &self.embedding_key
    }

    pub fn projection_key(&self) -> &ProjectionKeyV1 {
        &self.projection_key
    }

    pub fn privacy_domain(&self) -> &PrivacyDomainId {
        &self.embedding_key.privacy_domain
    }

    pub fn privacy_key_epoch(&self) -> u64 {
        self.embedding_key.privacy_key_epoch
    }
}

/// Search structure used over one compatible immutable vector generation.
///
/// This is intentionally distinct from [`EmbeddingProjectionKeyV1`]:
/// changing an index implementation or its parameters must rebuild only the
/// derived search structure and query caches, never the vector projection.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSearchIndexKindV1 {
    ExactFlat,
}

/// Complete identity inputs for one semantic search structure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SemanticSearchIndexProfileV1 {
    pub kind: SemanticSearchIndexKindV1,
    pub implementation_revision: String,
    pub parameters_digest: ManifestDigest,
}

/// Independent immutable identity of a derived semantic search structure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SemanticSearchIndexKeyV1 {
    pub kind: SemanticSearchIndexKindV1,
    pub schema_revision: String,
    pub profile_digest: ManifestDigest,
}

impl SemanticSearchIndexProfileV1 {
    pub fn exact_flat_v1() -> Result<Self, DomainError> {
        Ok(Self {
            kind: SemanticSearchIndexKindV1::ExactFlat,
            implementation_revision: "semantic.exact-flat.v1".to_owned(),
            parameters_digest: canonical_sha256(&(
                "tracedecay.semantic-exact-flat-parameters.v1",
                "scan-all-compatible-vectors",
                "canonical-distance-then-anchor",
            ))?,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_revision(
            &self.implementation_revision,
            "semantic search index implementation revision",
        )?;
        self.parameters_digest.validate()
    }

    pub fn index_key(&self) -> Result<SemanticSearchIndexKeyV1, DomainError> {
        self.validate()?;
        Ok(SemanticSearchIndexKeyV1 {
            kind: self.kind,
            schema_revision: SEMANTIC_SEARCH_INDEX_SCHEMA_V1.to_owned(),
            profile_digest: canonical_sha256(&(SEMANTIC_SEARCH_INDEX_KEY_DIGEST_DOMAIN, self))?,
        })
    }
}

impl SemanticSearchIndexKeyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_revision(
            &self.schema_revision,
            "semantic search index schema revision",
        )?;
        self.profile_digest.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ProjectionKeyV1 {
    pub kind: ProjectionKindV1,
    pub schema_revision: String,
    pub profile_digest: ManifestDigest,
}

/// The projection families query/semantic recognize.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionKindV1 {
    Lexical,
    Graph,
    Embedding,
}

/// Why a projection replay was requested.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionReplayReasonV1 {
    InitialProjection,
    SourceEdit,
    ProjectionProfileChange,
    FullRebuildIncompatible,
    QuarantinedCorruption,
    VerificationReplay,
}

/// One projector batch request (Plan 25).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionBatchRequestV1 {
    pub request_digest: ManifestDigest,
    pub changes: ChangedCodeChunkSetV1,
    pub previous_projection_key: Option<ProjectionKeyV1>,
    pub target_projection_key: ProjectionKeyV1,
    pub replay_reason: ProjectionReplayReasonV1,
}

/// What a projector did with one chunk.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionOperationV1 {
    Added,
    Updated,
    Deleted,
    Reused,
}

/// Outcome of one projection operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "outcome", content = "reason", rename_all = "snake_case")]
pub enum ProjectionOutcomeV1 {
    Applied,
    Reused,
    Skipped { reason: String },
    Failed { reason: String },
}

/// One per-chunk projection receipt (Plan 25). Receipts are deterministic
/// apart from store-owned operational timestamps, which are excluded from
/// receipt identity and digest. Publication rejects duplicate, missing,
/// extra, cross-generation, wrong-digest, or wrong-projection-key receipts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeChunkProjectionReceiptV1 {
    pub projection_key: ProjectionKeyV1,
    pub request_digest: ManifestDigest,
    pub prior_generation: Option<CodeGenerationId>,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub chunk_id: CodeSearchChunkId,
    pub prior_chunk_digest: Option<ContentDigest>,
    pub current_chunk_digest: Option<ContentDigest>,
    pub operation: ProjectionOperationV1,
    pub outcome: ProjectionOutcomeV1,
    pub output_digest: Option<ContentDigest>,
}

/// The complete receipt for one projection batch (Plan 25). Failed or
/// partial receipt sets remain inspectable but cannot activate a projection
/// generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionBatchReceiptV1 {
    pub target_projection_key: ProjectionKeyV1,
    pub request_digest: ManifestDigest,
    pub source_generation: CodeGenerationId,
    pub source_manifest_digest: ManifestDigest,
    pub receipts: Vec<CodeChunkProjectionReceiptV1>,
    pub reused_count: u64,
    pub publication_digest: ManifestDigest,
}

/// The mandatory base capability manifest (Plan 25). Consumers must reject a
/// missing, incompatible, mixed-generation, or unauthorized base manifest
/// before candidate production. Plan 31's optional semantic manifest augments
/// this base; its absence cannot block authorized lexical/graph retrieval.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexCapabilityManifestV1 {
    pub generation_id: CodeGenerationId,
    pub chunk_schema_revision: String,
    pub chunker_revision: ChunkerRevision,
    pub language_descriptor_revisions: Vec<LanguageDescriptorRevision>,
    pub available_grains: Vec<CodeSearchChunkGrainV1>,
    pub exact_term_kinds: Vec<ExactTechnicalTermKindV1>,
    pub supported_languages: Vec<super::identity::LanguageId>,
    pub edge_authority_classes: Vec<super::language::EdgeAuthorityV1>,
    pub privacy_domain: crate::research::id::PrivacyDomainId,
    pub privacy_key_epoch: u64,
    pub source_coverage: CoverageSummaryV1,
    pub sanitization_receipts: Vec<SanitizationReceiptId>,
    pub manifest_digest: ManifestDigest,
}

#[derive(Serialize)]
struct CodeIndexCapabilityManifestDigestInput<'a> {
    domain: &'static str,
    generation_id: &'a CodeGenerationId,
    chunk_schema_revision: &'a str,
    chunker_revision: &'a ChunkerRevision,
    language_descriptor_revisions: &'a [LanguageDescriptorRevision],
    available_grains: &'a [CodeSearchChunkGrainV1],
    exact_term_kinds: &'a [ExactTechnicalTermKindV1],
    supported_languages: &'a [super::identity::LanguageId],
    edge_authority_classes: &'a [super::language::EdgeAuthorityV1],
    privacy_domain: &'a crate::research::id::PrivacyDomainId,
    privacy_key_epoch: u64,
    source_coverage: &'a CoverageSummaryV1,
    sanitization_receipts: &'a [SanitizationReceiptId],
}

impl CodeIndexCapabilityManifestV1 {
    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&CodeIndexCapabilityManifestDigestInput {
            domain: CODE_INDEX_CAPABILITY_MANIFEST_DIGEST_DOMAIN,
            generation_id: &self.generation_id,
            chunk_schema_revision: &self.chunk_schema_revision,
            chunker_revision: &self.chunker_revision,
            language_descriptor_revisions: &self.language_descriptor_revisions,
            available_grains: &self.available_grains,
            exact_term_kinds: &self.exact_term_kinds,
            supported_languages: &self.supported_languages,
            edge_authority_classes: &self.edge_authority_classes,
            privacy_domain: &self.privacy_domain,
            privacy_key_epoch: self.privacy_key_epoch,
            source_coverage: &self.source_coverage,
            sanitization_receipts: &self.sanitization_receipts,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.generation_id.validate()?;
        validate_revision(
            &self.chunk_schema_revision,
            "capability chunk schema revision",
        )?;
        self.chunker_revision.validate()?;
        self.privacy_domain.validate()?;
        self.manifest_digest.validate()?;

        if self.language_descriptor_revisions.len() != self.supported_languages.len() {
            return Err(DomainError::SnapshotMismatch {
                field: "capability language descriptor revisions",
            });
        }
        if self.available_grains.is_empty()
            || self.exact_term_kinds.is_empty()
            || self.supported_languages.is_empty()
            || self.edge_authority_classes.is_empty()
            || self.sanitization_receipts.is_empty()
        {
            return Err(DomainError::Empty {
                field: "code index capability manifest",
            });
        }
        validate_sorted_unique(
            &self.language_descriptor_revisions,
            "capability language descriptor revisions",
        )?;
        validate_sorted_unique(&self.available_grains, "capability available grains")?;
        validate_sorted_unique(&self.exact_term_kinds, "capability exact term kinds")?;
        validate_sorted_unique(&self.supported_languages, "capability supported languages")?;
        validate_sorted_unique(
            &self.edge_authority_classes,
            "capability edge authority classes",
        )?;
        validate_sorted_unique(
            &self.sanitization_receipts,
            "capability sanitization receipts",
        )?;
        if self.compute_digest()? != self.manifest_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Source coverage and exclusion summary carried by the capability manifest.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoverageSummaryV1 {
    pub files_eligible: u64,
    pub files_excluded: u64,
    pub files_partial: u64,
    pub files_unsupported: u64,
    pub ranges_excluded: u64,
    pub ranges_unsupported: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_intelligence::identity::LanguageId;
    use crate::code_intelligence::language::EdgeAuthorityV1;
    use crate::research::id::{PrivacyDomainId, SanitizationReceiptId};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    #[test]
    fn ephemeral_query_view_is_bounded_and_redacts_its_text() {
        let view = EphemeralSanitizedQueryViewV1::sanitize(
            "private query text",
            id::<SanitizerRevision>("sanitizer.query.v1"),
            id::<QueryNormalizationRevision>("normalization.query.v1"),
        )
        .expect("bounded query view");

        assert_eq!(view.as_bytes(), b"private query text");
        assert!(!format!("{view:?}").contains("private query text"));
        assert!(
            EphemeralSanitizedQueryViewV1::sanitize(
                "x".repeat(MAX_EPHEMERAL_QUERY_VIEW_BYTES + 1),
                id::<SanitizerRevision>("sanitizer.query.v1"),
                id::<QueryNormalizationRevision>("normalization.query.v1"),
            )
            .is_err()
        );
    }

    fn change(chunk_id: &str, prior: Option<char>, current: Option<char>) -> ChangedCodeChunkV1 {
        ChangedCodeChunkV1 {
            chunk_id: id(chunk_id),
            prior_digest: prior.map(|byte| id(&digest(byte))),
            current_digest: current.map(|byte| id(&digest(byte))),
        }
    }

    fn changed_set() -> ChangedCodeChunkSetV1 {
        let mut changes = ChangedCodeChunkSetV1 {
            from_generation: Some(id("generation.1")),
            to_generation: id("generation.2"),
            manifest_digest: id(&digest('0')),
            added_or_changed: vec![change("chunk.added", None, Some('a'))],
            deleted: vec![change("chunk.deleted", Some('b'), None)],
            reused: vec![change("chunk.reused", Some('c'), Some('c'))],
        };
        changes.manifest_digest = changes.compute_digest().expect("digest computable");
        changes
    }

    fn capability_manifest() -> CodeIndexCapabilityManifestV1 {
        let mut manifest = CodeIndexCapabilityManifestV1 {
            generation_id: id("generation.2"),
            chunk_schema_revision: "code-search-chunk/v1".to_owned(),
            chunker_revision: id("chunker.v1"),
            language_descriptor_revisions: vec![id("descriptor.rust.v1")],
            available_grains: vec![
                CodeSearchChunkGrainV1::SymbolSignature,
                CodeSearchChunkGrainV1::SymbolBody,
            ],
            exact_term_kinds: vec![
                ExactTechnicalTermKindV1::WholeSymbol,
                ExactTechnicalTermKindV1::QualifiedName,
            ],
            supported_languages: vec![LanguageId::new("rust").unwrap()],
            edge_authority_classes: vec![
                EdgeAuthorityV1::SyntaxExact,
                EdgeAuthorityV1::NameResolved,
            ],
            privacy_domain: PrivacyDomainId::new("privacy.fixture").unwrap(),
            privacy_key_epoch: 1,
            source_coverage: CoverageSummaryV1 {
                files_eligible: 1,
                ..CoverageSummaryV1::default()
            },
            sanitization_receipts: vec![SanitizationReceiptId::new("receipt.fixture").unwrap()],
            manifest_digest: id(&digest('0')),
        };
        manifest.manifest_digest = manifest.compute_digest().expect("digest computable");
        manifest
    }

    #[test]
    fn symbol_grains_require_a_symbol_occurrence() {
        let anchor = CodeSearchChunkAnchorV1 {
            generation_id: id("generation.fixture"),
            file_occurrence_id: id("file.fixture"),
            symbol_occurrence_id: None,
            parent_chunk_id: None,
            source_span: SourceSpan {
                start_byte: 0,
                end_byte: 10,
            },
            grain: CodeSearchChunkGrainV1::SymbolBody,
            ordinal: 0,
        };
        assert!(anchor.validate().is_err());

        let mut file_anchor = anchor.clone();
        file_anchor.grain = CodeSearchChunkGrainV1::FileWindow;
        file_anchor.symbol_occurrence_id = Some(id("symbol.fixture"));
        assert!(file_anchor.validate().is_err());

        let mut symbol_anchor = anchor;
        symbol_anchor.symbol_occurrence_id = Some(id("symbol.fixture"));
        symbol_anchor
            .validate()
            .expect("symbol grain with occurrence");
    }

    #[test]
    fn bounded_sanitized_text_enforces_the_chunk_bound() {
        assert!(BoundedSanitizedText::new("x".repeat(MAX_CHUNK_TEXT_BYTES)).is_ok());
        assert!(BoundedSanitizedText::new("x".repeat(MAX_CHUNK_TEXT_BYTES + 1)).is_err());
    }

    #[test]
    fn exact_terms_must_be_nonempty_and_within_their_chunk_span() {
        let mut term = ExactTechnicalTermV1 {
            kind: ExactTechnicalTermKindV1::QualifiedName,
            original_bytes: b"module::symbol".to_vec(),
            canonical_bytes: b"module::symbol".to_vec(),
            span: SourceSpan {
                start_byte: 12,
                end_byte: 26,
            },
            symbol_occurrence_id: None,
        };
        term.validate_within(&SourceSpan {
            start_byte: 10,
            end_byte: 30,
        })
        .expect("whole exact term is inside the chunk");

        term.canonical_bytes.clear();
        assert!(
            term.validate_within(&SourceSpan {
                start_byte: 10,
                end_byte: 30,
            })
            .is_err()
        );

        term.canonical_bytes = b"module::symbol".to_vec();
        term.span.end_byte = 31;
        assert!(
            term.validate_within(&SourceSpan {
                start_byte: 10,
                end_byte: 30,
            })
            .is_err()
        );
    }

    #[test]
    fn public_technical_constructor_rejects_wrong_kind_and_contextual_terms() {
        let span = |value: &[u8]| SourceSpan {
            start_byte: 0,
            end_byte: value.len() as u64,
        };
        for (kind, value) in [
            (ExactTechnicalTermKindV1::QualifiedName, b"plain".as_slice()),
            (ExactTechnicalTermKindV1::Path, b"not-a-path".as_slice()),
            (
                ExactTechnicalTermKindV1::CompilerErrorCode,
                b"A1234".as_slice(),
            ),
            (
                ExactTechnicalTermKindV1::RuntimeErrorCode,
                b"E_NOT_A_RUNTIME_CODE".as_slice(),
            ),
            (ExactTechnicalTermKindV1::CliFlag, b"--UPPER".as_slice()),
            (
                ExactTechnicalTermKindV1::ToolName,
                b"unknown-tool".as_slice(),
            ),
            (
                ExactTechnicalTermKindV1::ConfigurationKey,
                b"two.parts".as_slice(),
            ),
            (
                ExactTechnicalTermKindV1::CommitIdentifier,
                b"deadbeef".as_slice(),
            ),
            (
                ExactTechnicalTermKindV1::CompilerErrorText,
                b"arbitrary prose".as_slice(),
            ),
            (
                ExactTechnicalTermKindV1::RuntimeErrorText,
                b"arbitrary prose".as_slice(),
            ),
        ] {
            assert!(
                ExactTechnicalTermV1::technical(kind, value.to_vec(), span(value)).is_err(),
                "{kind:?} accepted wrong-kind bytes"
            );
        }
    }

    #[test]
    fn chunk_validation_rejects_noncanonical_exact_term_order() {
        let mut chunk = CodeSearchChunkV1 {
            id: id("chunk.fixture"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: id("generation.fixture"),
                file_occurrence_id: id("file.fixture"),
                symbol_occurrence_id: Some(id("symbol.fixture")),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 20,
                },
                grain: CodeSearchChunkGrainV1::SymbolBody,
                ordinal: 0,
            },
            content_digest: id(&digest('a')),
            language_descriptor_revision: id("descriptor.v1"),
            chunker_revision: id("chunker.v1"),
            sanitizer_revision: id("sanitizer.v1"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Internal,
                policy_revision: id("policy.v1"),
            },
            exact_terms: vec![
                ExactTechnicalTermV1 {
                    kind: ExactTechnicalTermKindV1::WholeSymbol,
                    original_bytes: b"later".to_vec(),
                    canonical_bytes: b"later".to_vec(),
                    span: SourceSpan {
                        start_byte: 10,
                        end_byte: 15,
                    },
                    symbol_occurrence_id: Some(id("symbol.fixture")),
                },
                ExactTechnicalTermV1 {
                    kind: ExactTechnicalTermKindV1::WholeSymbol,
                    original_bytes: b"early".to_vec(),
                    canonical_bytes: b"early".to_vec(),
                    span: SourceSpan {
                        start_byte: 0,
                        end_byte: 5,
                    },
                    symbol_occurrence_id: Some(id("symbol.fixture")),
                },
            ],
            subtokens: vec!["later".to_owned(), "early".to_owned()],
            sanitized_text: BoundedSanitizedText::new("early.....later.....").unwrap(),
        };
        assert!(chunk.validate().is_err());

        chunk.exact_terms.reverse();
        chunk
            .validate()
            .expect("source-ordered exact terms validate");
        let decoded: CodeSearchChunkV1 =
            serde_json::from_slice(&serde_json::to_vec(&chunk).unwrap()).unwrap();
        assert_eq!(decoded, chunk);

        chunk.exact_terms[0].original_bytes = b"wrong".to_vec();
        chunk.exact_terms[0].canonical_bytes = b"wrong".to_vec();
        assert!(
            chunk.validate().is_err(),
            "a term cannot claim bytes that differ from its sanitized source span"
        );

        chunk.exact_terms[0].original_bytes = b"early".to_vec();
        chunk.exact_terms[0].canonical_bytes = b"wrong".to_vec();
        assert!(
            chunk.validate().is_err(),
            "a term cannot claim a canonical form that its type does not derive"
        );
    }

    #[test]
    fn forged_serialized_whole_symbol_term_is_rejected() {
        let chunk = CodeSearchChunkV1 {
            id: id("chunk.forged"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: id("generation.fixture"),
                file_occurrence_id: id("file.fixture"),
                symbol_occurrence_id: Some(id("symbol.real")),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 22,
                },
                grain: CodeSearchChunkGrainV1::SymbolSignature,
                ordinal: 0,
            },
            content_digest: id(&digest('a')),
            language_descriptor_revision: id("descriptor.v1"),
            chunker_revision: id("chunker.v1"),
            sanitizer_revision: id("sanitizer.v1"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Internal,
                policy_revision: id("policy.v1"),
            },
            exact_terms: Vec::new(),
            subtokens: vec!["comment".to_owned(), "fake".to_owned()],
            sanitized_text: BoundedSanitizedText::new("// fn comment_fake() {}").unwrap(),
        };
        let mut wire = serde_json::to_value(chunk).unwrap();
        wire["exact_terms"] = serde_json::json!([{
            "kind": "whole_symbol",
            "original_bytes": [99, 111, 109, 109, 101, 110, 116, 95, 102, 97, 107, 101],
            "canonical_bytes": [99, 111, 109, 109, 101, 110, 116, 95, 102, 97, 107, 101],
            "span": { "start_byte": 6, "end_byte": 18 }
        }]);

        assert!(
            serde_json::from_value::<CodeSearchChunkV1>(wire.clone()).is_err(),
            "serialized input cannot forge parser-owned WholeSymbol evidence"
        );

        wire["exact_terms"][0]["symbol_occurrence_id"] = serde_json::json!("symbol.forged");
        assert!(
            serde_json::from_value::<CodeSearchChunkV1>(wire).is_err(),
            "serialized symbol evidence must match the chunk occurrence"
        );
    }

    #[test]
    fn changed_chunk_partitions_are_disjoint_typed_and_canonical() {
        let valid = changed_set();
        valid.validate().expect("valid change partition");

        let mut duplicate = valid.clone();
        duplicate
            .deleted
            .push(change("chunk.added", Some('a'), None));
        duplicate.manifest_digest = duplicate.compute_digest().unwrap();
        assert!(duplicate.validate().is_err());

        let mut malformed_reuse = valid.clone();
        malformed_reuse.reused[0].current_digest = Some(id(&digest('d')));
        malformed_reuse.manifest_digest = malformed_reuse.compute_digest().unwrap();
        assert!(malformed_reuse.validate().is_err());

        let mut mixed_generation = valid.clone();
        mixed_generation.from_generation = Some(mixed_generation.to_generation.clone());
        mixed_generation.manifest_digest = mixed_generation.compute_digest().unwrap();
        assert!(mixed_generation.validate().is_err());
    }

    #[test]
    fn changed_chunk_digest_rejects_reordering_and_tampering() {
        let mut changes = changed_set();
        changes.added_or_changed = vec![
            change("chunk.z", None, Some('d')),
            change("chunk.a", None, Some('e')),
        ];
        changes.manifest_digest = changes.compute_digest().unwrap();
        assert!(changes.validate().is_err());

        let mut tampered = changed_set();
        tampered.to_generation = id("generation.3");
        assert!(matches!(
            tampered.validate(),
            Err(DomainError::DigestMismatch)
        ));
    }

    #[test]
    fn capability_manifest_requires_canonical_vectors_and_digest() {
        let valid = capability_manifest();
        valid.validate().expect("canonical capability manifest");

        let mut duplicate = valid.clone();
        duplicate.supported_languages.push(id("rust"));
        duplicate.manifest_digest = duplicate.compute_digest().unwrap();
        assert!(duplicate.validate().is_err());

        let mut reordered = valid.clone();
        reordered.available_grains.reverse();
        reordered.manifest_digest = reordered.compute_digest().unwrap();
        assert!(reordered.validate().is_err());

        let mut tampered = valid;
        tampered.privacy_key_epoch = 2;
        assert!(matches!(
            tampered.validate(),
            Err(DomainError::DigestMismatch)
        ));
    }

    #[test]
    fn language_descriptor_requires_canonical_extension_order() {
        let descriptor = super::super::language::LanguageDescriptorV1 {
            language: LanguageId::new("rust").unwrap(),
            descriptor_revision: id("descriptor.v1"),
            grammar_revision: id("grammar.v1"),
            extractor_revision: id("extractor.v1"),
            aliases: vec!["rs".to_owned()],
            extensions: vec!["rs".to_owned(), "rlib".to_owned()],
            root_markers: vec!["Cargo.toml".to_owned()],
            expando: super::super::language::ExpandoBehaviorV1::MarkGenerated,
            stable_member_spans: true,
            capabilities: super::super::language::LanguageCapabilitySetV1::default(),
        };
        assert!(descriptor.validate().is_err());
    }
}
