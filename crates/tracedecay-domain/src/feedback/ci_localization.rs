//! Advisory CI-failure localization contracts.
//!
//! CI remains the execution and pass/fail authority. These types retain only
//! localized evidence and inert suggestions; they contain no runnable CI
//! operation, retry command, scheduler token, or execution receipt.

use serde::{Deserialize, Serialize};

use crate::code_intelligence::identity::{
    CodeGenerationId, FileOccurrenceId, SourceSpan, SymbolOccurrenceId,
};
use crate::research::{
    CommitId, DomainError, ManifestDigest, ProviderId, RetrievalAnchorId, UtcMicros,
};

use super::FeedbackScopeV1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiFailureCoverageV1 {
    Complete,
    Partial,
    Unavailable,
    Denied,
    Stale,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiFailureLocalizationStateV1 {
    Complete,
    Partial,
    Stale,
    Unavailable,
    Denied,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiFailureKindV1 {
    TestFailure,
    CompileFailure,
    LintFailure,
    InfrastructureFailure,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiCallerRelationV1 {
    DirectCall,
    TransitiveCall,
}

/// A non-executable target category for a human-visible rerun suggestion.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CiInertRerunTargetV1 {
    Workflow,
    Job,
    Test,
}

/// Provider-owned run identity. Every field is an opaque provider identifier;
/// none is a command, URL, credential, or executable retry handle.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureRunIdentityV1 {
    pub workflow_id: String,
    pub job_id: String,
    pub check_suite_id: String,
    pub check_run_id: String,
    pub run_id: String,
    pub attempt_id: String,
}

impl CiFailureRunIdentityV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        for (value, field) in [
            (&self.workflow_id, "ci workflow id"),
            (&self.job_id, "ci job id"),
            (&self.check_suite_id, "ci check suite id"),
            (&self.check_run_id, "ci check run id"),
            (&self.run_id, "ci run id"),
            (&self.attempt_id, "ci attempt id"),
        ] {
            super::validate_label(value, field)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureParserIdentityV1 {
    pub parser_id: String,
    pub parser_version: String,
}

impl CiFailureParserIdentityV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        super::validate_label(&self.parser_id, "ci failure parser id")?;
        super::validate_label(&self.parser_version, "ci failure parser version")
    }
}

/// CI branch evidence must bind the provider-observed head to the immutable
/// head of the feedback scope rather than a mutable branch label alone.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureBranchEvidenceV1 {
    pub scope: FeedbackScopeV1,
    pub provider_head_commit_id: CommitId,
}

impl CiFailureBranchEvidenceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.scope.validate()?;
        self.provider_head_commit_id.validate()?;
        if self.scope.head_commit_id != self.provider_head_commit_id {
            return Err(DomainError::NonCanonical {
                field: "ci failure provider head commit",
            });
        }
        Ok(())
    }
}

/// Immutable generation evidence used to prevent a CI localization from
/// claiming that it applies to a different code-intelligence generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureGenerationEvidenceV1 {
    pub generation_id: CodeGenerationId,
    pub retrieval_anchor_id: RetrievalAnchorId,
}

impl CiFailureGenerationEvidenceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.generation_id.validate()?;
        self.retrieval_anchor_id.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureSymbolEvidenceV1 {
    pub retrieval_anchor_id: RetrievalAnchorId,
    pub file: FileOccurrenceId,
    pub span: SourceSpan,
    pub symbol: SymbolOccurrenceId,
}

impl CiFailureSymbolEvidenceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.retrieval_anchor_id.validate()?;
        self.file.validate()?;
        self.span.validate()?;
        self.symbol.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureCallerEvidenceV1 {
    pub retrieval_anchor_id: RetrievalAnchorId,
    pub caller_symbol: SymbolOccurrenceId,
    pub relation: CiCallerRelationV1,
}

impl CiFailureCallerEvidenceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.retrieval_anchor_id.validate()?;
        self.caller_symbol.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureTestEvidenceV1 {
    pub retrieval_anchor_id: RetrievalAnchorId,
    pub test_symbol: SymbolOccurrenceId,
}

impl CiFailureTestEvidenceV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.retrieval_anchor_id.validate()?;
        self.test_symbol.validate()
    }
}

/// A reference-only suggestion for a human or external CI UI. It intentionally
/// contains no command, client, credential, or method that could execute CI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiInertRerunHintV1 {
    pub target: CiInertRerunTargetV1,
    pub retrieval_anchor_id: Option<RetrievalAnchorId>,
}

impl CiInertRerunHintV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.retrieval_anchor_id
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)
    }
}

/// A localized CI failure. The result never claims that TraceDecay ran,
/// reran, verified, or influenced CI.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureLocalizationResultV1 {
    pub provider: ProviderId,
    pub run: CiFailureRunIdentityV1,
    pub parser: CiFailureParserIdentityV1,
    pub state: CiFailureLocalizationStateV1,
    pub coverage: CiFailureCoverageV1,
    pub failure_kind: CiFailureKindV1,
    pub failure_anchor: RetrievalAnchorId,
    pub failure_excerpt_digest: ManifestDigest,
    pub branch: CiFailureBranchEvidenceV1,
    pub generation: Option<CiFailureGenerationEvidenceV1>,
    pub symbol: Option<CiFailureSymbolEvidenceV1>,
    pub callers: Vec<CiFailureCallerEvidenceV1>,
    pub tests: Vec<CiFailureTestEvidenceV1>,
    pub rerun_hints: Vec<CiInertRerunHintV1>,
    pub observed_at: UtcMicros,
}

impl CiFailureLocalizationResultV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.run.validate()?;
        self.parser.validate()?;
        self.failure_anchor.validate()?;
        self.failure_excerpt_digest.validate()?;
        self.branch.validate()?;
        self.generation
            .as_ref()
            .map_or(Ok(()), CiFailureGenerationEvidenceV1::validate)?;
        self.symbol
            .as_ref()
            .map_or(Ok(()), CiFailureSymbolEvidenceV1::validate)?;
        for caller in &self.callers {
            caller.validate()?;
        }
        for test in &self.tests {
            test.validate()?;
        }
        for hint in &self.rerun_hints {
            hint.validate()?;
        }
        let coverage_matches = matches!(
            (self.state, self.coverage),
            (
                CiFailureLocalizationStateV1::Complete,
                CiFailureCoverageV1::Complete
            ) | (
                CiFailureLocalizationStateV1::Partial,
                CiFailureCoverageV1::Partial
            ) | (
                CiFailureLocalizationStateV1::Stale,
                CiFailureCoverageV1::Stale
            ) | (
                CiFailureLocalizationStateV1::Unavailable,
                CiFailureCoverageV1::Unavailable
            ) | (
                CiFailureLocalizationStateV1::Denied,
                CiFailureCoverageV1::Denied
            ) | (
                CiFailureLocalizationStateV1::Failed,
                CiFailureCoverageV1::Partial | CiFailureCoverageV1::Unavailable
            )
        );
        if !coverage_matches {
            return Err(DomainError::NonCanonical {
                field: "ci failure localization coverage",
            });
        }
        if self.state == CiFailureLocalizationStateV1::Complete && self.generation.is_none() {
            return Err(DomainError::NonCanonical {
                field: "complete ci failure generation evidence",
            });
        }
        if matches!(
            self.failure_kind,
            CiFailureKindV1::TestFailure
                | CiFailureKindV1::CompileFailure
                | CiFailureKindV1::LintFailure
        ) && self.state == CiFailureLocalizationStateV1::Complete
            && self.symbol.is_none()
        {
            return Err(DomainError::NonCanonical {
                field: "complete ci failure symbol evidence",
            });
        }
        if self.failure_kind == CiFailureKindV1::TestFailure
            && self.state == CiFailureLocalizationStateV1::Complete
            && self.tests.is_empty()
        {
            return Err(DomainError::NonCanonical {
                field: "complete ci failure test evidence",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::{CommitId, ProjectId, RepositoryId, WorktreeId};

    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn result() -> CiFailureLocalizationResultV1 {
        let scope = FeedbackScopeV1 {
            project_id: ProjectId::new("project.ci").unwrap(),
            repository_id: RepositoryId::new("repository.ci").unwrap(),
            worktree_id: WorktreeId::new("worktree.ci").unwrap(),
            branch_ref: "refs/heads/ci".to_owned(),
            head_commit_id: CommitId::new("commit.ci").unwrap(),
        };
        CiFailureLocalizationResultV1 {
            provider: ProviderId::new("provider.ci").unwrap(),
            run: CiFailureRunIdentityV1 {
                workflow_id: "workflow.1".to_owned(),
                job_id: "job.1".to_owned(),
                check_suite_id: "suite.1".to_owned(),
                check_run_id: "check.1".to_owned(),
                run_id: "run.1".to_owned(),
                attempt_id: "attempt.1".to_owned(),
            },
            parser: CiFailureParserIdentityV1 {
                parser_id: "parser.fixture".to_owned(),
                parser_version: "1".to_owned(),
            },
            state: CiFailureLocalizationStateV1::Complete,
            coverage: CiFailureCoverageV1::Complete,
            failure_kind: CiFailureKindV1::InfrastructureFailure,
            failure_anchor: RetrievalAnchorId::new("anchor.ci").unwrap(),
            failure_excerpt_digest: ManifestDigest::new(SHA).unwrap(),
            branch: CiFailureBranchEvidenceV1 {
                provider_head_commit_id: scope.head_commit_id.clone(),
                scope,
            },
            generation: None,
            symbol: None,
            callers: Vec::new(),
            tests: Vec::new(),
            rerun_hints: Vec::new(),
            observed_at: UtcMicros(1),
        }
    }

    #[test]
    fn complete_ci_localization_requires_exact_generation_evidence() {
        assert!(result().validate().is_err());
    }

    #[test]
    fn ci_provider_state_and_coverage_cannot_be_collapsed() {
        let mut mismatched = result();
        mismatched.state = CiFailureLocalizationStateV1::Partial;
        assert!(mismatched.validate().is_err());
    }
}
