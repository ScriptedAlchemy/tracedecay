//! Rebuild-only admission of legacy semantic-vector artifacts.
//!
//! Legacy vector bytes never cross this boundary. The migration inventory
//! exposes identity only; retained canonical code is the sole rebuild input.
//! Publication is returned as one owner-transaction command, so cancellation,
//! failure, or a crash before that transaction leaves the prior pointer intact.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeSearchChunkId, CodeSearchChunkV1, ContentDigest, ManifestDigest,
    VectorGenerationIdV1, canonical_sha256,
};

const LEGACY_MIGRATION_RECEIPT_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-migration-receipt.v1";
const LEGACY_MIGRATION_INVENTORY_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-inventory.v1";
const CANONICAL_CHUNK_SET_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-canonical-chunk-set.v1";

/// Read-only identity inventory. Deliberately contains no legacy vector bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyVectorInventoryV1 {
    pub expected_active_generation: Option<VectorGenerationIdV1>,
    pub entries: Vec<LegacyVectorInventoryEntryV1>,
}

impl LegacyVectorInventoryV1 {
    pub fn canonical_digest(&self) -> Result<ManifestDigest, LegacyVectorMigrationErrorV1> {
        let mut canonical = self.clone();
        canonical
            .entries
            .sort_by(|left, right| left.legacy_generation().cmp(right.legacy_generation()));
        validate_inventory(&canonical)?;
        canonical_sha256(&(
            LEGACY_MIGRATION_INVENTORY_DOMAIN_V1,
            &canonical.expected_active_generation,
            &canonical.entries,
        ))
        .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))
    }

    /// Code generations still named by readable vector inventory entries.
    ///
    /// This is the shared liveness authority for legacy migration reads,
    /// code-generation retention, and Doctor's exact collectable-byte reading.
    pub fn retained_readable_sources(&self) -> BTreeSet<CodeGenerationId> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                LegacyVectorInventoryEntryV1::Readable {
                    source_generation, ..
                } => Some(source_generation.clone()),
                LegacyVectorInventoryEntryV1::Unreadable { .. } => None,
            })
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "readability", rename_all = "snake_case")]
pub enum LegacyVectorInventoryEntryV1 {
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

pub trait LegacyVectorInventoryPortV1 {
    fn read_only_inventory(&self) -> Result<LegacyVectorInventoryV1, LegacyVectorMigrationErrorV1>;
}

/// Validated retained-code handoff accepted by the rebuild authority.
///
/// Construction rejects foreign, invalid, or duplicate chunks. No legacy
/// embedding values are representable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalEligibleChunkSetV1 {
    source_generation: CodeGenerationId,
    chunks: Vec<CodeSearchChunkV1>,
    digest: ManifestDigest,
}

impl CanonicalEligibleChunkSetV1 {
    pub fn try_from_chunks(
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
        let identities = chunks
            .iter()
            .map(|chunk| (chunk.id.clone(), chunk.content_digest.clone()))
            .collect::<Vec<_>>();
        let digest = canonical_chunk_set_digest(&source_generation, &identities)?;
        Ok(Self {
            source_generation,
            chunks,
            digest,
        })
    }

    pub fn source_generation(&self) -> &CodeGenerationId {
        &self.source_generation
    }

    pub fn chunks(&self) -> &[CodeSearchChunkV1] {
        &self.chunks
    }

    pub fn digest(&self) -> &ManifestDigest {
        &self.digest
    }
}

pub fn canonical_chunk_set_digest(
    source_generation: &CodeGenerationId,
    chunks: &[(CodeSearchChunkId, ContentDigest)],
) -> Result<ManifestDigest, LegacyVectorMigrationErrorV1> {
    source_generation
        .validate()
        .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
    if chunks
        .windows(2)
        .any(|pair| pair[0].0.cmp(&pair[1].0).is_ge())
    {
        return Err(LegacyVectorMigrationErrorV1::CanonicalCode(
            "canonical chunk identities are duplicated or unordered".to_owned(),
        ));
    }
    for (chunk_id, digest) in chunks {
        chunk_id
            .validate()
            .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
        digest
            .validate()
            .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
    }
    canonical_sha256(&(
        CANONICAL_CHUNK_SET_DOMAIN_V1,
        source_generation,
        chunks
            .iter()
            .map(|(chunk_id, digest)| (chunk_id, digest))
            .collect::<Vec<_>>(),
    ))
    .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))
}

/// Result of rebuilding one generation exclusively from retained canonical
/// eligible chunks. It is staged, never active.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StagedCanonicalVectorRebuildV1 {
    pub source_generation: CodeGenerationId,
    pub rebuilt_generation: VectorGenerationIdV1,
    pub canonical_chunk_set_digest: ManifestDigest,
}

pub trait LegacyVectorCanonicalRebuildPortV1 {
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

/// Production rebuild adapter over retained canonical code.
///
/// The callback may stage only into caller-owned scratch storage. This adapter
/// never accepts legacy vector bytes and has no live publication authority.
pub struct ProductionLegacyVectorCanonicalRebuilderV1<Stage> {
    retained: BTreeMap<CodeGenerationId, CanonicalEligibleChunkSetV1>,
    stage: Stage,
    staged_rebuilds: Vec<StagedCanonicalVectorRebuildV1>,
}

impl<Stage> ProductionLegacyVectorCanonicalRebuilderV1<Stage>
where
    Stage: FnMut(
        &CanonicalEligibleChunkSetV1,
    ) -> Result<StagedCanonicalVectorRebuildV1, LegacyVectorMigrationErrorV1>,
{
    pub fn try_new(
        retained: impl IntoIterator<Item = CanonicalEligibleChunkSetV1>,
        stage: Stage,
    ) -> Result<Self, LegacyVectorMigrationErrorV1> {
        let mut retained_by_generation = BTreeMap::new();
        for chunks in retained {
            if retained_by_generation
                .insert(chunks.source_generation().clone(), chunks)
                .is_some()
            {
                return Err(LegacyVectorMigrationErrorV1::CanonicalCode(
                    "duplicate retained source generation".to_owned(),
                ));
            }
        }
        Ok(Self {
            retained: retained_by_generation,
            stage,
            staged_rebuilds: Vec::new(),
        })
    }

    pub fn staged_rebuilds(&self) -> &[StagedCanonicalVectorRebuildV1] {
        &self.staged_rebuilds
    }
}

impl<Stage> LegacyVectorCanonicalRebuildPortV1 for ProductionLegacyVectorCanonicalRebuilderV1<Stage>
where
    Stage: FnMut(
        &CanonicalEligibleChunkSetV1,
    ) -> Result<StagedCanonicalVectorRebuildV1, LegacyVectorMigrationErrorV1>,
{
    fn retained_eligible_chunks(
        &mut self,
        source_generation: &CodeGenerationId,
    ) -> Result<Option<CanonicalEligibleChunkSetV1>, LegacyVectorMigrationErrorV1> {
        Ok(self.retained.get(source_generation).cloned())
    }

    fn rebuild_from_retained_eligible_code(
        &mut self,
        chunks: &CanonicalEligibleChunkSetV1,
    ) -> Result<StagedCanonicalVectorRebuildV1, LegacyVectorMigrationErrorV1> {
        if self.retained.get(chunks.source_generation()) != Some(chunks) {
            return Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch);
        }
        if let Some(staged) = self
            .staged_rebuilds
            .iter()
            .find(|staged| staged.source_generation == *chunks.source_generation())
        {
            return Ok(staged.clone());
        }
        let staged = (self.stage)(chunks)?;
        if staged.source_generation != *chunks.source_generation()
            || staged.canonical_chunk_set_digest != *chunks.digest()
        {
            return Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch);
        }
        self.staged_rebuilds.push(staged.clone());
        Ok(staged)
    }
}

pub trait LegacyVectorMigrationCancellationV1 {
    fn is_cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancelLegacyVectorMigrationV1;

impl LegacyVectorMigrationCancellationV1 for NeverCancelLegacyVectorMigrationV1 {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyVectorMigrationOutcomeKindV1 {
    RebuildFromRetainedEligibleCode,
    DropWithReceipt,
    QuarantineUnreadable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyVectorMigrationItemReceiptV1 {
    pub legacy_generation: VectorGenerationIdV1,
    pub outcome: LegacyVectorMigrationOutcomeKindV1,
    pub source_generation: Option<CodeGenerationId>,
    pub rebuilt_generation: Option<VectorGenerationIdV1>,
    pub canonical_chunk_set_digest: Option<ManifestDigest>,
    pub quarantine_reason_digest: Option<ManifestDigest>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyVectorMigrationCountsV1 {
    pub inventoried: u64,
    pub rebuilt: u64,
    pub dropped: u64,
    pub quarantined: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyVectorMigrationReceiptV1 {
    pub inventory_digest: ManifestDigest,
    pub expected_prior_active_generation: Option<VectorGenerationIdV1>,
    pub next_active_generation: Option<VectorGenerationIdV1>,
    pub counts: LegacyVectorMigrationCountsV1,
    pub items: Vec<LegacyVectorMigrationItemReceiptV1>,
    pub receipt_digest: ManifestDigest,
}

impl LegacyVectorMigrationReceiptV1 {
    pub fn validate(&self) -> Result<(), LegacyVectorMigrationErrorV1> {
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
        let canonical_order = self.items.windows(2).all(|pair| {
            pair[0]
                .legacy_generation
                .cmp(&pair[1].legacy_generation)
                .is_lt()
        });
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
        let inventory_entries = self
            .items
            .iter()
            .map(|item| match item.outcome {
                LegacyVectorMigrationOutcomeKindV1::RebuildFromRetainedEligibleCode
                | LegacyVectorMigrationOutcomeKindV1::DropWithReceipt => item
                    .source_generation
                    .clone()
                    .map(|source_generation| LegacyVectorInventoryEntryV1::Readable {
                        legacy_generation: item.legacy_generation.clone(),
                        source_generation,
                    })
                    .ok_or(LegacyVectorMigrationErrorV1::InvalidReceipt),
                LegacyVectorMigrationOutcomeKindV1::QuarantineUnreadable => item
                    .quarantine_reason_digest
                    .clone()
                    .map(|reason_digest| LegacyVectorInventoryEntryV1::Unreadable {
                        legacy_generation: item.legacy_generation.clone(),
                        reason_digest,
                    })
                    .ok_or(LegacyVectorMigrationErrorV1::InvalidReceipt),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let expected_inventory_digest = LegacyVectorInventoryV1 {
            expected_active_generation: self.expected_prior_active_generation.clone(),
            entries: inventory_entries,
        }
        .canonical_digest()
        .map_err(|_| LegacyVectorMigrationErrorV1::InvalidReceipt)?;
        let expected_next = self
            .expected_prior_active_generation
            .as_ref()
            .and_then(|active| {
                self.items
                    .iter()
                    .find(|item| &item.legacy_generation == active)
            })
            .and_then(|item| item.rebuilt_generation.clone());
        let expected_digest = canonical_sha256(&(
            LEGACY_MIGRATION_RECEIPT_DOMAIN_V1,
            &self.inventory_digest,
            &self.expected_prior_active_generation,
            &self.next_active_generation,
            &self.counts,
            &self.items,
        ))
        .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
        if !valid_items
            || !unique_legacy
            || !canonical_order
            || self.counts != expected_counts
            || self.inventory_digest != expected_inventory_digest
            || self.next_active_generation != expected_next
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
pub struct LegacyVectorMigrationOwnerTransactionV1 {
    pub expected_prior_active_generation: Option<VectorGenerationIdV1>,
    pub next_active_generation: Option<VectorGenerationIdV1>,
    pub receipt: LegacyVectorMigrationReceiptV1,
}

impl LegacyVectorMigrationOwnerTransactionV1 {
    pub fn validate(&self) -> Result<(), LegacyVectorMigrationErrorV1> {
        self.receipt.validate()?;
        if self.expected_prior_active_generation != self.receipt.expected_prior_active_generation
            || self.next_active_generation != self.receipt.next_active_generation
        {
            return Err(LegacyVectorMigrationErrorV1::InvalidReceipt);
        }
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
pub enum LegacyVectorMigrationErrorV1 {
    #[error("legacy vector inventory failed: {0}")]
    Inventory(String),
    #[error("legacy vector inventory contains duplicate generation identity")]
    DuplicateLegacyGeneration,
    #[error("legacy vector inventory active pointer is absent from the inventory")]
    DanglingActivePointer,
    #[error("legacy vector inventory changed before replacement")]
    InventoryChanged,
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

pub fn prepare_legacy_vector_migration<Inventory, Rebuilder, Cancellation>(
    inventory: &Inventory,
    rebuilder: &mut Rebuilder,
    cancellation: &Cancellation,
) -> Result<LegacyVectorMigrationOwnerTransactionV1, LegacyVectorMigrationErrorV1>
where
    Inventory: LegacyVectorInventoryPortV1,
    Rebuilder: LegacyVectorCanonicalRebuildPortV1,
    Cancellation: LegacyVectorMigrationCancellationV1,
{
    if cancellation.is_cancelled() {
        return Err(LegacyVectorMigrationErrorV1::Cancelled);
    }
    let mut snapshot = inventory.read_only_inventory()?;
    snapshot
        .entries
        .sort_by(|left, right| left.legacy_generation().cmp(right.legacy_generation()));
    validate_inventory(&snapshot)?;
    let inventory_digest = snapshot.canonical_digest()?;

    let mut rebuilt_generations = BTreeSet::new();
    let mut rebuilds_by_source: BTreeMap<
        CodeGenerationId,
        (
            Option<ManifestDigest>,
            Option<StagedCanonicalVectorRebuildV1>,
        ),
    > = BTreeMap::new();
    let mut items = Vec::with_capacity(snapshot.entries.len());
    for entry in std::mem::take(&mut snapshot.entries) {
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
            } => {
                if !rebuilds_by_source.contains_key(&source_generation) {
                    let retained = rebuilder.retained_eligible_chunks(&source_generation)?;
                    if cancellation.is_cancelled() {
                        return Err(LegacyVectorMigrationErrorV1::Cancelled);
                    }
                    let disposition = match retained {
                        None => (None, None),
                        Some(chunks) if chunks.chunks().is_empty() => {
                            (Some(chunks.digest().clone()), None)
                        }
                        Some(chunks) => {
                            if chunks.source_generation() != &source_generation {
                                return Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch);
                            }
                            let rebuilt = rebuilder.rebuild_from_retained_eligible_code(&chunks)?;
                            if cancellation.is_cancelled() {
                                return Err(LegacyVectorMigrationErrorV1::Cancelled);
                            }
                            if rebuilt.source_generation != source_generation
                                || rebuilt.canonical_chunk_set_digest != *chunks.digest()
                            {
                                return Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch);
                            }
                            if !rebuilt_generations.insert(rebuilt.rebuilt_generation.clone()) {
                                return Err(
                                    LegacyVectorMigrationErrorV1::DuplicateRebuiltGeneration,
                                );
                            }
                            (Some(chunks.digest().clone()), Some(rebuilt))
                        }
                    };
                    rebuilds_by_source.insert(source_generation.clone(), disposition);
                }
                let (canonical_chunk_set_digest, rebuilt) = rebuilds_by_source
                    .get(&source_generation)
                    .cloned()
                    .ok_or(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch)?;
                match rebuilt {
                    None => LegacyVectorMigrationItemReceiptV1 {
                        legacy_generation,
                        outcome: LegacyVectorMigrationOutcomeKindV1::DropWithReceipt,
                        source_generation: Some(source_generation),
                        rebuilt_generation: None,
                        canonical_chunk_set_digest,
                        quarantine_reason_digest: None,
                    },
                    Some(rebuilt) => LegacyVectorMigrationItemReceiptV1 {
                        legacy_generation,
                        outcome:
                            LegacyVectorMigrationOutcomeKindV1::RebuildFromRetainedEligibleCode,
                        source_generation: Some(source_generation),
                        rebuilt_generation: Some(rebuilt.rebuilt_generation),
                        canonical_chunk_set_digest: Some(rebuilt.canonical_chunk_set_digest),
                        quarantine_reason_digest: None,
                    },
                }
            }
        };
        items.push(item);
    }

    if cancellation.is_cancelled() {
        return Err(LegacyVectorMigrationErrorV1::Cancelled);
    }
    let mut current_inventory = inventory.read_only_inventory()?;
    current_inventory
        .entries
        .sort_by(|left, right| left.legacy_generation().cmp(right.legacy_generation()));
    validate_inventory(&current_inventory)?;
    if current_inventory.canonical_digest()? != inventory_digest {
        return Err(LegacyVectorMigrationErrorV1::InventoryChanged);
    }
    if cancellation.is_cancelled() {
        return Err(LegacyVectorMigrationErrorV1::Cancelled);
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
    let next_active_generation = snapshot
        .expected_active_generation
        .as_ref()
        .and_then(|active| items.iter().find(|item| &item.legacy_generation == active))
        .and_then(|item| item.rebuilt_generation.clone());
    let receipt_digest = canonical_sha256(&(
        LEGACY_MIGRATION_RECEIPT_DOMAIN_V1,
        &inventory_digest,
        &snapshot.expected_active_generation,
        &next_active_generation,
        &counts,
        &items,
    ))
    .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))?;
    let receipt = LegacyVectorMigrationReceiptV1 {
        inventory_digest,
        expected_prior_active_generation: snapshot.expected_active_generation.clone(),
        next_active_generation: next_active_generation.clone(),
        counts,
        items,
        receipt_digest,
    };
    let transaction = LegacyVectorMigrationOwnerTransactionV1 {
        expected_prior_active_generation: snapshot.expected_active_generation,
        next_active_generation,
        receipt,
    };
    transaction.validate()?;
    if cancellation.is_cancelled() {
        return Err(LegacyVectorMigrationErrorV1::Cancelled);
    }
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

    struct CancelWhenStaged<'a>(&'a std::cell::Cell<bool>);

    impl LegacyVectorMigrationCancellationV1 for CancelWhenStaged<'_> {
        fn is_cancelled(&self) -> bool {
            self.0.get()
        }
    }

    struct ChangingInventory {
        reads: std::cell::Cell<u8>,
        first: LegacyVectorInventoryV1,
        second: LegacyVectorInventoryV1,
    }

    impl LegacyVectorInventoryPortV1 for ChangingInventory {
        fn read_only_inventory(
            &self,
        ) -> Result<LegacyVectorInventoryV1, LegacyVectorMigrationErrorV1> {
            let reads = self.reads.get();
            self.reads.set(reads.saturating_add(1));
            Ok(if reads == 0 {
                self.first.clone()
            } else {
                self.second.clone()
            })
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
    fn production_rebuilder_owns_retained_chunks_and_reports_missing_generations() {
        let source = generation("generation.retained");
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![chunk("chunk.retained", &source)],
        )
        .expect("canonical chunks");
        let expected = retained.clone();
        let mut rebuilder =
            ProductionLegacyVectorCanonicalRebuilderV1::try_new(vec![retained], |_| {
                unreachable!("lookup does not stage")
            })
            .expect("production rebuilder");

        assert_eq!(
            rebuilder.retained_eligible_chunks(&source),
            Ok(Some(expected))
        );
        assert_eq!(
            rebuilder.retained_eligible_chunks(&generation("generation.missing")),
            Ok(None)
        );
        assert!(rebuilder.staged_rebuilds().is_empty());
    }

    #[test]
    fn production_rebuilder_validates_the_exact_retained_source_and_result_digest() {
        let source = generation("generation.retained");
        let other_source = generation("generation.other");
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![chunk("chunk.retained", &source)],
        )
        .expect("canonical chunks");
        let foreign = CanonicalEligibleChunkSetV1::try_from_chunks(
            other_source.clone(),
            vec![chunk("chunk.other", &other_source)],
        )
        .expect("foreign canonical chunks");
        let callback_calls = std::cell::Cell::new(0_u8);
        let mut rebuilder = ProductionLegacyVectorCanonicalRebuilderV1::try_new(
            vec![retained.clone()],
            |chunks: &CanonicalEligibleChunkSetV1| {
                let call = callback_calls.get();
                callback_calls.set(call + 1);
                Ok(StagedCanonicalVectorRebuildV1 {
                    source_generation: if call == 0 {
                        other_source.clone()
                    } else {
                        chunks.source_generation().clone()
                    },
                    rebuilt_generation: vector(DIGEST_B),
                    canonical_chunk_set_digest: if call == 0 {
                        chunks.digest().clone()
                    } else {
                        manifest(DIGEST_C)
                    },
                })
            },
        )
        .expect("production rebuilder");

        assert_eq!(
            rebuilder.rebuild_from_retained_eligible_code(&foreign),
            Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch)
        );
        assert_eq!(callback_calls.get(), 0);
        assert_eq!(
            rebuilder.rebuild_from_retained_eligible_code(&retained),
            Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch)
        );
        assert_eq!(callback_calls.get(), 1);
        assert_eq!(
            rebuilder.rebuild_from_retained_eligible_code(&retained),
            Err(LegacyVectorMigrationErrorV1::RebuildIdentityMismatch)
        );
        assert_eq!(callback_calls.get(), 2);
        assert!(rebuilder.staged_rebuilds().is_empty());
    }

    #[test]
    fn production_rebuilder_callback_failure_records_no_staged_result() {
        let source = generation("generation.retained");
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![chunk("chunk.retained", &source)],
        )
        .expect("canonical chunks");
        let mut rebuilder =
            ProductionLegacyVectorCanonicalRebuilderV1::try_new(vec![retained.clone()], |_| {
                Err(LegacyVectorMigrationErrorV1::CanonicalCode(
                    "staging failed".to_owned(),
                ))
            })
            .expect("production rebuilder");

        assert_eq!(
            rebuilder.rebuild_from_retained_eligible_code(&retained),
            Err(LegacyVectorMigrationErrorV1::CanonicalCode(
                "staging failed".to_owned()
            ))
        );
        assert!(rebuilder.staged_rebuilds().is_empty());
    }

    #[test]
    fn production_rebuilder_replays_one_staged_result_idempotently() {
        let source = generation("generation.retained");
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![chunk("chunk.retained", &source)],
        )
        .expect("canonical chunks");
        let calls = std::cell::Cell::new(0_u8);
        let mut rebuilder =
            ProductionLegacyVectorCanonicalRebuilderV1::try_new(vec![retained.clone()], |chunks| {
                calls.set(calls.get().saturating_add(1));
                Ok(StagedCanonicalVectorRebuildV1 {
                    source_generation: chunks.source_generation().clone(),
                    rebuilt_generation: vector(DIGEST_B),
                    canonical_chunk_set_digest: chunks.digest().clone(),
                })
            })
            .expect("production rebuilder");

        let first = rebuilder
            .rebuild_from_retained_eligible_code(&retained)
            .expect("first staged rebuild");
        let second = rebuilder
            .rebuild_from_retained_eligible_code(&retained)
            .expect("idempotent staged rebuild");

        assert_eq!(first, second);
        assert_eq!(calls.get(), 1);
        assert_eq!(rebuilder.staged_rebuilds(), &[first]);
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
    fn legacy_generations_with_one_retained_source_share_one_canonical_rebuild() {
        let source = generation("generation.shared");
        let legacy_a = vector(DIGEST_A);
        let legacy_b = vector(DIGEST_B);
        let rebuilt = vector(DIGEST_C);
        let inventory = Inventory(LegacyVectorInventoryV1 {
            expected_active_generation: Some(legacy_a.clone()),
            entries: vec![
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy_a,
                    source_generation: source.clone(),
                },
                LegacyVectorInventoryEntryV1::Readable {
                    legacy_generation: legacy_b,
                    source_generation: source.clone(),
                },
            ],
        });
        let canonical = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![chunk("chunk.shared", &source)],
        )
        .expect("canonical chunks");
        let mut rebuilder = Rebuilder {
            chunks: BTreeMap::from([(source.clone(), Some(canonical))]),
            rebuilt: BTreeMap::from([(source, rebuilt.clone())]),
            observed_chunk_ids: vec![],
        };

        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("shared rebuild migration");

        assert_eq!(transaction.next_active_generation, Some(rebuilt.clone()));
        assert_eq!(
            transaction
                .receipt
                .items
                .iter()
                .filter_map(|item| item.rebuilt_generation.as_ref())
                .collect::<Vec<_>>(),
            vec![&rebuilt, &rebuilt]
        );
        assert_eq!(rebuilder.observed_chunk_ids.len(), 1);
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
    fn cancellation_after_the_final_rebuild_returns_no_owner_transaction() {
        let source = generation("generation.cancel-after-rebuild");
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![chunk("chunk.cancel-after-rebuild", &source)],
        )
        .expect("canonical chunks");
        let inventory = Inventory(LegacyVectorInventoryV1 {
            expected_active_generation: Some(vector(DIGEST_A)),
            entries: vec![LegacyVectorInventoryEntryV1::Readable {
                legacy_generation: vector(DIGEST_A),
                source_generation: source,
            }],
        });
        let staged = std::cell::Cell::new(false);
        let mut rebuilder =
            ProductionLegacyVectorCanonicalRebuilderV1::try_new(vec![retained], |chunks| {
                staged.set(true);
                Ok(StagedCanonicalVectorRebuildV1 {
                    source_generation: chunks.source_generation().clone(),
                    rebuilt_generation: vector(DIGEST_B),
                    canonical_chunk_set_digest: chunks.digest().clone(),
                })
            })
            .expect("production rebuilder");

        assert_eq!(
            prepare_legacy_vector_migration(&inventory, &mut rebuilder, &CancelWhenStaged(&staged),),
            Err(LegacyVectorMigrationErrorV1::Cancelled)
        );
        assert_eq!(rebuilder.staged_rebuilds().len(), 1);
    }

    #[test]
    fn inventory_change_after_rebuild_rejects_the_owner_transaction() {
        let legacy = vector(DIGEST_A);
        let source = generation("generation.inventory-cas");
        let first = LegacyVectorInventoryV1 {
            expected_active_generation: Some(legacy.clone()),
            entries: vec![LegacyVectorInventoryEntryV1::Readable {
                legacy_generation: legacy.clone(),
                source_generation: source.clone(),
            }],
        };
        let second = LegacyVectorInventoryV1 {
            expected_active_generation: None,
            entries: vec![LegacyVectorInventoryEntryV1::Readable {
                legacy_generation: legacy,
                source_generation: source,
            }],
        };
        let inventory = ChangingInventory {
            reads: std::cell::Cell::new(0),
            first,
            second,
        };
        let mut rebuilder = Rebuilder {
            chunks: BTreeMap::new(),
            rebuilt: BTreeMap::new(),
            observed_chunk_ids: vec![],
        };

        assert_eq!(
            prepare_legacy_vector_migration(
                &inventory,
                &mut rebuilder,
                &NeverCancelLegacyVectorMigrationV1,
            ),
            Err(LegacyVectorMigrationErrorV1::InventoryChanged)
        );
        assert_eq!(inventory.reads.get(), 2);
    }

    #[test]
    fn durable_receipt_binds_inventory_and_pointer_replacement() {
        let source = generation("generation.receipt");
        let legacy = vector(DIGEST_A);
        let rebuilt = vector(DIGEST_B);
        let inventory = Inventory(LegacyVectorInventoryV1 {
            expected_active_generation: Some(legacy.clone()),
            entries: vec![LegacyVectorInventoryEntryV1::Readable {
                legacy_generation: legacy.clone(),
                source_generation: source.clone(),
            }],
        });
        let retained = CanonicalEligibleChunkSetV1::try_from_chunks(
            source.clone(),
            vec![chunk("chunk.receipt", &source)],
        )
        .expect("canonical chunks");
        let mut rebuilder = Rebuilder {
            chunks: BTreeMap::from([(source.clone(), Some(retained))]),
            rebuilt: BTreeMap::from([(source, rebuilt.clone())]),
            observed_chunk_ids: vec![],
        };
        let transaction = prepare_legacy_vector_migration(
            &inventory,
            &mut rebuilder,
            &NeverCancelLegacyVectorMigrationV1,
        )
        .expect("migration transaction");

        assert_eq!(
            transaction.receipt.inventory_digest,
            inventory.0.canonical_digest().expect("inventory digest")
        );
        assert_eq!(
            transaction.receipt.expected_prior_active_generation,
            Some(legacy)
        );
        assert_eq!(transaction.receipt.next_active_generation, Some(rebuilt));
        transaction.receipt.validate().expect("durable receipt");

        let mut tampered = transaction.receipt;
        tampered.next_active_generation = None;
        assert_eq!(
            tampered.validate(),
            Err(LegacyVectorMigrationErrorV1::InvalidReceipt)
        );
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
