use serde::{Deserialize, Serialize};

use super::super::{
    GraphProjectionIdentityV1, GraphPublicationReplayRetirementV1,
    GraphPublicationReplayTombstoneV1, GraphVerifiedHeadV1, StorageRuntimeContractErrorV1,
    StoreRuntimeBindingV1, StoreShardIdV1,
};
use super::{
    SemanticVectorSourceDependencyV1, SemanticVectorSourceGenerationId, SemanticVectorStageKey,
    SemanticVectorStageRecord, SemanticVectorStageState, SemanticVectorWriterFence,
};

pub const MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS: u16 = 256;
pub const MAX_SEMANTIC_VECTOR_ADOPTION_PAGE_RECORDS: u16 = 256;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct SemanticVectorStageCensusRevision(u64);

impl SemanticVectorStageCensusRevision {
    pub const INITIAL: Self = Self(0);

    pub fn new(value: u64) -> Result<Self, StorageRuntimeContractErrorV1> {
        if value > i64::MAX.unsigned_abs() {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector census revision",
                actual: value,
                max: i64::MAX.unsigned_abs(),
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for SemanticVectorStageCensusRevision {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SemanticVectorStageCensusRevision> for u64 {
    fn from(value: SemanticVectorStageCensusRevision) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageAdoptionCursor {
    pub binding: StoreRuntimeBindingV1,
    pub revision: SemanticVectorStageCensusRevision,
    pub after_stage_id: u64,
}

impl SemanticVectorStageAdoptionCursor {
    pub fn new(
        binding: StoreRuntimeBindingV1,
        revision: SemanticVectorStageCensusRevision,
        after_stage_id: u64,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        if after_stage_id == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "semantic vector adoption stage cursor",
            });
        }
        if after_stage_id > i64::MAX.unsigned_abs() {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector adoption stage cursor",
                actual: after_stage_id,
                max: i64::MAX.unsigned_abs(),
            });
        }
        Ok(Self {
            binding,
            revision,
            after_stage_id,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageAdoptionPageRequest {
    pub binding: StoreRuntimeBindingV1,
    pub after: Option<SemanticVectorStageAdoptionCursor>,
    pub max_records: u16,
}

impl SemanticVectorStageAdoptionPageRequest {
    pub fn new(
        binding: StoreRuntimeBindingV1,
        after: Option<SemanticVectorStageAdoptionCursor>,
        max_records: u16,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        if max_records == 0 || max_records > MAX_SEMANTIC_VECTOR_ADOPTION_PAGE_RECORDS {
            return Err(StorageRuntimeContractErrorV1::InvalidRange {
                field: "semantic vector adoption page records",
                min: 1,
                max: u64::from(MAX_SEMANTIC_VECTOR_ADOPTION_PAGE_RECORDS),
            });
        }
        if after
            .as_ref()
            .is_some_and(|cursor| cursor.binding != binding)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector adoption cursor binding",
            });
        }
        Ok(Self {
            binding,
            after,
            max_records,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStageAdoptionRecord {
    pub cursor: SemanticVectorStageAdoptionCursor,
    pub stage: SemanticVectorStageRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStageAdoptionPage {
    pub binding: StoreRuntimeBindingV1,
    pub revision: SemanticVectorStageCensusRevision,
    pub records: Vec<SemanticVectorStageAdoptionRecord>,
    pub continuation: Option<SemanticVectorStageAdoptionCursor>,
}

impl SemanticVectorStageAdoptionPage {
    pub fn new(
        binding: StoreRuntimeBindingV1,
        revision: SemanticVectorStageCensusRevision,
        records: Vec<SemanticVectorStageAdoptionRecord>,
        continuation: Option<SemanticVectorStageAdoptionCursor>,
        max_records: u16,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let invalid_continuation = continuation.as_ref().is_some_and(|cursor| {
            cursor.binding != binding
                || cursor.revision != revision
                || records.last().is_none_or(|record| cursor != &record.cursor)
        });
        if records.len() > usize::from(max_records) || invalid_continuation {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector adoption page records",
                actual: u64::try_from(records.len()).unwrap_or(u64::MAX),
                max: u64::from(max_records),
            });
        }
        Ok(Self {
            binding,
            revision,
            records,
            continuation,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageCensusCursor {
    pub shard_id: StoreShardIdV1,
    pub projection: Option<GraphProjectionIdentityV1>,
    pub revision: SemanticVectorStageCensusRevision,
    pub after_stage_id: u64,
    pub counts: SemanticVectorStageCensusCounts,
    pub record_digest: tracedecay_domain::ManifestDigest,
}

impl SemanticVectorStageCensusCursor {
    pub fn new(
        shard_id: StoreShardIdV1,
        projection: Option<GraphProjectionIdentityV1>,
        revision: SemanticVectorStageCensusRevision,
        after_stage_id: u64,
        counts: SemanticVectorStageCensusCounts,
        record_digest: tracedecay_domain::ManifestDigest,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        if after_stage_id == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "semantic vector census stage cursor",
            });
        }
        if after_stage_id > i64::MAX.unsigned_abs() {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector census stage cursor",
                actual: after_stage_id,
                max: i64::MAX.unsigned_abs(),
            });
        }
        if projection
            .as_ref()
            .is_some_and(|projection| projection.shard_id != shard_id)
        {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "semantic vector census cursor projection",
            });
        }
        Ok(Self {
            shard_id,
            projection,
            revision,
            after_stage_id,
            counts,
            record_digest,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageCensusRequest {
    pub shard_id: StoreShardIdV1,
    pub projection: Option<GraphProjectionIdentityV1>,
    pub after: Option<SemanticVectorStageCensusCursor>,
    pub max_records: u16,
}

impl SemanticVectorStageCensusRequest {
    pub fn new(
        projection: GraphProjectionIdentityV1,
        after: Option<SemanticVectorStageCensusCursor>,
        max_records: u16,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        if max_records == 0 || max_records > MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS {
            return Err(StorageRuntimeContractErrorV1::InvalidRange {
                field: "semantic vector census page records",
                min: 1,
                max: u64::from(MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS),
            });
        }
        Ok(Self {
            shard_id: projection.shard_id.clone(),
            projection: Some(projection),
            after,
            max_records,
        })
    }

    pub fn for_shard(
        shard_id: StoreShardIdV1,
        after: Option<SemanticVectorStageCensusCursor>,
        max_records: u16,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        if max_records == 0 || max_records > MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS {
            return Err(StorageRuntimeContractErrorV1::InvalidRange {
                field: "semantic vector census page records",
                min: 1,
                max: u64::from(MAX_SEMANTIC_VECTOR_CENSUS_PAGE_RECORDS),
            });
        }
        Ok(Self {
            shard_id,
            projection: None,
            after,
            max_records,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStageCensusRecord {
    pub cursor: SemanticVectorStageCensusCursor,
    pub stage: SemanticVectorStageRecord,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorStageCensusPage {
    pub shard_id: StoreShardIdV1,
    pub projection: Option<GraphProjectionIdentityV1>,
    pub revision: SemanticVectorStageCensusRevision,
    pub records: Vec<SemanticVectorStageCensusRecord>,
    pub continuation: Option<SemanticVectorStageCensusCursor>,
    pub complete_receipt: Option<SemanticVectorProjectCensusReceipt>,
}

impl SemanticVectorStageCensusPage {
    pub fn new(
        shard_id: StoreShardIdV1,
        projection: Option<GraphProjectionIdentityV1>,
        revision: SemanticVectorStageCensusRevision,
        records: Vec<SemanticVectorStageCensusRecord>,
        continuation: Option<SemanticVectorStageCensusCursor>,
        complete_receipt: Option<SemanticVectorProjectCensusReceipt>,
        max_records: u16,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let actual = u64::try_from(records.len()).map_err(|_| {
            StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector census page records",
                actual: u64::MAX,
                max: u64::from(max_records),
            }
        })?;
        let invalid_continuation = continuation.as_ref().is_some_and(|cursor| {
            cursor.shard_id != shard_id
                || cursor.projection != projection
                || cursor.revision != revision
                || records.last().is_none_or(|record| cursor != &record.cursor)
        });
        let invalid_completion = match (&continuation, &complete_receipt) {
            (Some(_), Some(_)) | (None, None) => true,
            (Some(_), None) => false,
            (None, Some(receipt)) => {
                receipt.shard_id != shard_id
                    || receipt.revision != revision
                    || records.last().map_or_else(
                        || receipt.counts != SemanticVectorStageCensusCounts::default(),
                        |record| {
                            receipt.counts != record.cursor.counts
                                || receipt.record_digest != record.cursor.record_digest
                        },
                    )
            }
        };
        if records.len() > usize::from(max_records) || invalid_continuation || invalid_completion {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector census page records",
                actual,
                max: u64::from(max_records),
            });
        }
        Ok(Self {
            shard_id,
            projection,
            revision,
            records,
            continuation,
            complete_receipt,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorStageCensusCounts {
    pub pending: u64,
    pub ready: u64,
    pub published: u64,
    pub cancelled: u64,
}

impl SemanticVectorStageCensusCounts {
    pub fn checked_add_record(
        &mut self,
        state: SemanticVectorStageState,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        let count = match state {
            SemanticVectorStageState::Pending => &mut self.pending,
            SemanticVectorStageState::ReadyToPublish => &mut self.ready,
            SemanticVectorStageState::Published => &mut self.published,
            SemanticVectorStageState::Cancelled => &mut self.cancelled,
        };
        *count = count
            .checked_add(1)
            .ok_or(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector census stage count",
                actual: u64::MAX,
                max: u64::MAX - 1,
            })?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorCensusDependencyV1 {
    pub semantic_generation_id: tracedecay_domain::VectorGenerationIdV1,
    pub source_scope: StoreShardIdV1,
    pub code_scope_hash: super::SemanticVectorCodeScopeHash,
    pub source_generation: SemanticVectorSourceGenerationId,
    pub source_dependency: SemanticVectorSourceDependencyV1,
    pub stage_state: SemanticVectorStageState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorProjectCensusReceipt {
    pub shard_id: StoreShardIdV1,
    pub revision: SemanticVectorStageCensusRevision,
    pub counts: SemanticVectorStageCensusCounts,
    pub record_digest: tracedecay_domain::ManifestDigest,
}

impl SemanticVectorProjectCensusReceipt {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.counts
            .pending
            .checked_add(self.counts.ready)
            .and_then(|count| count.checked_add(self.counts.published))
            .and_then(|count| count.checked_add(self.counts.cancelled))
            .ok_or(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector census total stage count",
                actual: u64::MAX,
                max: u64::MAX - 1,
            })?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorPublishedGenerationDependencyLookup {
    Missing,
    Published(SemanticVectorCensusDependencyV1),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorSourceScopeBindingLookup {
    Missing,
    Exact(StoreShardIdV1),
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemanticVectorRetirementCleanupCursor(u64);

impl SemanticVectorRetirementCleanupCursor {
    pub fn new(value: u64) -> Result<Self, StorageRuntimeContractErrorV1> {
        if value == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "semantic vector retirement cleanup cursor",
            });
        }
        if value > i64::MAX.unsigned_abs() {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "semantic vector retirement cleanup cursor",
                actual: value,
                max: i64::MAX.unsigned_abs(),
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorPublishedRetirement {
    pub stage: SemanticVectorStageKey,
    pub semantic_generation_id: tracedecay_domain::VectorGenerationIdV1,
    pub replay: GraphPublicationReplayRetirementV1,
    pub writer_fence: SemanticVectorWriterFence,
}

impl SemanticVectorPublishedRetirement {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        self.semantic_generation_id.validate().map_err(|_| {
            StorageRuntimeContractErrorV1::NonCanonical {
                field: "semantic vector retirement generation",
            }
        })?;
        self.replay.validate()?;
        self.writer_fence.validate_for(&self.stage.projection)?;
        if self.replay.key.projection != self.stage.projection {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "semantic vector retirement replay projection",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorPublishedRetirementOutcome {
    Retired(GraphPublicationReplayTombstoneV1),
    ExactReplay(GraphPublicationReplayTombstoneV1),
    CurrentVerifiedHead { head: GraphVerifiedHeadV1 },
    PendingReplay,
    Conflict,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticVectorRetirementCleanupRecord {
    pub cursor: SemanticVectorRetirementCleanupCursor,
    pub retirement: SemanticVectorPublishedRetirement,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVectorCancelledRetirement {
    pub stage: SemanticVectorStageKey,
    pub writer_fence: SemanticVectorWriterFence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SemanticVectorCancelledRetirementOutcome {
    Removed,
    ExactMissing,
    NotCancelled(SemanticVectorStageRecord),
}
