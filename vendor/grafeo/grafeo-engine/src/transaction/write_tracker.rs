//! Bridge between mutation operators and the transaction manager's write tracking.

use std::sync::Arc;

use grafeo_common::types::{EdgeId, NodeId, TransactionId};
use grafeo_core::execution::operators::{OperatorError, WriteTracker};

use super::TransactionManager;

/// Implements [`WriteTracker`] by forwarding to [`TransactionManager::record_write`].
///
/// Created by the planner when a transaction is active, and passed to each
/// mutation operator so it can record writes for conflict detection.
pub struct TransactionWriteTracker {
    manager: Arc<TransactionManager>,
}

impl TransactionWriteTracker {
    /// Creates a new write tracker backed by the given transaction manager.
    pub fn new(manager: Arc<TransactionManager>) -> Self {
        Self { manager }
    }
}

impl WriteTracker for TransactionWriteTracker {
    fn record_node_write(
        &self,
        transaction_id: TransactionId,
        node_id: NodeId,
    ) -> Result<(), OperatorError> {
        self.manager
            .record_write(transaction_id, node_id)
            .map_err(|e| OperatorError::WriteConflict(e.to_string()))
    }

    fn record_edge_write(
        &self,
        transaction_id: TransactionId,
        edge_id: EdgeId,
    ) -> Result<(), OperatorError> {
        self.manager
            .record_write(transaction_id, edge_id)
            .map_err(|e| OperatorError::WriteConflict(e.to_string()))
    }
}
