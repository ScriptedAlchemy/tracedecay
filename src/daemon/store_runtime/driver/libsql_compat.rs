use std::path::Path;

use crate::db::{Database, DatabaseAuthority};
use crate::errors::Result;

/// Explicit graph-store open behavior delegated to the existing libsql implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphStoreOpenMode {
    Initialize,
    Open,
    ReadOnly,
}

/// Daemon-owned compatibility driver for the existing graph database.
///
/// This is the single S11 seam for the graph engine swap. Every production
/// graph open (`initialize`/`open`/`open_read_only`) is funneled through here
/// rather than calling `Database::{initialize,open,open_read_only}` directly,
/// so the eventual rusqlite runtime registry has exactly one call site to
/// replace when it takes ownership of graph opens. The only permitted direct
/// graph opens outside this driver are one-shot migration inspection
/// (`crate::migrate`) and `#[cfg(test)]` fixtures, both enforced by the
/// `tests/storage_runtime_open_boundary.rs` allowlist gate.
pub(crate) struct GraphLibsqlCompatDriver;

impl GraphLibsqlCompatDriver {
    /// Opens a graph database through its matching existing entry point.
    pub(crate) async fn open(
        mode: GraphStoreOpenMode,
        db_path: &Path,
        authority: &DatabaseAuthority,
    ) -> Result<(Database, bool)> {
        match mode {
            GraphStoreOpenMode::Initialize => Database::initialize(db_path, authority).await,
            GraphStoreOpenMode::Open => Database::open(db_path, authority).await,
            GraphStoreOpenMode::ReadOnly => Database::open_read_only(db_path, authority).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::DatabaseAuthority;
    use crate::errors::TraceDecayError;

    use super::{GraphLibsqlCompatDriver, GraphStoreOpenMode};

    #[tokio::test]
    async fn explicit_open_modes_preserve_migrated_flags() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("graph.db");
        let authority =
            DatabaseAuthority::acquire_test(&db_path, "graph compat driver modes").unwrap();

        let (_, initialized_migrated) =
            GraphLibsqlCompatDriver::open(GraphStoreOpenMode::Initialize, &db_path, &authority)
                .await
                .unwrap();
        assert!(!initialized_migrated);

        let (_, opened_migrated) =
            GraphLibsqlCompatDriver::open(GraphStoreOpenMode::Open, &db_path, &authority)
                .await
                .unwrap();
        assert!(!opened_migrated);

        let (_, read_only_migrated) =
            GraphLibsqlCompatDriver::open(GraphStoreOpenMode::ReadOnly, &db_path, &authority)
                .await
                .unwrap();
        assert!(!read_only_migrated);
    }

    #[tokio::test]
    async fn authority_failures_are_not_translated() {
        let temp = tempfile::tempdir().unwrap();
        let authorized_path = temp.path().join("authorized.db");
        let other_path = temp.path().join("other.db");
        let authority =
            DatabaseAuthority::acquire_test(&authorized_path, "graph compat driver authority")
                .unwrap();

        for (mode, expected_operation) in [
            (GraphStoreOpenMode::Initialize, "initialize"),
            (GraphStoreOpenMode::Open, "open"),
            (GraphStoreOpenMode::ReadOnly, "open_read_only"),
        ] {
            let error = match GraphLibsqlCompatDriver::open(mode, &other_path, &authority).await {
                Ok(_) => panic!("mismatched authority must fail"),
                Err(error) => error,
            };
            let TraceDecayError::Database { message, operation } = error else {
                panic!("authority failure must retain the database error classification");
            };
            assert_eq!(operation, expected_operation);
            assert!(
                message.contains("database authority belongs to a different database"),
                "unexpected authority error: {message}"
            );
        }
    }
}
