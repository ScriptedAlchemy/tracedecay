//! Shared typed non-canonical cause taxonomy for chunking and increment
//! planning.
//!
//! Callers must not dump structured fields into free-form `format!` strings and
//! treat those strings as a schema. Stable [`NonCanonicalReasonCodeV1`] values
//! plus optional structured [`NonCanonicalDetailKeyV1`] details are the wire
//! and test contract; [`Display`] is derived from those fields for humans.

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;
use tracedecay_domain::research::DomainError;

/// Stable reason codes for non-canonical chunk / increment failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NonCanonicalReasonCodeV1 {
    IdentityField,
    IdentityValidation,
    DocumentChunkMembershipMismatch,
    ExactAuthorityMismatch,
    ExactAuthoritySetMismatch,
    ExactAuthorityDuplicateIdentity,
    ExactAuthorityCarryChanged,
    CarriedChunkMissingSymbol,
    LineageSymbolsUnordered,
    LineageSymbolMissingChunk,
    FileGraphNotCanonical,
    UnresolvedReferenceMissingAnchor,
    UnresolvedReferencesUnordered,
    UnresolvedReferenceEmptyName,
    UnresolvedReferenceEmptySpan,
    CloneBodyOccurrenceOrder,
    CloneBodyEmptyPath,
    CloneBodyEmptySpan,
    CloneBodyPayloadDigestMismatch,
    CloneBodyPayloadNotCanonical,
    CloneBodyMissingSymbol,
    CloneBodyNotBoundToIndexedSymbol,
    ImportEmptyPathOrSpecifier,
    ImportEmptyBindingName,
    ImportEmptySourceSpan,
    ImportExceedsFileExtent,
    ImportMultiFile,
    ImportUnordered,
    ImportNamespaceMismatch,
    ImportModuleKindMismatch,
    ImportAuthorityMismatch,
    SchemaMissingEvidence,
    SchemaLanguageMismatch,
    SchemaStatusMismatch,
    SchemaExceedsFileExtent,
    SchemaUnordered,
    DuplicateParserNodeId,
    GraphRematerializeFailed,
    SanitizedBytesNotUtf8,
    ParsedPrefixHostFit,
    ParsedPrefixNotUtf8Boundary,
    SymbolMissingPublishedSpan,
    SymbolStartHostFit,
    SymbolEndHostFit,
    SymbolSpanNotUtf8,
    ParserImportRowsDigest,
    CarryForwardDigestMismatch,
    UnplannedReextractedEvidence,
}

impl NonCanonicalReasonCodeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IdentityField => "identity_field",
            Self::IdentityValidation => "identity_validation",
            Self::DocumentChunkMembershipMismatch => "document_chunk_membership_mismatch",
            Self::ExactAuthorityMismatch => "exact_authority_mismatch",
            Self::ExactAuthoritySetMismatch => "exact_authority_set_mismatch",
            Self::ExactAuthorityDuplicateIdentity => "exact_authority_duplicate_identity",
            Self::ExactAuthorityCarryChanged => "exact_authority_carry_changed",
            Self::CarriedChunkMissingSymbol => "carried_chunk_missing_symbol",
            Self::LineageSymbolsUnordered => "lineage_symbols_unordered",
            Self::LineageSymbolMissingChunk => "lineage_symbol_missing_chunk",
            Self::FileGraphNotCanonical => "file_graph_not_canonical",
            Self::UnresolvedReferenceMissingAnchor => "unresolved_reference_missing_anchor",
            Self::UnresolvedReferencesUnordered => "unresolved_references_unordered",
            Self::UnresolvedReferenceEmptyName => "unresolved_reference_empty_name",
            Self::UnresolvedReferenceEmptySpan => "unresolved_reference_empty_span",
            Self::CloneBodyOccurrenceOrder => "clone_body_occurrence_order",
            Self::CloneBodyEmptyPath => "clone_body_empty_path",
            Self::CloneBodyEmptySpan => "clone_body_empty_span",
            Self::CloneBodyPayloadDigestMismatch => "clone_body_payload_digest_mismatch",
            Self::CloneBodyPayloadNotCanonical => "clone_body_payload_not_canonical",
            Self::CloneBodyMissingSymbol => "clone_body_missing_symbol",
            Self::CloneBodyNotBoundToIndexedSymbol => "clone_body_not_bound_to_indexed_symbol",
            Self::ImportEmptyPathOrSpecifier => "import_empty_path_or_specifier",
            Self::ImportEmptyBindingName => "import_empty_binding_name",
            Self::ImportEmptySourceSpan => "import_empty_source_span",
            Self::ImportExceedsFileExtent => "import_exceeds_file_extent",
            Self::ImportMultiFile => "import_multi_file",
            Self::ImportUnordered => "import_unordered",
            Self::ImportNamespaceMismatch => "import_namespace_mismatch",
            Self::ImportModuleKindMismatch => "import_module_kind_mismatch",
            Self::ImportAuthorityMismatch => "import_authority_mismatch",
            Self::SchemaMissingEvidence => "schema_missing_evidence",
            Self::SchemaLanguageMismatch => "schema_language_mismatch",
            Self::SchemaStatusMismatch => "schema_status_mismatch",
            Self::SchemaExceedsFileExtent => "schema_exceeds_file_extent",
            Self::SchemaUnordered => "schema_unordered",
            Self::DuplicateParserNodeId => "duplicate_parser_node_id",
            Self::GraphRematerializeFailed => "graph_rematerialize_failed",
            Self::SanitizedBytesNotUtf8 => "sanitized_bytes_not_utf8",
            Self::ParsedPrefixHostFit => "parsed_prefix_host_fit",
            Self::ParsedPrefixNotUtf8Boundary => "parsed_prefix_not_utf8_boundary",
            Self::SymbolMissingPublishedSpan => "symbol_missing_published_span",
            Self::SymbolStartHostFit => "symbol_start_host_fit",
            Self::SymbolEndHostFit => "symbol_end_host_fit",
            Self::SymbolSpanNotUtf8 => "symbol_span_not_utf8",
            Self::ParserImportRowsDigest => "parser_import_rows_digest",
            Self::CarryForwardDigestMismatch => "carry_forward_digest_mismatch",
            Self::UnplannedReextractedEvidence => "unplanned_reextracted_evidence",
        }
    }
}

impl fmt::Display for NonCanonicalReasonCodeV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Structured detail keys for non-canonical causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NonCanonicalDetailKeyV1 {
    Field,
    Detail,
    File,
    FileOccurrenceId,
    LeftPayloadDigest,
    LeftSymbolOccurrenceId,
    LeftBound,
    RightPayloadDigest,
    RightSymbolOccurrenceId,
    RightBound,
    QualifiedName,
    Error,
}

impl NonCanonicalDetailKeyV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Detail => "detail",
            Self::File => "file",
            Self::FileOccurrenceId => "file_occurrence_id",
            Self::LeftPayloadDigest => "left_payload_digest",
            Self::LeftSymbolOccurrenceId => "left_symbol_occurrence_id",
            Self::LeftBound => "left_bound",
            Self::RightPayloadDigest => "right_payload_digest",
            Self::RightSymbolOccurrenceId => "right_symbol_occurrence_id",
            Self::RightBound => "right_bound",
            Self::QualifiedName => "qualified_name",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for NonCanonicalDetailKeyV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed non-canonical cause: stable reason code plus structured details.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub struct NonCanonicalCauseV1 {
    reason_code: NonCanonicalReasonCodeV1,
    details: BTreeMap<NonCanonicalDetailKeyV1, String>,
}

impl NonCanonicalCauseV1 {
    pub fn new(reason_code: NonCanonicalReasonCodeV1) -> Self {
        Self {
            reason_code,
            details: BTreeMap::new(),
        }
    }

    pub fn with(mut self, key: NonCanonicalDetailKeyV1, value: impl Into<String>) -> Self {
        self.details.insert(key, value.into());
        self
    }

    pub const fn reason_code(&self) -> NonCanonicalReasonCodeV1 {
        self.reason_code
    }

    pub fn details(&self) -> &BTreeMap<NonCanonicalDetailKeyV1, String> {
        &self.details
    }

    pub fn from_domain(error: DomainError) -> Self {
        let field = match &error {
            DomainError::Empty { field }
            | DomainError::NonCanonical { field }
            | DomainError::DuplicateId { field }
            | DomainError::UnknownReference { field }
            | DomainError::SnapshotMismatch { field }
            | DomainError::UnsafeText { field }
            | DomainError::InvalidRange { field } => Some(*field),
            _ => None,
        };
        match field {
            Some(field) => Self::new(NonCanonicalReasonCodeV1::IdentityField)
                .with(NonCanonicalDetailKeyV1::Field, field),
            None => Self::new(NonCanonicalReasonCodeV1::IdentityValidation)
                .with(NonCanonicalDetailKeyV1::Detail, error.to_string()),
        }
    }
}

impl fmt::Display for NonCanonicalCauseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason_code.as_str())?;
        if self.details.is_empty() {
            return Ok(());
        }
        write!(f, " {{")?;
        let mut first = true;
        for (key, value) in &self.details {
            if !first {
                write!(f, ",")?;
            }
            first = false;
            write!(f, " {}={}", key.as_str(), value)?;
        }
        write!(f, " }}")
    }
}

/// Map a domain identity validation failure into the shared cause taxonomy.
pub fn noncanonical_from_domain(error: DomainError) -> NonCanonicalCauseV1 {
    NonCanonicalCauseV1::from_domain(error)
}

/// Map an opaque displayable failure under a stable reason code.
pub fn noncanonical_detail(
    reason_code: NonCanonicalReasonCodeV1,
    error: impl fmt::Display,
) -> NonCanonicalCauseV1 {
    NonCanonicalCauseV1::new(reason_code).with(NonCanonicalDetailKeyV1::Detail, error.to_string())
}
