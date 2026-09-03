//! Storage-neutral code-search chunk and projection contracts.
//!
//! These values are immutable logical records, not rows coupled to a lexical
//! table, vector table, or vendor index. Chunks are the replayable source for
//! lexical and later model/version-specific projections; embeddings never
//! become source or symbol authority.
//!
//! Code search does not define parallel ranking, fusion-profile,
//! contribution, candidate, cursor, or hydration types here; those live in
//! `crate::retrieval`.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::research::id::{ManifestDigest, PrivacyDomainId, SanitizationReceiptId};
use crate::research::{DomainError, canonical_sha256};

use super::identity::{
    ChunkerRevision, CodeGenerationId, CodeSearchChunkId, ContentDigest, FileOccurrenceId,
    LanguageDescriptorRevision, PolicyRevisionId, QueryNormalizationRevision, SanitizerRevision,
    SourceSpan, SymbolOccurrenceId,
};
use super::token_grammar;

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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedSanitizedText(Arc<str>);

impl BoundedSanitizedText {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.len() > MAX_CHUNK_TEXT_BYTES {
            return Err(DomainError::UnsafeText {
                field: "bounded sanitized chunk text",
            });
        }
        Ok(Self(Arc::from(value)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for BoundedSanitizedText {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
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

/// The five deterministic chunk grains. Symbol signatures and
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

/// The classification of one whole exact technical term. Whole exact terms
/// and language-profiled subtokens are distinct fields.
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

/// One whole exact technical term extracted as evidence. Extraction
/// evidence only; protected lexical policy is applied separately.
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
    let valid = match kind {
        ExactTechnicalTermKindV1::QualifiedName => token_grammar::is_qualified_name_token(text),
        ExactTechnicalTermKindV1::Path => token_grammar::is_path_token(text),
        ExactTechnicalTermKindV1::CompilerErrorCode => {
            token_grammar::is_compiler_error_code_token(text)
        }
        ExactTechnicalTermKindV1::RuntimeErrorCode => {
            token_grammar::is_runtime_error_code_token(text)
        }
        ExactTechnicalTermKindV1::CliFlag => token_grammar::is_cli_flag_token(text),
        ExactTechnicalTermKindV1::ToolName => token_grammar::is_tool_name_token(text),
        ExactTechnicalTermKindV1::ConfigurationKey => {
            token_grammar::is_configuration_key_token(text)
        }
        ExactTechnicalTermKindV1::CommitIdentifier => {
            token_grammar::is_commit_identifier_token(text)
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SensitivityDecision {
    pub level: SensitivityLevelV1,
    pub policy_revision: PolicyRevisionId,
}

/// Sensitivity levels; privacy-domain or key-epoch changes rebuild canonical
/// eligibility when policy output changes.
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

/// Ordered changed/reused/deleted chunk manifest between two generations.
/// Downstream projectors prove exactly which generation-bound chunks they
/// consumed, skipped, replaced, or removed. A no-op generation
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

/// How one canonical chunk becomes the text handed to the embedding model.
///
/// Composition is projection identity: the same chunk under two compositions
/// is two different tensor inputs, so their vectors never share a projection
/// key. On the wire the field is absent for `SanitizedText`, which keeps the
/// shipped composition's persisted projection digests byte-identical.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingDocumentCompositionV1 {
    /// The chunk's sanitized text, unchanged.
    #[default]
    SanitizedText,
    /// A deterministic symbol-context header (symbol kind and name, enclosing
    /// scope) ahead of the sanitized text, with the whole document bounded by
    /// [`EmbeddingProjectionKeyV1::document_byte_budget`].
    SymbolContextHeader,
}

impl EmbeddingDocumentCompositionV1 {
    pub const fn is_sanitized_text(&self) -> bool {
        matches!(self, Self::SanitizedText)
    }
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
/// [`ProjectionKeyV1`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingProjectionKeyV1 {
    pub model_artifact_digest: ManifestDigest,
    pub tokenizer_digest: ManifestDigest,
    pub config_digest: ManifestDigest,
    pub query_instruction_digest: Option<ManifestDigest>,
    pub document_instruction_digest: Option<ManifestDigest>,
    /// How each chunk's tensor input is composed. Skipped on the wire for
    /// `SanitizedText` so the shipped composition's canonical digest — and
    /// every vector generation persisted under it — is unchanged; every other
    /// composition serializes and therefore mints its own projection key.
    #[serde(
        default,
        skip_serializing_if = "EmbeddingDocumentCompositionV1::is_sanitized_text"
    )]
    pub document_composition: EmbeddingDocumentCompositionV1,
    pub pooling: EmbeddingPoolingV1,
    pub truncation_side: EmbeddingTruncationSideV1,
    pub truncation_length: u32,
    /// Exact number of documents in every full inference tensor. The final
    /// tensor may be shorter. This is projection identity because changing
    /// the padded tensor shape can change floating-point vector bytes.
    pub inference_batch_size: u32,
    /// Exact sanitized-text byte ceiling for every inference group. This is
    /// projection identity because it can split a count-valid group and alter
    /// the native runtime's tensor boundaries.
    pub inference_batch_bytes: u32,
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
        if self.inference_batch_size == 0 {
            return Err(DomainError::Empty {
                field: "embedding inference batch size",
            });
        }
        if self.inference_batch_bytes == 0 {
            return Err(DomainError::Empty {
                field: "embedding inference batch byte ceiling",
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

    /// Byte budget of one composed embedding document: the per-document share
    /// of the admitted inference group byte ceiling. A full group of
    /// `inference_batch_size` documents each within this budget therefore
    /// always fits `inference_batch_bytes`, so composing documents never moves
    /// the canonical group boundaries derived from the chunks' sanitized text.
    pub fn document_byte_budget(&self) -> Result<usize, DomainError> {
        if self.inference_batch_size == 0 {
            return Err(DomainError::Empty {
                field: "embedding inference batch size",
            });
        }
        let budget = self.inference_batch_bytes / self.inference_batch_size;
        if budget == 0 {
            return Err(DomainError::Empty {
                field: "embedding document byte budget",
            });
        }
        usize::try_from(budget).map_err(|_| DomainError::NonCanonical {
            field: "embedding document byte budget",
        })
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
    /// Approximate HNSW candidate generation over the serving generation's
    /// persisted vector index, exact-rescored with the canonical distance.
    /// Candidate coverage is index-bounded: published distances stay exact,
    /// but the candidate set is not guaranteed to equal a full scan's.
    AnnHnswExactRescore,
}

/// Deterministic adaptive recall depth for approximate candidate generation.
///
/// An approximate index answers with a ranked prefix whose depth the caller
/// chooses. Too shallow a prefix loses recall the exact rescore can never
/// recover; too deep a prefix rescores rows that cannot enter the retained
/// top-k. Instead of a fixed oversample the caller grows the depth
/// geometrically: every pass asks for the ranks past the previous depth, and
/// the loop continues only while the pass was *saturated* (the index returned
/// the full increment, so deeper ranks exist) and the pool is still below
/// `target(retained_cap)`. `max_depth` bounds the deepest pass.
///
/// The sequence of depths and the stop reason are pure functions of the
/// policy, the retained cap, and the row counts each pass returned, so two
/// executions over the same index are byte-identical. Every field is committed
/// into the owning search-index profile's parameters digest.
///
/// Only the semantic ANN path uses this policy. The exact and lexical lanes
/// read posting ports that return an exact, complete ranking prefix bounded by
/// the lane cap: there is no approximate candidate stage whose recall a deeper
/// request could recover, so a deeper read there only costs work and changes
/// nothing the fusion stage observes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct AdaptiveRecallDepthPolicyV1 {
    /// Ranking depth of the first pass.
    pub initial_depth: u32,
    /// Multiplier applied to the depth between saturated passes; at least 2.
    pub growth_factor: u32,
    /// Deepest rank any pass may request.
    pub max_depth: u32,
    /// The pool target is `retained_cap × target_multiplier`, floored below.
    pub target_multiplier: u32,
    /// Smallest pool the loop aims for regardless of the retained cap.
    pub target_floor: u32,
}

/// Why an adaptive recall loop stopped after a pass.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveRecallStopV1 {
    /// The pool reached the policy target.
    TargetReached,
    /// The pass returned fewer rows than requested: the index holds no deeper
    /// ranks, so growing the depth could not add candidates.
    Unsaturated,
    /// The pass already searched `max_depth` and the pool is below target.
    MaxDepth,
}

/// The policy's decision after one pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptiveRecallStepV1 {
    /// Ask the next pass for the ranks in `(searched_depth, next_depth]`.
    Grow {
        next_depth: u32,
    },
    Stop(AdaptiveRecallStopV1),
}

impl AdaptiveRecallDepthPolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.initial_depth == 0 {
            return Err(DomainError::Empty {
                field: "adaptive recall initial depth",
            });
        }
        if self.growth_factor < 2 {
            return Err(DomainError::NonCanonical {
                field: "adaptive recall growth factor",
            });
        }
        if self.max_depth < self.initial_depth {
            return Err(DomainError::NonCanonical {
                field: "adaptive recall max depth",
            });
        }
        if self.target_multiplier == 0 {
            return Err(DomainError::Empty {
                field: "adaptive recall target multiplier",
            });
        }
        if self.target_floor == 0 {
            return Err(DomainError::Empty {
                field: "adaptive recall target floor",
            });
        }
        Ok(())
    }

    /// Pool size the loop aims for when `retained_cap` rows will be kept.
    pub const fn target(&self, retained_cap: u32) -> u32 {
        let scaled = retained_cap.saturating_mul(self.target_multiplier);
        if scaled > self.target_floor {
            scaled
        } else {
            self.target_floor
        }
    }

    /// Depth of the first pass.
    pub const fn first_depth(&self) -> u32 {
        if self.initial_depth < self.max_depth {
            self.initial_depth
        } else {
            self.max_depth
        }
    }

    /// Decide what follows a pass that searched `searched_depth`, was asked
    /// for `requested` new ranks, answered `returned` rows, and left `pool`
    /// distinct candidates gathered so far.
    pub const fn step(
        &self,
        retained_cap: u32,
        searched_depth: u32,
        requested: u32,
        returned: u32,
        pool: u32,
    ) -> AdaptiveRecallStepV1 {
        if pool >= self.target(retained_cap) {
            return AdaptiveRecallStepV1::Stop(AdaptiveRecallStopV1::TargetReached);
        }
        if returned < requested {
            return AdaptiveRecallStepV1::Stop(AdaptiveRecallStopV1::Unsaturated);
        }
        if searched_depth >= self.max_depth {
            return AdaptiveRecallStepV1::Stop(AdaptiveRecallStopV1::MaxDepth);
        }
        let grown = searched_depth.saturating_mul(self.growth_factor);
        let next_depth = if grown < self.max_depth {
            grown
        } else {
            self.max_depth
        };
        AdaptiveRecallStepV1::Grow { next_depth }
    }
}

/// The recall policy the canonical ANN profile commits to: start at 200
/// ranks, double while saturated and under `max(cap × 5, 50)`, never past
/// 2000. The ceiling stays under the vector store's 4096-row hard search
/// bound so no pass is ever silently clamped by the port.
pub const SEMANTIC_ANN_RECALL_POLICY_V1: AdaptiveRecallDepthPolicyV1 =
    AdaptiveRecallDepthPolicyV1 {
        initial_depth: 200,
        growth_factor: 2,
        max_depth: 2_000,
        target_multiplier: 5,
        target_floor: 50,
    };

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

    /// The canonical HNSW candidate-generation profile: exact rescoring over
    /// candidates gathered under [`SEMANTIC_ANN_RECALL_POLICY_V1`].
    pub fn ann_hnsw_exact_rescore_v1() -> Result<Self, DomainError> {
        Self::ann_hnsw_exact_rescore(&SEMANTIC_ANN_RECALL_POLICY_V1)
    }

    /// HNSW candidate generation with exact rescoring under one adaptive
    /// recall policy. The parameters digest commits to every policy field, so
    /// a tuning change mints a new index identity instead of silently
    /// shifting served candidate sets.
    pub fn ann_hnsw_exact_rescore(
        recall_policy: &AdaptiveRecallDepthPolicyV1,
    ) -> Result<Self, DomainError> {
        recall_policy.validate()?;
        Ok(Self {
            kind: SemanticSearchIndexKindV1::AnnHnswExactRescore,
            implementation_revision: "semantic.ann-hnsw-exact-rescore.v1".to_owned(),
            parameters_digest: canonical_sha256(&(
                "tracedecay.semantic-ann-hnsw-exact-rescore-parameters.v1",
                recall_policy,
                "exact-rescore-canonical-distance-then-anchor",
                "flat-fallback-on-missing-or-incomplete-index",
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

/// Identity of one projection profile: kind, schema revision, and a
/// canonical profile digest. `EmbeddingProjectionKeyV1` is the typed
/// semantic profile whose digest occupies `profile_digest`; adapters cannot
/// define a second projection-key identity.
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "outcome", content = "reason", rename_all = "snake_case")]
pub enum ProjectionOutcomeV1 {
    Applied,
    Reused,
    Skipped { reason: String },
    Failed { reason: String },
}

/// One per-chunk projection receipt. Receipts are deterministic
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

/// The complete receipt for one projection batch. Failed or
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

/// The mandatory base capability manifest. Consumers must reject a missing,
/// incompatible, mixed-generation, or unauthorized base manifest before
/// candidate production. The optional semantic manifest augments this base;
/// its absence cannot block authorized lexical/graph retrieval.
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

    fn embedding_key() -> EmbeddingProjectionKeyV1 {
        EmbeddingProjectionKeyV1 {
            model_artifact_digest: id(&digest('a')),
            tokenizer_digest: id(&digest('b')),
            config_digest: id(&digest('c')),
            query_instruction_digest: None,
            document_instruction_digest: None,
            document_composition: EmbeddingDocumentCompositionV1::SanitizedText,
            pooling: EmbeddingPoolingV1::Mean,
            truncation_side: EmbeddingTruncationSideV1::Right,
            truncation_length: 512,
            inference_batch_size: 8,
            inference_batch_bytes: 8 * 512 * 4,
            runtime_backend: "fastembed-ort".to_owned(),
            runtime_build_revision: "ort-fixture".to_owned(),
            device_class: EmbeddingDeviceClassV1::Cpu,
            dimensions: 8,
            metric: EmbeddingMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            precision: EmbeddingPrecisionV1::Fp32,
            chunk_schema_revision: "code-search-chunk.v1".to_owned(),
            chunker_revision: id("chunker.v1"),
            privacy_domain: PrivacyDomainId::new("privacy.fixture").unwrap(),
            privacy_key_epoch: 1,
        }
    }

    #[test]
    fn document_composition_is_projection_identity() {
        let sanitized = embedding_key();
        let same = embedding_key();
        let mut header = embedding_key();
        header.document_composition = EmbeddingDocumentCompositionV1::SymbolContextHeader;

        let sanitized_digest = sanitized.canonical_digest().expect("digest");
        assert_eq!(sanitized_digest, same.canonical_digest().expect("digest"));
        assert_ne!(sanitized_digest, header.canonical_digest().expect("digest"));
        assert_ne!(
            sanitized.projection_key().expect("projection key"),
            header.projection_key().expect("projection key")
        );

        let sanitized_json = serde_json::to_string(&sanitized).expect("JSON");
        assert!(
            !sanitized_json.contains("document_composition"),
            "the shipped composition must serialize exactly as before it existed"
        );
        let header_json = serde_json::to_string(&header).expect("JSON");
        assert!(header_json.contains(r#""document_composition":"symbol_context_header""#));

        let restored: EmbeddingProjectionKeyV1 =
            serde_json::from_str(&sanitized_json).expect("wire without the field");
        assert_eq!(restored, sanitized);
        let restored_header: EmbeddingProjectionKeyV1 =
            serde_json::from_str(&header_json).expect("wire with the field");
        assert_eq!(restored_header, header);
    }

    #[test]
    fn document_byte_budget_is_the_per_document_share_of_the_group_ceiling() {
        let key = embedding_key();
        assert_eq!(key.document_byte_budget().expect("budget"), 512 * 4);

        let mut uneven = embedding_key();
        uneven.inference_batch_bytes = 1_001;
        uneven.inference_batch_size = 10;
        assert_eq!(uneven.document_byte_budget().expect("budget"), 100);

        let mut starved = embedding_key();
        starved.inference_batch_bytes = 3;
        starved.inference_batch_size = 8;
        assert!(starved.document_byte_budget().is_err());
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
    fn bounded_sanitized_text_clones_share_backing_and_preserve_wire_shape() {
        let text = BoundedSanitizedText::new("pub fn retained() {}\n").expect("bounded text");
        let cloned = text.clone();
        assert_eq!(
            text.as_str().as_ptr(),
            cloned.as_str().as_ptr(),
            "chunk clones must share the retained source allocation"
        );
        let wire = serde_json::to_string(&text).expect("serialize bounded text");
        assert_eq!(wire, r#""pub fn retained() {}\n""#);
        let decoded: BoundedSanitizedText =
            serde_json::from_str(&wire).expect("deserialize bounded text");
        assert_eq!(decoded, text);
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

    #[test]
    fn adaptive_recall_policy_steps_are_pure_and_bounded() {
        let policy = SEMANTIC_ANN_RECALL_POLICY_V1;
        policy.validate().expect("canonical policy validates");
        assert_eq!(policy.first_depth(), 200);
        assert_eq!(policy.target(2), 50, "small caps use the floor");
        assert_eq!(policy.target(100), 500);

        // Target reached wins over saturation.
        assert_eq!(
            policy.step(2, 200, 200, 200, 200),
            AdaptiveRecallStepV1::Stop(AdaptiveRecallStopV1::TargetReached)
        );
        // Under target but the pass came back short: nothing deeper exists.
        assert_eq!(
            policy.step(100, 200, 200, 150, 150),
            AdaptiveRecallStepV1::Stop(AdaptiveRecallStopV1::Unsaturated)
        );
        // Under target and saturated: double.
        assert_eq!(
            policy.step(100, 200, 200, 200, 200),
            AdaptiveRecallStepV1::Grow { next_depth: 400 }
        );
        // Growth clamps at the ceiling instead of overshooting it.
        assert_eq!(
            policy.step(500, 1_600, 800, 800, 1_600),
            AdaptiveRecallStepV1::Grow { next_depth: 2_000 }
        );
        // At the ceiling, saturated and under target stops on MaxDepth.
        assert_eq!(
            policy.step(500, 2_000, 400, 400, 2_000),
            AdaptiveRecallStepV1::Stop(AdaptiveRecallStopV1::MaxDepth)
        );

        let mut degenerate = policy;
        degenerate.growth_factor = 1;
        assert!(
            degenerate.validate().is_err(),
            "a non-growing loop never terminates on growth"
        );
        let mut inverted = policy;
        inverted.max_depth = 100;
        assert!(
            inverted.validate().is_err(),
            "the ceiling must admit the first pass"
        );
    }

    #[test]
    fn ann_profile_digest_commits_to_every_recall_policy_field() {
        let canonical = SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore_v1()
            .expect("canonical ann profile");
        let again =
            SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore(&SEMANTIC_ANN_RECALL_POLICY_V1)
                .expect("same policy");
        assert_eq!(
            canonical, again,
            "identical policies mint identical identities"
        );
        assert_eq!(
            canonical.index_key().expect("key"),
            again.index_key().expect("key")
        );

        let variants: [fn(&mut AdaptiveRecallDepthPolicyV1); 5] = [
            |policy| policy.initial_depth += 1,
            |policy| policy.growth_factor += 1,
            |policy| policy.max_depth += 1,
            |policy| policy.target_multiplier += 1,
            |policy| policy.target_floor += 1,
        ];
        for tweak in variants {
            let mut tweaked = SEMANTIC_ANN_RECALL_POLICY_V1;
            tweak(&mut tweaked);
            let profile = SemanticSearchIndexProfileV1::ann_hnsw_exact_rescore(&tweaked)
                .expect("tweaked policy validates");
            assert_ne!(
                profile.parameters_digest, canonical.parameters_digest,
                "a policy change must mint a new parameters digest: {tweaked:?}"
            );
            assert_ne!(
                profile.index_key().expect("key").profile_digest,
                canonical.index_key().expect("key").profile_digest
            );
        }
    }
}
