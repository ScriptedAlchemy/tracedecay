//! Epoch-scoped ordered adjacency identities for cursor paging.
//!
//! The first page of a start/kind/direction triple walks the frontier once,
//! decodes identities only, and retains the sorted list. Later pages are a
//! cursor seek plus a `limit`-sized copy.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::{GraphDbError, GraphEntityId, GraphNamespace, GraphRelationId, GraphRelationKind};

const MAX_CACHED_ADJACENCY_INDEXES: usize = 32;
/// Identity-count cap. Entry count alone is not a memory bound: 32 cached
/// hubs of 100k ids would pin millions of relation identities for the rest
/// of the epoch. Evict the map when an insert would push the retained
/// identity total past this ceiling.
const MAX_CACHED_ADJACENCY_IDS: usize = 1_000_000;

#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct AdjacencyIndexKey {
    pub(crate) namespace: String,
    pub(crate) start: String,
    pub(crate) incoming: bool,
    pub(crate) kinds: Box<[String]>,
}

impl AdjacencyIndexKey {
    pub(crate) fn new(
        namespace: &GraphNamespace,
        start: &GraphEntityId,
        incoming: bool,
        kinds: &std::collections::BTreeSet<GraphRelationKind>,
    ) -> Self {
        Self {
            namespace: namespace.as_str().to_owned(),
            start: start.as_str().to_owned(),
            incoming,
            kinds: kinds
                .iter()
                .map(|kind| kind.as_str().to_owned())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }
}

#[derive(Default)]
pub(crate) struct AdjacencyIdIndexCache {
    epoch: AtomicU64,
    entries: RwLock<AdjacencyEntries>,
}

#[derive(Default)]
struct AdjacencyEntries {
    epoch: u64,
    indexes: HashMap<AdjacencyIndexKey, Arc<[GraphRelationId]>>,
    cached_ids: usize,
}

impl AdjacencyIdIndexCache {
    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn get(
        &self,
        key: &AdjacencyIndexKey,
    ) -> Result<Option<Arc<[GraphRelationId]>>, GraphDbError> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let entries = self
            .entries
            .read()
            .map_err(|_| GraphDbError::unavailable("graph adjacency id cache is poisoned"))?;
        if entries.epoch != epoch {
            return Ok(None);
        }
        Ok(entries.indexes.get(key).map(Arc::clone))
    }

    pub(crate) fn insert(
        &self,
        key: AdjacencyIndexKey,
        ids: Arc<[GraphRelationId]>,
    ) -> Result<Arc<[GraphRelationId]>, GraphDbError> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let mut entries = self
            .entries
            .write()
            .map_err(|_| GraphDbError::unavailable("graph adjacency id cache is poisoned"))?;
        if entries.epoch != epoch {
            entries.indexes.clear();
            entries.cached_ids = 0;
            entries.epoch = epoch;
        }
        let incoming_ids = ids.len();
        if entries.indexes.len() >= MAX_CACHED_ADJACENCY_INDEXES
            || entries.cached_ids.saturating_add(incoming_ids) > MAX_CACHED_ADJACENCY_IDS
        {
            entries.indexes.clear();
            entries.cached_ids = 0;
        }
        if incoming_ids <= MAX_CACHED_ADJACENCY_IDS {
            if let Some(previous) = entries.indexes.insert(key, Arc::clone(&ids)) {
                entries.cached_ids = entries.cached_ids.saturating_sub(previous.len());
            }
            entries.cached_ids = entries.cached_ids.saturating_add(incoming_ids);
        }
        Ok(ids)
    }
}

pub(crate) fn page_ids(
    ids: &[GraphRelationId],
    after: Option<&GraphRelationId>,
    limit: usize,
) -> Vec<GraphRelationId> {
    let start = match after {
        Some(cursor) => ids.partition_point(|id| id <= cursor),
        None => 0,
    };
    ids[start..].iter().take(limit).cloned().collect()
}
