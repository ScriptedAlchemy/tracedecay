//! Storage-neutral code-search chunk and projection contracts.
//!
//! These values are immutable logical records, not rows coupled to a lexical
//! table or vendor index. Chunks are the replayable source for lexical and
//! graph projections; a projection never becomes source or symbol authority.
//!
//! Code search does not define parallel ranking, fusion-profile,
//! contribution, candidate, cursor, or hydration types here; those live in
//! `crate::retrieval`.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::research::id::{ManifestDigest, SanitizationReceiptId};
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
const CODE_SOURCE_FULL_REPLAY_DIGEST_DOMAIN: &str = "tracedecay.code-source-full-replay.v1";
const CODE_INDEX_CAPABILITY_MANIFEST_DIGEST_DOMAIN: &str = "tracedecay.code-index-capability.v1";
pub const PROJECTION_PUBLICATION_SEPARATOR: &str = "tracedecay.projection-batch-receipt.v1";

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
///
/// Wire form: the source bytes travel as `original_text` when they are UTF-8
/// (every parser-emitted term is a slice of sanitized text, so this is the
/// production case) and as an `original_bytes` array otherwise. The canonical
/// bytes are a pure function of `kind` and the original bytes, so they are
/// derived on read rather than persisted. Readers accept the earlier shape
/// (`original_bytes` array plus `canonical_bytes` array) unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExactTechnicalTermV1 {
    kind: ExactTechnicalTermKindV1,
    original_bytes: Vec<u8>,
    canonical_bytes: Vec<u8>,
    span: SourceSpan,
    symbol_occurrence_id: Option<SymbolOccurrenceId>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ExactTechnicalTermWireRefV1<'a> {
    kind: ExactTechnicalTermKindV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    original_bytes: Option<&'a [u8]>,
    span: SourceSpan,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol_occurrence_id: Option<&'a SymbolOccurrenceId>,
}

impl Serialize for ExactTechnicalTermV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let original_text = std::str::from_utf8(&self.original_bytes).ok();
        ExactTechnicalTermWireRefV1 {
            kind: self.kind,
            original_text,
            original_bytes: original_text
                .is_none()
                .then_some(self.original_bytes.as_slice()),
            span: self.span,
            symbol_occurrence_id: self.symbol_occurrence_id.as_ref(),
        }
        .serialize(serializer)
    }
}

impl ExactTechnicalTermV1 {
    fn canonical_bytes_for(kind: ExactTechnicalTermKindV1, original_bytes: &[u8]) -> Vec<u8> {
        match kind {
            ExactTechnicalTermKindV1::CliFlag
            | ExactTechnicalTermKindV1::ConfigurationKey
            | ExactTechnicalTermKindV1::ToolName
            | ExactTechnicalTermKindV1::CommitIdentifier => original_bytes.to_ascii_lowercase(),
            _ => original_bytes.to_vec(),
        }
    }
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

    /// Rebuild a term from the parts an admitted projection persisted. This
    /// is the binary-storage counterpart of the wire deserializer: the same
    /// shape validation runs, and the canonical bytes are re-derived rather
    /// than trusted.
    pub fn from_persisted_parts(
        kind: ExactTechnicalTermKindV1,
        original_bytes: Vec<u8>,
        span: SourceSpan,
        symbol_occurrence_id: Option<SymbolOccurrenceId>,
    ) -> Result<Self, DomainError> {
        Self::from_parts(kind, original_bytes, span, symbol_occurrence_id)
    }

    fn from_parts(
        kind: ExactTechnicalTermKindV1,
        original_bytes: Vec<u8>,
        span: SourceSpan,
        symbol_occurrence_id: Option<SymbolOccurrenceId>,
    ) -> Result<Self, DomainError> {
        let canonical_bytes = Self::canonical_bytes_for(kind, &original_bytes);
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
        if self.canonical_bytes != Self::canonical_bytes_for(self.kind, &self.original_bytes) {
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
            #[serde(default)]
            original_text: Option<String>,
            #[serde(default)]
            original_bytes: Option<Vec<u8>>,
            /// Retained only for terms persisted before the canonical bytes
            /// became derived; when present it must still recompute.
            #[serde(default)]
            canonical_bytes: Option<Vec<u8>>,
            span: SourceSpan,
            #[serde(default)]
            symbol_occurrence_id: Option<SymbolOccurrenceId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let original_bytes = match (wire.original_text, wire.original_bytes) {
            (Some(text), None) => text.into_bytes(),
            (None, Some(bytes)) => bytes,
            (Some(_), Some(_)) | (None, None) => {
                return Err(serde::de::Error::custom(
                    "exact technical term must carry exactly one of original_text or original_bytes",
                ));
            }
        };
        let canonical_bytes = Self::canonical_bytes_for(wire.kind, &original_bytes);
        if wire
            .canonical_bytes
            .is_some_and(|persisted| persisted != canonical_bytes)
        {
            return Err(serde::de::Error::custom(DomainError::NonCanonical {
                field: "exact technical term canonical bytes",
            }));
        }
        let term = Self {
            kind: wire.kind,
            original_bytes,
            canonical_bytes,
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

/// The two source identities sealed by one code generation.
///
/// The incremental digest authenticates the physical generation transition
/// and all three change partitions. The full-replay digest authenticates only
/// the complete ordered chunk corpus, so byte-identical source remains the
/// same across generation-id churn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeGenerationSourceCommitmentsV1 {
    pub incremental_manifest_digest: ManifestDigest,
    pub full_replay_digest: ManifestDigest,
}

#[derive(Serialize)]
struct CodeSourceFullReplayDigestInput<'a> {
    domain: &'static str,
    chunks: &'a [(CodeSearchChunkId, ContentDigest)],
}

/// Digest a complete source corpus in canonical chunk-identity order.
pub fn code_source_full_replay_digest(
    chunks: &[(CodeSearchChunkId, ContentDigest)],
) -> Result<ManifestDigest, DomainError> {
    for (chunk, digest) in chunks {
        chunk.validate()?;
        digest.validate()?;
    }
    if chunks.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(DomainError::NonCanonical {
            field: "full replay source chunk order",
        });
    }
    canonical_sha256(&CodeSourceFullReplayDigestInput {
        domain: CODE_SOURCE_FULL_REPLAY_DIGEST_DOMAIN,
        chunks,
    })
}

impl CodeGenerationSourceCommitmentsV1 {
    pub fn from_changed_chunks(
        changes: &ChangedCodeChunkSetV1,
        full_source: &[(CodeSearchChunkId, ContentDigest)],
    ) -> Result<Self, DomainError> {
        changes.validate()?;
        Ok(Self {
            incremental_manifest_digest: changes.manifest_digest.clone(),
            full_replay_digest: code_source_full_replay_digest(full_source)?,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.incremental_manifest_digest.validate()?;
        self.full_replay_digest.validate()
    }

    pub fn validate_for_source(
        &self,
        full_source: &[(CodeSearchChunkId, ContentDigest)],
    ) -> Result<(), DomainError> {
        self.validate()?;
        if self.full_replay_digest != code_source_full_replay_digest(full_source)? {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
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

/// Identity of one projection profile: kind, schema revision, and a
/// canonical profile digest. Adapters cannot define a second
/// projection-key identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct ProjectionKeyV1 {
    pub kind: ProjectionKindV1,
    pub schema_revision: String,
    pub profile_digest: ManifestDigest,
}

/// The projection families the code index recognizes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionKindV1 {
    Lexical,
    Graph,
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

pub fn projection_batch_publication_digest(
    batch: &ProjectionBatchReceiptV1,
) -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&(
        PROJECTION_PUBLICATION_SEPARATOR,
        &batch.target_projection_key,
        &batch.request_digest,
        &batch.source_generation,
        &batch.source_manifest_digest,
        &batch.receipts,
        batch.reused_count,
    ))
}

/// The mandatory base capability manifest. Consumers must reject a missing,
/// incompatible, mixed-generation, or unauthorized base manifest before
/// candidate production.
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

/// Capability identity is deliberately generation-independent.
///
/// `generation_id` is provenance, not capability: two generations that sealed
/// the same source under the same runtime revisions, coverage, sanitization
/// receipts, and privacy identity offer the identical indexing authority, and
/// a checkout that reseals the same commit must not be refused as
/// capability-incompatible. The manifest still carries its `generation_id`,
/// and `CodeIndexPublishedGenerationV1` still refuses a capability manifest
/// naming a different generation than its own, so the pairing stays bound —
/// by that invariant rather than by this digest.
#[derive(Serialize)]
struct CodeIndexCapabilityManifestDigestInput<'a> {
    domain: &'static str,
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
    fn source_commitments_preserve_incremental_and_full_replay_identities() {
        let incremental = changed_set();
        let full_source = vec![
            (id("chunk.added"), id(&digest('a'))),
            (id("chunk.reused"), id(&digest('c'))),
        ];
        let first =
            CodeGenerationSourceCommitmentsV1::from_changed_chunks(&incremental, &full_source)
                .expect("source commitments");

        assert_eq!(
            first.incremental_manifest_digest,
            incremental.compute_digest().expect("incremental digest")
        );
        assert_ne!(
            first.incremental_manifest_digest, first.full_replay_digest,
            "the generation-bound changed-set digest is not a full replay identity"
        );

        let mut republished = incremental;
        republished.from_generation = Some(id("generation.8"));
        republished.to_generation = id("generation.9");
        republished.manifest_digest = republished.compute_digest().expect("republished digest");
        let second =
            CodeGenerationSourceCommitmentsV1::from_changed_chunks(&republished, &full_source)
                .expect("republished source commitments");

        assert_ne!(
            first.incremental_manifest_digest, second.incremental_manifest_digest,
            "incremental identity must retain its generation watermarks"
        );
        assert_eq!(
            first.full_replay_digest, second.full_replay_digest,
            "full replay identity must survive generation-id churn over identical source"
        );

        let mut tampered = first;
        tampered.full_replay_digest = id(&digest('f'));
        assert!(tampered.validate_for_source(&full_source).is_err());
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
    fn capability_identity_survives_a_new_generation_but_not_a_new_capability() {
        let sealed = capability_manifest();
        // The same source resealed under a new generation id — a checkout, a
        // detached HEAD, a rollback that mints a fresh generation.
        let mut resealed = sealed.clone();
        resealed.generation_id = id("generation.v1.0cbc773a.00000002.resealed");
        resealed.manifest_digest = resealed.compute_digest().expect("digest computable");
        assert_eq!(
            sealed.manifest_digest, resealed.manifest_digest,
            "a new generation id must not change capability identity"
        );

        // A real capability or coverage change still refuses.
        for changed in [
            {
                let mut changed = sealed.clone();
                changed.chunker_revision = id("chunker.v2");
                changed
            },
            {
                let mut changed = sealed.clone();
                changed.privacy_key_epoch = 2;
                changed
            },
            {
                let mut changed = sealed.clone();
                changed.source_coverage.files_eligible = 2;
                changed
            },
            {
                let mut changed = sealed.clone();
                changed.sanitization_receipts = vec![id("receipt.other")];
                changed
            },
        ] {
            assert_ne!(
                sealed.manifest_digest,
                changed.compute_digest().expect("digest computable"),
                "a capability, privacy, coverage, or sanitization change must refuse reuse"
            );
        }
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
