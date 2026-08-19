use serde::{Deserialize, Serialize};

use super::{
    GraphProjectionIdentityV1, GraphPublicationReplayCursorV1, GraphPublicationReplayTombstoneV1,
    MAX_GRAPH_REPLAY_PAGE_RECORDS_V1, MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1,
    StorageRuntimeContractErrorV1, validate_graph_publication_shard,
};

/// Bounded keyset request for retired publications whose native graph state
/// still needs deletion before their retained replay source can be finalized.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationRetiredCleanupPageRequestV1 {
    pub projection: GraphProjectionIdentityV1,
    pub after: Option<GraphPublicationReplayCursorV1>,
    pub max_records: u16,
}

impl GraphPublicationRetiredCleanupPageRequestV1 {
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
        validate_graph_publication_shard(
            &self.projection.shard_id,
            "graph retired replay cleanup page",
        )?;
        if self
            .after
            .as_ref()
            .is_some_and(|cursor| cursor.projection != self.projection)
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph retired cleanup cursor projection",
            });
        }
        if self.max_records == 0 {
            return Err(StorageRuntimeContractErrorV1::Zero {
                field: "graph retired cleanup page records",
            });
        }
        if self.max_records > MAX_GRAPH_REPLAY_PAGE_RECORDS_V1 {
            return Err(StorageRuntimeContractErrorV1::LimitExceeded {
                field: "graph retired cleanup page records",
                actual: u64::from(self.max_records),
                max: u64::from(MAX_GRAPH_REPLAY_PAGE_RECORDS_V1),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphPublicationRetiredCleanupPageV1 {
    pub records: Vec<GraphPublicationReplayTombstoneV1>,
    pub continuation: Option<GraphPublicationReplayCursorV1>,
}

impl GraphPublicationRetiredCleanupPageV1 {
    pub fn new(
        records: Vec<GraphPublicationReplayTombstoneV1>,
        continuation: Option<GraphPublicationReplayCursorV1>,
    ) -> Result<Self, StorageRuntimeContractErrorV1> {
        for record in &records {
            GraphPublicationReplayTombstoneV1::new(
                record.sequence,
                record.retirement(),
                record.canonical_replay_source.clone(),
            )?;
        }
        if records.len() > usize::from(MAX_GRAPH_REPLAY_PAGE_RECORDS_V1) {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "graph retired cleanup page records",
                actual: records.len(),
                max: usize::from(MAX_GRAPH_REPLAY_PAGE_RECORDS_V1),
            });
        }
        if records
            .iter()
            .any(|record| record.canonical_replay_source.is_none())
        {
            return Err(StorageRuntimeContractErrorV1::Empty {
                field: "graph retired cleanup source",
            });
        }
        let payload_bytes = records.iter().try_fold(0_usize, |total, record| {
            let source_bytes = record.canonical_replay_source.as_ref().map_or(0, Vec::len);
            let dependency_bytes = serde_json::to_vec(&record.direct_dependency_generations)
                .map_err(|_| StorageRuntimeContractErrorV1::NonCanonical {
                    field: "graph retired cleanup dependency encoding",
                })?
                .len();
            total
                .checked_add(source_bytes)
                .and_then(|value| value.checked_add(dependency_bytes))
                .ok_or(StorageRuntimeContractErrorV1::TooLong {
                    field: "graph retired cleanup page payload",
                    actual: usize::MAX,
                    max: MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1,
                })
        })?;
        if payload_bytes > MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1 {
            return Err(StorageRuntimeContractErrorV1::TooLong {
                field: "graph retired cleanup page payload",
                actual: payload_bytes,
                max: MAX_GRAPH_REPLAY_PAGE_SOURCE_BYTES_V1,
            });
        }
        if records
            .windows(2)
            .any(|window| window[0].sequence >= window[1].sequence)
        {
            return Err(StorageRuntimeContractErrorV1::NonCanonical {
                field: "graph retired cleanup sequence order",
            });
        }
        if records.first().is_some_and(|first| {
            records
                .iter()
                .any(|record| record.key.projection != first.key.projection)
        }) {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph retired cleanup page projection",
            });
        }
        if continuation.is_some() && records.is_empty() {
            return Err(StorageRuntimeContractErrorV1::Empty {
                field: "graph retired cleanup continuation records",
            });
        }
        if continuation
            .as_ref()
            .zip(records.last())
            .is_some_and(|(cursor, last)| {
                cursor.sequence != last.sequence || cursor.projection != last.key.projection
            })
        {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "graph retired cleanup continuation",
            });
        }
        Ok(Self {
            records,
            continuation,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphRetiredReplayCleanupFinalizeOutcomeV1 {
    Finalized(GraphPublicationReplayTombstoneV1),
    ExactReplay(GraphPublicationReplayTombstoneV1),
    Conflict,
    Missing,
}
