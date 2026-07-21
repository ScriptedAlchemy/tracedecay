use std::path::Path;

use crate::errors::Result;

use super::GraphStoreCompat;

impl GraphStoreCompat<'_> {
    pub(crate) async fn checkpoint(&self) -> Result<()> {
        self.database.checkpoint().await
    }

    pub(crate) async fn snapshot_to(&self, destination: &Path) -> Result<()> {
        self.database.snapshot_to(destination).await
    }

    pub(crate) async fn quick_check(&self) -> Result<bool> {
        self.database.quick_check().await
    }

    pub(crate) async fn rebuild_fts(&self) -> Result<()> {
        self.database.rebuild_fts().await
    }
}
