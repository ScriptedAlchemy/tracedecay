//! Generation-exact joins for Plan 25/Plan 05 test-attribution evidence.
//!
//! This module validates immutable generation, source-revision, test-map, and
//! occurrence/content watermarks. It does not discover tests, execute them,
//! traverse the graph, or rank candidates. The owning attribution evidence
//! class is preserved verbatim, and stale/unknown evidence can never be
//! upgraded to proof of execution or correctness.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::ser::SerializeSeq;
use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, CommitId, ComponentVersion, ContentDigest,
    FileOccurrenceId, GenerationTestAttributionV1, ManifestDigest, SymbolOccurrenceId,
    TestAttributionEvidenceClassV1, canonical_sha256,
};

use super::capabilities::expected_seal_digest;
use crate::intake::ValidatedCodeSnapshotV1;

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
    attributions: CheckedSlice<'a, &'a GenerationTestAttributionV1>,
    occurrences: CheckedSlice<'a, &'a TestAttributionOccurrenceV1>,
}

/// Preserve ordinary slice serialization while checking the owning operation
/// between records. Canonical ordering and hashing stay with canonical_sha256.
struct CheckedSlice<'a, T> {
    items: &'a [T],
    interrupted: &'a dyn Fn() -> bool,
}

impl<T: Serialize> Serialize for CheckedSlice<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.items.len()))?;
        for item in self.items {
            if (self.interrupted)() {
                return Err(serde::ser::Error::custom("test attribution interrupted"));
            }
            sequence.serialize_element(item)?;
        }
        sequence.end()
    }
}

fn checkpoint(interrupted: &dyn Fn() -> bool) -> Result<(), GenerationTestJoinErrorV1> {
    if interrupted() {
        Err(GenerationTestJoinErrorV1::Interrupted)
    } else {
        Ok(())
    }
}

impl TestAttributionWatermarkV1 {
    pub fn recompute_evidence_digest(
        &self,
        attributions: &[GenerationTestAttributionV1],
        occurrences: &[TestAttributionOccurrenceV1],
    ) -> Result<ManifestDigest, GenerationTestJoinErrorV1> {
        let attributions = canonical_attributions(attributions.iter().collect())?;
        index_occurrences(occurrences)?;
        let occurrences = canonical_occurrences(occurrences);
        self.digest_canonical(&attributions, &occurrences)
    }

    fn digest_canonical(
        &self,
        attributions: &[&GenerationTestAttributionV1],
        occurrences: &[&TestAttributionOccurrenceV1],
    ) -> Result<ManifestDigest, GenerationTestJoinErrorV1> {
        canonical_sha256(&TestAttributionEvidenceDigestInput {
            domain: TEST_ATTRIBUTION_EVIDENCE_SEPARATOR,
            generation_id: &self.generation_id,
            snapshot_digest: &self.snapshot_digest,
            content_identity: &self.content_identity,
            source_revision: &self.source_revision,
            attribution_revision: &self.attribution_revision,
            coverage: &self.coverage,
            attributions: CheckedSlice {
                items: attributions,
                interrupted: &|| false,
            },
            occurrences: CheckedSlice {
                items: occurrences,
                interrupted: &|| false,
            },
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
    /// Shared with every other record covering the same occurrence: a test
    /// covers its whole transitive closure, so owning each occurrence per
    /// record made the join quadratic in resident strings.
    pub covered_occurrences: Vec<Arc<TestAttributionOccurrenceV1>>,
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
    #[error("test attribution preparation was interrupted")]
    Interrupted,
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
    /// Bytes this join holds: record slots, each record's attribution
    /// identifiers and covered-occurrence handles, and the shared occurrence
    /// copies once each.
    #[must_use]
    pub fn retained_bytes(&self) -> u64 {
        use std::mem::size_of;
        let occurrence_bytes = |occurrence: &TestAttributionOccurrenceV1| {
            size_of::<TestAttributionOccurrenceV1>()
                .saturating_add(occurrence.occurrence_id.as_str().len())
                .saturating_add(occurrence.file_occurrence_id.as_str().len())
                .saturating_add(occurrence.content_digest.as_str().len())
        };
        let mut shared = HashSet::new();
        let bytes = self.records.iter().fold(0_usize, |bytes, record| {
            let covered_ids =
                record
                    .attribution
                    .covered_occurrences
                    .iter()
                    .fold(0_usize, |bytes, id| {
                        bytes
                            .saturating_add(size_of::<SymbolOccurrenceId>())
                            .saturating_add(id.as_str().len())
                    });
            let covered_shared =
                record
                    .covered_occurrences
                    .iter()
                    .fold(0_usize, |bytes, covered| {
                        let first = shared.insert(Arc::as_ptr(covered));
                        bytes
                            .saturating_add(size_of::<Arc<TestAttributionOccurrenceV1>>())
                            .saturating_add(if first { occurrence_bytes(covered) } else { 0 })
                    });
            bytes
                .saturating_add(size_of::<GenerationTestJoinRecordV1>())
                .saturating_add(record.attribution.test_occurrence.as_str().len())
                .saturating_add(record.test_occurrence.as_ref().map_or(0, occurrence_bytes))
                .saturating_add(covered_ids)
                .saturating_add(covered_shared)
        });
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

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
        let attributions = canonical_attributions(attributions.to_vec())?;
        if watermark.digest_canonical(
            &attributions.iter().collect::<Vec<_>>(),
            &canonical_occurrences(occurrences),
        )? != watermark.evidence_digest
        {
            return Err(GenerationTestJoinErrorV1::StaleAttributionWatermark);
        }
        Self::join_canonical(
            generation,
            snapshot,
            attributions,
            occurrence_by_id,
            watermark,
            &|| false,
        )
    }

    /// Mint evidence from the owning producer without immediately rehashing
    /// its complete relation set as though it came from another authority.
    pub(crate) fn produce(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        attributions: Vec<GenerationTestAttributionV1>,
        occurrences: &[TestAttributionOccurrenceV1],
        attribution_revision: ComponentVersion,
        coverage: TestAttributionJoinInputCoverageV1,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<Self, GenerationTestJoinErrorV1> {
        checkpoint(interrupted)?;
        validate_generation_snapshot(generation, snapshot)?;
        let occurrence_by_id = index_occurrences(occurrences)?;
        let attributions = canonical_attributions(attributions)?;
        checkpoint(interrupted)?;
        let digest = tracing::trace_span!("code_index.test_attribution.digest").entered();
        let evidence_digest = canonical_sha256(&TestAttributionEvidenceDigestInput {
            domain: TEST_ATTRIBUTION_EVIDENCE_SEPARATOR,
            generation_id: &generation.generation_id,
            snapshot_digest: &generation.snapshot_digest,
            content_identity: &snapshot.snapshot.content_identity,
            source_revision: &snapshot.snapshot.source_revision,
            attribution_revision: &attribution_revision,
            coverage: &coverage,
            attributions: CheckedSlice {
                items: &attributions.iter().collect::<Vec<_>>(),
                interrupted,
            },
            occurrences: CheckedSlice {
                items: &canonical_occurrences(occurrences),
                interrupted,
            },
        })
        .map_err(|error| {
            if interrupted() {
                GenerationTestJoinErrorV1::Interrupted
            } else {
                GenerationTestJoinErrorV1::Contract(error.to_string())
            }
        })?;
        drop(digest);
        let watermark = TestAttributionWatermarkV1 {
            generation_id: generation.generation_id.clone(),
            snapshot_digest: generation.snapshot_digest.clone(),
            content_identity: snapshot.snapshot.content_identity.clone(),
            source_revision: snapshot.snapshot.source_revision.clone(),
            attribution_revision,
            evidence_digest,
            coverage,
        };
        validate_watermark(generation, snapshot, &watermark)?;
        Self::join_canonical(
            generation,
            snapshot,
            attributions,
            occurrence_by_id,
            &watermark,
            interrupted,
        )
    }

    fn join_canonical(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        attributions: Vec<GenerationTestAttributionV1>,
        occurrence_by_id: HashMap<&SymbolOccurrenceId, &TestAttributionOccurrenceV1>,
        watermark: &TestAttributionWatermarkV1,
        interrupted: &dyn Fn() -> bool,
    ) -> Result<Self, GenerationTestJoinErrorV1> {
        // These indices are used only for exact lookups. Canonical record and
        // evidence ordering is established separately, so repeated covered
        // occurrences need not compare their long identities down a tree.
        let content_by_file: HashMap<&FileOccurrenceId, &ContentDigest> = snapshot
            .snapshot
            .files
            .iter()
            .map(|file| (&file.file_occurrence_id, &file.content_digest))
            .collect();

        // Content identity belongs to each occurrence, not to every test
        // that reaches it. Retain only drift; the common current case has no
        // repeated file lookup or digest comparison per covered pair.
        let content_drift = occurrence_by_id
            .iter()
            .filter_map(|(id, occurrence)| {
                match content_by_file.get(&occurrence.file_occurrence_id) {
                    None => Some((
                        *id,
                        (
                            GenerationTestJoinDispositionV1::MissingOccurrence {
                                occurrence_id: (*id).clone(),
                            },
                            GenerationTestJoinPartialReasonV1::MissingOccurrence {
                                occurrence_id: (*id).clone(),
                            },
                        ),
                    )),
                    Some(expected) if **expected != occurrence.content_digest => Some((
                        *id,
                        (
                            GenerationTestJoinDispositionV1::StaleContent {
                                occurrence_id: (*id).clone(),
                                expected: (*expected).clone(),
                                observed: occurrence.content_digest.clone(),
                            },
                            GenerationTestJoinPartialReasonV1::StaleContent {
                                occurrence_id: (*id).clone(),
                            },
                        ),
                    )),
                    Some(_) => None,
                }
            })
            .collect::<HashMap<_, _>>();

        let mut partial_reasons = match &watermark.coverage {
            TestAttributionJoinInputCoverageV1::Complete => Vec::new(),
            TestAttributionJoinInputCoverageV1::Partial { reason } => {
                vec![GenerationTestJoinPartialReasonV1::InputPartial {
                    reason: reason.clone(),
                }]
            }
        };
        let shared_occurrences: HashMap<&SymbolOccurrenceId, Arc<TestAttributionOccurrenceV1>> =
            occurrence_by_id
                .iter()
                .map(|(id, occurrence)| (*id, Arc::new((*occurrence).clone())))
                .collect();
        checkpoint(interrupted)?;
        let mut records = Vec::with_capacity(attributions.len());
        for attribution in attributions {
            checkpoint(interrupted)?;
            let test_occurrence = occurrence_by_id.get(&attribution.test_occurrence).copied();
            let mut covered_occurrences = Vec::with_capacity(attribution.covered_occurrences.len());
            for occurrence in &attribution.covered_occurrences {
                checkpoint(interrupted)?;
                if let Some(covered) = shared_occurrences.get(occurrence) {
                    covered_occurrences.push(Arc::clone(covered));
                }
            }
            // Resolution already proved presence for every requested identity
            // when no entry was filtered. With no content drift, checking the
            // same covered identities again cannot change the disposition.
            let occurrences_to_check = (test_occurrence.is_none()
                || covered_occurrences.len() != attribution.covered_occurrences.len()
                || !content_drift.is_empty())
            .then_some(&occurrence_by_id);
            let disposition = disposition_for(
                generation,
                snapshot,
                watermark,
                occurrences_to_check,
                &content_drift,
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
        checkpoint(interrupted)?;
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

fn canonical_attributions<T: Borrow<GenerationTestAttributionV1>>(
    mut canonical: Vec<T>,
) -> Result<Vec<T>, GenerationTestJoinErrorV1> {
    for attribution in &canonical {
        validate_attribution(attribution.borrow())?;
    }
    canonical.sort_by(|left, right| {
        let left = left.borrow();
        let right = right.borrow();
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
) -> Vec<&TestAttributionOccurrenceV1> {
    let mut canonical = occurrences.iter().collect::<Vec<_>>();
    canonical.sort_by(|left, right| left.occurrence_id.cmp(&right.occurrence_id));
    canonical
}

fn disposition_for(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
    watermark: &TestAttributionWatermarkV1,
    occurrences_to_check: Option<&HashMap<&SymbolOccurrenceId, &TestAttributionOccurrenceV1>>,
    content_drift: &HashMap<
        &SymbolOccurrenceId,
        (
            GenerationTestJoinDispositionV1,
            GenerationTestJoinPartialReasonV1,
        ),
    >,
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

    if let Some(occurrences) = occurrences_to_check {
        for occurrence_id in std::iter::once(&attribution.test_occurrence)
            .chain(attribution.covered_occurrences.iter())
        {
            if !occurrences.contains_key(occurrence_id) {
                partial_reasons.push(GenerationTestJoinPartialReasonV1::MissingOccurrence {
                    occurrence_id: occurrence_id.clone(),
                });
                return GenerationTestJoinDispositionV1::MissingOccurrence {
                    occurrence_id: occurrence_id.clone(),
                };
            }
            if let Some((disposition, reason)) = content_drift.get(occurrence_id) {
                partial_reasons.push(reason.clone());
                return disposition.clone();
            }
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
) -> Result<HashMap<&SymbolOccurrenceId, &TestAttributionOccurrenceV1>, GenerationTestJoinErrorV1> {
    let mut by_id = HashMap::with_capacity(occurrences.len());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn attribution_digest_slice_keeps_canonical_bytes_and_interrupts_between_records() {
        struct Record<'a> {
            value: u64,
            visited: &'a Cell<usize>,
            stop: &'a Cell<bool>,
        }
        impl Serialize for Record<'_> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.visited.set(self.visited.get() + 1);
                let result = self.value.serialize(serializer);
                self.stop.set(true);
                result
            }
        }
        let values = [3_u64, 7, 11];
        let checked = CheckedSlice {
            items: &values,
            interrupted: &|| false,
        };
        assert_eq!(
            canonical_sha256(&checked).unwrap(),
            canonical_sha256(&values).unwrap()
        );
        let visited = Cell::new(0);
        let stop = Cell::new(false);
        let records = values.map(|value| Record {
            value,
            visited: &visited,
            stop: &stop,
        });
        let interrupted = || stop.get();
        assert!(
            canonical_sha256(&CheckedSlice {
                items: &records,
                interrupted: &interrupted
            })
            .is_err()
        );
        assert_eq!(
            visited.get(),
            1,
            "cancellation stops inside serialization, before the next record"
        );
    }
}
