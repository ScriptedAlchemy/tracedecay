//! Read-side provider envelopes for generation-bound code-index joins.
//!
//! These values describe one read from an existing authority. They are not a
//! persistence interface and never copy Git, diagnostic, graph, or test
//! records into another store.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, ProviderEvaluationStateV1, RetrievalAnchorId,
    SymbolOccurrenceId, TestAttributionEvidenceClassV1,
};

use super::diagnostics::GenerationDiagnosticJoinV1;
use super::git_join::{GenerationGitBlameJoinV1, GenerationGitHistoryJoinV1, GenerationGitJoinV1};
use super::impact_join::GenerationImpactJoinV1;
use super::test_attribution::GenerationTestJoinV1;

/// Native graph-impact evidence consumed by generation-bound index joins.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexGraphImpactEvidenceV1 {
    pub affected_files: Vec<FileOccurrenceId>,
    pub affected_callers: Vec<SymbolOccurrenceId>,
    pub evidence_anchors: Vec<RetrievalAnchorId>,
}

/// Native affected-test evidence consumed by generation-bound index joins.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexAffectedTestAttributionV1 {
    pub test: SymbolOccurrenceId,
    pub evidence_class: TestAttributionEvidenceClassV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeIndexAffectedTestsEvidenceV1 {
    pub tests: Vec<SymbolOccurrenceId>,
    #[serde(default)]
    pub attributions: Vec<CodeIndexAffectedTestAttributionV1>,
}

/// Coverage counters reported independently by one provider.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum GenerationProviderCoverageV1 {
    Complete {
        examined: u64,
        eligible: u64,
        excluded: u64,
    },
    Partial {
        examined: u64,
        eligible: u64,
        excluded: u64,
        unknown: u64,
        capped: bool,
    },
    Unavailable,
}

impl GenerationProviderCoverageV1 {
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete { .. })
    }

    fn validate(&self) -> Result<(), GenerationProviderContractErrorV1> {
        match self {
            Self::Complete {
                examined,
                eligible,
                excluded,
            } => {
                if eligible.saturating_add(*excluded) != *examined {
                    return Err(GenerationProviderContractErrorV1::InvalidCoverage);
                }
            }
            Self::Partial {
                examined,
                eligible,
                excluded,
                unknown,
                ..
            } => {
                if eligible.saturating_add(*excluded).saturating_add(*unknown) > *examined {
                    return Err(GenerationProviderContractErrorV1::InvalidCoverage);
                }
            }
            Self::Unavailable => {}
        }
        Ok(())
    }
}

/// One provider result with state and coverage kept separate from its typed
/// authority-owned payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationProviderReadV1<T> {
    pub provider_state: ProviderEvaluationStateV1,
    pub coverage: GenerationProviderCoverageV1,
    pub evidence: Option<T>,
}

impl<T> GenerationProviderReadV1<T> {
    pub fn new(
        provider_state: ProviderEvaluationStateV1,
        coverage: GenerationProviderCoverageV1,
        evidence: Option<T>,
    ) -> Result<Self, GenerationProviderContractErrorV1> {
        let result = Self {
            provider_state,
            coverage,
            evidence,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), GenerationProviderContractErrorV1> {
        self.coverage.validate()?;
        match self.provider_state {
            ProviderEvaluationStateV1::SupportedCompletedComplete => {
                if !self.coverage.is_complete() || self.evidence.is_none() {
                    return Err(GenerationProviderContractErrorV1::StateCoverageMismatch);
                }
            }
            ProviderEvaluationStateV1::Partial => {
                if self.coverage.is_complete()
                    || matches!(&self.coverage, GenerationProviderCoverageV1::Unavailable)
                    || self.evidence.is_none()
                {
                    return Err(GenerationProviderContractErrorV1::StateCoverageMismatch);
                }
            }
            ProviderEvaluationStateV1::Cancelled | ProviderEvaluationStateV1::TimedOut => {
                if self.coverage.is_complete()
                    || (self.evidence.is_some()
                        && matches!(&self.coverage, GenerationProviderCoverageV1::Unavailable))
                {
                    return Err(GenerationProviderContractErrorV1::StateCoverageMismatch);
                }
            }
            ProviderEvaluationStateV1::Unsupported
            | ProviderEvaluationStateV1::Absent
            | ProviderEvaluationStateV1::Indexing
            | ProviderEvaluationStateV1::Stale
            | ProviderEvaluationStateV1::Failed
            | ProviderEvaluationStateV1::Unavailable => {
                if self.coverage.is_complete() || self.evidence.is_some() {
                    return Err(GenerationProviderContractErrorV1::StateCoverageMismatch);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenerationProviderContractErrorV1 {
    #[error("provider coverage counters are inconsistent")]
    InvalidCoverage,
    #[error("provider state, coverage, and evidence are inconsistent")]
    StateCoverageMismatch,
}

/// Read adapter implemented by the existing Git authority for the query
/// owner. Joined values are views over native Git results, never Git storage.
pub trait GenerationGitJoinReadPort {
    fn read_git_diff(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitJoinV1>;

    fn read_git_history(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitHistoryJoinV1>;

    fn read_git_blame(
        &self,
        generation: &CodeGenerationId,
        file: &FileOccurrenceId,
    ) -> GenerationProviderReadV1<GenerationGitBlameJoinV1>;
}

/// Read adapter implemented over the managed diagnostic authority.
pub trait GenerationDiagnosticJoinReadPort {
    fn read_generation_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationDiagnosticJoinV1>;
}

/// Read adapter over canonical generation-bound test-attribution records.
pub trait GenerationTestAttributionJoinReadPort {
    fn read_test_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestJoinV1>;
}

/// Existing graph and test authorities implement this adapter; the code index
/// only validates the occurrence identities in their returned payloads.
pub trait GenerationImpactEvidenceReadPort {
    fn read_graph_impact(
        &self,
        generation: &CodeGenerationId,
        symbol: &SymbolOccurrenceId,
    ) -> GenerationProviderReadV1<CodeIndexGraphImpactEvidenceV1>;

    fn read_affected_tests(
        &self,
        generation: &CodeGenerationId,
        symbol: &SymbolOccurrenceId,
    ) -> GenerationProviderReadV1<CodeIndexAffectedTestsEvidenceV1>;
}

/// Query-owner adapter over the validated graph/test composition.
pub trait GenerationImpactJoinReadPort {
    fn read_generation_impact(
        &self,
        generation: &CodeGenerationId,
        symbol: &SymbolOccurrenceId,
    ) -> GenerationProviderReadV1<GenerationImpactJoinV1>;
}
