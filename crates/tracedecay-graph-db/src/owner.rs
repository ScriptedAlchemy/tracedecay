use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::location::PersistentGraphStoreState;
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use crate::{GraphCancellation, GraphDbLocation, GraphDurability, GraphFormatVersion};
use crate::{GraphDb, GraphDbError, GraphDbOpenOptions, GraphDbRuntimeState, GraphSnapshot};

/// Opaque client authority for one registered graph runtime.
///
/// An independently issued lease contributes one client to the owning runtime.
/// Clones share its drop token, so they remain one client until the last clone
/// is dropped. The raw `Arc<GraphDb>` remains private to the token.
#[derive(Clone)]
pub struct GraphDbLeaseV1 {
    token: Arc<GraphDbLeaseToken>,
}

struct GraphDbLeaseToken {
    database: Arc<GraphDb>,
    clients: Arc<AtomicUsize>,
}

impl Drop for GraphDbLeaseToken {
    fn drop(&mut self) {
        self.clients.fetch_sub(1, Ordering::AcqRel);
    }
}

impl std::fmt::Debug for GraphDbLeaseV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDbLeaseV1")
            .field("state", &self.runtime_state())
            .finish_non_exhaustive()
    }
}

impl Deref for GraphDbLeaseV1 {
    type Target = GraphDb;

    fn deref(&self) -> &Self::Target {
        &self.token.database
    }
}

impl GraphDbLeaseV1 {
    /// Opens a zero-copy read snapshot without exposing shared raw ownership.
    pub fn snapshot(&self) -> Result<GraphSnapshot, GraphDbError> {
        let mut snapshot = self.token.database.snapshot()?;
        snapshot.retain_client(self.clone());
        Ok(snapshot)
    }
}

pub struct GraphDbOwner {
    database: Arc<GraphDb>,
    clients: Arc<AtomicUsize>,
}

impl std::fmt::Debug for GraphDbOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDbOwner")
            .field("state", &self.runtime_state())
            .finish_non_exhaustive()
    }
}

impl GraphDbOwner {
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn memory(cancellation: Arc<dyn GraphCancellation>) -> Result<Self, GraphDbError> {
        Self::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation,
        })
    }

    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn open(options: GraphDbOpenOptions) -> Result<Self, GraphDbError> {
        GraphDb::open(options).map(Self::from_database)
    }

    pub(crate) fn open_registered(
        options: GraphDbOpenOptions,
        persistent_store_state: PersistentGraphStoreState,
    ) -> Result<Self, GraphDbError> {
        GraphDb::open_with_store_state(options, Some(persistent_store_state))
            .map(Self::from_database)
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn lease(&self) -> GraphDbLeaseV1 {
        self.issue_lease()
    }

    #[cfg(not(any(feature = "test-helpers", feature = "eval-helpers")))]
    pub(crate) fn lease(&self) -> GraphDbLeaseV1 {
        self.issue_lease()
    }

    #[must_use]
    pub fn runtime_state(&self) -> GraphDbRuntimeState {
        self.database.runtime_state()
    }

    pub fn close(&self) -> Result<(), GraphDbError> {
        self.database.close()
    }

    pub(crate) fn is_unleased(&self) -> bool {
        self.clients.load(Ordering::Acquire) == 0
    }

    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn is_closed(&self) -> bool {
        self.database.inner.closed.load(Ordering::Acquire)
    }

    fn from_database(database: Arc<GraphDb>) -> Self {
        Self {
            database,
            clients: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn issue_lease(&self) -> GraphDbLeaseV1 {
        self.clients.fetch_add(1, Ordering::AcqRel);
        GraphDbLeaseV1 {
            token: Arc::new(GraphDbLeaseToken {
                database: Arc::clone(&self.database),
                clients: Arc::clone(&self.clients),
            }),
        }
    }
}
