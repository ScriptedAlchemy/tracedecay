//! Relational replay and verified-head authority for graph publications.
//!
//! This module stores no graph entities, relations, adjacency, or query index.
//! Production access is exclusively through [`GraphPublicationExactSqlStorage`]
//! over an already-authorized exact-SQL attachment.

use tracedecay_store::{
    GraphCanonicalReplaySourceDigestV1, GraphDependencyGenerationClosureDigestV1,
    GraphDependencyGenerationIdentityV1, GraphGenerationIdV1, GraphNamespaceV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
    GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationReplayRecordV1, GraphPublicationReplayRetirementV1,
    GraphPublicationReplayTombstoneV1, GraphPublicationReplayV1, GraphPublicationSequenceV1,
    GraphPublicationStoreErrorV1, GraphPublicationStoreResultV1, GraphRecoveredGenerationDigestV1,
    GraphVerifiedHeadV1, StoreShardIdV1,
};

#[path = "graph_publication/exact.rs"]
mod exact;
pub use exact::GraphPublicationExactSqlStorage;
pub(crate) use exact::append_replay_in_transaction;

pub const GRAPH_PUBLICATION_SCHEMA_V1: &str = include_str!("graph_publication_schema.sql");

pub(crate) fn authoritative_verified_head_in_transaction(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    projection: &GraphProjectionIdentityV1,
) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
    exact::authoritative_verified_head_in_transaction(transaction, projection)
}

pub(crate) fn active_replay_in_transaction(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    key: &GraphPublicationKeyV1,
) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
    exact::active_replay_in_transaction(transaction, key)
}

pub(crate) fn retire_replay_in_transaction(
    transaction: &crate::exact_sql::ExactSqlTransaction,
    request: &GraphPublicationReplayRetirementV1,
) -> GraphPublicationStoreResultV1<tracedecay_store::GraphReplayRetirementOutcomeV1> {
    exact::retire_replay_in_transaction(transaction, request)
}

#[derive(Clone)]
struct EncodedProjection {
    shard_id: String,
    namespace: String,
    projection: String,
}

impl EncodedProjection {
    fn new(identity: &GraphProjectionIdentityV1) -> GraphPublicationStoreResultV1<Self> {
        Ok(Self {
            shard_id: serde_json::to_string(&identity.shard_id)
                .map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)?,
            namespace: identity.namespace.as_str().to_owned(),
            projection: identity.projection.as_str().to_owned(),
        })
    }
}

struct RawReplay {
    sequence: i64,
    shard_id: String,
    namespace: String,
    projection: String,
    generation: String,
    idempotency_key: String,
    input_digest: String,
    dependency_generation_closure_digest: String,
    direct_dependency_bytes: i64,
    expected_prior_head: Option<String>,
    expected_recovered_digest: String,
    canonical_replay_source_digest: String,
    canonical_replay_source: Vec<u8>,
}

struct RawReplayTombstone {
    sequence: i64,
    shard_id: String,
    namespace: String,
    projection: String,
    generation: String,
    idempotency_key: String,
    input_digest: String,
    dependency_generation_closure_digest: String,
    direct_dependency_bytes: i64,
    expected_prior_head: Option<String>,
    expected_recovered_digest: String,
    canonical_replay_source_digest: String,
    canonical_replay_source: Option<Vec<u8>>,
}

struct RawVerifiedHead {
    sequence: i64,
    recovered_digest: String,
    shard_id: String,
    namespace: String,
    projection: String,
    generation: String,
    idempotency_key: String,
    input_digest: String,
    dependency_generation_closure_digest: String,
    expected_recovered_digest: String,
}

struct RawReplayMetadata {
    sequence: i64,
    shard_id: String,
    namespace: String,
    projection: String,
    generation: String,
    idempotency_key: String,
    input_digest: String,
    dependency_generation_closure_digest: String,
    expected_prior_head: Option<String>,
    expected_recovered_digest: String,
}

struct ReplayMetadata {
    sequence: GraphPublicationSequenceV1,
    key: GraphPublicationKeyV1,
    input_digest: GraphPublicationInputDigestV1,
    dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1,
    expected_prior_head: Option<GraphVerifiedHeadV1>,
    expected_recovered_digest: GraphRecoveredGenerationDigestV1,
}

impl ReplayMetadata {
    fn verified_head(
        &self,
        recovered_digest: GraphRecoveredGenerationDigestV1,
    ) -> GraphPublicationStoreResultV1<GraphVerifiedHeadV1> {
        if recovered_digest != self.expected_recovered_digest {
            return Err(GraphPublicationStoreErrorV1::Corrupt(
                "verified graph digest differs from replay metadata".to_owned(),
            ));
        }
        Ok(GraphVerifiedHeadV1 {
            sequence: self.sequence,
            key: self.key.clone(),
            input_digest: self.input_digest.clone(),
            dependency_generation_closure_digest: self.dependency_generation_closure_digest.clone(),
            recovered_digest,
        })
    }
}

fn decode_replay(
    raw: RawReplay,
    direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
) -> GraphPublicationStoreResultV1<GraphPublicationReplayRecordV1> {
    let encoded_dependency_bytes =
        encode_direct_dependency_generations(&direct_dependency_generations)?;
    if usize::try_from(raw.direct_dependency_bytes).ok() != Some(encoded_dependency_bytes.len()) {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay dependency byte accounting differs from normalized rows".to_owned(),
        ));
    }
    let canonical_replay_source_digest =
        GraphCanonicalReplaySourceDigestV1::new(raw.canonical_replay_source_digest)
            .map_err(corrupt)?;
    let expected_prior_head = raw
        .expected_prior_head
        .map(|value| serde_json::from_str(&value).map_err(corrupt))
        .transpose()?;
    let replay = GraphPublicationReplayV1::new(
        GraphPublicationKeyV1::new(
            GraphProjectionIdentityV1 {
                shard_id: serde_json::from_str::<StoreShardIdV1>(&raw.shard_id).map_err(corrupt)?,
                namespace: GraphNamespaceV1::new(raw.namespace).map_err(corrupt)?,
                projection: GraphProjectionIdV1::new(raw.projection).map_err(corrupt)?,
            },
            GraphGenerationIdV1::new(raw.generation).map_err(corrupt)?,
            GraphPublicationIdempotencyKeyV1::new(raw.idempotency_key).map_err(corrupt)?,
        ),
        GraphPublicationInputDigestV1::new(raw.input_digest).map_err(corrupt)?,
        GraphDependencyGenerationClosureDigestV1::new(raw.dependency_generation_closure_digest)
            .map_err(corrupt)?,
        direct_dependency_generations,
        expected_prior_head,
        GraphRecoveredGenerationDigestV1::new(raw.expected_recovered_digest).map_err(corrupt)?,
        raw.canonical_replay_source,
    )
    .map_err(corrupt)?;
    if replay.canonical_replay_source_digest != canonical_replay_source_digest {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph replay source digest does not match its stored source".to_owned(),
        ));
    }
    GraphPublicationReplayRecordV1::new(sequence_from_i64(raw.sequence)?, replay).map_err(corrupt)
}

fn decode_tombstone(
    raw: RawReplayTombstone,
    direct_dependency_generations: Vec<GraphDependencyGenerationIdentityV1>,
) -> GraphPublicationStoreResultV1<GraphPublicationReplayTombstoneV1> {
    let encoded_dependency_bytes =
        encode_direct_dependency_generations(&direct_dependency_generations)?;
    if usize::try_from(raw.direct_dependency_bytes).ok() != Some(encoded_dependency_bytes.len()) {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "graph tombstone dependency byte accounting differs from normalized rows".to_owned(),
        ));
    }
    GraphPublicationReplayTombstoneV1::new(
        sequence_from_i64(raw.sequence)?,
        GraphPublicationReplayRetirementV1::new(
            GraphPublicationKeyV1::new(
                GraphProjectionIdentityV1 {
                    shard_id: serde_json::from_str::<StoreShardIdV1>(&raw.shard_id)
                        .map_err(corrupt)?,
                    namespace: GraphNamespaceV1::new(raw.namespace).map_err(corrupt)?,
                    projection: GraphProjectionIdV1::new(raw.projection).map_err(corrupt)?,
                },
                GraphGenerationIdV1::new(raw.generation).map_err(corrupt)?,
                GraphPublicationIdempotencyKeyV1::new(raw.idempotency_key).map_err(corrupt)?,
            ),
            GraphPublicationInputDigestV1::new(raw.input_digest).map_err(corrupt)?,
            GraphDependencyGenerationClosureDigestV1::new(raw.dependency_generation_closure_digest)
                .map_err(corrupt)?,
            direct_dependency_generations,
            raw.expected_prior_head
                .map(|value| serde_json::from_str(&value).map_err(corrupt))
                .transpose()?,
            GraphRecoveredGenerationDigestV1::new(raw.expected_recovered_digest)
                .map_err(corrupt)?,
            GraphCanonicalReplaySourceDigestV1::new(raw.canonical_replay_source_digest)
                .map_err(corrupt)?,
        )
        .map_err(corrupt)?,
        raw.canonical_replay_source,
    )
    .map_err(corrupt)
}

fn decode_verified_head(
    raw: RawVerifiedHead,
) -> GraphPublicationStoreResultV1<GraphVerifiedHeadV1> {
    let recovered_digest =
        GraphRecoveredGenerationDigestV1::new(raw.recovered_digest).map_err(corrupt)?;
    let expected_recovered_digest =
        GraphRecoveredGenerationDigestV1::new(raw.expected_recovered_digest).map_err(corrupt)?;
    if recovered_digest != expected_recovered_digest {
        return Err(GraphPublicationStoreErrorV1::Corrupt(
            "verified graph head recovered digest differs from its replay".to_owned(),
        ));
    }
    Ok(GraphVerifiedHeadV1 {
        sequence: sequence_from_i64(raw.sequence)?,
        key: GraphPublicationKeyV1::new(
            GraphProjectionIdentityV1 {
                shard_id: serde_json::from_str::<StoreShardIdV1>(&raw.shard_id).map_err(corrupt)?,
                namespace: GraphNamespaceV1::new(raw.namespace).map_err(corrupt)?,
                projection: GraphProjectionIdV1::new(raw.projection).map_err(corrupt)?,
            },
            GraphGenerationIdV1::new(raw.generation).map_err(corrupt)?,
            GraphPublicationIdempotencyKeyV1::new(raw.idempotency_key).map_err(corrupt)?,
        ),
        input_digest: GraphPublicationInputDigestV1::new(raw.input_digest).map_err(corrupt)?,
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1::new(
            raw.dependency_generation_closure_digest,
        )
        .map_err(corrupt)?,
        recovered_digest,
    })
}

fn decode_replay_metadata(raw: RawReplayMetadata) -> GraphPublicationStoreResultV1<ReplayMetadata> {
    Ok(ReplayMetadata {
        sequence: sequence_from_i64(raw.sequence)?,
        key: GraphPublicationKeyV1::new(
            GraphProjectionIdentityV1 {
                shard_id: serde_json::from_str::<StoreShardIdV1>(&raw.shard_id).map_err(corrupt)?,
                namespace: GraphNamespaceV1::new(raw.namespace).map_err(corrupt)?,
                projection: GraphProjectionIdV1::new(raw.projection).map_err(corrupt)?,
            },
            GraphGenerationIdV1::new(raw.generation).map_err(corrupt)?,
            GraphPublicationIdempotencyKeyV1::new(raw.idempotency_key).map_err(corrupt)?,
        ),
        input_digest: GraphPublicationInputDigestV1::new(raw.input_digest).map_err(corrupt)?,
        dependency_generation_closure_digest: GraphDependencyGenerationClosureDigestV1::new(
            raw.dependency_generation_closure_digest,
        )
        .map_err(corrupt)?,
        expected_prior_head: raw
            .expected_prior_head
            .map(|head| serde_json::from_str(&head).map_err(corrupt))
            .transpose()?,
        expected_recovered_digest: GraphRecoveredGenerationDigestV1::new(
            raw.expected_recovered_digest,
        )
        .map_err(corrupt)?,
    })
}

fn encode_optional_head(
    head: Option<&GraphVerifiedHeadV1>,
) -> GraphPublicationStoreResultV1<Option<String>> {
    head.map(|head| {
        serde_json::to_string(head).map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
    })
    .transpose()
}

fn encode_direct_dependency_generations(
    dependencies: &[GraphDependencyGenerationIdentityV1],
) -> GraphPublicationStoreResultV1<Vec<u8>> {
    serde_json::to_vec(dependencies).map_err(|_| GraphPublicationStoreErrorV1::Infrastructure)
}

fn sequence_from_i64(value: i64) -> GraphPublicationStoreResultV1<GraphPublicationSequenceV1> {
    let value = u64::try_from(value).map_err(|_| {
        GraphPublicationStoreErrorV1::Corrupt("graph publication sequence is negative".to_owned())
    })?;
    GraphPublicationSequenceV1::new(value).map_err(corrupt)
}

fn sequence_to_i64(value: GraphPublicationSequenceV1) -> GraphPublicationStoreResultV1<i64> {
    i64::try_from(value.get()).map_err(|_| {
        GraphPublicationStoreErrorV1::Corrupt(
            "graph publication sequence exceeds SQLite integer range".to_owned(),
        )
    })
}

fn ensure_not_interrupted(
    context: &GraphPublicationOperationContextV1<'_>,
) -> GraphPublicationStoreResultV1<()> {
    context.interruption().map_or(Ok(()), |reason| {
        Err(GraphPublicationStoreErrorV1::Interrupted(reason))
    })
}

fn begin_verified_commit(
    context: &GraphPublicationOperationContextV1<'_>,
) -> GraphPublicationStoreResultV1<()> {
    if context.try_begin_verified_commit() {
        return Ok(());
    }
    context.interruption().map_or(
        Err(GraphPublicationStoreErrorV1::Infrastructure),
        |reason| Err(GraphPublicationStoreErrorV1::Interrupted(reason)),
    )
}

fn begin_replay_retirement_commit(
    context: &GraphPublicationOperationContextV1<'_>,
) -> GraphPublicationStoreResultV1<()> {
    if context.try_begin_replay_retirement_commit() {
        return Ok(());
    }
    context.interruption().map_or(
        Err(GraphPublicationStoreErrorV1::Infrastructure),
        |reason| Err(GraphPublicationStoreErrorV1::Interrupted(reason)),
    )
}

fn begin_retired_cleanup_finalize_commit(
    context: &GraphPublicationOperationContextV1<'_>,
) -> GraphPublicationStoreResultV1<()> {
    if context.try_begin_retired_cleanup_finalize_commit() {
        return Ok(());
    }
    context.interruption().map_or(
        Err(GraphPublicationStoreErrorV1::Infrastructure),
        |reason| Err(GraphPublicationStoreErrorV1::Interrupted(reason)),
    )
}

fn corrupt(error: impl std::fmt::Display) -> GraphPublicationStoreErrorV1 {
    GraphPublicationStoreErrorV1::Corrupt(error.to_string())
}

#[cfg(test)]
#[path = "graph_publication/tests.rs"]
mod tests;
