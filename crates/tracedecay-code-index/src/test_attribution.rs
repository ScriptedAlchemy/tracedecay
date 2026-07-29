//! Generation-exact joins for Plan 25/Plan 05 test-attribution evidence.
//!
//! This module validates immutable generation, source-revision, test-map, and
//! occurrence/content watermarks. It does not discover tests, execute them,
//! traverse the graph, or rank candidates. The owning attribution evidence
//! class is preserved verbatim, and stale/unknown evidence can never be
//! upgraded to proof of execution or correctness.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, CommitId, ComponentVersion, ContentDigest,
    FileOccurrenceId, GenerationTestAttributionV1, ManifestDigest, SymbolOccurrenceId,
    TestAttributionEvidenceClassV1, ValidatedCodeSnapshotV1, canonical_sha256,
};

use super::capabilities::expected_seal_digest;

const TEST_ATTRIBUTION_EVIDENCE_SEPARATOR: &str = "tracedecay.test-attribution-evidence.v1";

/// Completeness reported by the test-attribution producer.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum TestAttributionJoinInputCoverageV1 {
    Complete,
    Partial { reason: String },
}

/// Independent test-map watermark retained beside the code-generation
/// watermark.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestAttributionWatermarkV1 {
    pub generation_id: CodeGenerationId,
    pub snapshot_digest: ManifestDigest,
    pub content_identity: ContentDigest,
    pub source_revision: Option<CommitId>,
    pub attribution_revision: ComponentVersion,
    pub evidence_digest: ManifestDigest,
    pub coverage: TestAttributionJoinInputCoverageV1,
}

#[derive(Serialize)]
struct TestAttributionEvidenceDigestInput<'a> {
    domain: &'static str,
    generation_id: &'a CodeGenerationId,
    snapshot_digest: &'a ManifestDigest,
    content_identity: &'a ContentDigest,
    source_revision: &'a Option<CommitId>,
    attribution_revision: &'a ComponentVersion,
    coverage: &'a TestAttributionJoinInputCoverageV1,
    attributions: &'a [GenerationTestAttributionV1],
    occurrences: &'a [TestAttributionOccurrenceV1],
}

impl TestAttributionWatermarkV1 {
    pub fn recompute_evidence_digest(
        &self,
        attributions: &[GenerationTestAttributionV1],
        occurrences: &[TestAttributionOccurrenceV1],
    ) -> Result<ManifestDigest, GenerationTestJoinErrorV1> {
        let attributions = canonical_attributions(attributions)?;
        let occurrences = canonical_occurrences(occurrences)?;
        canonical_sha256(&TestAttributionEvidenceDigestInput {
            domain: TEST_ATTRIBUTION_EVIDENCE_SEPARATOR,
            generation_id: &self.generation_id,
            snapshot_digest: &self.snapshot_digest,
            content_identity: &self.content_identity,
            source_revision: &self.source_revision,
            attribution_revision: &self.attribution_revision,
            coverage: &self.coverage,
            attributions: &attributions,
            occurrences: &occurrences,
        })
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))
    }
}

/// Exact generation-local symbol occurrence/content binding supplied by the
/// canonical graph/test-map authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct TestAttributionOccurrenceV1 {
    pub occurrence_id: SymbolOccurrenceId,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
}

/// Why the joined attribution set is not complete current evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationTestJoinPartialReasonV1 {
    InputPartial { reason: String },
    StaleGeneration { test_occurrence: SymbolOccurrenceId },
    StaleSourceRevision { test_occurrence: SymbolOccurrenceId },
    AttributionRevisionMismatch { test_occurrence: SymbolOccurrenceId },
    MissingOccurrence { occurrence_id: SymbolOccurrenceId },
    StaleContent { occurrence_id: SymbolOccurrenceId },
    StaleEvidence { test_occurrence: SymbolOccurrenceId },
    UnknownUnsupported { test_occurrence: SymbolOccurrenceId },
}

/// Overall test-attribution join coverage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum GenerationTestJoinCoverageV1 {
    Complete,
    Partial {
        reasons: Vec<GenerationTestJoinPartialReasonV1>,
    },
}

/// Typed disposition of one attribution record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum GenerationTestJoinDispositionV1 {
    Current {
        evidence_class: TestAttributionEvidenceClassV1,
    },
    StaleEvidence,
    UnknownUnsupported,
    StaleGeneration {
        record_generation: CodeGenerationId,
    },
    StaleSourceRevision {
        expected: Option<CommitId>,
        observed: Option<CommitId>,
    },
    AttributionRevisionMismatch {
        expected: ComponentVersion,
        observed: ComponentVersion,
    },
    MissingOccurrence {
        occurrence_id: SymbolOccurrenceId,
    },
    StaleContent {
        occurrence_id: SymbolOccurrenceId,
        expected: ContentDigest,
        observed: ContentDigest,
    },
}

/// One owning attribution record plus resolved exact occurrence evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationTestJoinRecordV1 {
    pub attribution: GenerationTestAttributionV1,
    pub test_occurrence: Option<TestAttributionOccurrenceV1>,
    pub covered_occurrences: Vec<TestAttributionOccurrenceV1>,
    pub disposition: GenerationTestJoinDispositionV1,
}

/// Deterministic generation-aware test-attribution join.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationTestJoinV1 {
    pub generation_id: CodeGenerationId,
    pub code_snapshot_digest: ManifestDigest,
    pub code_content_identity: ContentDigest,
    pub test_watermark: TestAttributionWatermarkV1,
    pub records: Vec<GenerationTestJoinRecordV1>,
    pub coverage: GenerationTestJoinCoverageV1,
}

/// Failures of the join contract. Per-record drift remains a typed successful
/// result.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenerationTestJoinErrorV1 {
    #[error("the code generation does not seal the supplied sanitized snapshot")]
    StaleGenerationWatermark,
    #[error("the test-attribution watermark is stale")]
    StaleAttributionWatermark,
    #[error("duplicate occurrence evidence for {0}")]
    DuplicateOccurrence(SymbolOccurrenceId),
    #[error("invalid generation or attribution evidence: {0}")]
    Contract(String),
}

impl GenerationTestJoinV1 {
    /// Join canonical test-attribution records to one exact code generation.
    pub fn join(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        attributions: &[GenerationTestAttributionV1],
        occurrences: &[TestAttributionOccurrenceV1],
        watermark: &TestAttributionWatermarkV1,
    ) -> Result<Self, GenerationTestJoinErrorV1> {
        validate_generation_snapshot(generation, snapshot)?;
        validate_watermark(generation, snapshot, watermark)?;
        let occurrence_by_id = index_occurrences(occurrences)?;
        let attributions = canonical_attributions(attributions)?;
        if watermark.recompute_evidence_digest(&attributions, occurrences)?
            != watermark.evidence_digest
        {
            return Err(GenerationTestJoinErrorV1::StaleAttributionWatermark);
        }
        let content_by_file: BTreeMap<&FileOccurrenceId, &ContentDigest> = snapshot
            .snapshot
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, &file.content_digest))
            .collect();

        let mut partial_reasons = match &watermark.coverage {
            TestAttributionJoinInputCoverageV1::Complete => Vec::new(),
            TestAttributionJoinInputCoverageV1::Partial { reason } => {
                vec![GenerationTestJoinPartialReasonV1::InputPartial {
                    reason: reason.clone(),
                }]
            }
        };
        let mut records = Vec::with_capacity(attributions.len());
        for attribution in attributions {
            let test_occurrence = occurrence_by_id.get(&attribution.test_occurrence).copied();
            let covered_occurrences: Vec<TestAttributionOccurrenceV1> = attribution
                .covered_occurrences
                .iter()
                .filter_map(|occurrence| occurrence_by_id.get(occurrence).copied().cloned())
                .collect();
            let disposition = disposition_for(
                generation,
                snapshot,
                watermark,
                &occurrence_by_id,
                &content_by_file,
                &attribution,
                &mut partial_reasons,
            );
            records.push(GenerationTestJoinRecordV1 {
                attribution,
                test_occurrence: test_occurrence.cloned(),
                covered_occurrences,
                disposition,
            });
        }

        partial_reasons.sort();
        partial_reasons.dedup();
        let coverage = if partial_reasons.is_empty() {
            GenerationTestJoinCoverageV1::Complete
        } else {
            GenerationTestJoinCoverageV1::Partial {
                reasons: partial_reasons,
            }
        };
        Ok(Self {
            generation_id: generation.generation_id.clone(),
            code_snapshot_digest: generation.snapshot_digest.clone(),
            code_content_identity: snapshot.snapshot.content_identity.clone(),
            test_watermark: watermark.clone(),
            records,
            coverage,
        })
    }
}

fn canonical_attributions(
    attributions: &[GenerationTestAttributionV1],
) -> Result<Vec<GenerationTestAttributionV1>, GenerationTestJoinErrorV1> {
    let mut canonical = attributions.to_vec();
    for attribution in &canonical {
        validate_attribution(attribution)?;
    }
    canonical.sort_by(|left, right| {
        (
            &left.generation_id,
            &left.source_revision,
            &left.test_occurrence,
            &left.covered_occurrences,
            left.evidence_class,
            &left.attribution_revision,
        )
            .cmp(&(
                &right.generation_id,
                &right.source_revision,
                &right.test_occurrence,
                &right.covered_occurrences,
                right.evidence_class,
                &right.attribution_revision,
            ))
    });
    Ok(canonical)
}

fn canonical_occurrences(
    occurrences: &[TestAttributionOccurrenceV1],
) -> Result<Vec<TestAttributionOccurrenceV1>, GenerationTestJoinErrorV1> {
    index_occurrences(occurrences)?;
    let mut canonical = occurrences.to_vec();
    canonical.sort_by(|left, right| left.occurrence_id.cmp(&right.occurrence_id));
    Ok(canonical)
}

fn disposition_for(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
    watermark: &TestAttributionWatermarkV1,
    occurrences: &BTreeMap<&SymbolOccurrenceId, &TestAttributionOccurrenceV1>,
    content_by_file: &BTreeMap<&FileOccurrenceId, &ContentDigest>,
    attribution: &GenerationTestAttributionV1,
    partial_reasons: &mut Vec<GenerationTestJoinPartialReasonV1>,
) -> GenerationTestJoinDispositionV1 {
    if attribution.generation_id != generation.generation_id {
        partial_reasons.push(GenerationTestJoinPartialReasonV1::StaleGeneration {
            test_occurrence: attribution.test_occurrence.clone(),
        });
        return GenerationTestJoinDispositionV1::StaleGeneration {
            record_generation: attribution.generation_id.clone(),
        };
    }
    if attribution.source_revision != snapshot.snapshot.source_revision {
        partial_reasons.push(GenerationTestJoinPartialReasonV1::StaleSourceRevision {
            test_occurrence: attribution.test_occurrence.clone(),
        });
        return GenerationTestJoinDispositionV1::StaleSourceRevision {
            expected: snapshot.snapshot.source_revision.clone(),
            observed: attribution.source_revision.clone(),
        };
    }
    if attribution.attribution_revision != watermark.attribution_revision {
        partial_reasons.push(
            GenerationTestJoinPartialReasonV1::AttributionRevisionMismatch {
                test_occurrence: attribution.test_occurrence.clone(),
            },
        );
        return GenerationTestJoinDispositionV1::AttributionRevisionMismatch {
            expected: watermark.attribution_revision.clone(),
            observed: attribution.attribution_revision.clone(),
        };
    }

    for occurrence_id in
        std::iter::once(&attribution.test_occurrence).chain(attribution.covered_occurrences.iter())
    {
        let Some(occurrence) = occurrences.get(occurrence_id).copied() else {
            partial_reasons.push(GenerationTestJoinPartialReasonV1::MissingOccurrence {
                occurrence_id: occurrence_id.clone(),
            });
            return GenerationTestJoinDispositionV1::MissingOccurrence {
                occurrence_id: occurrence_id.clone(),
            };
        };
        let Some(expected) = content_by_file.get(&occurrence.file_occurrence_id).copied() else {
            partial_reasons.push(GenerationTestJoinPartialReasonV1::MissingOccurrence {
                occurrence_id: occurrence_id.clone(),
            });
            return GenerationTestJoinDispositionV1::MissingOccurrence {
                occurrence_id: occurrence_id.clone(),
            };
        };
        if expected != &occurrence.content_digest {
            partial_reasons.push(GenerationTestJoinPartialReasonV1::StaleContent {
                occurrence_id: occurrence_id.clone(),
            });
            return GenerationTestJoinDispositionV1::StaleContent {
                occurrence_id: occurrence_id.clone(),
                expected: expected.clone(),
                observed: occurrence.content_digest.clone(),
            };
        }
    }

    match attribution.evidence_class {
        TestAttributionEvidenceClassV1::ConservativeDependencyCandidates
        | TestAttributionEvidenceClassV1::ObservedCoverageCandidates
        | TestAttributionEvidenceClassV1::PredictiveRankedCandidates => {
            GenerationTestJoinDispositionV1::Current {
                evidence_class: attribution.evidence_class,
            }
        }
        TestAttributionEvidenceClassV1::StaleEvidence => {
            partial_reasons.push(GenerationTestJoinPartialReasonV1::StaleEvidence {
                test_occurrence: attribution.test_occurrence.clone(),
            });
            GenerationTestJoinDispositionV1::StaleEvidence
        }
        TestAttributionEvidenceClassV1::UnknownUnsupported => {
            partial_reasons.push(GenerationTestJoinPartialReasonV1::UnknownUnsupported {
                test_occurrence: attribution.test_occurrence.clone(),
            });
            GenerationTestJoinDispositionV1::UnknownUnsupported
        }
    }
}

fn index_occurrences(
    occurrences: &[TestAttributionOccurrenceV1],
) -> Result<BTreeMap<&SymbolOccurrenceId, &TestAttributionOccurrenceV1>, GenerationTestJoinErrorV1>
{
    let mut by_id = BTreeMap::new();
    for occurrence in occurrences {
        occurrence
            .occurrence_id
            .validate()
            .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
        occurrence
            .file_occurrence_id
            .validate()
            .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
        occurrence
            .content_digest
            .validate()
            .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
        if by_id
            .insert(&occurrence.occurrence_id, occurrence)
            .is_some()
        {
            return Err(GenerationTestJoinErrorV1::DuplicateOccurrence(
                occurrence.occurrence_id.clone(),
            ));
        }
    }
    Ok(by_id)
}

fn validate_attribution(
    attribution: &GenerationTestAttributionV1,
) -> Result<(), GenerationTestJoinErrorV1> {
    attribution
        .generation_id
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    if let Some(source_revision) = &attribution.source_revision {
        source_revision
            .validate()
            .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    }
    attribution
        .test_occurrence
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    attribution
        .attribution_revision
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    for occurrence in &attribution.covered_occurrences {
        occurrence
            .validate()
            .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    }
    if attribution
        .covered_occurrences
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(GenerationTestJoinErrorV1::Contract(
            "covered occurrence identities must be sorted and unique".to_owned(),
        ));
    }
    Ok(())
}

fn validate_generation_snapshot(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
) -> Result<(), GenerationTestJoinErrorV1> {
    snapshot
        .snapshot
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    if generation.snapshot_digest != snapshot.intake_digest {
        return Err(GenerationTestJoinErrorV1::StaleGenerationWatermark);
    }
    generation
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    let seal = expected_seal_digest(generation)
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    if seal != generation.seal.expected_digest {
        return Err(GenerationTestJoinErrorV1::StaleGenerationWatermark);
    }
    Ok(())
}

fn validate_watermark(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
    watermark: &TestAttributionWatermarkV1,
) -> Result<(), GenerationTestJoinErrorV1> {
    watermark
        .generation_id
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .snapshot_digest
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .content_identity
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .attribution_revision
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .evidence_digest
        .validate()
        .map_err(|error| GenerationTestJoinErrorV1::Contract(error.to_string()))?;
    if watermark.generation_id != generation.generation_id
        || watermark.snapshot_digest != generation.snapshot_digest
        || watermark.content_identity != snapshot.snapshot.content_identity
        || watermark.source_revision != snapshot.snapshot.source_revision
    {
        return Err(GenerationTestJoinErrorV1::StaleAttributionWatermark);
    }
    Ok(())
}
