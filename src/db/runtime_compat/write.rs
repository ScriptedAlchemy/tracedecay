use crate::errors::Result;
use crate::types::{Edge, FileRecord, Node};

use super::GraphStoreCompat;

impl GraphStoreCompat<'_> {
    /// Preserves the existing node/edge/file transaction as one operation.
    pub(crate) async fn insert_all(
        &self,
        nodes: &[Node],
        edges: &[Edge],
        files: &[FileRecord],
    ) -> Result<()> {
        self.database.insert_all(nodes, edges, files).await
    }
}
