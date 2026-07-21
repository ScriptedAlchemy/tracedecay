use std::path::Path;

use crate::db::{Database, DatabaseAuthority};
use crate::errors::Result;

use super::GraphStoreOpenMode;

impl GraphStoreOpenMode {
    /// Opens through the matching existing graph-store entry point.
    pub(crate) async fn open(
        self,
        db_path: &Path,
        authority: &DatabaseAuthority,
    ) -> Result<(Database, bool)> {
        match self {
            Self::Initialize => Database::initialize(db_path, authority).await,
            Self::Open => Database::open(db_path, authority).await,
            Self::ReadOnly => Database::open_read_only(db_path, authority).await,
        }
    }
}
