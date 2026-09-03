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

    pub(crate) fn keys(
        &self,
        store: &dyn GraphStore,
        label: &str,
    ) -> Result<Vec<String>, GraphDbError> {
        let epoch = self.epoch.load(Ordering::Acquire);
        if let Some(keys) = self.cached(label, epoch)? {
            return Ok(keys.to_vec());
        }
        let keys = label_keys(store, label);
        let stored = Arc::<[String]>::from(keys);
        let mut entries = self
            .entries
            .write()
            .map_err(|_| GraphDbError::unavailable("graph label key cache is poisoned"))?;
        if entries.epoch != epoch {
            entries.keys.clear();
            entries.epoch = epoch;
        }
        entries.keys.insert(label.to_owned(), Arc::clone(&stored));
        Ok(stored.to_vec())
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
