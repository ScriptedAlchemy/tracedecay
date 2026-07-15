// Rust guideline compliant 2025-10-17
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use libsql::{Builder, Connection, OpenFlags, Transaction, TransactionBehavior};

use crate::errors::{Result, TraceDecayError};

use super::{DatabaseAuthority, migrations};

mod integrity;
mod pragmas;
mod registry;

pub use pragmas::SQLITE_UNSAFE_FAST_ENV;
#[cfg(test)]
pub(crate) use pragmas::{adaptive_cache_sizes, platform_safe_mmap_size};
pub(crate) use pragmas::{platform_safe_journal_mode, platform_safe_synchronous_mode};
use registry::{DatabaseInner, database_slot};

/// `SQLite` database backing the code graph, powered by libsql.
#[derive(Clone)]
pub struct Database {
    inner: Arc<DatabaseInner>,
}

/// A writer connection that cannot outlive the canonical database's writer
/// lane. The connection is isolated from the retained query-only handle, so
/// cancellation cannot leave readers inside an unfinished transaction.
pub(crate) struct DatabaseWriterConnection<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    conn: Connection,
}

/// An immediate transaction that retains the canonical writer lane until the
/// transaction commits, rolls back, or is dropped.
pub(crate) struct DatabaseWriteTransaction<'a> {
    transaction: Transaction,
    guard: tokio::sync::MutexGuard<'a, ()>,
}

impl Deref for DatabaseWriteTransaction<'_> {
    type Target = Transaction;

    fn deref(&self) -> &Self::Target {
        &self.transaction
    }
}

impl DatabaseWriterConnection<'_> {
    pub(crate) async fn execute(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> libsql::Result<u64> {
        self.conn.execute(sql, params).await
    }

    #[cfg(test)]
    pub(crate) async fn execute_batch(&self, sql: &str) -> libsql::Result<libsql::BatchRows> {
        self.conn.execute_batch(sql).await
    }

    pub(crate) async fn query(
        &self,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> libsql::Result<libsql::Rows> {
        self.conn.query(sql, params).await
    }

    pub(crate) fn memory_store(&self) -> crate::memory::store::MemoryStore<'_> {
        crate::memory::store::MemoryStore::new(&self.conn)
    }

    pub(crate) fn fact_retriever(&self) -> crate::memory::retrieval::FactRetriever<'_> {
        crate::memory::retrieval::FactRetriever::new(&self.conn)
    }
}

impl DatabaseWriteTransaction<'_> {
    pub(crate) async fn commit(self) -> Result<()> {
        let Self { transaction, guard } = self;
        let transaction = transaction.commit().await;
        drop(guard);
        transaction.map_err(|error| TraceDecayError::Database {
            message: format!("failed to commit isolated writer transaction: {error}"),
            operation: "commit write transaction".to_string(),
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn rollback(self) -> Result<()> {
        let Self { transaction, guard } = self;
        let transaction = transaction.rollback().await;
        drop(guard);
        transaction.map_err(|error| TraceDecayError::Database {
            message: format!("failed to roll back isolated writer transaction: {error}"),
            operation: "rollback write transaction".to_string(),
        })
    }
}

impl Database {
    /// Creates a new database at `db_path`, creating parent directories if needed.
    ///
    /// An explicit [`DatabaseAuthority`] is required; opening writable storage
    /// without process authority is intentionally unsupported.
    ///
    /// Opens a libsql connection, applies performance pragmas, and runs all
    /// schema migrations up to the latest version.
    /// Returns `(Self, migrated)` where `migrated` is `true` if schema
    /// migrations were applied during initialization.
    pub async fn initialize(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "initialize")?;
        let exclusive_maintenance = authority.role() == super::DatabaseAuthorityRole::Maintenance;
        let slot = database_slot(authority.database_identity_key());
        let mut open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "initialize"));
            }
            return Ok((Self { inner }, false));
        }
        let is_fresh = std::fs::metadata(db_path).map_or(true, |metadata| metadata.len() == 0);
        if !is_fresh {
            integrity::validate_sqlite_header(db_path, "initialize", false)?;
            integrity::validate_read_only(db_path).await?;
        }
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Database {
                message: format!("failed to create database directory: {e}"),
                operation: "initialize".to_string(),
            })?;
        }

        let db =
            Builder::new_local(db_path)
                .build()
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to open database: {e}"),
                    operation: "initialize".to_string(),
                })?;

        let conn = db.connect().map_err(|e| TraceDecayError::Database {
            message: format!("failed to connect to database: {e}"),
            operation: "initialize".to_string(),
        })?;

        if is_fresh {
            pragmas::apply_fresh_storage(&conn).await?;
            migrations::configure_fresh_auto_vacuum(&conn, "initialize").await?;
        }
        let file_size = std::fs::metadata(db_path).map_or(0, |metadata| metadata.len());
        pragmas::apply(&conn, file_size).await?;
        let migrated = if is_fresh {
            migrations::create_schema(&conn).await?;
            false
        } else if exclusive_maintenance {
            migrations::migrate_with_exclusive_maintenance(&conn).await?
        } else {
            migrations::migrate(&conn).await?
        };
        conn.execute_batch("PRAGMA query_only = ON;")
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to seal retained database reader: {error}"),
                operation: "initialize".to_string(),
            })?;

        let inner = Arc::new(DatabaseInner {
            conn,
            db,
            writable: true,
            writer: tokio::sync::Mutex::new(()),
            _authority: authority,
            _slot: Some(slot.clone()),
        });
        *open = Arc::downgrade(&inner);
        Ok((Self { inner }, migrated))
    }

    /// Opens an existing database at `db_path`, applies performance pragmas,
    /// and runs any pending schema migrations.
    ///
    /// An explicit [`DatabaseAuthority`] is required; opening writable storage
    /// without process authority is intentionally unsupported.
    ///
    /// Returns `(Self, migrated)` where `migrated` is `true` if schema
    /// migrations were applied during open.
    pub async fn open(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open")?;
        let exclusive_maintenance = authority.role() == super::DatabaseAuthorityRole::Maintenance;
        let slot = database_slot(authority.database_identity_key());
        let mut open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "open"));
            }
            return Ok((Self { inner }, false));
        }
        let is_fresh = std::fs::metadata(db_path).map_or(true, |metadata| metadata.len() == 0);
        integrity::validate_sqlite_header(db_path, "open", true)?;
        if !is_fresh {
            integrity::validate_read_only(db_path).await?;
        }
        let db =
            Builder::new_local(db_path)
                .build()
                .await
                .map_err(|e| TraceDecayError::Database {
                    message: format!("failed to open database: {e}"),
                    operation: "open".to_string(),
                })?;

        let conn = db.connect().map_err(|e| TraceDecayError::Database {
            message: format!("failed to connect to database: {e}"),
            operation: "open".to_string(),
        })?;

        let file_size = std::fs::metadata(db_path).map_or(0, |m| m.len());
        if is_fresh {
            pragmas::apply_fresh_storage(&conn).await?;
        }
        pragmas::apply(&conn, file_size).await?;
        let migrated = if exclusive_maintenance {
            migrations::migrate_with_exclusive_maintenance(&conn).await?
        } else {
            migrations::migrate(&conn).await?
        };
        conn.execute_batch("PRAGMA query_only = ON;")
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to seal retained database reader: {error}"),
                operation: "open".to_string(),
            })?;

        let inner = Arc::new(DatabaseInner {
            conn,
            db,
            writable: true,
            writer: tokio::sync::Mutex::new(()),
            _authority: authority,
            _slot: Some(slot.clone()),
        });
        *open = Arc::downgrade(&inner);
        Ok((Self { inner }, migrated))
    }

    /// Opens an existing database in read-only mode.
    ///
    /// This intentionally skips write-oriented PRAGMAs and migrations so
    /// status/verification paths can inspect read-only `SQLite` files without
    /// creating WAL files or attempting schema updates.
    /// An explicit [`DatabaseAuthority`] is still required so every local
    /// database handle participates in the same process-ownership contract.
    pub async fn open_read_only(
        db_path: &Path,
        authority: &DatabaseAuthority,
    ) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open_read_only")?;
        integrity::validate_sqlite_header(db_path, "open_read_only", false)?;
        let db = Builder::new_local(db_path)
            .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
            .build()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to open database read-only: {e}"),
                operation: "open_read_only".to_string(),
            })?;

        let conn = db.connect().map_err(|e| TraceDecayError::Database {
            message: format!("failed to connect to database read-only: {e}"),
            operation: "open_read_only".to_string(),
        })?;

        let file_size = std::fs::metadata(db_path).map_or(0, |m| m.len());
        pragmas::apply_read_only(&conn, file_size).await?;
        integrity::validate(&conn, "open_read_only").await?;

        let inner = Arc::new(DatabaseInner {
            conn,
            db,
            writable: false,
            writer: tokio::sync::Mutex::new(()),
            _authority: authority,
            _slot: None,
        });
        Ok((Self { inner }, false))
    }

    /// Returns the retained query-only libsql connection.
    ///
    /// Mutations must use [`Self::writer_connection`] or an isolated
    /// transaction while holding [`Self::writer`].
    pub fn conn(&self) -> &Connection {
        &self.inner.conn
    }

    /// Executes one autocommit-style mutation through the canonical writer
    /// broker. Primarily useful for fixtures and maintenance adapters that do
    /// not need to retain a raw writable connection.
    #[doc(hidden)]
    pub async fn execute_write(
        &self,
        operation: &str,
        sql: &str,
        params: impl libsql::params::IntoParams,
    ) -> Result<u64> {
        let transaction = self.begin_write_transaction(operation).await?;
        let changed =
            transaction
                .execute(sql, params)
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to execute brokered write: {error}"),
                    operation: operation.to_string(),
                })?;
        transaction.commit().await?;
        Ok(changed)
    }

    /// Executes a SQL batch atomically through the canonical writer broker.
    #[doc(hidden)]
    pub async fn execute_write_batch(&self, operation: &str, sql: &str) -> Result<()> {
        let transaction = self.begin_write_transaction(operation).await?;
        transaction
            .execute_batch(sql)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to execute brokered write batch: {error}"),
                operation: operation.to_string(),
            })?;
        transaction.commit().await
    }

    /// Acquires the process-local writer lane for this canonical database.
    ///
    /// Writable handles opened for the same database share one `DatabaseInner`,
    /// so this guard coordinates MCP, dashboard, and automation mutations.
    pub(crate) async fn writer(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.writer.lock().await
    }

    pub(super) async fn open_writer_connection_unguarded(
        &self,
        operation: &str,
    ) -> Result<Connection> {
        let conn = self
            .inner
            .db
            .connect()
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to open isolated writer connection: {error}"),
                operation: operation.to_string(),
            })?;
        pragmas::apply(&conn, 0).await?;
        Ok(conn)
    }

    /// Opens an isolated writer while holding the process-local writer lane.
    /// The handle cannot escape the guard, preventing raw DML from bypassing
    /// serialization or joining a transaction on the retained reader.
    pub(crate) async fn writer_connection(
        &self,
        operation: &str,
    ) -> Result<DatabaseWriterConnection<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        Ok(DatabaseWriterConnection {
            _guard: guard,
            conn,
        })
    }

    /// Starts a query-only snapshot on a separate connection that cannot join
    /// a transaction running on the retained writable connection.
    pub(crate) async fn begin_isolated_read_snapshot(
        &self,
        operation: &str,
    ) -> Result<Transaction> {
        let conn = self
            .inner
            .db
            .connect()
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to open isolated read connection: {error}"),
                operation: operation.to_string(),
            })?;
        conn.execute_batch("PRAGMA query_only = ON; PRAGMA busy_timeout = 5000;")
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to configure isolated read connection: {error}"),
                operation: operation.to_string(),
            })?;
        conn.transaction()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin isolated read snapshot: {error}"),
                operation: operation.to_string(),
            })
    }

    /// Starts an immediate transaction that owns the canonical writer lane.
    /// Dropping the returned capability rolls back before releasing the lane.
    pub(crate) async fn begin_write_transaction(
        &self,
        operation: &str,
    ) -> Result<DatabaseWriteTransaction<'_>> {
        let guard = self.writer().await;
        let conn = self.open_writer_connection_unguarded(operation).await?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin isolated writer transaction: {error}"),
                operation: operation.to_string(),
            })?;
        Ok(DatabaseWriteTransaction { transaction, guard })
    }

    /// Releases this database handle.
    ///
    /// The underlying connection remains open until all cloned handles are
    /// released.
    pub fn close(self) {
        drop(self);
    }

    /// Checkpoints the WAL back into the main database file.
    ///
    /// This ensures all committed transactions are merged into the main DB
    /// before the process exits, preventing a stale WAL file on next startup.
    pub async fn checkpoint(&self) -> Result<()> {
        let _writer = self.writer().await;
        self.checkpoint_unguarded().await
    }

    pub(crate) async fn checkpoint_unguarded(&self) -> Result<()> {
        let conn = self.open_writer_connection_unguarded("checkpoint").await?;
        let mut rows = conn
            .query("PRAGMA wal_checkpoint(TRUNCATE);", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to checkpoint WAL: {e}"),
                operation: "checkpoint".to_string(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to read WAL checkpoint status: {e}"),
                operation: "checkpoint".to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                message: "WAL checkpoint returned no status row".to_string(),
                operation: "checkpoint".to_string(),
            })?;
        let busy: i64 = row.get(0).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read WAL checkpoint busy status: {e}"),
            operation: "checkpoint".to_string(),
        })?;
        let log_frames: i64 = row.get(1).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read WAL checkpoint frame count: {e}"),
            operation: "checkpoint".to_string(),
        })?;
        let checkpointed_frames: i64 = row.get(2).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read WAL checkpoint completion count: {e}"),
            operation: "checkpoint".to_string(),
        })?;
        if busy != 0 || checkpointed_frames < log_frames {
            return Err(TraceDecayError::Database {
                message: format!(
                    "WAL checkpoint incomplete: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
                ),
                operation: "checkpoint".to_string(),
            });
        }
        Ok(())
    }

    /// Writes a transactionally consistent copy of this database.
    ///
    /// `VACUUM INTO` reads from one `SQLite` snapshot, so concurrent WAL
    /// checkpoints cannot leave the destination with a partially copied
    /// B-tree. The destination must not already exist.
    pub async fn snapshot_to(&self, destination: &Path) -> Result<()> {
        let _writer = self.writer().await;
        self.snapshot_to_unguarded(destination).await
    }

    pub(crate) async fn snapshot_to_unguarded(&self, destination: &Path) -> Result<()> {
        let destination = destination
            .to_str()
            .ok_or_else(|| TraceDecayError::Database {
                message: format!(
                    "snapshot destination is not valid UTF-8: '{}'",
                    destination.display()
                ),
                operation: "snapshot".to_string(),
            })?;
        let connection = self
            .inner
            .db
            .connect()
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to open database snapshot connection: {e}"),
                operation: "snapshot".to_string(),
            })?;
        connection
            .execute_batch("PRAGMA busy_timeout = 120000;")
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to configure database snapshot connection: {e}"),
                operation: "snapshot".to_string(),
            })?;
        connection
            .execute("VACUUM INTO ?1", libsql::params![destination])
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to create consistent database snapshot: {e}"),
                operation: "snapshot".to_string(),
            })?;
        Ok(())
    }

    /// Runs VACUUM and ANALYZE to reclaim space and update query planner statistics.
    /// Returns the on-disk size of the database file in bytes.
    pub async fn size(&self) -> Result<u64> {
        let mut rows = self
            .inner
            .conn
            .query(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                (),
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to get database size: {e}"),
                operation: "size".to_string(),
            })?;

        let row = rows
            .next()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to read database size row: {e}"),
                operation: "size".to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                message: "no result from page size query".to_string(),
                operation: "size".to_string(),
            })?;

        let size = row.get::<i64>(0).map_err(|e| TraceDecayError::Database {
            message: format!("failed to read size value: {e}"),
            operation: "size".to_string(),
        })?;

        Ok(size as u64)
    }

    /// Runs `PRAGMA quick_check` and returns `true` if the database is intact.
    ///
    /// This is faster than `integrity_check` — it verifies B-tree structure
    /// without cross-checking index contents against table data.
    pub async fn quick_check(&self) -> Result<bool> {
        Ok(integrity::quick_check_result(
            &self.inner.conn,
            "quick_check",
            "failed to run quick_check",
        )
        .await?
        .is_some_and(|result| result == "ok"))
    }

    /// Maintenance-only: rebuilds the FTS5 index from the content table.
    ///
    /// This fixes FTS-only corruption (e.g. from an interrupted bulk load)
    /// without requiring a full re-index of the codebase. Callers must hold
    /// exclusive maintenance ownership; read paths must never invoke this.
    pub async fn rebuild_fts(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("rebuild_fts").await?;
        self.rebuild_fts_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub(crate) async fn rebuild_fts_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction
            .execute("INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild')", ())
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to rebuild FTS index: {e}"),
                operation: "rebuild_fts".to_string(),
            })?;
        Ok(())
    }

    /// Drops secondary indexes, disables fsync/FK, and clears FTS for fast
    /// bulk loading. Callers should insert data sorted by PK so the primary
    /// B-tree gets sequential appends. Call `end_bulk_load` afterwards to
    /// rebuild indexes in one optimized pass.
    pub async fn begin_bulk_load(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("begin_bulk_load").await?;
        self.begin_bulk_load_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub(crate) async fn begin_bulk_load_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction
            .execute_batch(
                "DROP INDEX IF EXISTS idx_nodes_kind;
             DROP INDEX IF EXISTS idx_nodes_name;
             DROP INDEX IF EXISTS idx_nodes_qualified_name;
             DROP INDEX IF EXISTS idx_nodes_file_path;
             DROP INDEX IF EXISTS idx_nodes_file_path_start_line;
             DROP INDEX IF EXISTS idx_edges_source;
             DROP INDEX IF EXISTS idx_edges_target;
             DROP INDEX IF EXISTS idx_edges_kind;
             DROP INDEX IF EXISTS idx_edges_source_kind;
             DROP INDEX IF EXISTS idx_edges_target_kind;
             DROP INDEX IF EXISTS idx_edges_unique;
             DROP INDEX IF EXISTS idx_unresolved_refs_from_node_id;
             DROP INDEX IF EXISTS idx_unresolved_refs_reference_name;
             DROP INDEX IF EXISTS idx_unresolved_refs_file_path;
             DROP TRIGGER IF EXISTS nodes_fts_insert;
             DROP TRIGGER IF EXISTS nodes_fts_delete;
             DROP TRIGGER IF EXISTS nodes_fts_update;
             DELETE FROM nodes_fts;",
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: format!("failed to begin bulk load: {e}"),
                operation: "begin_bulk_load".to_string(),
            })?;
        Ok(())
    }

    /// Recreates secondary indexes (benefiting from sorted row order),
    /// restores FTS triggers and content, and re-enables normal durability.
    pub async fn end_bulk_load(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("end_bulk_load").await?;
        self.end_bulk_load_unguarded(&transaction).await?;
        transaction.commit().await
    }

    pub(crate) async fn end_bulk_load_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_nodes_kind ON nodes(kind);
             CREATE INDEX IF NOT EXISTS idx_nodes_name ON nodes(name);
             CREATE INDEX IF NOT EXISTS idx_nodes_qualified_name ON nodes(qualified_name);
             CREATE INDEX IF NOT EXISTS idx_nodes_file_path ON nodes(file_path);
             CREATE INDEX IF NOT EXISTS idx_nodes_file_path_start_line ON nodes(file_path, start_line);
             CREATE INDEX IF NOT EXISTS idx_edges_source_kind ON edges(source, kind);
             CREATE INDEX IF NOT EXISTS idx_edges_target_kind ON edges(target, kind);
             CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
             CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_unique ON edges(source, target, kind, COALESCE(line, -1));
             CREATE INDEX IF NOT EXISTS idx_unresolved_refs_from_node_id ON unresolved_refs(from_node_id);
             CREATE INDEX IF NOT EXISTS idx_unresolved_refs_reference_name ON unresolved_refs(reference_name);
             CREATE INDEX IF NOT EXISTS idx_unresolved_refs_file_path ON unresolved_refs(file_path);
             CREATE TRIGGER IF NOT EXISTS nodes_fts_insert AFTER INSERT ON nodes BEGIN
                 INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
                 VALUES (NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
             END;
             CREATE TRIGGER IF NOT EXISTS nodes_fts_delete AFTER DELETE ON nodes BEGIN
                 INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
                 VALUES ('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
             END;
             CREATE TRIGGER IF NOT EXISTS nodes_fts_update AFTER UPDATE ON nodes BEGIN
                 INSERT INTO nodes_fts(nodes_fts, rowid, name, qualified_name, docstring, signature)
                 VALUES ('delete', OLD.rowid, OLD.name, OLD.qualified_name, OLD.docstring, OLD.signature);
                 INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
                 VALUES (NEW.rowid, NEW.name, NEW.qualified_name, NEW.docstring, NEW.signature);
             END;
             INSERT INTO nodes_fts(rowid, name, qualified_name, docstring, signature)
                 SELECT rowid, name, qualified_name, docstring, signature FROM nodes;",
        ).await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to end bulk load: {e}"),
            operation: "end_bulk_load".to_string(),
        })?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;

    /// Serializes tests that mutate [`SQLITE_UNSAFE_FAST_ENV`]; process env is
    /// shared across threads under plain `cargo test`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var(self.key, previous);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    fn adaptive_new_db_gets_minimum() {
        let (cache_kb, mmap) = adaptive_cache_sizes(0);
        assert_eq!(cache_kb, 2 * MB / KB); // 2 MB in KiB = 2048
        assert_eq!(mmap, 0);
    }

    #[test]
    fn adaptive_small_db() {
        // 5 MB DB → cache = 2 MB (floor), mmap = 10 MB
        let (cache_kb, mmap) = adaptive_cache_sizes(5 * MB);
        assert_eq!(cache_kb, 2 * MB / KB);
        assert_eq!(mmap, 10 * MB);
    }

    #[test]
    fn adaptive_medium_db() {
        // 100 MB DB → cache = 25 MB, mmap = 200 MB
        let (cache_kb, mmap) = adaptive_cache_sizes(100 * MB);
        assert_eq!(cache_kb, 25 * MB / KB);
        assert_eq!(mmap, 200 * MB);
    }

    #[test]
    fn adaptive_large_db() {
        // 500 MB DB → cache = 64 MB (cap), mmap = 256 MB (cap)
        let (cache_kb, mmap) = adaptive_cache_sizes(500 * MB);
        assert_eq!(cache_kb, 64 * MB / KB);
        assert_eq!(mmap, 256 * MB);
    }

    #[test]
    fn adaptive_very_large_db() {
        // 2 GB DB → both capped at max
        let (cache_kb, mmap) = adaptive_cache_sizes(2 * 1024 * MB);
        assert_eq!(cache_kb, 64 * MB / KB);
        assert_eq!(mmap, 256 * MB);
    }

    #[test]
    fn mmap_disabled_for_every_graph_database() {
        let raw = 200 * MB;
        let effective = platform_safe_mmap_size(raw);
        assert_eq!(effective, 0);
        assert_eq!(platform_safe_mmap_size(0), 0);
    }

    #[tokio::test]
    async fn repeated_authorized_opens_share_one_physical_connection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "connection reuse").unwrap();
        let (first, _) = Database::initialize(&path, &authority).await.unwrap();
        let (second, _) = Database::open(&path, &authority).await.unwrap();
        let mut readers = Vec::new();
        for _ in 0..12 {
            readers.push(Database::open_read_only(&path, &authority).await.unwrap().0);
        }

        assert!(Arc::ptr_eq(&first.inner, &second.inner));
        assert!(
            readers
                .iter()
                .all(|reader| !Arc::ptr_eq(&first.inner, &reader.inner))
        );
        assert!(readers.iter().all(|reader| !reader.inner.writable));
        assert!(first.inner.writable);
    }

    #[tokio::test]
    async fn repeated_authorized_opens_share_one_writer_lane() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "writer reuse").unwrap();
        let (first, _) = Database::initialize(&path, &authority).await.unwrap();
        let (second, _) = Database::open(&path, &authority).await.unwrap();

        let first_writer = first.writer().await;
        assert!(second.inner.writer.try_lock().is_err());
        drop(first_writer);
        assert!(second.inner.writer.try_lock().is_ok());
    }

    #[tokio::test]
    async fn twelve_handles_serialize_isolated_writer_connections() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "twelve writers").unwrap();
        let (first, _) = Database::initialize(&path, &authority).await.unwrap();
        first
            .writer_connection("create writer counter")
            .await
            .unwrap()
            .execute_batch(
                "CREATE TABLE writer_counter (value INTEGER NOT NULL);
                 INSERT INTO writer_counter(value) VALUES (0);",
            )
            .await
            .unwrap();

        let mut tasks = Vec::new();
        for _ in 0..12 {
            let handle = Database::open(&path, &authority).await.unwrap().0;
            tasks.push(tokio::spawn(async move {
                handle
                    .writer_connection("increment writer counter")
                    .await
                    .unwrap()
                    .execute("UPDATE writer_counter SET value = value + 1", ())
                    .await
                    .unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }

        let mut rows = first
            .conn()
            .query("SELECT value FROM writer_counter", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            12
        );
    }

    #[tokio::test]
    async fn retained_reader_never_observes_uncommitted_writer_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "paused writer read").unwrap();
        let (db, _) = Database::initialize(&path, &authority).await.unwrap();
        db.writer_connection("seed paused writer")
            .await
            .unwrap()
            .execute_batch(
                "CREATE TABLE paused_writer (value INTEGER NOT NULL);
                 INSERT INTO paused_writer(value) VALUES (0);",
            )
            .await
            .unwrap();

        let transaction = db.begin_write_transaction("pause writer").await.unwrap();
        transaction
            .execute("UPDATE paused_writer SET value = 1", ())
            .await
            .unwrap();

        let mut before = db
            .conn()
            .query("SELECT value FROM paused_writer", ())
            .await
            .unwrap();
        assert_eq!(
            before.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        transaction.commit().await.unwrap();

        let mut after = db
            .conn()
            .query("SELECT value FROM paused_writer", ())
            .await
            .unwrap();
        assert_eq!(
            after.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn cancelled_write_transaction_rolls_back_before_releasing_lane() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "cancelled writer").unwrap();
        let (db, _) = Database::initialize(&path, &authority).await.unwrap();
        db.writer_connection("seed cancelled writer")
            .await
            .unwrap()
            .execute_batch(
                "CREATE TABLE cancelled_writer (value INTEGER NOT NULL);
                 INSERT INTO cancelled_writer(value) VALUES (0);",
            )
            .await
            .unwrap();

        let task_db = db.clone();
        let (updated_tx, updated_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let transaction = task_db
                .begin_write_transaction("cancelled update")
                .await
                .unwrap();
            transaction
                .execute("UPDATE cancelled_writer SET value = 1", ())
                .await
                .unwrap();
            let _ = updated_tx.send(());
            std::future::pending::<()>().await;
            drop(transaction);
        });
        updated_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        let transaction = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            db.begin_write_transaction("writer after cancellation"),
        )
        .await
        .expect("writer lane remained locked after cancellation")
        .unwrap();
        let mut rows = transaction
            .query("SELECT value FROM cancelled_writer", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            0
        );
        drop(rows);
        transaction.commit().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn writable_symlink_aliases_share_one_database_slot() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "writer alias reuse").unwrap();
        let (direct, _) = Database::initialize(&path, &authority).await.unwrap();
        let alias = temp.path().join("graph-alias.db");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let (through_alias, _) = Database::open(&alias, &authority).await.unwrap();

        assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn writable_case_aliases_share_one_database_slot() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("SlotCase");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("Graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "writer case reuse").unwrap();
        let (direct, _) = Database::initialize(&path, &authority).await.unwrap();
        let alias = directory.join("graph.db");
        let (through_alias, _) = Database::open(&alias, &authority).await.unwrap();

        assert!(Arc::ptr_eq(&direct.inner, &through_alias.inner));
    }

    #[tokio::test]
    async fn checkpoint_waits_for_shared_writer_lane() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "checkpoint writer lane").unwrap();
        let (first, _) = Database::initialize(&path, &authority).await.unwrap();
        let (second, _) = Database::open(&path, &authority).await.unwrap();
        let writer = first.writer().await;
        let mut checkpoint = tokio::spawn(async move { second.checkpoint().await });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut checkpoint)
                .await
                .is_err()
        );
        drop(writer);
        checkpoint.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn retained_database_guard_keeps_authority_alive_for_query_connection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "dashboard guard").unwrap();
        let (db, _) = Database::initialize(&path, &authority).await.unwrap();
        let raw = db.conn().clone();
        let guard = Arc::new(db.clone());
        drop(db);
        drop(authority);

        assert!(matches!(
            crate::db::probe_writer_owner(&path).unwrap(),
            crate::db::WriterOwnership::Active(_)
        ));
        raw.query("SELECT 1", ()).await.unwrap();

        drop(guard);
        assert_eq!(
            crate::db::probe_writer_owner(&path).unwrap(),
            crate::db::WriterOwnership::Idle
        );
        drop(raw);
    }

    #[tokio::test]
    async fn read_only_first_open_does_not_block_writable_owner() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("graph.db");
        let authority = DatabaseAuthority::acquire_test(&path, "readonly upgrade").unwrap();
        let (seed, _) = Database::initialize(&path, &authority).await.unwrap();
        drop(seed);

        let (reader, _) = Database::open_read_only(&path, &authority).await.unwrap();
        let (writer, _) = Database::open(&path, &authority).await.unwrap();
        assert!(!Arc::ptr_eq(&reader.inner, &writer.inner));
        writer
            .writer_connection("reader isolation test")
            .await
            .unwrap()
            .execute("CREATE TABLE reader_did_not_poison_writer (id INTEGER)", ())
            .await
            .unwrap();
        assert!(
            writer
                .conn()
                .execute("CREATE TABLE forbidden_retained_write (id INTEGER)", ())
                .await
                .is_err()
        );
        assert!(
            reader
                .conn()
                .execute("CREATE TABLE forbidden_reader_write (id INTEGER)", ())
                .await
                .is_err()
        );
    }

    #[test]
    fn journal_mode_uses_wal_except_on_windows() {
        let _lock = ENV_LOCK.lock().unwrap();
        // Pin the CI-only escape hatch off: Windows CI exports it for the
        // whole test run, and this test asserts the durable defaults.
        let _env = EnvVarGuard::unset(SQLITE_UNSAFE_FAST_ENV);
        if cfg!(windows) {
            assert_eq!(platform_safe_journal_mode(), "DELETE");
        } else {
            assert_eq!(platform_safe_journal_mode(), "WAL");
        }
    }

    #[test]
    fn synchronous_mode_matches_platform_journal_mode() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::unset(SQLITE_UNSAFE_FAST_ENV);
        if cfg!(windows) {
            assert_eq!(platform_safe_synchronous_mode(), "FULL");
        } else {
            assert_eq!(platform_safe_synchronous_mode(), "NORMAL");
        }
    }

    #[test]
    fn unsafe_fast_env_overrides_journal_and_synchronous_modes() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::set(SQLITE_UNSAFE_FAST_ENV, "1");
        assert_eq!(platform_safe_journal_mode(), "MEMORY");
        assert_eq!(platform_safe_synchronous_mode(), "OFF");
    }

    #[test]
    fn unsafe_fast_env_requires_exact_value_one() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _env = EnvVarGuard::set(SQLITE_UNSAFE_FAST_ENV, "true");
        assert_ne!(platform_safe_journal_mode(), "MEMORY");
        assert_ne!(platform_safe_synchronous_mode(), "OFF");
    }
}
