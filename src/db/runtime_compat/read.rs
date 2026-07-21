use crate::errors::Result;
use crate::types::{GraphStats, Node, SearchResult};

use super::GraphStoreCompat;

impl GraphStoreCompat<'_> {
    pub(crate) async fn get_stats(&self) -> Result<GraphStats> {
        self.database.get_stats().await
    }

    pub(crate) async fn get_node_by_id(&self, id: &str) -> Result<Option<Node>> {
        self.database.get_node_by_id(id).await
    }

    pub(crate) async fn search_nodes(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        self.database.search_nodes(query, limit).await
    }
}
