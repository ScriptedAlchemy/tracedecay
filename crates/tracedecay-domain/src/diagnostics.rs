//! Generation-bound diagnostic records (Plan 35, "Universal managed
//! diagnostics"; query/12-diagnostic-persistence authority packet).
//!
//! These are storage-neutral logical records: no store rows, no runtime, no
//! transport. Every durable diagnostic is bound to an immutable
//! code-intelligence generation, a canonical file occurrence with content
//! digest and range encoding, and full producer provenance. Dirty LSP
//! overlays can never be represented by this contract — overlay state is
//! session-only and lives outside the durable record (Plan 35: "Dirty-overlay
//! diagnostics are never sealed into a clean code-intelligence generation";
//! "stale findings cannot cross snapshots").
//!
//! The code index refers to these records only through
//! `GenerationDiagnosticAttachmentV1::diagnostic_anchor` (Plan 25); it never
//! stores a duplicate diagnostic record.

use serde::{Deserialize, Serialize};

use crate::code_intelligence::identity::{
    CodeGenerationId, ContentDigest, FileOccurrenceId, SourceSpan, SymbolOccurrenceId,
};
use crate::research::id::{
    CommitId, ComponentVersion, ManifestDigest, ProviderId, RefId, RepositoryId, RetrievalAnchorId,
    SanitizationReceiptId, WorktreeId,
};
use crate::research::time::UtcMicros;
use crate::research::{DomainError, canonical_sha256};

/// Maximum byte length of the sanitized display message. Raw analyzer stderr,
/// environment values, command lines, unsanitized source, and private host
/// payloads are never diagnostic messages (Plan 35).
pub const MAX_DIAGNOSTIC_MESSAGE_BYTES: usize = 4096;

/// Maximum length of the stable producer diagnostic code.
pub const MAX_DIAGNOSTIC_CODE_LEN: usize = 128;

const DIAGNOSTIC_MESSAGE_DIGEST_DOMAIN: &str = "tracedecay.diagnostic-message.v1";

/// Diagnostic severity. Source severity is preserved exactly; TraceDecay
/// never raises severity because several producers agree (Plan 35).
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
pub enum DiagnosticSeverityV1 {
    Error,
    Warning,
    Information,
    Hint,
}

/// The cataloged producer kinds that may publish durable diagnostics
/// (Plan 35, "Diagnostic sources"). Runtime, storage, migration,
/// configuration, session, or daemon-health findings without a truthful
/// source range are Doctor or application findings and never become
/// `GenerationDiagnosticV1` records.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticProducerKindV1 {
    UpstreamCompiler,
    LanguageServer,
    TracedecayStructural,
    TracedecayGraphIntegrity,
    TracedecayPolicy,
    TracedecayCodeHealth,
    GenerationConsistency,
    AuthorizedExternalAnalyzer,
}

/// Evidence class for one diagnostic record (Plan 35: evidence class is part
/// of canonical diagnostic identity).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticEvidenceClassV1 {
    /// Directly observed against the exact clean generation it names.
    ObservedCurrent,
    /// Reported by the cataloged producer for the exact clean generation.
    ProducerReported,
    /// Derived from TraceDecay structural/graph analysis of the generation.
    DerivedStructural,
    /// The evidence class cannot be established; the record is retained for
    /// audit but must not be treated as current truth.
    UnknownUnsupported,
}

/// Producer provenance for one diagnostic. Diagnostic identity includes
/// producer provenance: identical findings from the same logical producer and
/// revision collapse; findings from distinct producers remain distinct
/// (Plan 35, "Merge and publication semantics").
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticProvenanceV1 {
    pub producer_kind: DiagnosticProducerKindV1,
    pub producer: ProviderId,
    pub analyzer_revision: ComponentVersion,
    pub configuration_revision: ComponentVersion,
    pub sanitization_receipt: Option<SanitizationReceiptId>,
}

impl DiagnosticProvenanceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.producer.validate()?;
        self.analyzer_revision.validate()?;
        self.configuration_revision.validate()?;
        if let Some(receipt) = &self.sanitization_receipt {
            receipt.validate()?;
        }
        Ok(())
    }
}

/// Current-vs-stale typing for a durable diagnostic record. Publication is
/// version-monotone: a newer clean generation clears or supersedes the prior
/// publication deterministically, and stale findings cannot cross snapshots
/// (Plan 35). Stale and historical records remain queryable through
/// application APIs but are excluded from active publication.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DiagnosticRecordStateV1 {
    /// Current for exactly the clean generation named by the record.
    Current,
    /// A successor clean generation republished the same logical finding
    /// space; this record is historical.
    Superseded {
        successor_generation: CodeGenerationId,
    },
    /// A clean generation completed and deterministically removed this
    /// finding (resolution, deletion, source-revision drift, or content or
    /// generation change).
    Cleared {
        cleared_in_generation: CodeGenerationId,
    },
}

impl DiagnosticRecordStateV1 {
    pub const fn is_current(&self) -> bool {
        matches!(self, Self::Current)
    }

    fn validate(&self, own_generation: &CodeGenerationId) -> Result<(), DomainError> {
        match self {
            Self::Current => Ok(()),
            Self::Superseded {
                successor_generation,
            } => {
                successor_generation.validate()?;
                if successor_generation == own_generation {
                    return Err(DomainError::SelfSupersession);
                }
                Ok(())
            }
            Self::Cleared {
                cleared_in_generation,
            } => {
                cleared_in_generation.validate()?;
                if cleared_in_generation == own_generation {
                    return Err(DomainError::SelfSupersession);
                }
                Ok(())
            }
        }
    }
}

/// One durable, generation-bound diagnostic record (Plan 35, "Canonical
/// diagnostic identity"). Every field is part of canonical identity; the
/// display message remains sanitized product data.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationDiagnosticV1 {
    /// Plan 13 anchor addressing this record. The code index's
    /// `GenerationDiagnosticAttachmentV1::diagnostic_anchor` points here.
    pub diagnostic_anchor: RetrievalAnchorId,
    /// The immutable clean code-intelligence generation this record is bound
    /// to. Findings never cross generations.
    pub generation_id: CodeGenerationId,
    pub repository: RepositoryId,
    pub worktree: Option<WorktreeId>,
    pub reference: Option<RefId>,
    pub source_revision: Option<CommitId>,
    /// Canonical file identity the diagnostic attaches to.
    pub file_occurrence_id: FileOccurrenceId,
    /// Content digest of the attached file inside the generation.
    pub content_digest: ContentDigest,
    /// Range encoding inside the sanitized file (byte range; mutable line
    /// numbers are never identity).
    pub span: SourceSpan,
    /// Enclosing symbol occurrence, when exact attachment is possible.
    pub symbol_occurrence_id: Option<SymbolOccurrenceId>,
    /// Stable producer diagnostic code (for example `E0308`).
    pub code: String,
    pub severity: DiagnosticSeverityV1,
    /// Sanitized display message; bounded by [`MAX_DIAGNOSTIC_MESSAGE_BYTES`].
    pub message: String,
    /// Integrity digest over the sanitized message.
    pub message_digest: ManifestDigest,
    pub provenance: DiagnosticProvenanceV1,
    pub evidence_class: DiagnosticEvidenceClassV1,
    /// Collection time for the evidence.
    pub collected_at: UtcMicros,
    /// Current-vs-stale typing; publication is version-monotone.
    pub state: DiagnosticRecordStateV1,
}

impl GenerationDiagnosticV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.diagnostic_anchor.validate()?;
        self.generation_id.validate()?;
        self.repository.validate()?;
        if let Some(worktree) = &self.worktree {
            worktree.validate()?;
        }
        if let Some(reference) = &self.reference {
            reference.validate()?;
        }
        if let Some(source_revision) = &self.source_revision {
            source_revision.validate()?;
        }
        self.file_occurrence_id.validate()?;
        self.content_digest.validate()?;
        self.span.validate()?;
        if let Some(symbol_occurrence_id) = &self.symbol_occurrence_id {
            symbol_occurrence_id.validate()?;
        }
        validate_diagnostic_code(&self.code)?;
        validate_sanitized_message(&self.message)?;
        self.message_digest.validate()?;
        if self.compute_message_digest()? != self.message_digest {
            return Err(DomainError::DigestMismatch);
        }
        self.provenance.validate()?;
        self.state.validate(&self.generation_id)?;
        Ok(())
    }

    /// Compute the canonical domain-separated integrity digest for the
    /// sanitized diagnostic message.
    pub fn compute_message_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(DIAGNOSTIC_MESSAGE_DIGEST_DOMAIN, &self.message))
    }

    /// True only while the record is current for its own clean generation.
    pub const fn is_current(&self) -> bool {
        self.state.is_current()
    }

    /// Returns a copy marked superseded by `successor_generation`. A record
    /// can only be superseded out of the current state, and a generation can
    /// never supersede itself (version-monotone publication, Plan 35).
    pub fn supersede(&self, successor_generation: CodeGenerationId) -> Result<Self, DomainError> {
        successor_generation.validate()?;
        if successor_generation == self.generation_id {
            return Err(DomainError::SelfSupersession);
        }
        if !self.state.is_current() {
            return Err(DomainError::NonCanonical {
                field: "diagnostic record state transition",
            });
        }
        let mut next = self.clone();
        next.state = DiagnosticRecordStateV1::Superseded {
            successor_generation,
        };
        Ok(next)
    }

    /// Returns a copy marked cleared by a clean generation that completed
    /// without this finding. A record can only be cleared out of the current
    /// state, and a generation can never clear itself.
    pub fn clear(&self, cleared_in_generation: CodeGenerationId) -> Result<Self, DomainError> {
        cleared_in_generation.validate()?;
        if cleared_in_generation == self.generation_id {
            return Err(DomainError::SelfSupersession);
        }
        if !self.state.is_current() {
            return Err(DomainError::NonCanonical {
                field: "diagnostic record state transition",
            });
        }
        let mut next = self.clone();
        next.state = DiagnosticRecordStateV1::Cleared {
            cleared_in_generation,
        };
        Ok(next)
    }
}

fn validate_diagnostic_code(code: &str) -> Result<(), DomainError> {
    if code.is_empty() {
        return Err(DomainError::Empty {
            field: "diagnostic code",
        });
    }
    if !crate::canonical_text::is_canonical_text_within(code, MAX_DIAGNOSTIC_CODE_LEN) {
        return Err(DomainError::NonCanonical {
            field: "diagnostic code",
        });
    }
    Ok(())
}

fn validate_sanitized_message(message: &str) -> Result<(), DomainError> {
    if message.is_empty() {
        return Err(DomainError::Empty {
            field: "diagnostic message",
        });
    }
    if message.len() > MAX_DIAGNOSTIC_MESSAGE_BYTES {
        return Err(DomainError::UnsafeText {
            field: "diagnostic message",
        });
    }
    if message.chars().any(char::is_control) {
        return Err(DomainError::UnsafeText {
            field: "diagnostic message",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn fixture_record() -> GenerationDiagnosticV1 {
        let mut record = GenerationDiagnosticV1 {
            diagnostic_anchor: id("anchor.diagnostic.1"),
            generation_id: id("generation.clean.1"),
            repository: id("repository.fixture"),
            worktree: Some(id("worktree.fixture")),
            reference: Some(id("ref.main")),
            source_revision: Some(id("commit.abc123")),
            file_occurrence_id: id("file.occurrence.1"),
            content_digest: id(&digest('a')),
            span: SourceSpan {
                start_byte: 10,
                end_byte: 42,
            },
            symbol_occurrence_id: Some(id("symbol.occurrence.1")),
            code: "E0308".to_owned(),
            severity: DiagnosticSeverityV1::Error,
            message: "mismatched types".to_owned(),
            message_digest: id(&digest('b')),
            provenance: DiagnosticProvenanceV1 {
                producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
                producer: id("producer.rustc"),
                analyzer_revision: id("analyzer.v1"),
                configuration_revision: id("config.v1"),
                sanitization_receipt: Some(id("receipt.sanitization.1")),
            },
            evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
            collected_at: UtcMicros(1_700_000_000_000_000),
            state: DiagnosticRecordStateV1::Current,
        };
        record.message_digest = record.compute_message_digest().expect("digest computable");
        record
    }

    #[test]
    fn fixture_record_validates() {
        fixture_record().validate().expect("valid fixture record");
    }

    #[test]
    fn message_is_bounded_and_sanitized() {
        let mut record = fixture_record();
        record.message = String::new();
        assert!(matches!(record.validate(), Err(DomainError::Empty { .. })));

        let mut record = fixture_record();
        record.message = "x".repeat(MAX_DIAGNOSTIC_MESSAGE_BYTES + 1);
        assert!(matches!(
            record.validate(),
            Err(DomainError::UnsafeText { .. })
        ));

        let mut record = fixture_record();
        record.message = "contains\u{0007}bell".to_owned();
        assert!(matches!(
            record.validate(),
            Err(DomainError::UnsafeText { .. })
        ));
    }

    #[test]
    fn message_digest_must_match_the_sanitized_message() {
        let mut record = fixture_record();
        record.message = "different sanitized message".to_owned();
        assert!(matches!(
            record.validate(),
            Err(DomainError::DigestMismatch)
        ));

        record.message_digest = record.compute_message_digest().unwrap();
        record.validate().expect("recomputed message digest");
    }

    #[test]
    fn code_is_bounded_and_canonical() {
        let mut record = fixture_record();
        record.code = String::new();
        assert!(matches!(record.validate(), Err(DomainError::Empty { .. })));

        let mut record = fixture_record();
        record.code = " E0308".to_owned();
        assert!(matches!(
            record.validate(),
            Err(DomainError::NonCanonical { .. })
        ));

        let mut record = fixture_record();
        record.code = "x".repeat(MAX_DIAGNOSTIC_CODE_LEN + 1);
        assert!(matches!(
            record.validate(),
            Err(DomainError::NonCanonical { .. })
        ));
    }

    #[test]
    fn supersession_requires_a_distinct_generation() {
        let record = fixture_record();
        assert!(matches!(
            record.clone().supersede(record.generation_id.clone()),
            Err(DomainError::SelfSupersession)
        ));
        let superseded = record
            .supersede(id("generation.clean.2"))
            .expect("distinct successor supersedes");
        assert!(!superseded.is_current());
        superseded.validate().expect("superseded record validates");
    }

    #[test]
    fn clearing_requires_a_distinct_generation() {
        let record = fixture_record();
        assert!(matches!(
            record.clone().clear(record.generation_id.clone()),
            Err(DomainError::SelfSupersession)
        ));
        let cleared = record
            .clear(id("generation.clean.2"))
            .expect("distinct generation clears");
        assert!(matches!(
            cleared.state,
            DiagnosticRecordStateV1::Cleared { .. }
        ));
        cleared.validate().expect("cleared record validates");
    }

    #[test]
    fn stale_records_cannot_transition_again() {
        let record = fixture_record();
        let superseded = record.supersede(id("generation.clean.2")).unwrap();
        assert!(superseded.supersede(id("generation.clean.3")).is_err());
        assert!(superseded.clear(id("generation.clean.3")).is_err());
    }

    #[test]
    fn state_rejects_self_referencing_generations() {
        let mut record = fixture_record();
        record.state = DiagnosticRecordStateV1::Superseded {
            successor_generation: record.generation_id.clone(),
        };
        assert!(matches!(
            record.validate(),
            Err(DomainError::SelfSupersession)
        ));
    }

    #[test]
    fn record_round_trips_through_json() {
        let record = fixture_record()
            .supersede(id("generation.clean.2"))
            .unwrap();
        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: GenerationDiagnosticV1 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, parsed);
    }
}
