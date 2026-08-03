use super::{
    Arc, CheckpointBlockers, CheckpointOutcome, CheckpointRequest, DATABASE_HEALTH_GATE, Database,
    DatabaseHealth, DatabaseWriteTransaction, Path, Result, TraceDecayError,
    database_checkpoint_probe, database_health,
};

impl Database {
    /// Releases this database handle.
    ///
    /// The underlying connection remains open until all cloned handles are
    /// released.
    pub fn close(self) {
        drop(self);
    }

    /// Applies the canonical runtime's bounded WAL checkpoint policy.
    pub async fn checkpoint(&self) -> Result<()> {
        self.require_active_write_scope("checkpoint")?;
        let _writer = self.writer().await;
        self.checkpoint_unguarded().await
    }

    pub async fn release_connection_memory(&self) -> Result<()> {
        self.inner
            .conn
            .execute_batch("PRAGMA shrink_memory")
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to release graph reader cache: {error}"),
                operation: "release graph database memory".to_owned(),
            })?;
        if let Some(connection) = &self.inner.write_conn {
            connection
                .execute_batch("PRAGMA shrink_memory")
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to release graph writer cache: {error}"),
                    operation: "release graph database memory".to_owned(),
                })?;
        }
        Ok(())
    }

    /// Forces a complete WAL truncation through the retained writer actor.
    ///
    /// This is narrower than the pressure-based runtime checkpoint policy:
    /// only an exclusive-maintenance authority may use it, and success means
    /// `SQLite` reported no busy readers and no remaining log frames. Offline
    /// migration artifacts need that proof before they can be attached.
    pub async fn truncate_wal_for_offline_maintenance(&self) -> Result<()> {
        self.require_active_write_scope("truncate WAL for offline maintenance")?;
        let authority = self.write_authority()?;
        if authority.role() != super::DatabaseAuthorityRole::Maintenance {
            return Err(TraceDecayError::Database {
                message: "WAL truncation requires exclusive maintenance authority".to_owned(),
                operation: "truncate WAL for offline maintenance".to_owned(),
            });
        }
        let _writer = self.writer().await;
        let connection = self
            .open_writer_connection_unguarded("truncate WAL for offline maintenance")
            .await?;
        let mut rows = connection
            .checkpoint_wal_truncate()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to truncate WAL through the writer actor: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read WAL truncation result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                message: "WAL truncation returned no result".to_owned(),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let busy = row
            .get::<i64>(0)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation busy result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let log_frames = row
            .get::<i64>(1)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation frame result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        let checkpointed_frames = row
            .get::<i64>(2)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation checkpoint result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?;
        if busy != 0 || log_frames != 0 || checkpointed_frames != 0 {
            return Err(TraceDecayError::Database {
                message: format!(
                    "WAL truncation incomplete: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
                ),
                operation: "truncate WAL for offline maintenance".to_owned(),
            });
        }
        if rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to finish WAL truncation result: {error}"),
                operation: "truncate WAL for offline maintenance".to_owned(),
            })?
            .is_some()
        {
            return Err(TraceDecayError::Database {
                message: "WAL truncation returned multiple results".to_owned(),
                operation: "truncate WAL for offline maintenance".to_owned(),
            });
        }
        Ok(())
    }

    /// Produces a standalone checkpointed fixture artifact.
    #[cfg(any(test, feature = "test-transport"))]
    #[doc(hidden)]
    pub async fn truncate_wal_for_test_artifact(&self) -> Result<()> {
        self.truncate_wal_for_offline_maintenance().await
    }

    pub(crate) async fn checkpoint_unguarded(&self) -> Result<()> {
        let authority = self.write_authority()?;
        let request = CheckpointRequest::new(
            CheckpointBlockers::default(),
            Arc::new(database_checkpoint_probe()?),
        );
        let outcome = self
            .inner
            ._runtime
            .run_checkpoint(request, authority)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("registered checkpoint failed: {error:?}"),
                operation: "checkpoint".to_owned(),
            })?;
        match outcome {
            CheckpointOutcome::BelowSoft { .. } | CheckpointOutcome::Complete { .. } => Ok(()),
            CheckpointOutcome::Pending { .. } => Err(TraceDecayError::Database {
                message: "registered checkpoint remains pending".to_owned(),
                operation: "checkpoint".to_owned(),
            }),
            CheckpointOutcome::Interrupted { reason, .. } => Err(TraceDecayError::Database {
                message: format!("registered checkpoint was interrupted: {reason:?}"),
                operation: "checkpoint".to_owned(),
            }),
        }
    }

    /// Writes a transactionally consistent copy of this database.
    ///
    /// The writer-owned online-backup command copies one consistent `SQLite`
    /// snapshot in bounded steps. The destination must not already exist.
    pub async fn snapshot_to(&self, destination: &Path) -> Result<()> {
        self.require_active_write_scope("snapshot_to")?;
        let _writer = self.writer().await;
        self.snapshot_to_unguarded(destination).await
    }

    pub(crate) async fn snapshot_to_unguarded(&self, destination: &Path) -> Result<()> {
        if destination.to_str().is_none() {
            return Err(TraceDecayError::Database {
                message: format!(
                    "snapshot destination is not valid UTF-8: '{}'",
                    destination.display()
                ),
                operation: "snapshot".to_string(),
            });
        }
        let authority = self.write_authority()?;
        self.inner
            ._runtime
            .snapshot_to(destination.to_path_buf(), authority)
            .await
            .map(|_| ())
            .map_err(|error| TraceDecayError::Database {
                message: format!("registered online backup failed: {error:?}"),
                operation: "snapshot".to_owned(),
            })
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
        Ok(self.quick_check_report().await?.is_none())
    }

    /// Runs `PRAGMA quick_check` on a fresh reader and returns its problem
    /// report, if any.
    ///
    /// `None` means the database is intact. A pragma that returns no rows is
    /// reported as a problem rather than silently treated as healthy.
    pub async fn quick_check_report(&self) -> Result<Option<String>> {
        Ok(match self.health_on_fresh_reader("quick_check").await? {
            DatabaseHealth::Healthy => None,
            DatabaseHealth::FtsOnlyCorruption(problem) | DatabaseHealth::Corrupt(problem) => {
                Some(problem)
            }
        })
    }

    /// Rebuilds the FTS5 index from the content table under the canonical
    /// writer lane.
    ///
    /// This fixes FTS-only corruption (e.g. from an interrupted bulk load)
    /// without requiring a full re-index of the codebase. Callers must hold
    /// managed-daemon or exclusive-maintenance authority; read paths must
    /// never invoke this.
    pub async fn rebuild_fts(&self) -> Result<()> {
        let transaction = self.begin_write_transaction("rebuild_fts").await?;
        self.rebuild_fts_unguarded(&transaction).await?;
        transaction.commit().await
    }

    /// Checks the retained post-open connection and repairs only a proven
    /// `nodes_fts`-only failure. The existing rebuild path owns the canonical
    /// writer lane, so concurrent writers complete before repair starts.
    pub async fn repair_fts_after_open(&self) -> Result<Option<String>> {
        let problem = match self.health_on_fresh_reader("post_open_health").await? {
            DatabaseHealth::Healthy => return Ok(None),
            DatabaseHealth::FtsOnlyCorruption(problem) => problem,
            DatabaseHealth::Corrupt(problem) => {
                return Err(TraceDecayError::Database {
                    message: format!("database quick_check failed: {problem}"),
                    operation: "post_open_health".to_string(),
                });
            }
        };

        self.rebuild_fts().await?;
        match self.health_on_fresh_reader("post_repair_health").await? {
            DatabaseHealth::Healthy => Ok(Some(problem)),
            DatabaseHealth::FtsOnlyCorruption(remaining) | DatabaseHealth::Corrupt(remaining) => {
                Err(TraceDecayError::Database {
                    message: format!("FTS repair did not restore database health: {remaining}"),
                    operation: "post_repair_health".to_string(),
                })
            }
        }
    }

    async fn health_on_fresh_reader(&self, operation: &str) -> Result<DatabaseHealth> {
        let queued_at = std::time::Instant::now();
        let _health_guard = DATABASE_HEALTH_GATE
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let wait_ms = u64::try_from(queued_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::debug!(
            event = "database_health_check",
            phase = "start",
            operation,
            wait_ms,
            "database health check started"
        );
        let started_at = std::time::Instant::now();
        let snapshot = self.inner.conn.health_read_snapshot().await.map_err(|e| {
            TraceDecayError::Database {
                message: format!("failed to begin database health snapshot: {e}"),
                operation: operation.to_string(),
            }
        })?;
        let result = database_health(&snapshot, operation).await;
        let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::debug!(
            event = "database_health_check",
            phase = "complete",
            operation,
            elapsed_ms,
            healthy = matches!(&result, Ok(DatabaseHealth::Healthy)),
            "database health check finished"
        );
        result
    }

    pub(crate) async fn rebuild_fts_unguarded(
        &self,
        transaction: &DatabaseWriteTransaction<'_>,
    ) -> Result<()> {
        transaction
            .execute_batch(
                "DROP TABLE nodes_fts;
                 CREATE VIRTUAL TABLE nodes_fts USING fts5(
                     name, qualified_name, docstring, signature,
                     content='nodes', content_rowid='rowid'
                 );
                 INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');",
            )
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

    pub async fn begin_bulk_load_unguarded(
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
             -- nodes_fts is an external-content FTS5 table: a plain DELETE
             -- computes the terms to remove from the CURRENT content rows, so
             -- any index/content divergence survives it and the end-of-load
             -- reinsert then duplicates entries (malformed inverted index).
             -- 'delete-all' wipes the index structures unconditionally.
             INSERT INTO nodes_fts(nodes_fts) VALUES('delete-all');",
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

    pub async fn end_bulk_load_unguarded(
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
             -- Canonical external-content resync: 'rebuild' derives the whole
             -- index from the content table, correct even if the index was
             -- not perfectly empty when the bulk load began.
             INSERT INTO nodes_fts(nodes_fts) VALUES('rebuild');",
        ).await.map_err(|e| TraceDecayError::Database {
            message: format!("failed to end bulk load: {e}"),
            operation: "end_bulk_load".to_string(),
        })?;
        Ok(())
    }
}
