//! Epoch-scoped caches for label expansion and projection quarantine approval.
//!
//! Both reuse the same invalidation choke point as
//! [`crate::projection_identity_index::IdentityIndexCache`]: every site that
//! takes the database write lock bumps the epoch, so a cached hit is valid
//! only for the exact store generation it was built against.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use grafeo_core::graph::GraphStore;

use crate::schema::label_keys;
use crate::{GraphDbError, GraphNamespace, GraphProjectionId};

#[derive(Default)]
pub(crate) struct LabelKeyCache {
    epoch: AtomicU64,
    entries: RwLock<LabelKeyEntries>,
}

#[derive(Default)]
struct LabelKeyEntries {
    epoch: u64,
    keys: HashMap<String, Arc<[String]>>,
}

impl LabelKeyCache {
    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// The store keys that carry `label` at the current epoch, shared with
    /// the cache entry rather than copied out of it.
    ///
    /// The returned slice is immutable and stays valid for the caller that
    /// holds it, but it authorizes nothing: after a write bumps the epoch,
    /// the next lookup recomputes from the store while an in-flight read may
    /// finish with its old list.
    pub(crate) fn keys(
        &self,
        store: &dyn GraphStore,
        label: &str,
    ) -> Result<Arc<[String]>, GraphDbError> {
        let epoch = self.epoch.load(Ordering::Acquire);
        if let Some(keys) = self.cached(label, epoch)? {
            return Ok(keys);
        }
        let stored = Arc::<[String]>::from(label_keys(store, label));
        let mut entries = self
            .entries
            .write()
            .map_err(|_| GraphDbError::unavailable("graph label key cache is poisoned"))?;
        if entries.epoch != epoch {
            entries.keys.clear();
            entries.epoch = epoch;
        }
        entries.keys.insert(label.to_owned(), Arc::clone(&stored));
        Ok(stored)
    }

    fn cached(&self, label: &str, epoch: u64) -> Result<Option<Arc<[String]>>, GraphDbError> {
        let entries = self
            .entries
            .read()
            .map_err(|_| GraphDbError::unavailable("graph label key cache is poisoned"))?;
        if entries.epoch != epoch {
            return Ok(None);
        }
        Ok(entries.keys.get(label).map(Arc::clone))
    }
}

#[cfg(test)]
mod label_key_cache_tests {
    use grafeo_core::graph::lpg::LpgStore;

    use super::*;

    #[test]
    fn hits_share_one_list_and_invalidation_recomputes_without_revoking_it() {
        let store = LpgStore::new().expect("in-memory store");
        store.create_node(&["Entity"]);
        let cache = LabelKeyCache::default();

        let first = cache.keys(&store, "Entity").expect("first lookup");
        let second = cache.keys(&store, "Entity").expect("cached lookup");
        assert!(
            Arc::ptr_eq(&first, &second),
            "a hit shares the cached slice"
        );
        assert_eq!(&*first, ["Entity".to_owned()]);

        let missing = cache.keys(&store, "Absent").expect("missing label");
        assert_eq!(
            &*missing,
            ["Absent".to_owned()],
            "a label the store lacks still resolves to itself"
        );

        cache.invalidate();
        store.create_node(&["Entity|Fresh"]);
        let refreshed = cache.keys(&store, "Fresh").expect("post-write lookup");
        assert_eq!(&*refreshed, ["Entity|Fresh".to_owned()]);
        let after_write = cache.keys(&store, "Entity").expect("recomputed lookup");
        assert!(
            !Arc::ptr_eq(&first, &after_write),
            "a bumped epoch recomputes rather than serving the old list"
        );
        assert_eq!(
            &*first,
            ["Entity".to_owned()],
            "the admitted old list is intact"
        );
    }
}

#[derive(Default)]
pub(crate) struct ProjectionApprovalCache {
    epoch: AtomicU64,
    entries: RwLock<ApprovalEntries>,
}

#[derive(Default)]
struct ApprovalEntries {
    epoch: u64,
    approved: BTreeSet<(GraphNamespace, GraphProjectionId)>,
}

impl ProjectionApprovalCache {
    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn approve(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        check: impl FnOnce() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let key = (namespace.clone(), projection.clone());
        {
            let entries = self.entries.read().map_err(|_| {
                GraphDbError::unavailable("graph projection approval cache is poisoned")
            })?;
            if entries.epoch == epoch && entries.approved.contains(&key) {
                return Ok(());
            }
        }
        check()?;
        let mut entries = self.entries.write().map_err(|_| {
            GraphDbError::unavailable("graph projection approval cache is poisoned")
        })?;
        if entries.epoch != epoch {
            entries.approved.clear();
            entries.epoch = epoch;
        }
        entries.approved.insert(key);
        Ok(())
    }
}
