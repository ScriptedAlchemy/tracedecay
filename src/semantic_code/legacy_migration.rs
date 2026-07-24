//! Rebuild-only admission of legacy semantic-vector artifacts.
//!
//! Legacy vector bytes never cross this boundary. The migration inventory
//! exposes identity only; retained canonical code is the sole rebuild input.
//! Publication is returned as one owner-transaction command, so cancellation,
//! failure, or a crash before that transaction leaves the prior pointer intact.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1, ManifestDigest, VectorGenerationIdV1,
    canonical_sha256,
};

const LEGACY_MIGRATION_RECEIPT_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-migration-receipt.v1";
const CANONICAL_CHUNK_SET_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-canonical-chunk-set.v1";

/// Read-only identity inventory. Deliberately contains no legacy vector bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyVectorInventoryV1 {
    pub expected_active_generation: Option<VectorGenerationIdV1>,
    pub entries: Vec<LegacyVectorInventoryEntryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "readability", rename_all = "snake_case")]
pub(crate) enum LegacyVectorInventoryEntryV1 {
    Readable {
        legacy_generation: VectorGenerationIdV1,
        source_generation: CodeGenerationId,
    },
    Unreadable {
        legacy_generation: VectorGenerationIdV1,
        reason_digest: ManifestDigest,
    },
}

impl LegacyVectorInventoryEntryV1 {
    fn legacy_generation(&self) -> &VectorGenerationIdV1 {
        match self {
            Self::Readable {
                legacy_generation, ..
            }
            | Self::Unreadable {
                legacy_generation, ..
            } => legacy_generation,
        }
    }
}

pub(crate) trait LegacyVectorInventoryPortV1 {
    fn read_only_inventory(&self) -> Result<LegacyVectorInventoryV1, LegacyVectorMigrationErrorV1>;
}

/// Validated retained-code handoff accepted by the rebuild authority.
///
/// Construction rejects foreign, invalid, or duplicate chunks. No legacy
/// embedding values are representable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalEligibleChunkSetV1 {
    source_generation: CodeGenerationId,
    chunks: Vec<CodeSearchChunkV1>,
    digest: ManifestDigest,
}

impl CanonicalEligibleChunkSetV1 {
    pub(crate) fn try_from_chunks(
        source_generation: CodeGenerationId,
        mut chunks: Vec<CodeSearchChunkV1>,
    ) -> Result<Self, LegacyVectorMigrationErrorV1> {
        let mut seen = BTreeSet::new();
        for chunk in &chunks {
            chunk
                .validate()
                .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
            if chunk.anchor.generation_id != source_generation {
                return Err(LegacyVectorMigrationErrorV1::ForeignCanonicalChunk(
                    chunk.id.clone(),
                ));
            }
            if !seen.insert(chunk.id.clone()) {
                return Err(LegacyVectorMigrationErrorV1::DuplicateCanonicalChunk(
                    chunk.id.clone(),
                ));
            }
        }
        chunks.sort_by(|left, right| left.id.cmp(&right.id));
        let digest = canonical_sha256(&(
            CANONICAL_CHUNK_SET_DOMAIN_V1,
            &source_generation,
            chunks
                .iter()
                .map(|chunk| (&chunk.id, &chunk.content_digest))
                .collect::<Vec<_>>(),
        ))
        .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
        Ok(Self {
            source_generation,
            chunks,
            digest,
        })
    }

    pub(crate) fn source_generation(&self) -> &CodeGenerationId {
        &self.source_generation
    }

    pub(crate) fn chunks(&self) -> &[CodeSearchChunkV1] {
        &self.chunks
    }

    pub(crate) fn digest(&self) -> &ManifestDigest {
        &self.digest
    }
}

/// Result of rebuilding one generation exclusively from retained canonical
/// eligible chunks. It is staged, never active.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct StagedCanonicalVectorRebuildV1 {
    pub source_generation: CodeGenerationId,
    pub rebuilt_generation: VectorGenerationIdV1,
    pub canonical_chunk_set_digest: ManifestDigest,
}

pub(crate) trait LegacyVectorCanonicalRebuildPortV1 {
    /// Returns `None` when no retained eligible canonical code remains.
    fn retained_eligible_chunks(
        &mut self,
        source_generation: &CodeGenerationId,
    ) -> Result<Option<CanonicalEligibleChunkSetV1>, LegacyVectorMigrationErrorV1>;

    /// Stages a fresh generation. The implementation receives canonical code,
    /// not legacy vectors, and must not publish or change an active pointer.
    fn rebuild_from_retained_eligible_code(
        &mut self,
        chunks: &CanonicalEligibleChunkSetV1,
    ) -> Result<StagedCanonicalVectorRebuildV1, LegacyVectorMigrationErrorV1>;
}

pub(crate) trait LegacyVectorMigrationCancellationV1 {
    fn is_cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NeverCancelLegacyVectorMigrationV1;

impl LegacyVectorMigrationCancellationV1 for NeverCancelLegacyVectorMigrationV1 {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LegacyVectorMigrationOutcomeKindV1 {
    RebuildFromRetainedEligibleCode,
    DropWithReceipt,
    QuarantineUnreadable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyVectorMigrationItemReceiptV1 {
    pub legacy_generation: VectorGenerationIdV1,
    pub outcome: LegacyVectorMigrationOutcomeKindV1,
    pub source_generation: Option<CodeGenerationId>,
    pub rebuilt_generation: Option<VectorGenerationIdV1>,
    pub canonical_chunk_set_digest: Option<ManifestDigest>,
    pub quarantine_reason_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyVectorMigrationCountsV1 {
    pub inventoried: u64,
    pub rebuilt: u64,
    pub dropped: u64,
    pub quarantined: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyVectorMigrationReceiptV1 {
    pub counts: LegacyVectorMigrationCountsV1,
    pub items: Vec<LegacyVectorMigrationItemReceiptV1>,
    pub receipt_digest: ManifestDigest,
}

impl LegacyVectorMigrationReceiptV1 {
    pub(crate) fn validate(&self) -> Result<(), LegacyVectorMigrationErrorV1> {
        let valid_items = self.items.iter().all(|item| match item.outcome {
            LegacyVectorMigrationOutcomeKindV1::RebuildFromRetainedEligibleCode => {
                item.source_generation.is_some()
                    && item.rebuilt_generation.is_some()
                    && item.canonical_chunk_set_digest.is_some()
                    && item.quarantine_reason_digest.is_none()
            }
            LegacyVectorMigrationOutcomeKindV1::DropWithReceipt => {
                item.source_generation.is_some()
                    && item.rebuilt_generation.is_none()
                    && item.quarantine_reason_digest.is_none()
            }
            LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable => {
                item.source_generation.is_none()
                    && item.rebuilt_generation.is_none()
                    && item.canonical_chunk_set_digest.is_none()
                    && item.quarantine_reason_digest.is_some()
            }
        });
        let unique_legacy = self
            .items
            .iter()
            .map(|item| &item.legacy_generation)
            .collect::<BTreeSet<_>>()
            .len()
            == self.items.len();
        let expected_counts = LegacyVectorMigrationCountsV1 {
            inventoried: self.items.len() as u64,
            rebuilt: count_outcome(
                &self.items,
                LegacyVectorMigrationOutcomeKindV1::RebuildFromRetainedEligibleCode,
            ),
            dropped: count_outcome(
                &self.items,
                LegacyVectorMigrationOutcomeKindV1::DropWithReceipt,
            ),
            quarantined: count_outcome(
                &self.items,
                LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable,
            ),
        };
        let expected_digest = canonical_sha256(&(
            LEGACY_MIGRATION_RECEIPT_DOMAIN_V1,
            &self.counts,
            &self.items,
        ))
        .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
        if !valid_items
            || !unique_legacy
            || self.counts != expected_counts
            || self.receipt_digest != expected_digest
        {
            return Err(LegacyVectorMigrationErrorV1::InvalidReceipt);
        }
        Ok(())
    }
}

/// Typed handoff for the existing owner transaction. The owner persists the
/// receipt and swaps (or clears) the active pointer together. Until then, the
/// expected prior pointer remains authoritative.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegacyVectorMigrationOwnerTransactionV1 {
    pub expected_prior_active_generation: Option<VectorGenerationIdV1>,
    pub next_active_generation: Option<VectorGenerationIdV1>,
    pub receipt: LegacyVectorMigrationReceiptV1,
}

impl LegacyVectorMigrationOwnerTransactionV1 {
    pub(crate) fn validate(&self) -> Result<(), LegacyVectorMigrationErrorV1> {
        self.receipt.validate()?;
        let active_item = self
            .expected_prior_active_generation
            .as_ref()
            .and_then(|active| {
                self.receipt
                    .items
                    .iter()
                    .find(|item| &item.legacy_generation == active)
            });
        if self.expected_prior_active_generation.is_some() && active_item.is_none() {
            return Err(LegacyVectorMigrationErrorV1::InvalidReceipt);
        }
        let expected_next = active_item.and_then(|item| item.rebuilt_generation.clone());
        if self.next_active_generation != expected_next {
            return Err(LegacyVectorMigrationErrorV1::InvalidReceipt);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum LegacyVectorMigrationErrorV1 {
    #[error("legacy vector inventory failed: {0}")]
    Inventory(String),
    #[error("legacy vector inventory contains duplicate generation identity")]
    DuplicateLegacyGeneration,
    #[error("legacy vector inventory active pointer is absent from the inventory")]
    DanglingActivePointer,
    #[error("canonical retained code is invalid: {0}")]
    CanonicalCode(String),
    #[error("canonical chunk belongs to a foreign source generation: {0:?}")]
    ForeignCanonicalChunk(CodeSearchChunkId),
    #[error("canonical chunk is duplicated: {0:?}")]
    DuplicateCanonicalChunk(CodeSearchChunkId),
    #[error("canonical rebuild identity does not match its input")]
    RebuildIdentityMismatch,
    #[error("canonical rebuild produced a duplicate generation identity")]
    DuplicateRebuiltGeneration,
    #[error("legacy vector migration was cancelled")]
    Cancelled,
    #[error("legacy vector migration receipt is invalid")]
    InvalidReceipt,
}

pub(crate) fn prepare_legacy_vector_migration<Inventory, Rebuilder, Cancellation>(
    inventory: &Inventory,
    rebuilder: &mut Rebuilder,
    cancellation: &Cancellation,
) -> Result<LegacyVectorMigrationOwnerTransactionV1, LegacyVectorMigrationErrorV1>
where
    Inventory: LegacyVectorInventoryPortV1,
    Rebuilder: LegacyVectorCanonicalRebuildPortV1,
    Cancellation: LegacyVectorMigrationCancellationV1,
{
    let mut inventory = inventory.read_only_inventory()?;
    inventory
        .entries
        .sort_by(|left, right| left.legacy_generation().cmp(right.legacy_generation()));
    validate_inventory(&inventory)?;

    let mut rebuilt_generations = BTreeSet::new();
    let mut items = Vec::with_capacity(inventory.entries.len());
    for entry in inventory.entries {
        if cancellation.is_cancelled() {
            return Err(LegacyVectorMigrationErrorV1::Cancelled);
        }
        let item = match entry {
            LegacyVectorInventoryEntryV1::Unreadable {
                legacy_generation,
                reason_digest,
            } => LegacyVectorMigrationItemReceiptV1 {
                legacy_generation,
                outcome: LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable,
                source_generation: None,
                rebuilt_generation: None,
                canonical_chunk_set_digest: None,
                quarantine_reason_digest: Some(reason_digest),
            },
            LegacyVectorInventoryEntryV1::Readable {
                legacy_generation,
                source_generation,
            } => match rebuilder.retained_eligible_chunks(&source_generation)? {
                None => LegacyVectorMigrationItemReceiptV1 {
                    legacy_generation,
                    outcome: LegacyVectorMigrationOutcomeKindV1::DropWithReceipt,
                    source_generation: Some(source_generation),
                    rebuilt_generation: None,
                    canonical_chunk_set_digest: None,
                    quarantine_reason_digest: None,
                },
                Some(chunks) if chunks.chunks().is_empty() => LegacyVectorMigrationItemReceiptV1 {
                    legacy_generation,
                    outcome: LegacyVectorMigrationOutcomeKindV1::DropWithReceipt,
                    source_generation: Some(source_generation),
                    rebuilt_generation: None,
                    canonical_chunk_set_digest: Some(chunks.digest().clone()),
                    quarantine_reason_digest: None,
                },
                Some(chunks) => {
                    if chunks.source_generation() != &source_generation {
                        return Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch);
                    }
                    let rebuilt = rebuilder.rebuild_from_retained_eligible_code(&chunks)?;
                    if rebuilt.source_generation != source_generation
                        || rebuilt.canonical_chunk_set_digest != *chunks.digest()
                    {
                        return Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch);
                    }
                    if !rebuilt_generations.insert(rebuilt.rebuilt_generation.clone()) {
                        return Err(LegacyVectorMigrationErrorV1::DuplicateRebuiltGeneration);
                    }
                    LegacyVectorMigrationItemReceiptV1 {
                        legacy_generation,
                        outcome:
                            LegacyVectorMigrationOutcomeKindV1::RebuildFromRetainedEligibleCode,
                        source_generation: Some(source_generation),
                        rebuilt_generation: Some(rebuilt.rebuilt_generation),
                        canonical_chunk_set_digest: Some(rebuilt.canonical_chunk_set_digest),
                        quarantine_reason_digest: None,
                    }
                }
            },
        };
        items.push(item);
    }

    let counts = LegacyVectorMigrationCountsV1 {
        inventoried: items.len() as u64,
        rebuilt: count_outcome(
            &items,
            LegacyVectorMigrationOutcomeKindV1::RebuildFromRetainedEligibleCode,
        ),
        dropped: count_outcome(&items, LegacyVectorMigrationOutcomeKindV1::DropWithReceipt),
        quarantined: count_outcome(
            &items,
            LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable,
        ),
    };
    let receipt_digest =
        canonical_sha256(&(LEGACY_MIGRATION_RECEIPT_DOMAIN_V1, &counts, &items))
            .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
    let receipt = LegacyVectorMigrationReceiptV1 {
        counts,
        items,
        receipt_digest,
    };
    let next_active_generation = inventory
        .expected_active_generation
        .as_ref()
        .and_then(|active| {
            receipt
                .items
                .iter()
                .find(|item| &item.legacy_generation == active)
        })
        .and_then(|item| item.rebuilt_generation.clone());
    let transaction = LegacyVectorMigrationOwnerTransactionV1 {
        expected_prior_active_generation: inventory.expected_active_generation,
        next_active_generation,
        receipt,
    };
    transaction.validate()?;
    Ok(transaction)
}

fn validate_inventory(
    inventory: &LegacyVectorInventoryV1,
) -> Result<(), LegacyVectorMigrationErrorV1> {
    let generations = inventory
        .entries
        .iter()
        .map(LegacyVectorInventoryEntryV1::legacy_generation)
        .collect::<BTreeSet<_>>();
    if generations.len() != inventory.entries.len() {
        return Err(LegacyVectorMigrationErrorV1::DuplicateLegacyGeneration);
    }
    if inventory
        .expected_active_generation
        .as_ref()
        .is_some_and(|active| !generations.contains(active))
    {
        return Err(LegacyVectorMigrationErrorV1::DanglingActivePointer);
    }
    Ok(())
}

fn count_outcome(
    items: &[LegacyVectorMigrationItemReceiptV1],
    outcome: LegacyVectorMigrationOutcomeKindV1,
) -> u64 {
    items.iter().filter(|item| item.outcome == outcome).count() as u64
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::{
        BoundedSanitizedText, ChunkerRevision, CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1,
        ContentDigest, FileOccurrenceId, LanguageDescriptorRevision, PolicyRevisionId,
        SanitizerRevision, SensitivityDecision, SensitivityLevelV1, SourceSpan,
    };

    use super::*;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DIGEST_C: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[derive(Clone)]
    struct Inventory(LegacyVectorInventoryV1);

    impl LegacyVectorInventoryPortV1 for Inventory {
        fn read_only_inventory(
            &self,
        ) -> Result<LegacyVectorInventoryV1, LegacyVectorMigrationErrorV1> {
            Ok(self.0.clone())
        }
    }

    struct Rebuilder {
        chunks: BTreeMap<CodeGenerationId, Option<CanonicalEligibleChunkSetV1>>,
        rebuilt: BTreeMap<CodeGenerationId, VectorGenerationIdV1>,
        observed_chunk_ids: Vec<CodeSearchChunkId>,
    }

    impl LegacyVectorCanonicalRebuildPortV1 for Rebuilder {
        fn retained_eligible_chunks(
            &mut self,
            source_generation: &CodeGenerationId,
        ) -> Result<Option<CanonicalEligibleChunkSetV1>, LegacyVectorMigrationErrorV1> {
            Ok(self.chunks.get(source_generation).cloned().flatten())
        }

        fn rebuild_from_retained_eligible_code(
            &mut self,
            chunks: &CanonicalEligibleChunkSetV1,
        ) -> Result<StagedCanonicalVectorRebuildV1, LegacyVectorMigrationErrorV1> {
            self.observed_chunk_ids
                .extend(chunks.chunks().iter().map(|chunk| chunk.id.clone()));
            Ok(StagedCanonicalVectorRebuildV1 {
                source_generation: chunks.source_generation().clone(),
                rebuilt_generation: self
                    .rebuilt
                    .get(chunks.source_generation())
                    .expect("fixture rebuild identity")
                    .clone(),
                canonical_chunk_set_digest: chunks.digest().clone(),
            })
        }
    }

    struct CancelAfterFirst(std::cell::Cell<u8>);

    impl LegacyVectorMigrationCancellationV1 for CancelAfterFirst {
        fn is_cancelled(&self) -> bool {
            let seen = self.0.get();
            self.0.set(seen + 1);
            seen > 0
        }
    }

    fn manifest(value: &str) -> ManifestDigest {
        ManifestDigest::new(value).expect("digest")
    }

    fn generation(value: &str) -> CodeGenerationId {
        CodeGenerationId::new(value).expect("generation")
    }

    fn vector(value: &str) -> VectorGenerationIdV1 {
        VectorGenerationIdV1::new(manifest(value))
    }

    fn chunk(id: &str, source: &CodeGenerationId) -> CodeSearchChunkV1 {
        CodeSearchChunkV1 {
            id: CodeSearchChunkId::new(id).expect("chunk id"),
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: source.clone(),
                file_occurrence_id: FileOccurrenceId::new("file.rs").expect("file id"),
                symbol_occurrence_id: None,
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 4,
                },
                grain: CodeSearchChunkGrainV1::FileWindow,
                ordinal: 0,
            },
            content_digest: ContentDigest::new(DIGEST_A).expect("content digest"),
            language_descriptor_revision: LanguageDescriptorRevision::new("rust.v1")
                .expect("descriptor"),
            chunker_revision: ChunkerRevision::new("chunker.v1").expect("chunker"),
            sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("sanitizer"),
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.v1").expect("policy"),
            },
            exact_terms: vec![],
            subtokens: vec![],
            sanitized_text: BoundedSanitizedText::new("code").expect("text"),
        }
    }

    #[test]
    fn every_item_has_one_deterministic_rebuild_drop_or_quarantine_outcome() {
        let source_a = generation("generation.a");
        let source_b = generation("generation.b");
        let legacy_a = vector(DIGEST_A);
        let legacy_b = vector(DIGEST_B);
        let legacy_c = vector(DIGEST_C);
        let inventory = Inventory(LegacyVectorInventoryV1 {
            expected_active_generation: Some(legacy_a.clone()),
            entries: vec![
                LegacyVectorInventoryEntryV1::Unreadable {
                    legacy_generation: legacy_c,
                    reason_digest: manifest(DIGEST_C),
                },
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy_b,
                    source_generation: source_b.clone(),
                },
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy_a,
                    source_generation: source_a.clone(),
                },
            ],
        });
        let canonical = CanonicalEligibleChunkSetV1::try_from_chunks(
            source_a.clone(),
            vec![chunk("chunk.a", &source_a)],
        )
        .expect("canonical chunks");
        let mut rebuilder = Rebuilder {
            chunks: BTreeMap::from([(source_a.clone(), Some(canonical)), (source_b, None)]),
            rebuilt: BTreeMap::from([(source_a, vector(DIGEST_C))]),
            observed_chunk_ids: vec![],
        };

        let first = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("migration");
        let mut second_rebuilder = Rebuilder {
            chunks: rebuilder.chunks.clone(),
            rebuilt: rebuilder.rebuilt.clone(),
            observed_chunk_ids: vec![],
        };
        let second = prepare_legacy_vector_migration(
            &inventory,
            &mut second_rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("repeat");

        assert_eq!(first, second);
        assert_eq!(first.receipt.counts.inventoried, 3);
        assert_eq!(first.receipt.counts.rebuilt, 1);
        assert_eq!(first.receipt.counts.dropped, 1);
        assert_eq!(first.receipt.counts.quarantined, 1);
        assert_eq!(rebuilder.observed_chunk_ids.len(), 1);
        assert_eq!(first.next_active_generation, Some(vector(DIGEST_C)));
    }

    #[test]
    fn cancellation_returns_no_owner_transaction_or_pointer_swap() {
        let source_a = generation("generation.a");
        let source_b = generation("generation.b");
        let inventory = Inventory(LegacyVectorInventoryV1 {
            expected_active_generation: Some(vector(DIGEST_A)),
            entries: vec![
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: vector(DIGEST_A),
                    source_generation: source_a.clone(),
                },
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: vector(DIGEST_B),
                    source_generation: source_b.clone(),
                },
            ],
        });
        let mut rebuilder = Rebuilder {
            chunks: BTreeMap::from([
                (
                    source_a.clone(),
                    Some(
                        CanonicalEligibleChunkSetV1::try_from_chunks(
                            source_a.clone(),
                            vec![chunk("chunk.a", &source_a)],
                        )
                        .unwrap(),
                    ),
                ),
                (
                    source_b.clone(),
                    Some(
                        CanonicalEligibleChunkSetV1::try_from_chunks(
                            source_b.clone(),
                            vec![chunk("chunk.b", &source_b)],
                        )
                        .unwrap(),
                    ),
                ),
            ]),
            rebuilt: BTreeMap::from([(source_a, vector(DIGEST_B)), (source_b, vector(DIGEST_C))]),
            observed_chunk_ids: vec![],
        };

        let result = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &CancelAfterFirst(std::cell::Cell::new(0)),
        );

        assert_eq!(result, Err(LegacyVectorMigrationErrorV1::Cancelled));
    }

    #[test]
    fn duplicate_inventory_is_rejected_before_any_rebuild() {
        let legacy = vector(DIGEST_A);
        let source = generation("generation.a");
        let inventory = Inventory(LegacyVectorInventoryV1 {
            expected_active_generation: Some(legacy.clone()),
            entries: vec![
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy.clone(),
                    source_generation: source.clone(),
                },
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy,
                    source_generation: source,
                },
            ],
        });
        let mut rebuilder = Rebuilder {
            chunks: BTreeMap::new(),
            rebuilt: BTreeMap::new(),
            observed_chunk_ids: vec![],
        };

        let result = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        );

        assert_eq!(
            result,
            Err(LegacyVectorMigrationErrorV1::DuplicateLegacyGeneration)
        );
        assert!(rebuilder.observed_chunk_ids.is_empty());
    }
}
