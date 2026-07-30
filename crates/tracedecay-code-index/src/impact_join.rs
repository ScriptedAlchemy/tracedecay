//! Generation-exact joins over existing graph-impact and affected-test reads.
//!
//! The graph and test providers remain authoritative. This module retains
//! their typed outcomes and resolves only exact immutable occurrence bindings;
//! it does not traverse a graph, select tests, or persist another result set.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, ContentDigest, FileOccurrenceId, ManifestDigest,
    ProviderEvaluationStateV1, SymbolOccurrenceId, ValidatedCodeSnapshotV1,
};

use super::capabilities::expected_seal_digest;
use super::provider::{
    CodeIndexAffectedTestsEvidenceV1, CodeIndexGraphImpactEvidenceV1,
    GenerationProviderContractErrorV1, GenerationProviderReadV1,
};

/// Exact generation-local symbol occurrence supplied by the canonical graph
/// authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct GenerationOccurrenceBindingV1 {
    pub generation_id: CodeGenerationId,
    pub symbol_occurrence_id: SymbolOccurrenceId,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
}

/// Composite coverage keeps graph and test provider states independent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum GenerationImpactJoinCoverageV1 {
    Complete,
    Partial {
        graph: ProviderEvaluationStateV1,
        affected_tests: ProviderEvaluationStateV1,
    },
    Unavailable {
        graph: ProviderEvaluationStateV1,
        affected_tests: ProviderEvaluationStateV1,
    },
}

/// Read-side composition of graph impact and affected tests for one immutable
/// code generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationImpactJoinV1 {
    pub generation_id: CodeGenerationId,
    pub code_snapshot_digest: ManifestDigest,
    pub code_content_identity: ContentDigest,
    pub graph_provider: GenerationProviderReadV1<CodeIndexGraphImpactEvidenceV1>,
    pub test_provider: GenerationProviderReadV1<CodeIndexAffectedTestsEvidenceV1>,
    pub affected_callers: Vec<GenerationOccurrenceBindingV1>,
    pub affected_tests: Vec<GenerationOccurrenceBindingV1>,
    pub coverage: GenerationImpactJoinCoverageV1,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenerationImpactJoinErrorV1 {
    #[error("the code generation does not seal the supplied sanitized snapshot")]
    StaleGenerationWatermark,
    #[error("duplicate occurrence binding for {0}")]
    DuplicateOccurrence(SymbolOccurrenceId),
    #[error("graph impact names unknown file occurrence {0}")]
    MissingFile(FileOccurrenceId),
    #[error("impact evidence names unknown symbol occurrence {0}")]
    MissingOccurrence(SymbolOccurrenceId),
    #[error("occurrence {0} belongs to another generation")]
    StaleOccurrenceGeneration(SymbolOccurrenceId),
    #[error("occurrence {0} has stale file content")]
    StaleOccurrenceContent(SymbolOccurrenceId),
    #[error("provider outcome is inconsistent: {0}")]
    Provider(GenerationProviderContractErrorV1),
    #[error("invalid generation or impact evidence: {0}")]
    Contract(String),
}

impl GenerationImpactJoinV1 {
    pub fn join(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        graph_provider: GenerationProviderReadV1<CodeIndexGraphImpactEvidenceV1>,
        test_provider: GenerationProviderReadV1<CodeIndexAffectedTestsEvidenceV1>,
        occurrences: &[GenerationOccurrenceBindingV1],
    ) -> Result<Self, GenerationImpactJoinErrorV1> {
        validate_generation_snapshot(generation, snapshot)?;
        graph_provider
            .validate()
            .map_err(GenerationImpactJoinErrorV1::Provider)?;
        test_provider
            .validate()
            .map_err(GenerationImpactJoinErrorV1::Provider)?;

        let occurrence_by_id = index_occurrences(occurrences)?;
        let content_by_file: BTreeMap<&FileOccurrenceId, &ContentDigest> = snapshot
            .snapshot
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, &file.content_digest))
            .collect();

        let mut affected_callers = Vec::new();
        if let Some(graph) = &graph_provider.evidence {
            validate_unique(&graph.affected_files)?;
            validate_unique(&graph.affected_callers)?;
            validate_unique(&graph.evidence_anchors)?;
            for file in &graph.affected_files {
                if !content_by_file.contains_key(file) {
                    return Err(GenerationImpactJoinErrorV1::MissingFile(file.clone()));
                }
            }
            for caller in &graph.affected_callers {
                affected_callers.push(resolve_occurrence(
                    generation,
                    &content_by_file,
                    &occurrence_by_id,
                    caller,
                )?);
            }
        }

        let mut affected_tests = Vec::new();
        if let Some(tests) = &test_provider.evidence {
            validate_unique(&tests.tests)?;
            for test in &tests.tests {
                affected_tests.push(resolve_occurrence(
                    generation,
                    &content_by_file,
                    &occurrence_by_id,
                    test,
                )?);
            }
        }

        let coverage = match (
            graph_provider.provider_state,
            test_provider.provider_state,
            graph_provider.evidence.is_some(),
            test_provider.evidence.is_some(),
        ) {
            (
                ProviderEvaluationStateV1::SupportedCompletedComplete,
                ProviderEvaluationStateV1::SupportedCompletedComplete,
                true,
                true,
            ) => GenerationImpactJoinCoverageV1::Complete,
            (graph, affected_tests, false, false) => GenerationImpactJoinCoverageV1::Unavailable {
                graph,
                affected_tests,
            },
            (graph, affected_tests, _, _) => GenerationImpactJoinCoverageV1::Partial {
                graph,
                affected_tests,
            },
        };

        Ok(Self {
            generation_id: generation.generation_id.clone(),
            code_snapshot_digest: generation.snapshot_digest.clone(),
            code_content_identity: snapshot.snapshot.content_identity.clone(),
            graph_provider,
            test_provider,
            affected_callers,
            affected_tests,
            coverage,
        })
    }
}

fn index_occurrences(
    occurrences: &[GenerationOccurrenceBindingV1],
) -> Result<
    BTreeMap<&SymbolOccurrenceId, &GenerationOccurrenceBindingV1>,
    GenerationImpactJoinErrorV1,
> {
    let mut by_id = BTreeMap::new();
    for occurrence in occurrences {
        occurrence
            .generation_id
            .validate()
            .map_err(|error| GenerationImpactJoinErrorV1::Contract(error.to_string()))?;
        occurrence
            .symbol_occurrence_id
            .validate()
            .map_err(|error| GenerationImpactJoinErrorV1::Contract(error.to_string()))?;
        occurrence
            .file_occurrence_id
            .validate()
            .map_err(|error| GenerationImpactJoinErrorV1::Contract(error.to_string()))?;
        occurrence
            .content_digest
            .validate()
            .map_err(|error| GenerationImpactJoinErrorV1::Contract(error.to_string()))?;
        if by_id
            .insert(&occurrence.symbol_occurrence_id, occurrence)
            .is_some()
        {
            return Err(GenerationImpactJoinErrorV1::DuplicateOccurrence(
                occurrence.symbol_occurrence_id.clone(),
            ));
        }
    }
    Ok(by_id)
}

fn resolve_occurrence(
    generation: &CodeGenerationManifestV1,
    content_by_file: &BTreeMap<&FileOccurrenceId, &ContentDigest>,
    occurrence_by_id: &BTreeMap<&SymbolOccurrenceId, &GenerationOccurrenceBindingV1>,
    occurrence_id: &SymbolOccurrenceId,
) -> Result<GenerationOccurrenceBindingV1, GenerationImpactJoinErrorV1> {
    let occurrence = occurrence_by_id
        .get(occurrence_id)
        .copied()
        .ok_or_else(|| GenerationImpactJoinErrorV1::MissingOccurrence(occurrence_id.clone()))?;
    if occurrence.generation_id != generation.generation_id {
        return Err(GenerationImpactJoinErrorV1::StaleOccurrenceGeneration(
            occurrence_id.clone(),
        ));
    }
    let Some(content) = content_by_file.get(&occurrence.file_occurrence_id) else {
        return Err(GenerationImpactJoinErrorV1::MissingFile(
            occurrence.file_occurrence_id.clone(),
        ));
    };
    if *content != &occurrence.content_digest {
        return Err(GenerationImpactJoinErrorV1::StaleOccurrenceContent(
            occurrence_id.clone(),
        ));
    }
    Ok(occurrence.clone())
}

fn validate_unique<T: Ord + Clone>(values: &[T]) -> Result<(), GenerationImpactJoinErrorV1> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value.clone()) {
            return Err(GenerationImpactJoinErrorV1::Contract(
                "provider returned duplicate evidence identity".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_generation_snapshot(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
) -> Result<(), GenerationImpactJoinErrorV1> {
    snapshot
        .snapshot
        .validate()
        .map_err(|error| GenerationImpactJoinErrorV1::Contract(error.to_string()))?;
    if generation.snapshot_digest != snapshot.intake_digest {
        return Err(GenerationImpactJoinErrorV1::StaleGenerationWatermark);
    }
    generation
        .validate()
        .map_err(|error| GenerationImpactJoinErrorV1::Contract(error.to_string()))?;
    let seal = expected_seal_digest(generation)
        .map_err(|error| GenerationImpactJoinErrorV1::Contract(error.to_string()))?;
    if seal != generation.seal.expected_digest {
        return Err(GenerationImpactJoinErrorV1::StaleGenerationWatermark);
    }
    Ok(())
}
