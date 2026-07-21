mod maintenance;
mod read;
mod write;

use super::Database;

/// Borrowed compatibility boundary for existing graph-store operations.
pub(crate) struct GraphStoreCompat<'db> {
    database: &'db Database,
}

impl<'db> GraphStoreCompat<'db> {
    pub(crate) const fn new(database: &'db Database) -> Self {
        Self { database }
    }
}
