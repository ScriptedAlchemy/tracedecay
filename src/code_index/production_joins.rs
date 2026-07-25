//! Production read adapters for generation-bound Git, diagnostic, and test
//! joins.
//!
//! Each adapter reads from an owning authority on every request and performs
//! the join against one immutable code-generation snapshot. It retains no
//! parallel Git, diagnostic, graph, or test store.

use std::sync::Arc;

use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, FileOccurrenceId, GenerationDiagnosticV1,
    GenerationTestAttributionV1, GitBlameV1, GitDiffV1, GitHistoryV1, ProviderEvaluationStateV1,
    ValidatedCodeSnapshotV1,
};

use super::diagnostics::{
    DiagnosticEvidenceWatermarkV1, GenerationDiagnosticJoinCoverageV1,
    GenerationDiagnosticJoinErrorV1, GenerationDiagnosticJoinV1,
};
use super::git_join::{
    GenerationGitBlameJoinCoverageV1, GenerationGitBlameJoinV1, GenerationGitContextProvidersV1,
    GenerationGitHistoryJoinCoverageV1, GenerationGitHistoryJoinV1, GenerationGitJoinCoverageV1,
    GenerationGitJoinErrorV1, GenerationGitJoinV1, GenerationGitReadWatermarkV1,
    GenerationGitWatermarkV1, GitFileContentIdentityV1, GitSymbolLineBindingV1,
};
use super::provider::{
    GenerationDiagnosticJoinReadPort, GenerationGitJoinReadPort, GenerationProviderCoverageV1,
    GenerationProviderReadV1, GenerationTestAttributionJoinReadPort,
};
use super::test_attribution::{
    GenerationTestJoinCoverageV1, GenerationTestJoinErrorV1, GenerationTestJoinV1,
    TestAttributionOccurrenceV1, TestAttributionWatermarkV1,
};

/// Exact immutable code-generation authority used by all three adapters.
#[derive(Clone, Debug)]
pub struct GenerationJoinCodeAuthorityV1 {
    pub manifest: CodeGenerationManifestV1,
    pub snapshot: ValidatedCodeSnapshotV1,
}

impl GenerationJoinCodeAuthorityV1 {
    fn matches(&self, generation: &CodeGenerationId) -> bool {
        &self.manifest.generation_id == generation
            && self.manifest.snapshot_digest == self.snapshot.intake_digest
    }
}

#[derive(Clone, Debug)]
pub struct GenerationGitDiffEvidenceV1 {
    pub diff: GitDiffV1,
    pub watermark: GenerationGitWatermarkV1,
    pub file_contents: Vec<GitFileContentIdentityV1>,
}

#[derive(Clone, Debug)]
pub struct GenerationGitHistoryEvidenceV1 {
    pub history: GitHistoryV1,
    pub watermark: GenerationGitReadWatermarkV1,
}

#[derive(Clone, Debug)]
pub struct GenerationGitBlameEvidenceV1 {
    pub blame: GitBlameV1,
    pub watermark: GenerationGitReadWatermarkV1,
    pub file_content: GitFileContentIdentityV1,
    pub symbol_bindings: Vec<GitSymbolLineBindingV1>,
}

/// Native Git/capture authority. Returned watermarks are independent of the
/// code generation and are validated by the adapter before publication.
pub trait GenerationGitEvidenceAuthorityV1: Send + Sync {
    fn read_diff(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitDiffEvidenceV1>;

    fn read_history(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitHistoryEvidenceV1>;

    fn read_blame(
        &self,
        generation: &CodeGenerationId,
        file: &FileOccurrenceId,
    ) -> GenerationProviderReadV1<GenerationGitBlameEvidenceV1>;
}

/// Canonical occurrence/graph/diagnostic/test composition for one Git read.
/// This remains separate from [`GenerationGitEvidenceAuthorityV1`] so native
/// Git never becomes the authority for derived impact evidence.
pub trait GenerationGitContextAuthorityV1: Send + Sync {
    fn read_context(&self, generation: &CodeGenerationId) -> GenerationGitContextProvidersV1;
}

pub struct ProductionGenerationGitJoinReaderV1 {
    code: GenerationJoinCodeAuthorityV1,
    git: Arc<dyn GenerationGitEvidenceAuthorityV1>,
    context: Arc<dyn GenerationGitContextAuthorityV1>,
}

impl ProductionGenerationGitJoinReaderV1 {
    pub fn new(
        code: GenerationJoinCodeAuthorityV1,
        git: Arc<dyn GenerationGitEvidenceAuthorityV1>,
        context: Arc<dyn GenerationGitContextAuthorityV1>,
    ) -> Self {
        Self { code, git, context }
    }
}

impl GenerationGitJoinReadPort for ProductionGenerationGitJoinReaderV1 {
    fn read_git_diff(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitJoinV1> {
        if !self.code.matches(generation) {
            return unavailable();
        }
        map_join(
            self.git.read_diff(generation),
            |evidence| {
                let context = self.context.read_context(generation);
                GenerationGitJoinV1::join_with_context(
                    &self.code.manifest,
                    &self.code.snapshot,
                    &evidence.diff,
                    &evidence.watermark,
                    &evidence.file_contents,
                    &context,
                )
            },
            |join| matches!(join.coverage, GenerationGitJoinCoverageV1::Complete),
            git_error_state,
        )
    }

    fn read_git_history(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationGitHistoryJoinV1> {
        if !self.code.matches(generation) {
            return unavailable();
        }
        map_join(
            self.git.read_history(generation),
            |evidence| {
                GenerationGitHistoryJoinV1::join(
                    &self.code.manifest,
                    &self.code.snapshot,
                    &evidence.history,
                    &evidence.watermark,
                )
            },
            |join| matches!(join.coverage, GenerationGitHistoryJoinCoverageV1::Complete),
            git_error_state,
        )
    }

    fn read_git_blame(
        &self,
        generation: &CodeGenerationId,
        file: &FileOccurrenceId,
    ) -> GenerationProviderReadV1<GenerationGitBlameJoinV1> {
        if !self.code.matches(generation) {
            return unavailable();
        }
        map_join(
            self.git.read_blame(generation, file),
            |evidence| {
                let joined = GenerationGitBlameJoinV1::join(
                    &self.code.manifest,
                    &self.code.snapshot,
                    &evidence.blame,
                    &evidence.watermark,
                    &evidence.file_content,
                    &evidence.symbol_bindings,
                )?;
                if &joined.file_occurrence_id != file {
                    return Err(GenerationGitJoinErrorV1::StaleGitEvidence);
                }
                Ok(joined)
            },
            |join| matches!(join.coverage, GenerationGitBlameJoinCoverageV1::Complete),
            git_error_state,
        )
    }
}

#[derive(Clone, Debug)]
pub struct GenerationDiagnosticEvidenceV1 {
    pub records: Vec<GenerationDiagnosticV1>,
    pub watermark: DiagnosticEvidenceWatermarkV1,
}

pub trait GenerationDiagnosticEvidenceAuthorityV1: Send + Sync {
    fn read_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationDiagnosticEvidenceV1>;
}

pub struct ProductionGenerationDiagnosticJoinReaderV1 {
    code: GenerationJoinCodeAuthorityV1,
    diagnostics: Arc<dyn GenerationDiagnosticEvidenceAuthorityV1>,
}

impl ProductionGenerationDiagnosticJoinReaderV1 {
    pub fn new(
        code: GenerationJoinCodeAuthorityV1,
        diagnostics: Arc<dyn GenerationDiagnosticEvidenceAuthorityV1>,
    ) -> Self {
        Self { code, diagnostics }
    }
}

impl GenerationDiagnosticJoinReadPort for ProductionGenerationDiagnosticJoinReaderV1 {
    fn read_generation_diagnostics(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationDiagnosticJoinV1> {
        if !self.code.matches(generation) {
            return unavailable();
        }
        map_join(
            self.diagnostics.read_diagnostics(generation),
            |evidence| {
                GenerationDiagnosticJoinV1::join(
                    &self.code.manifest,
                    &self.code.snapshot,
                    &evidence.records,
                    &evidence.watermark,
                )
            },
            |join| matches!(join.coverage, GenerationDiagnosticJoinCoverageV1::Complete),
            diagnostic_error_state,
        )
    }
}

#[derive(Clone, Debug)]
pub struct GenerationTestAttributionEvidenceV1 {
    pub attributions: Vec<GenerationTestAttributionV1>,
    pub occurrences: Vec<TestAttributionOccurrenceV1>,
    pub watermark: TestAttributionWatermarkV1,
}

pub trait GenerationTestAttributionEvidenceAuthorityV1: Send + Sync {
    fn read_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestAttributionEvidenceV1>;
}

pub struct ProductionGenerationTestAttributionJoinReaderV1 {
    code: GenerationJoinCodeAuthorityV1,
    attribution: Arc<dyn GenerationTestAttributionEvidenceAuthorityV1>,
}

impl ProductionGenerationTestAttributionJoinReaderV1 {
    pub fn new(
        code: GenerationJoinCodeAuthorityV1,
        attribution: Arc<dyn GenerationTestAttributionEvidenceAuthorityV1>,
    ) -> Self {
        Self { code, attribution }
    }
}

impl GenerationTestAttributionJoinReadPort for ProductionGenerationTestAttributionJoinReaderV1 {
    fn read_test_attribution(
        &self,
        generation: &CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
        if !self.code.matches(generation) {
            return unavailable();
        }
        map_join(
            self.attribution.read_attribution(generation),
            |evidence| {
                GenerationTestJoinV1::join(
                    &self.code.manifest,
                    &self.code.snapshot,
                    &evidence.attributions,
                    &evidence.occurrences,
                    &evidence.watermark,
                )
            },
            |join| matches!(join.coverage, GenerationTestJoinCoverageV1::Complete),
            test_error_state,
        )
    }
}

fn unavailable<T>() -> GenerationProviderReadV1<T> {
    GenerationProviderReadV1 {
        provider_state: ProviderEvaluationStateV1::Unavailable,
        coverage: GenerationProviderCoverageV1::Unavailable,
        evidence: None,
    }
}

fn map_join<Raw, Joined, Error>(
    read: GenerationProviderReadV1<Raw>,
    join: impl FnOnce(&Raw) -> Result<Joined, Error>,
    complete: impl FnOnce(&Joined) -> bool,
    error_state: impl FnOnce(&Error) -> ProviderEvaluationStateV1,
) -> GenerationProviderReadV1<Joined> {
    if read.validate().is_err() {
        return abstain(ProviderEvaluationStateV1::Failed);
    }
    let Some(raw) = read.evidence.as_ref() else {
        return GenerationProviderReadV1 {
            provider_state: read.provider_state,
            coverage: read.coverage,
            evidence: None,
        };
    };
    let joined = match join(raw) {
        Ok(joined) => joined,
        Err(error) => return abstain(error_state(&error)),
    };
    let is_complete = complete(&joined);
    let (provider_state, coverage) = if !is_complete
        && read.provider_state == ProviderEvaluationStateV1::SupportedCompletedComplete
    {
        (
            ProviderEvaluationStateV1::Partial,
            partial_coverage(read.coverage),
        )
    } else {
        (read.provider_state, read.coverage)
    };
    GenerationProviderReadV1 {
        provider_state,
        coverage,
        evidence: Some(joined),
    }
}

fn partial_coverage(coverage: GenerationProviderCoverageV1) -> GenerationProviderCoverageV1 {
    match coverage {
        GenerationProviderCoverageV1::Complete {
            examined,
            eligible,
            excluded,
        } => GenerationProviderCoverageV1::Partial {
            examined,
            eligible,
            excluded,
            unknown: 0,
            capped: false,
        },
        coverage => coverage,
    }
}

fn abstain<T>(provider_state: ProviderEvaluationStateV1) -> GenerationProviderReadV1<T> {
    GenerationProviderReadV1 {
        provider_state,
        coverage: GenerationProviderCoverageV1::Unavailable,
        evidence: None,
    }
}

fn git_error_state(error: &GenerationGitJoinErrorV1) -> ProviderEvaluationStateV1 {
    match error {
        GenerationGitJoinErrorV1::StaleGenerationWatermark
        | GenerationGitJoinErrorV1::RepositoryMismatch
        | GenerationGitJoinErrorV1::WorktreeMismatch
        | GenerationGitJoinErrorV1::ReferenceMismatch
        | GenerationGitJoinErrorV1::StaleSourceRevision
        | GenerationGitJoinErrorV1::StaleContentWatermark
        | GenerationGitJoinErrorV1::StaleGitEvidence
        | GenerationGitJoinErrorV1::BlamePathMismatch
        | GenerationGitJoinErrorV1::StaleSymbolGeneration(_)
        | GenerationGitJoinErrorV1::StaleSymbolFile(_)
        | GenerationGitJoinErrorV1::StaleSymbolContent(_)
        | GenerationGitJoinErrorV1::MissingSnapshotFile(_)
        | GenerationGitJoinErrorV1::ContentMismatch(_)
        | GenerationGitJoinErrorV1::DispositionMismatch(_) => ProviderEvaluationStateV1::Stale,
        GenerationGitJoinErrorV1::DuplicateSymbolBinding(_)
        | GenerationGitJoinErrorV1::DuplicateImpact(_)
        | GenerationGitJoinErrorV1::InvalidSymbolLineRange(_)
        | GenerationGitJoinErrorV1::DuplicateContentIdentity(_)
        | GenerationGitJoinErrorV1::MissingContentIdentity(_)
        | GenerationGitJoinErrorV1::Contract(_) => ProviderEvaluationStateV1::Failed,
    }
}

fn diagnostic_error_state(error: &GenerationDiagnosticJoinErrorV1) -> ProviderEvaluationStateV1 {
    match error {
        GenerationDiagnosticJoinErrorV1::StaleGenerationWatermark
        | GenerationDiagnosticJoinErrorV1::StaleDiagnosticWatermark => {
            ProviderEvaluationStateV1::Stale
        }
        GenerationDiagnosticJoinErrorV1::DuplicateDiagnostic(_)
        | GenerationDiagnosticJoinErrorV1::Contract(_) => ProviderEvaluationStateV1::Failed,
    }
}

fn test_error_state(error: &GenerationTestJoinErrorV1) -> ProviderEvaluationStateV1 {
    match error {
        GenerationTestJoinErrorV1::StaleGenerationWatermark
        | GenerationTestJoinErrorV1::StaleAttributionWatermark => ProviderEvaluationStateV1::Stale,
        GenerationTestJoinErrorV1::DuplicateOccurrence(_)
        | GenerationTestJoinErrorV1::Contract(_) => ProviderEvaluationStateV1::Failed,
    }
}
