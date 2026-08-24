//! The single driver-neutral codec between typed diagnostic domain values and
//! their canonical stored column text.
//!
//! Two SQLite engines persist `generation_diagnostics`: the root
//! `DiagnosticsStore` and the concrete `DiagnosticExecutor` in the rusqlite
//! runtime crate. Both must agree byte for byte on `record_state`,
//! `state_generation`, `severity`, `producer_kind`, and `evidence_class`, or a
//! cutover silently reinterprets already-persisted rows. Owning that mapping
//! here makes the two engines share one table instead of two hand-maintained
//! copies.
//!
//! Parsers return `Option` rather than an error type on purpose: each engine
//! reports decode failures through its own error channel with its own wording,
//! and neither message is allowed to change just because the mapping moved.

use tracedecay_domain::{
    CodeGenerationId, DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticRecordStateV1,
    DiagnosticSeverityV1,
};

/// Stored `record_state` text for a live record.
pub const DIAGNOSTIC_STATE_CURRENT: &str = "current";
/// Stored `record_state` text for a record replaced by a later generation.
pub const DIAGNOSTIC_STATE_SUPERSEDED: &str = "superseded";
/// Stored `record_state` text for a record cleared by a later generation.
pub const DIAGNOSTIC_STATE_CLEARED: &str = "cleared";

/// The stored discriminant of `record_state`, decoupled from the
/// `state_generation` back-pointer that two of its three forms carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticRecordStateKindV1 {
    Current,
    Superseded,
    Cleared,
}

impl DiagnosticRecordStateKindV1 {
    /// Classifies stored `record_state` text. `None` marks an unknown state;
    /// the caller decides how to report it.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            DIAGNOSTIC_STATE_CURRENT => Some(Self::Current),
            DIAGNOSTIC_STATE_SUPERSEDED => Some(Self::Superseded),
            DIAGNOSTIC_STATE_CLEARED => Some(Self::Cleared),
            _ => None,
        }
    }

    /// The exact text persisted in `record_state`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => DIAGNOSTIC_STATE_CURRENT,
            Self::Superseded => DIAGNOSTIC_STATE_SUPERSEDED,
            Self::Cleared => DIAGNOSTIC_STATE_CLEARED,
        }
    }

    /// The domain field name of the `state_generation` back-pointer this kind
    /// carries, or `None` for the current state, which stores SQL `NULL`.
    ///
    /// Callers use the name to build a decode error naming the exact field.
    pub const fn state_generation_field(self) -> Option<&'static str> {
        match self {
            Self::Current => None,
            Self::Superseded => Some("successor_generation"),
            Self::Cleared => Some("cleared_in_generation"),
        }
    }

    /// Rebuilds the typed state from this kind plus the decoded back-pointer.
    ///
    /// Returns `None` when the two disagree — a non-current kind with no
    /// generation, or a current kind carrying one — so the caller reports a
    /// corrupt row rather than inventing a state.
    pub fn into_state(
        self,
        state_generation: Option<CodeGenerationId>,
    ) -> Option<DiagnosticRecordStateV1> {
        match (self, state_generation) {
            (Self::Current, None) => Some(DiagnosticRecordStateV1::Current),
            (Self::Superseded, Some(successor_generation)) => {
                Some(DiagnosticRecordStateV1::Superseded {
                    successor_generation,
                })
            }
            (Self::Cleared, Some(cleared_in_generation)) => {
                Some(DiagnosticRecordStateV1::Cleared {
                    cleared_in_generation,
                })
            }
            _ => None,
        }
    }
}

/// Projects a typed record state onto its `(record_state, state_generation)`
/// column pair. The back-pointer borrows from `state`, so a caller that needs
/// an owned column value copies it explicitly.
pub fn diagnostic_state_columns(state: &DiagnosticRecordStateV1) -> (&'static str, Option<&str>) {
    match state {
        DiagnosticRecordStateV1::Current => (DIAGNOSTIC_STATE_CURRENT, None),
        DiagnosticRecordStateV1::Superseded {
            successor_generation,
        } => (
            DIAGNOSTIC_STATE_SUPERSEDED,
            Some(successor_generation.as_str()),
        ),
        DiagnosticRecordStateV1::Cleared {
            cleared_in_generation,
        } => (
            DIAGNOSTIC_STATE_CLEARED,
            Some(cleared_in_generation.as_str()),
        ),
    }
}

/// The exact text persisted in `severity`.
pub const fn diagnostic_severity_name(severity: DiagnosticSeverityV1) -> &'static str {
    match severity {
        DiagnosticSeverityV1::Error => "error",
        DiagnosticSeverityV1::Warning => "warning",
        DiagnosticSeverityV1::Information => "information",
        DiagnosticSeverityV1::Hint => "hint",
    }
}

/// Decodes stored `severity` text. `None` marks an unknown severity.
pub fn parse_diagnostic_severity(value: &str) -> Option<DiagnosticSeverityV1> {
    match value {
        "error" => Some(DiagnosticSeverityV1::Error),
        "warning" => Some(DiagnosticSeverityV1::Warning),
        "information" => Some(DiagnosticSeverityV1::Information),
        "hint" => Some(DiagnosticSeverityV1::Hint),
        _ => None,
    }
}

/// The exact text persisted in `producer_kind`.
pub const fn diagnostic_producer_kind_name(kind: DiagnosticProducerKindV1) -> &'static str {
    match kind {
        DiagnosticProducerKindV1::UpstreamCompiler => "upstream_compiler",
        DiagnosticProducerKindV1::LanguageServer => "language_server",
        DiagnosticProducerKindV1::TracedecayStructural => "tracedecay_structural",
        DiagnosticProducerKindV1::TracedecayGraphIntegrity => "tracedecay_graph_integrity",
        DiagnosticProducerKindV1::TracedecayPolicy => "tracedecay_policy",
        DiagnosticProducerKindV1::TracedecayCodeHealth => "tracedecay_code_health",
        DiagnosticProducerKindV1::GenerationConsistency => "generation_consistency",
        DiagnosticProducerKindV1::AuthorizedExternalAnalyzer => "authorized_external_analyzer",
    }
}

/// Decodes stored `producer_kind` text. `None` marks an unknown producer.
pub fn parse_diagnostic_producer_kind(value: &str) -> Option<DiagnosticProducerKindV1> {
    match value {
        "upstream_compiler" => Some(DiagnosticProducerKindV1::UpstreamCompiler),
        "language_server" => Some(DiagnosticProducerKindV1::LanguageServer),
        "tracedecay_structural" => Some(DiagnosticProducerKindV1::TracedecayStructural),
        "tracedecay_graph_integrity" => Some(DiagnosticProducerKindV1::TracedecayGraphIntegrity),
        "tracedecay_policy" => Some(DiagnosticProducerKindV1::TracedecayPolicy),
        "tracedecay_code_health" => Some(DiagnosticProducerKindV1::TracedecayCodeHealth),
        "generation_consistency" => Some(DiagnosticProducerKindV1::GenerationConsistency),
        "authorized_external_analyzer" => {
            Some(DiagnosticProducerKindV1::AuthorizedExternalAnalyzer)
        }
        _ => None,
    }
}

/// The exact text persisted in `evidence_class`.
pub const fn diagnostic_evidence_class_name(class: DiagnosticEvidenceClassV1) -> &'static str {
    match class {
        DiagnosticEvidenceClassV1::ObservedCurrent => "observed_current",
        DiagnosticEvidenceClassV1::ProducerReported => "producer_reported",
        DiagnosticEvidenceClassV1::DerivedStructural => "derived_structural",
        DiagnosticEvidenceClassV1::UnknownUnsupported => "unknown_unsupported",
    }
}

/// Decodes stored `evidence_class` text. `None` marks an unknown class.
pub fn parse_diagnostic_evidence_class(value: &str) -> Option<DiagnosticEvidenceClassV1> {
    match value {
        "observed_current" => Some(DiagnosticEvidenceClassV1::ObservedCurrent),
        "producer_reported" => Some(DiagnosticEvidenceClassV1::ProducerReported),
        "derived_structural" => Some(DiagnosticEvidenceClassV1::DerivedStructural),
        "unknown_unsupported" => Some(DiagnosticEvidenceClassV1::UnknownUnsupported),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(value: &str) -> CodeGenerationId {
        CodeGenerationId::new(value).expect("valid fixture generation")
    }

    #[test]
    fn every_record_state_round_trips_through_its_columns() {
        let cases = [
            DiagnosticRecordStateV1::Current,
            DiagnosticRecordStateV1::Superseded {
                successor_generation: generation("generation.successor"),
            },
            DiagnosticRecordStateV1::Cleared {
                cleared_in_generation: generation("generation.cleared"),
            },
        ];
        for state in cases {
            let (column, back_pointer) = diagnostic_state_columns(&state);
            let kind =
                DiagnosticRecordStateKindV1::parse(column).expect("encoded state text must decode");
            assert_eq!(kind.as_str(), column);
            assert_eq!(
                kind.state_generation_field().is_some(),
                back_pointer.is_some()
            );
            let decoded = kind
                .into_state(back_pointer.map(generation))
                .expect("state columns must rebuild the typed state");
            assert_eq!(decoded, state);
        }
    }

    #[test]
    fn state_columns_and_back_pointer_must_agree() {
        assert!(
            DiagnosticRecordStateKindV1::Superseded
                .into_state(None)
                .is_none(),
            "a superseded row without a successor is corrupt"
        );
        assert!(
            DiagnosticRecordStateKindV1::Cleared
                .into_state(None)
                .is_none(),
            "a cleared row without a clearing generation is corrupt"
        );
        assert!(
            DiagnosticRecordStateKindV1::Current
                .into_state(Some(generation("generation.unexpected")))
                .is_none(),
            "a current row must not carry a state generation"
        );
        assert!(DiagnosticRecordStateKindV1::parse("archived").is_none());
    }

    #[test]
    fn every_enumerated_value_round_trips_through_its_column_text() {
        for severity in [
            DiagnosticSeverityV1::Error,
            DiagnosticSeverityV1::Warning,
            DiagnosticSeverityV1::Information,
            DiagnosticSeverityV1::Hint,
        ] {
            assert_eq!(
                parse_diagnostic_severity(diagnostic_severity_name(severity)),
                Some(severity)
            );
        }
        for kind in [
            DiagnosticProducerKindV1::UpstreamCompiler,
            DiagnosticProducerKindV1::LanguageServer,
            DiagnosticProducerKindV1::TracedecayStructural,
            DiagnosticProducerKindV1::TracedecayGraphIntegrity,
            DiagnosticProducerKindV1::TracedecayPolicy,
            DiagnosticProducerKindV1::TracedecayCodeHealth,
            DiagnosticProducerKindV1::GenerationConsistency,
            DiagnosticProducerKindV1::AuthorizedExternalAnalyzer,
        ] {
            assert_eq!(
                parse_diagnostic_producer_kind(diagnostic_producer_kind_name(kind)),
                Some(kind)
            );
        }
        for class in [
            DiagnosticEvidenceClassV1::ObservedCurrent,
            DiagnosticEvidenceClassV1::ProducerReported,
            DiagnosticEvidenceClassV1::DerivedStructural,
            DiagnosticEvidenceClassV1::UnknownUnsupported,
        ] {
            assert_eq!(
                parse_diagnostic_evidence_class(diagnostic_evidence_class_name(class)),
                Some(class)
            );
        }
        assert!(parse_diagnostic_severity("fatal").is_none());
        assert!(parse_diagnostic_producer_kind("linter").is_none());
        assert!(parse_diagnostic_evidence_class("guessed").is_none());
    }
}
