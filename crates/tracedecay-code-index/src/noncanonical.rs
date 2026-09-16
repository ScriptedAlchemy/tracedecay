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
    IdentityValidation,
    Empty,
    DomainNonCanonical,
    DuplicateId,
    UnknownReference,
    SnapshotMismatch,
    UnsafeText,
    InvalidRange,
    InvalidConfidence,
    ActivityFacetOnActivitySubject,
    SelfSupersession,
    AuthorshipWithoutProviderLinkage,
    InvalidTimeInterval,
    InvalidRedactionCounts,
    NonCertainDeclaration,
    DigestMismatch,
    CanonicalSerialization,
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
            Self::IdentityValidation => "identity_validation",
            Self::Empty => "empty",
            Self::DomainNonCanonical => "domain_non_canonical",
            Self::DuplicateId => "duplicate_id",
            Self::UnknownReference => "unknown_reference",
            Self::SnapshotMismatch => "snapshot_mismatch",
            Self::UnsafeText => "unsafe_text",
            Self::InvalidRange => "invalid_range",
            Self::InvalidConfidence => "invalid_confidence",
            Self::ActivityFacetOnActivitySubject => "activity_facet_on_activity_subject",
            Self::SelfSupersession => "self_supersession",
            Self::AuthorshipWithoutProviderLinkage => "authorship_without_provider_linkage",
            Self::InvalidTimeInterval => "invalid_time_interval",
            Self::InvalidRedactionCounts => "invalid_redaction_counts",
            Self::NonCertainDeclaration => "non_certain_declaration",
            Self::DigestMismatch => "digest_mismatch",
            Self::CanonicalSerialization => "canonical_serialization",
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
        match error {
            DomainError::Empty { field } => Self::new(NonCanonicalReasonCodeV1::Empty)
                .with(NonCanonicalDetailKeyV1::Field, field),
            DomainError::NonCanonical { field } => {
                Self::new(NonCanonicalReasonCodeV1::DomainNonCanonical)
                    .with(NonCanonicalDetailKeyV1::Field, field)
            }
            DomainError::DuplicateId { field } => Self::new(NonCanonicalReasonCodeV1::DuplicateId)
                .with(NonCanonicalDetailKeyV1::Field, field),
            DomainError::UnknownReference { field } => {
                Self::new(NonCanonicalReasonCodeV1::UnknownReference)
                    .with(NonCanonicalDetailKeyV1::Field, field)
            }
            DomainError::SnapshotMismatch { field } => {
                Self::new(NonCanonicalReasonCodeV1::SnapshotMismatch)
                    .with(NonCanonicalDetailKeyV1::Field, field)
            }
            DomainError::UnsafeText { field } => Self::new(NonCanonicalReasonCodeV1::UnsafeText)
                .with(NonCanonicalDetailKeyV1::Field, field),
            DomainError::InvalidRange { field } => {
                Self::new(NonCanonicalReasonCodeV1::InvalidRange)
                    .with(NonCanonicalDetailKeyV1::Field, field)
            }
            DomainError::InvalidConfidence => {
                Self::new(NonCanonicalReasonCodeV1::InvalidConfidence)
            }
            DomainError::ActivityFacetOnActivitySubject => {
                Self::new(NonCanonicalReasonCodeV1::ActivityFacetOnActivitySubject)
            }
            DomainError::SelfSupersession => Self::new(NonCanonicalReasonCodeV1::SelfSupersession),
            DomainError::AuthorshipWithoutProviderLinkage => {
                Self::new(NonCanonicalReasonCodeV1::AuthorshipWithoutProviderLinkage)
            }
            DomainError::InvalidTimeInterval => {
                Self::new(NonCanonicalReasonCodeV1::InvalidTimeInterval)
            }
            DomainError::InvalidRedactionCounts => {
                Self::new(NonCanonicalReasonCodeV1::InvalidRedactionCounts)
            }
            DomainError::NonCertainDeclaration => {
                Self::new(NonCanonicalReasonCodeV1::NonCertainDeclaration)
            }
            DomainError::DigestMismatch => Self::new(NonCanonicalReasonCodeV1::DigestMismatch),
            DomainError::CanonicalSerialization(detail) => {
                Self::new(NonCanonicalReasonCodeV1::CanonicalSerialization)
                    .with(NonCanonicalDetailKeyV1::Detail, detail)
            }
        }
    }
}

impl fmt::Display for NonCanonicalCauseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason_code.as_str())?;
        if self.details.is_empty()
            || self.reason_code == NonCanonicalReasonCodeV1::CloneBodyOccurrenceOrder
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_domain_maps_field_bearing_errors_one_to_one() {
        let cases = [
            (
                DomainError::Empty { field: "chunk_id" },
                NonCanonicalReasonCodeV1::Empty,
                "chunk_id",
            ),
            (
                DomainError::NonCanonical {
                    field: "file_occurrence_id",
                },
                NonCanonicalReasonCodeV1::DomainNonCanonical,
                "file_occurrence_id",
            ),
            (
                DomainError::DuplicateId {
                    field: "symbol_occurrence_id",
                },
                NonCanonicalReasonCodeV1::DuplicateId,
                "symbol_occurrence_id",
            ),
            (
                DomainError::UnknownReference { field: "anchor_id" },
                NonCanonicalReasonCodeV1::UnknownReference,
                "anchor_id",
            ),
            (
                DomainError::SnapshotMismatch {
                    field: "snapshot_digest",
                },
                NonCanonicalReasonCodeV1::SnapshotMismatch,
                "snapshot_digest",
            ),
            (
                DomainError::UnsafeText { field: "source" },
                NonCanonicalReasonCodeV1::UnsafeText,
                "source",
            ),
            (
                DomainError::InvalidRange { field: "byte_span" },
                NonCanonicalReasonCodeV1::InvalidRange,
                "byte_span",
            ),
        ];

        for (error, expected_code, expected_field) in cases {
            let cause = NonCanonicalCauseV1::from_domain(error);
            assert_eq!(cause.reason_code(), expected_code);
            assert_eq!(
                cause
                    .details()
                    .get(&NonCanonicalDetailKeyV1::Field)
                    .map(String::as_str),
                Some(expected_field)
            );
            assert!(
                !cause
                    .details()
                    .contains_key(&NonCanonicalDetailKeyV1::Detail)
            );
        }
    }

    #[test]
    fn from_domain_maps_fieldless_errors_without_to_string_dump() {
        let cases = [
            (
                DomainError::InvalidConfidence,
                NonCanonicalReasonCodeV1::InvalidConfidence,
            ),
            (
                DomainError::ActivityFacetOnActivitySubject,
                NonCanonicalReasonCodeV1::ActivityFacetOnActivitySubject,
            ),
            (
                DomainError::SelfSupersession,
                NonCanonicalReasonCodeV1::SelfSupersession,
            ),
            (
                DomainError::AuthorshipWithoutProviderLinkage,
                NonCanonicalReasonCodeV1::AuthorshipWithoutProviderLinkage,
            ),
            (
                DomainError::InvalidTimeInterval,
                NonCanonicalReasonCodeV1::InvalidTimeInterval,
            ),
            (
                DomainError::InvalidRedactionCounts,
                NonCanonicalReasonCodeV1::InvalidRedactionCounts,
            ),
            (
                DomainError::NonCertainDeclaration,
                NonCanonicalReasonCodeV1::NonCertainDeclaration,
            ),
            (
                DomainError::DigestMismatch,
                NonCanonicalReasonCodeV1::DigestMismatch,
            ),
        ];

        for (error, expected_code) in cases {
            let cause = NonCanonicalCauseV1::from_domain(error);
            assert_eq!(cause.reason_code(), expected_code);
            assert!(cause.details().is_empty());
        }

        let serialized = NonCanonicalCauseV1::from_domain(DomainError::CanonicalSerialization(
            "payload encoding failed".to_owned(),
        ));
        assert_eq!(
            serialized.reason_code(),
            NonCanonicalReasonCodeV1::CanonicalSerialization
        );
        assert_eq!(
            serialized
                .details()
                .get(&NonCanonicalDetailKeyV1::Detail)
                .map(String::as_str),
            Some("payload encoding failed")
        );
    }

    #[test]
    fn domain_reason_codes_use_snake_case_wire_names() {
        assert_eq!(NonCanonicalReasonCodeV1::Empty.as_str(), "empty");
        assert_eq!(
            NonCanonicalReasonCodeV1::DomainNonCanonical.as_str(),
            "domain_non_canonical"
        );
        assert_eq!(
            NonCanonicalReasonCodeV1::DuplicateId.as_str(),
            "duplicate_id"
        );
        assert_eq!(
            NonCanonicalReasonCodeV1::UnknownReference.as_str(),
            "unknown_reference"
        );
        assert_eq!(
            NonCanonicalReasonCodeV1::SnapshotMismatch.as_str(),
            "snapshot_mismatch"
        );
        assert_eq!(NonCanonicalReasonCodeV1::UnsafeText.as_str(), "unsafe_text");
        assert_eq!(
            NonCanonicalReasonCodeV1::InvalidRange.as_str(),
            "invalid_range"
        );
    }

    #[test]
    fn clone_order_display_omits_identity_details_without_discarding_them() {
        let cause = NonCanonicalCauseV1::new(NonCanonicalReasonCodeV1::CloneBodyOccurrenceOrder)
            .with(
                NonCanonicalDetailKeyV1::LeftSymbolOccurrenceId,
                "symbol.v1.left",
            )
            .with(NonCanonicalDetailKeyV1::LeftPayloadDigest, "sha256:payload");
        assert_eq!(cause.to_string(), "clone_body_occurrence_order");
        assert_eq!(
            cause.details()[&NonCanonicalDetailKeyV1::LeftSymbolOccurrenceId],
            "symbol.v1.left"
        );
        assert_eq!(
            cause.details()[&NonCanonicalDetailKeyV1::LeftPayloadDigest],
            "sha256:payload"
        );
    }
}
