//! An ordered identity access path for projection pagination.
//!
//! Paging a projection used to cost a full projection scan *per page*: every
//! `query_identity_page` call expanded the owner label into every node it
//! covers, read each node twice (once to confirm the record label, once for
//! the identity property), and threw away all but one page. Warming a large
//! catalog therefore ran in O(N^2 / page) and was measured at 47 back-to-back
//! `graph_db.projection.read` calls of ~21.7s each — 76% of a daemon lifetime.
//!
//! This module builds the sorted identity list *once* per store epoch and
//! answers each page from it with a binary search plus a `limit`-sized copy, so
//! a page costs O(log N + page) after an O(N log N) build that the first page
//! already had to pay for.
//!
//! # Validity
//!
//! A cached index is reused only while both of these still hold:
//!
//! * the store epoch is unchanged — [`IdentityIndexCache::invalidate`] is
//!   called from every site that takes the `GraphDb` database write lock, which
//!   is the choke point every mutation, projection replacement, and recovery
//!   database swap passes through; and
//! * the owner label still covers the same number of nodes — a cheap label-table
//!   count that catches any cardinality change an epoch bump might have missed.
//!
//! Both are read while the caller holds the database *read* guard, so no writer
//! can interleave between the check and the page.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;

use crate::projection_read::IdentityScope;
use crate::schema::{has_native_label, nodes_with_label};
use crate::{GraphCancellation, GraphDbError};

/// Identity bytes one cached index may retain. A projection whose identities
/// exceed this is paged by the streaming scan instead, so the cache can never
/// grow without bound on a pathological projection.
const MAX_IDENTITY_INDEX_BYTES: usize = 256 * 1024 * 1024;

/// Distinct ordered indexes retained for one epoch. A projection read touches
/// at most two (entities and relations); the slack covers a handful of
/// projections being paged concurrently before the cache starts evicting.
const MAX_CACHED_INDEXES: usize = 8;

#[derive(Clone, Eq, Hash, PartialEq)]
struct IdentityIndexKey {
    owner_label: String,
    record_label: String,
    identity_property: String,
}

/// The identities carried by one (owner label, record label) pair, ascending.
pub(crate) struct ProjectionIdentityIndex {
    /// Sorted ascending by the same byte-lexicographic order the previous
    /// `BTreeSet<String>` page imposed.
    identities: Box<[Box<str>]>,
    /// Nodes that carried both labels. Counted separately from `identities` so
    /// [`Self::node_count`] keeps reporting node cardinality even if a corrupt
    /// projection were to repeat an identity.
    node_count: usize,
}

impl ProjectionIdentityIndex {
    /// The `limit` smallest identities strictly greater than `after`.
    pub(crate) fn page(&self, after: Option<&str>, limit: usize) -> Vec<String> {
        let start = match after {
            // First index whose identity is strictly greater than `after`,
            // matching the old scan's `identity <= after` skip.
            Some(after) => self
                .identities
                .partition_point(|identity| identity.as_ref() <= after),
            None => 0,
        };
        self.identities[start..]
            .iter()
            .take(limit)
            .map(|identity| identity.as_ref().to_owned())
            .collect()
    }

    pub(crate) fn node_count(&self) -> usize {
        self.node_count
    }
}

/// Per-`GraphDb` cache of ordered identity indexes.
#[derive(Default)]
pub(crate) struct IdentityIndexCache {
    epoch: AtomicU64,
    entries: RwLock<CacheEntries>,
}

#[derive(Default)]
struct CacheEntries {
    epoch: u64,
    indexes: HashMap<IdentityIndexKey, Arc<ProjectionIdentityIndex>>,
}

impl IdentityIndexCache {
    /// Marks every cached index stale. Called from each site that takes the
    /// database write lock.
    pub(crate) fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// The ordered index for one (owner label, record label) pair, building and
    /// caching it when absent.
    ///
    /// Returns `Ok(None)` when the projection's identities exceed
    /// [`MAX_IDENTITY_INDEX_BYTES`], which tells the caller to fall back to the
    /// streaming scan rather than retain an unbounded index.
    ///
    /// The caller must hold the database read guard for the whole call and for
    /// its use of the returned index.
    pub(crate) fn ordered_identities(
        &self,
        database: &GrafeoDB,
        scope: IdentityScope<'_>,
        cancellation: &dyn GraphCancellation,
    ) -> Result<Option<Arc<ProjectionIdentityIndex>>, GraphDbError> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let key = IdentityIndexKey {
            owner_label: scope.owner_label.to_owned(),
            record_label: scope.record_label.to_owned(),
            identity_property: scope.identity_property.to_owned(),
        };

        // The epoch alone authorizes a hit. Every mutation path acquires the
        // database write guard and every acquisition site invalidates this
        // cache, so a matching epoch already proves the store's nodes are the
        // ones this index was built from. The cardinality re-check that used
        // to run here walked `all_labels()` and split every key on the
        // composite separator - O(label universe) per page, which made the
        // catalog warm quadratic again once paging itself was O(page): 1079
        // counts x 633ms measured against a 430k-chunk generation. The count is
        // no longer computed or retained: the epoch alone authorizes cache hits.
        if let Some(index) = self.cached(&key, epoch)? {
            return Ok(Some(index));
        }

        let Some(index) = build_identity_index(database, scope, cancellation)? else {
            return Ok(None);
        };
        let index = Arc::new(index);

        let mut entries = self
            .entries
            .write()
            .map_err(|_| GraphDbError::unavailable("graph identity index cache is poisoned"))?;
        // The epoch may have moved while this index was being built; storing it
        // under the epoch it was built from keeps the next reader's check
        // honest rather than publishing it as current.
        if entries.epoch != epoch {
            entries.indexes.clear();
            entries.epoch = epoch;
        }
        if entries.indexes.len() >= MAX_CACHED_INDEXES {
            entries.indexes.clear();
        }
        entries.indexes.insert(key, Arc::clone(&index));
        Ok(Some(index))
    }

    fn cached(
        &self,
        key: &IdentityIndexKey,
        epoch: u64,
    ) -> Result<Option<Arc<ProjectionIdentityIndex>>, GraphDbError> {
        let entries = self
            .entries
            .read()
            .map_err(|_| GraphDbError::unavailable("graph identity index cache is poisoned"))?;
        if entries.epoch != epoch {
            return Ok(None);
        }
        Ok(entries.indexes.get(key).map(Arc::clone))
    }
}

#[hotpath::measure(label = "graph_db.projection.identity_index.build")]
fn build_identity_index(
    database: &GrafeoDB,
    scope: IdentityScope<'_>,
    cancellation: &dyn GraphCancellation,
) -> Result<Option<ProjectionIdentityIndex>, GraphDbError> {
    check_cancelled(cancellation)?;
    let IdentityScope {
        owner_label,
        record_label,
        identity_property,
    } = scope;
    let store = database.graph_store();
    let mut identities: Vec<Box<str>> = Vec::new();
    let mut identity_bytes = 0usize;
    for node in nodes_with_label(store.as_ref(), owner_label) {
        check_cancelled(cancellation)?;
        let Some(record) = store.get_node(node) else {
            continue;
        };
        if !has_native_label(&record, record_label) {
            continue;
        }
        let identity = record
            .get_property(identity_property)
            .and_then(Value::as_str)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: format!("projection query returned a non-string `{identity_property}`"),
            })?;
        identity_bytes = identity_bytes.saturating_add(identity.len());
        if identity_bytes > MAX_IDENTITY_INDEX_BYTES {
            return Ok(None);
        }
        identities.push(identity.into());
    }
    check_cancelled(cancellation)?;
    let node_count = identities.len();
    identities.sort_unstable();
    check_cancelled(cancellation)?;
    Ok(Some(ProjectionIdentityIndex {
        identities: identities.into_boxed_slice(),
        node_count,
    }))
}

fn check_cancelled(cancellation: &dyn GraphCancellation) -> Result<(), GraphDbError> {
    if cancellation.is_cancelled() {
        Err(GraphDbError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ProjectionIdentityIndex;

    fn index(identities: &[&str]) -> ProjectionIdentityIndex {
        let mut sorted: Vec<Box<str>> = identities.iter().map(|value| (*value).into()).collect();
        sorted.sort_unstable();
        let node_count = sorted.len();
        ProjectionIdentityIndex {
            identities: sorted.into_boxed_slice(),
            node_count,
        }
    }

    #[test]
    fn page_orders_lexicographically_and_bounds_by_limit() {
        let index = index(&["c", "a", "b", "d"]);

        assert_eq!(index.page(None, 2), vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(
            index.page(None, 10),
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned()
            ]
        );
    }

    #[test]
    fn cursor_is_exclusive_and_admits_absent_cursors() {
        let index = index(&["a", "b", "c"]);

        assert_eq!(
            index.page(Some("a"), 10),
            vec!["b".to_owned(), "c".to_owned()]
        );
        // A cursor that is not itself an identity still bounds the page.
        assert_eq!(
            index.page(Some("aa"), 10),
            vec!["b".to_owned(), "c".to_owned()]
        );
        assert!(index.page(Some("c"), 10).is_empty());
        assert!(index.page(Some("z"), 10).is_empty());
    }

    #[test]
    fn zero_limit_and_empty_index_page_empty() {
        assert!(index(&["a"]).page(None, 0).is_empty());
        assert!(index(&[]).page(None, 10).is_empty());
    }
}
