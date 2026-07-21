mod maintenance;
mod open;
mod read;
mod write;

use super::Database;

/// Existing graph-store open behavior exposed without reimplementing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphStoreOpenMode {
    Initialize,
    Open,
    ReadOnly,
}

/// Borrowed compatibility boundary for existing graph-store operations.
pub(crate) struct GraphStoreCompat<'db> {
    database: &'db Database,
}

impl<'db> GraphStoreCompat<'db> {
    pub(crate) const fn new(database: &'db Database) -> Self {
        Self { database }
    }
}
