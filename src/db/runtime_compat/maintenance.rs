use crate::errors::Result;

use super::GraphStoreCompat;

impl GraphStoreCompat<'_> {
    pub(crate) async fn quick_check(&self) -> Result<bool> {
        self.database.quick_check().await
    }
}
