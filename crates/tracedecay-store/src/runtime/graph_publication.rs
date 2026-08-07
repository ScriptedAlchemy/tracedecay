use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use super::identity::{canonical_id, validate_canonical_id};
use super::{StorageRuntimeContractErrorV1, StoreShardIdV1, StoreShardScopeV1};

/// Bound for opaque replay-source envelopes materialized through exact SQL.
pub const MAX_GRAPH_REPLAY_SOURCE_BYTES_V1: usize = 4 * 1024 * 1024;
pub const MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1: usize = 256;
pub const MAX_GRAPH_REPLAY_DIRECT_DEPENDENCY_BYTES_V1: usize = 1024 * 1024;
pub const MAX_GRAPH_REPLAY_PAGE_RECORDS_V1: u16 = 64;
pub const MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1: usize = MAX_GRAPH_REPLAY_SOURCE_BYTES_V1;
pub const MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1: u16 = 64;

#[path = "graph_publication/cleanup.rs"]
mod cleanup;
pub use cleanup::{
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationRetiredCleanupPageV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1,
};
#[path = "graph_publication/operation.rs"]
mod operation;
pub use operation::{
    GraphPublicationOperationContextV1, GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1,
};
#[path = "graph_publication/store.rs"]
mod store;
pub use store::GraphPublicationStoreV1;

canonical_id!(GraphNamespaceV1, "graph namespace");
canonical_id!(GraphProjectionIdV1, "graph projection id");
canonical_id!(GraphGenerationIdV1, "graph generation id");
canonical_id!(
    GraphPublicationIdempotencyKeyV1,
    "graph publication idempotency key"
);

macro_rules! sha256_digest {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, StorageRuntimeContractErrorV1> {
                let value = value.into();
                validate_sha256_digest(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = StorageRuntimeContractErrorV1;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

sha256_digest!(
    GraphPublicationInputDigestV1,
    "graph publication input digest"
);
sha256_digest!(
    GraphDependencyGenerationClosureDigestV1,
    "graph dependency generation closure digest"
);
sha256_digest!(
    GraphRecoveredGenerationDigestV1,
    "graph recovered generation digest"
);
sha256_digest!(
    GraphCanonicalReplaySourceDigestV1,
    "graph canonical replay source digest"
);

impl GraphCanonicalReplaySourceDigestV1 {
    pub fn for_source(source: &[u8]) -> Self {
        Self(
            tracedecay_domain::canonical_text::encode_tagged_lowercase_hex(
                "sha256:",
                &Sha256::digest(source),
            ),
        )
    }
}

fn validate_sha256_digest(
    value: &str,
    field: &'static str,
) -> Result<(), StorageRuntimeContractErrorV1> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(StorageRuntimeContractErrorV1::NonCanonical { field });
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StorageRuntimeContractErrorV1::NonCanonical { field });
    }
    Ok(())
}

/// Exact relational scope of one rebuildable graph projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct GraphProjectionIdentityV1 {
    pub shard_id: StoreShardIdV1,
    pub namespace: GraphNamespaceV1,
    pub projection: GraphProjectionIdV1,
}

/// Immutable event identity used for graph publication replay and idempotency.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationKeyV1 {
    pub projection: GraphProjectionIdentityV1,
    pub generation: GraphGenerationIdV1,
    pub idempotency_key: GraphPublicationIdempotencyKeyV1,
}

impl GraphPublicationKeyV1 {
    pub fn new(
        projection: GraphProjectionIdentityV1,
        generation: GraphGenerationIdV1,
        idempotency_key: GraphPublicationIdempotencyKeyV1,
    ) -> Self {
        Self {
            projection,
            generation,
            idempotency_key,
        }
    }
}

/// One direct generation dependency needed to locate and retain replay state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct GraphDependencyGenerationIdentityV1 {
    pub projection: GraphProjectionIdentityV1,
    pub generation: GraphGenerationIdV1,
}

impl GraphDependencyGenerationIdentityV1 {
    pub fn new(projection: GraphProjectionIdentityV1, generation: GraphGenerationIdV1) -> Self {
        Self {
            projection,
            generation,
        }
    }
}

/// Canonical replay-source envelope retained outside the disposable graph DB.
///
/// The bytes are opaque to the relational store. The owning publisher defines
/// whether they encode a small inline source or a sealed durable-source
/// descriptor, and supplies the digest of the fully recovered projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationReplayV1 {
    pub key: GraphPublicationKeyV1,
    pub input_digest: GraphPublicationInputDigestV1,
    pub dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
    pub direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
    pub expected_prior_head: Option<GraphVerifiedHeadV1>,
    pub expected_recovered_digest: GraphRecoveredGenerationDigestV1,
    pub canonical_replay_source_digest: GraphCanonicalReplaySourceDigestV1,
    pub canonical_replay_source: Vec<u8>,
}

impl GraphPublicationReplayV1 {
    pub fn new(
        key: GraphPublicationKeyV1,
        input_digest: GraphPublicationInputDigestV1,
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
        direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        expected_recovered_digest: GraphRecoveredGenerationDigestV1,
        canonical_replay_source: Vec<u8>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let canonical_replay_source_digest =
            GraphCanonicalReplaySourceDigestV1::for_source(&canonical_replay_source);
        let replay = Self {
            key,
            input_digest,
            dependency_generation_closure_digest,
            direct_dependency_generations,
            expected_prior_head,
            expected_recovered_digest,
            canonical_replay_source_digest,
            canonical_replay_source,
        };
        replay.validate()?;
        Ok(replay)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_project_shard(&self.key.projection.shard_id, "graph publication replay")?;
        if self
            .expected_prior_head
            .as_ref()
            .is_some_and(|head| head.key.projection != self.key.projection)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay prior projection",
            });
        }
        if self
            .expected_prior_head
            .as_ref()
            .is_some_and(|head| head.key.generation == self.key.generation)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay prior generation",
            });
        }
        validate_direct_dependency_generations(&self.key, &self.direct_dependency_generations)?;
        if self.canonical_replay_source.is_empty() {
            return Err(StorageRuntimeContractErrorV1::Empty {
                field: "graph replay source",
            });
        }
        if self.canonical_replay_source.len() > MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "graph replay source",
                actual: self.canonical_replay_source.len(),
                max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            });
        }
        if self.canonical_replay_source_digest
            != GraphCanonicalReplaySourceDigestV1::for_source(&self.canonical_replay_source)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay source digest",
            });
        }
        let dependency_bytes =
            encoded_direct_dependency_bytes(&self.direct_dependency_generations)?;
        let payload_bytes = dependency_bytes
            .checked_add(self.canonical_replay_source.len())
            .ok_or(StorageRuntimeContractErrorV1::TooLong {
                field: "graph replay payload",
                actual: usize::MAX,
                max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            })?;
        if payload_bytes > MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "graph replay payload",
                actual: payload_bytes,
                max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            });
        }
        Ok(())
    }

    fn payload_bytes(&self) -> Result<usize, StorageRuntimeContractErrorV1> {
        encoded_direct_dependency_bytes(&self.direct_dependency_generations)?
            .checked_add(self.canonical_replay_source.len())
            .ok_or(StorageRuntimeContractErrorV1::TooLong {
                field: "graph replay payload",
                actual: usize::MAX,
                max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
            })
    }
}

fn validate_direct_dependency_generations(
    owner: &GraphPublicationKeyV1,
    dependencies: &[GraphDependencyGenerationIdentityV1],
) -> Result<(), StorageRuntimeContractErrorV1> {
    if dependencies.len() > MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1 {
        return Err(StorageRuntimeContractErrorV1::TooLong {
            field: "graph replay direct dependencies",
            actual: dependencies.len(),
            max: MAX_GRAPH_REPLAY_DIRECT_DEPENDENCIES_V1,
        });
    }
    if dependencies.windows(2).any(|window| window[0] >= window[1]) {
        return Err(StorageRuntimeContractErrorV1::NonCanonical {
            field: "graph replay direct dependency order",
        });
    }
    for dependency in dependencies {
        if dependency.projection.shard_id != owner.projection.shard_id {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "graph replay direct dependency",
            });
        }
        if dependency.projection == owner.projection {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay self dependency",
            });
        }
    }
    encoded_direct_dependency_bytes(dependencies).map(|_| ())
}

fn validate_project_shard(
    shard_id: &StoreShardIdV1,
    operation: &'static str,
) -> Result<(), StorageRuntimeContractErrorV1> {
    if matches!(&shard_id.scope, StoreShardScopeV1::Project { .. }) {
        Ok(())
    } else {
        Err(StorageRuntimeContractErrorV1::OperationScopeMismatch {
            operation,
            shard_family: "non-project",
        })
    }
}

fn encoded_direct_dependency_bytes(
    dependencies: &[GraphDependencyGenerationIdentityV1],
) -> Result<usize, StorageRuntimeContractErrorV1> {
    let encoded = serde_json::to_vec(dependencies).map_err(|_| {
        StorageRuntimeContractErrorV1::NonCanonical {
            field: "graph replay direct dependency encoding",
        }
    })?;
    if encoded.len() > MAX_GRAPH_REPLAY_DIRECT_DEPENDENCY_BYTES_V1 {
        return Err(StorageRuntimeContractErrorV1::TooLong {
            field: "graph replay direct dependency encoding",
            actual: encoded.len(),
            max: MAX_GRAPH_REPLAY_DIRECT_DEPENDENCY_BYTES_V1,
        });
    }
    Ok(encoded.len())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "u64", into = "u64")]
pub struct GraphPublicationSequenceV1(u64);

impl GraphPublicationSequenceV1 {
    pub fn new(value: u64) -> Result<Self, StorageRuntimeContractErrorV1> {
        if value == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "graph publication sequence",
            });
        }
        if value > i64::MAX.unsigned_abs() {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "graph publication sequence",
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

impl TryFrom<u64> for GraphPublicationSequenceV1 {
    type Error = StorageRuntimeContractErrorV1;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<GraphPublicationSequenceV1> for u64 {
    fn from(value: GraphPublicationSequenceV1) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationReplayCursorV1 {
    pub projection: GraphProjectionIdentityV1,
    pub sequence: GraphPublicationSequenceV1,
}

impl GraphPublicationReplayCursorV1 {
    pub fn new(
        projection: GraphProjectionIdentityV1,
        sequence: GraphPublicationSequenceV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        validate_project_shard(&projection.shard_id, "graph publication replay cursor")?;
        Ok(Self {
            projection,
            sequence,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationReplayRecordV1 {
    pub sequence: GraphPublicationSequenceV1,
    pub publication: GraphPublicationReplayV1,
}

impl GraphPublicationReplayRecordV1 {
    pub fn new(
        sequence: GraphPublicationSequenceV1,
        publication: GraphPublicationReplayV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        publication.validate()?;
        Ok(Self {
            sequence,
            publication,
        })
    }
}

/// Bounded keyset page request for replay-driven recovery and graph GC.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationReplayPageRequestV1 {
    pub projection: GraphProjectionIdentityV1,
    pub after: Option<GraphPublicationReplayCursorV1>,
    pub max_records: u16,
}

impl GraphPublicationReplayPageRequestV1 {
    pub fn new(
        projection: GraphProjectionIdentityV1,
        after: Option<GraphPublicationReplayCursorV1>,
        max_records: u16,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let request = Self {
            projection,
            after,
            max_records,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_project_shard(&self.projection.shard_id, "graph publication replay page")?;
        if self
            .after
            .as_ref()
            .is_some_and(|after| after.projection != self.projection)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay page cursor projection",
            });
        }
        if self.max_records == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "graph replay page records",
            });
        }
        if self.max_records > MAX_GRAPH_REPLAY_PAGE_RECORDS_V1 {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "graph replay page records",
                actual: u64::from(self.max_records),
                max: u64::from(MAX_GRAPH_REPLAY_PAGE_RECORDS_V1),
            });
        }
        Ok(())
    }
}

/// One payload-bounded replay page.
/// `continuation` is present only when another record exists after the last
/// returned sequence. Passing it as the next request's `after` cursor makes
/// restart-safe enumeration independent of concurrent projections.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationReplayPageV1 {
    pub records: Vec<GraphPublicationReplayRecordV1>,
    pub continuation: Option<GraphPublicationReplayCursorV1>,
}

impl GraphPublicationReplayPageV1 {
    pub fn new(
        records: Vec<GraphPublicationReplayRecordV1>,
        continuation: Option<GraphPublicationReplayCursorV1>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        for record in &records {
            record.publication.validate()?;
        }
        if records.len() > usize::from(MAX_GRAPH_REPLAY_PAGE_RECORDS_V1) {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "graph replay page records",
                actual: records.len(),
                max: usize::from(MAX_GRAPH_REPLAY_PAGE_RECORDS_V1),
            });
        }
        let payload_bytes = records.iter().try_fold(0_usize, |total, record| {
            let record_bytes = record.publication.payload_bytes()?;
            total
                .checked_add(record_bytes)
                .ok_or(StorageRuntimeContractErrorV1::TooLong {
                    field: "graph replay page payload",
                    actual: usize::MAX,
                    max: MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1,
                })
        })?;
        if payload_bytes > MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1 {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "graph replay page payload",
                actual: payload_bytes,
                max: MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1,
            });
        }
        if records
            .windows(2)
            .any(|window| window[0].sequence >= window[1].sequence)
        {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "graph replay page sequence order",
            });
        }
        if records.first().is_some_and(|first| {
            records
                .iter()
                .any(|record| record.publication.key.projection != first.publication.key.projection)
        }) {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay page projection",
            });
        }
        if continuation.is_some() && records.is_empty() {
            return Err(StorageRuntimeContractErrorV1::Empty {
                field: "graph replay page continuation records",
            });
        }
        if continuation
            .as_ref()
            .zip(records.last())
            .is_some_and(|(continuation, last)| {
                continuation.sequence != last.sequence
                    || continuation.projection != last.publication.key.projection
            })
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph replay page continuation",
            });
        }
        Ok(Self {
            records,
            continuation,
        })
    }
}

/// Bounded keyset inventory request for graph projections in one project shard.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationProjectionPageRequestV1 {
    pub shard_id: StoreShardIdV1,
    pub after: Option<GraphProjectionIdentityV1>,
    pub max_records: u16,
}

impl GraphPublicationProjectionPageRequestV1 {
    pub fn new(
        shard_id: StoreShardIdV1,
        after: Option<GraphProjectionIdentityV1>,
        max_records: u16,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let request = Self {
            shard_id,
            after,
            max_records,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_project_shard(&self.shard_id, "graph publication projection inventory")?;
        if self
            .after
            .as_ref()
            .is_some_and(|after| after.shard_id != self.shard_id)
        {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "graph projection page cursor",
            });
        }
        if self.max_records == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "graph projection page records",
            });
        }
        if self.max_records > MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1 {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "graph projection page records",
                actual: u64::from(self.max_records),
                max: u64::from(MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationProjectionPageV1 {
    pub projections: Vec<GraphProjectionIdentityV1>,
    pub continuation: Option<GraphProjectionIdentityV1>,
}

impl GraphPublicationProjectionPageV1 {
    pub fn new(
        projections: Vec<GraphProjectionIdentityV1>,
        continuation: Option<GraphProjectionIdentityV1>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        if projections.len() > usize::from(MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1) {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "graph projection page records",
                actual: projections.len(),
                max: usize::from(MAX_GRAPH_PUBLICATION_PROJECTION_PAGE_RECORDS_V1),
            });
        }
        if projections.windows(2).any(|window| window[0] >= window[1]) {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "graph projection page order",
            });
        }
        if projections.first().is_some_and(|first| {
            projections
                .iter()
                .any(|projection| projection.shard_id != first.shard_id)
        }) {
            return Err(StorageRuntimeContractErrorV1::ShardMismatch {
                field: "graph projection page",
            });
        }
        if continuation.is_some() && projections.is_empty() {
            return Err(StorageRuntimeContractErrorV1::Empty {
                field: "graph projection page continuation records",
            });
        }
        if continuation
            .as_ref()
            .zip(projections.last())
            .is_some_and(|(continuation, last)| continuation != last)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph projection page continuation",
            });
        }
        Ok(Self {
            projections,
            continuation,
        })
    }
}

/// Durable evidence retained after an exact historical replay is collected.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationReplayTombstoneV1 {
    pub sequence: GraphPublicationSequenceV1,
    pub key: GraphPublicationKeyV1,
    pub input_digest: GraphPublicationInputDigestV1,
    pub dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
    pub direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
    pub expected_prior_head: Option<GraphVerifiedHeadV1>,
    pub expected_recovered_digest: GraphRecoveredGenerationDigestV1,
    pub canonical_replay_source_digest: GraphCanonicalReplaySourceDigestV1,
    pub canonical_replay_source: Option<Vec<u8>>,
}

/// Exact evidence required before a historical replay may be retired.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationReplayRetirementV1 {
    pub key: GraphPublicationKeyV1,
    pub input_digest: GraphPublicationInputDigestV1,
    pub dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
    pub direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
    pub expected_prior_head: Option<GraphVerifiedHeadV1>,
    pub expected_recovered_digest: GraphRecoveredGenerationDigestV1,
    pub canonical_replay_source_digest: GraphCanonicalReplaySourceDigestV1,
}

impl GraphPublicationReplayRetirementV1 {
    pub fn new(
        key: GraphPublicationKeyV1,
        input_digest: GraphPublicationInputDigestV1,
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
        direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        expected_recovered_digest: GraphRecoveredGenerationDigestV1,
        canonical_replay_source_digest: GraphCanonicalReplaySourceDigestV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        let request = Self {
            key,
            input_digest,
            dependency_generation_closure_digest,
            direct_dependency_generations,
            expected_prior_head,
            expected_recovered_digest,
            canonical_replay_source_digest,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_project_shard(
            &self.key.projection.shard_id,
            "graph publication replay retirement",
        )?;
        validate_direct_dependency_generations(&self.key, &self.direct_dependency_generations)?;
        if self
            .expected_prior_head
            .as_ref()
            .is_some_and(|head| head.key.projection != self.key.projection)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph retirement prior projection",
            });
        }
        if self
            .expected_prior_head
            .as_ref()
            .is_some_and(|head| head.key.generation == self.key.generation)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph retirement prior generation",
            });
        }
        Ok(())
    }
}

impl GraphPublicationReplayTombstoneV1 {
    pub fn new(
        sequence: GraphPublicationSequenceV1,
        retirement: GraphPublicationReplayRetirementV1,
        canonical_replay_source: Option<Vec<u8>>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        retirement.validate()?;
        if let Some(source) = canonical_replay_source.as_ref() {
            if source.is_empty() {
                return Err(StorageRuntimeContractErrorV1::Empty {
                    field: "graph retired cleanup source",
                });
            }
            if GraphCanonicalReplaySourceDigestV1::for_source(source)
                != retirement.canonical_replay_source_digest
            {
                return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                    field: "graph retired cleanup source digest",
                });
            }
            let payload_bytes =
                encoded_direct_dependency_bytes(&retirement.direct_dependency_generations)?
                    .checked_add(source.len())
                    .ok_or(StorageRuntimeContractErrorV1::TooLong {
                        field: "graph retired cleanup payload",
                        actual: usize::MAX,
                        max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
                    })?;
            if payload_bytes > MAX_GRAPH_REPLAY_SOURCE_BYTES_V1 {
                return Err(StorageRuntimeContractErrorV1::TooLong {
                    field: "graph retired cleanup payload",
                    actual: payload_bytes,
                    max: MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
                });
            }
        }
        Ok(Self {
            sequence,
            key: retirement.key,
            input_digest: retirement.input_digest,
            dependency_generation_closure_digest: retirement.dependency_generation_closure_digest,
            direct_dependency_generations: retirement.direct_dependency_generations,
            expected_prior_head: retirement.expected_prior_head,
            expected_recovered_digest: retirement.expected_recovered_digest,
            canonical_replay_source_digest: retirement.canonical_replay_source_digest,
            canonical_replay_source,
        })
    }

    pub fn retirement(&self) -> GraphPublicationReplayRetirementV1 {
        GraphPublicationReplayRetirementV1 {
            key: self.key.clone(),
            input_digest: self.input_digest.clone(),
            dependency_generation_closure_digest: self.dependency_generation_closure_digest.clone(),
            direct_dependency_generations: self.direct_dependency_generations.clone(),
            expected_prior_head: self.expected_prior_head.clone(),
            expected_recovered_digest: self.expected_recovered_digest.clone(),
            canonical_replay_source_digest: self.canonical_replay_source_digest.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphPublicationReplayLookupV1 {
    Active(GraphPublicationReplayRecordV1),
    Retired(GraphPublicationReplayTombstoneV1),
    Missing,
}

/// The only graph generation a relational reader may treat as recovered.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphVerifiedHeadV1 {
    pub sequence: GraphPublicationSequenceV1,
    pub key: GraphPublicationKeyV1,
    pub input_digest: GraphPublicationInputDigestV1,
    pub dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
    pub recovered_digest: GraphRecoveredGenerationDigestV1,
}

impl GraphVerifiedHeadV1 {
    pub fn from_replay(
        replay: &GraphPublicationReplayRecordV1,
        recovered_digest: GraphRecoveredGenerationDigestV1,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        if recovered_digest != replay.publication.expected_recovered_digest {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph recovered generation digest",
            });
        }
        Ok(Self {
            sequence: replay.sequence,
            key: replay.publication.key.clone(),
            input_digest: replay.publication.input_digest.clone(),
            dependency_generation_closure_digest: replay
                .publication
                .dependency_generation_closure_digest
                .clone(),
            recovered_digest,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphVerifiedHeadCompareAndSwapV1 {
    pub publication_key: GraphPublicationKeyV1,
    pub input_digest: GraphPublicationInputDigestV1,
    pub dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
    pub recovered_digest: GraphRecoveredGenerationDigestV1,
    pub expected_prior_head: Option<GraphVerifiedHeadV1>,
}

impl GraphVerifiedHeadCompareAndSwapV1 {
    pub fn validate(&self) -> Result<(), StorageRuntimeContractErrorV1> {
        validate_project_shard(
            &self.publication_key.projection.shard_id,
            "graph verified head compare and swap",
        )?;
        if self
            .expected_prior_head
            .as_ref()
            .is_some_and(|head| head.key.projection != self.publication_key.projection)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph verified prior projection",
            });
        }
        if self
            .expected_prior_head
            .as_ref()
            .is_some_and(|head| head.key.generation == self.publication_key.generation)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph verified prior generation",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphReplayAppendOutcomeV1 {
    Appended(GraphPublicationReplayRecordV1),
    /// Exact replay of the sole unverified pending candidate.
    ExactReplay(GraphPublicationReplayRecordV1),
    /// Exact replay of a generation proven verified by ordered head history.
    ExactVerifiedReplay {
        replay: GraphPublicationReplayRecordV1,
        receipt: Box<GraphVerifiedHeadV1>,
    },
    Conflict {
        existing: GraphPublicationReplayRecordV1,
    },
    RetiredReplayConflict {
        retired: GraphPublicationReplayTombstoneV1,
    },
    VerifiedHeadConflict {
        actual: Option<GraphVerifiedHeadV1>,
    },
    PendingReplayConflict {
        pending: GraphPublicationReplayRecordV1,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphVerifiedHeadCasOutcomeV1 {
    Advanced(GraphVerifiedHeadV1),
    ExactReplay(GraphVerifiedHeadV1),
    Conflict {
        actual: Option<GraphVerifiedHeadV1>,
    },
    ReplayInputConflict {
        existing: GraphPublicationReplayRecordV1,
    },
    RecoveredDigestMismatch {
        expected: GraphRecoveredGenerationDigestV1,
        actual: GraphRecoveredGenerationDigestV1,
    },
    RetiredReplay(GraphPublicationReplayTombstoneV1),
    MissingReplay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphReplayRetirementOutcomeV1 {
    Retired(GraphPublicationReplayTombstoneV1),
    ExactReplay(GraphPublicationReplayTombstoneV1),
    CurrentVerifiedHead {
        head: GraphVerifiedHeadV1,
    },
    PendingReplay {
        pending: GraphPublicationReplayRecordV1,
    },
    Conflict,
    Missing,
}

#[cfg(test)]
#[path = "graph_publication/tests.rs"]
mod tests;
