use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::location::PersistentGraphStoreState;
#[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
use crate::{GraphCancellation, GraphDbLocation, GraphDurability, GraphFormatVersion};
use crate::{GraphDb, GraphDbError, GraphDbOpenOptions, GraphDbRuntimeState};

pub struct GraphDbOwner {
    database: Arc<GraphDb>,
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
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub fn memory(cancellation: Arc<dyn GraphCancellation>) -> Result<Self, GraphDbError> {
        Self::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation,
        })
    }

    pub(crate) fn open(options: GraphDbOpenOptions) -> Result<Self, GraphDbError> {
        GraphDb::open(options).map(|database| Self { database })
    }

    pub(crate) fn open_registered(
        options: GraphDbOpenOptions,
        persistent_store_state: PersistentGraphStoreState,
    ) -> Result<Self, GraphDbError> {
        GraphDb::open_with_store_state(options, Some(persistent_store_state))
            .map(|database| Self { database })
    }

    #[must_use]
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn handle(&self) -> Arc<GraphDb> {
        Arc::clone(&self.database)
    }

    #[must_use]
    #[cfg(not(any(feature = "test-helpers", feature = "eval-helpers")))]
    pub(crate) fn handle(&self) -> Arc<GraphDb> {
        Arc::clone(&self.database)
    }

    #[must_use]
    pub fn runtime_state(&self) -> GraphDbRuntimeState {
        self.database.runtime_state()
    }

    pub fn close(&self) -> Result<(), GraphDbError> {
        self.database.close()
    }

    pub(crate) fn is_unleased(&self) -> bool {
        Arc::strong_count(&self.database) == 1
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.database.inner.closed.load(Ordering::Acquire)
    }
}
