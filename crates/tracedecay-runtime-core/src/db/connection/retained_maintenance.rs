use super::{
    Arc, CheckpointBlockers, CheckpointOutcome, CheckpointRequest, DATABASE_HEALTH_GATE, Database,
    DatabaseAuthorityRole, DatabaseHealth, DatabaseStorageTelemetryHandle, Result, TraceDecayError,
    database_checkpoint_probe, database_health,
};

impl Database {
    /// Releases this database handle.
    pub fn close(self) {
        drop(self);
    }

    /// Applies the canonical retained runtime's bounded WAL checkpoint policy.
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
                message: format!("failed to release SQLite reader cache: {error}"),
                operation: "release SQLite database memory".to_owned(),
            })?;
        if let Some(connection) = &self.inner.write_conn {
            connection
                .execute_batch("PRAGMA shrink_memory")
                .await
                .map_err(|error| TraceDecayError::Database {
                    message: format!("failed to release SQLite writer cache: {error}"),
                    operation: "release SQLite database memory".to_owned(),
                })?;
        }
        Ok(())
    }

    /// Forces a complete WAL truncation through the retained writer actor.
    pub async fn truncate_wal_for_offline_maintenance(&self) -> Result<()> {
        const OPERATION: &str = "truncate WAL for offline maintenance";
        self.require_active_write_scope(OPERATION)?;
        let authority = self.write_authority()?;
        if authority.role() != DatabaseAuthorityRole::Maintenance {
            return Err(TraceDecayError::Database {
                message: "WAL truncation requires exclusive maintenance authority".to_owned(),
                operation: OPERATION.to_owned(),
            });
        }
        let _writer = self.writer().await;
        let connection = self.open_writer_connection_unguarded(OPERATION).await?;
        let mut rows = connection
            .checkpoint_wal_truncate()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to truncate WAL through the writer actor: {error}"),
                operation: OPERATION.to_owned(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read WAL truncation result: {error}"),
                operation: OPERATION.to_owned(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                message: "WAL truncation returned no result".to_owned(),
                operation: OPERATION.to_owned(),
            })?;
        let busy = row
            .get::<i64>(0)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation busy result: {error}"),
                operation: OPERATION.to_owned(),
            })?;
        let log_frames = row
            .get::<i64>(1)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation frame result: {error}"),
                operation: OPERATION.to_owned(),
            })?;
        let checkpointed_frames = row
            .get::<i64>(2)
            .map_err(|error| TraceDecayError::Database {
                message: format!("invalid WAL truncation checkpoint result: {error}"),
                operation: OPERATION.to_owned(),
            })?;
        if busy != 0 || log_frames != 0 || checkpointed_frames != 0 {
            return Err(TraceDecayError::Database {
                message: format!(
                    "WAL truncation incomplete: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
                ),
                operation: OPERATION.to_owned(),
            });
        }
        if rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to finish WAL truncation result: {error}"),
                operation: OPERATION.to_owned(),
            })?
            .is_some()
        {
            return Err(TraceDecayError::Database {
                message: "WAL truncation returned multiple results".to_owned(),
                operation: OPERATION.to_owned(),
            });
        }
        Ok(())
    }

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
            .client
            .runtime()
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

    pub async fn size(&self) -> Result<u64> {
        let mut rows = self
            .inner
            .conn
            .query(
                "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to get database size: {error}"),
                operation: "size".to_owned(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read database size row: {error}"),
                operation: "size".to_owned(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                message: "no result from page size query".to_owned(),
                operation: "size".to_owned(),
            })?;
        let size = row
            .get::<i64>(0)
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to read size value: {error}"),
                operation: "size".to_owned(),
            })?;
        Ok(size as u64)
    }

    pub async fn quick_check(&self) -> Result<bool> {
        Ok(self.quick_check_report().await?.is_none())
    }

    pub async fn quick_check_report(&self) -> Result<Option<String>> {
        Ok(match self.health_on_fresh_reader("quick_check").await? {
            DatabaseHealth::Healthy => None,
            DatabaseHealth::Corrupt(problem) => Some(problem),
        })
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
        let snapshot = self
            .inner
            .conn
            .health_read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to begin database health snapshot: {error}"),
                operation: operation.to_owned(),
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

    pub fn storage_telemetry_handle(&self) -> Result<DatabaseStorageTelemetryHandle> {
        let handle = self
            .client
            .runtime()
            .telemetry_read_handle()
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to attach SQLite-store telemetry reader: {error:?}"),
                operation: "attach SQLite-store telemetry reader".to_owned(),
            })?;
        Ok(DatabaseStorageTelemetryHandle {
            handle,
            _client_guard: self.client_guard(),
        })
    }

    pub async fn storage_page_counts(&self) -> Result<(u64, u64, u64)> {
        self.client
            .runtime()
            .storage_page_counts(std::time::Duration::from_secs(5))
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to sample SQLite-store pages: {error:?}"),
                operation: "sample SQLite-store pages".to_owned(),
            })
    }

    /// Runs bounded incremental vacuum through the canonical writer lane.
    pub async fn run_incremental_vacuum(&self, pages: u64) -> Result<()> {
        let authority = self.write_authority()?;
        self.client
            .runtime()
            .run_bounded_incremental_compaction(pages, authority)
            .await
            .map_err(|error| TraceDecayError::Database {
                message: format!("failed to compact SQLite store: {error:?}"),
                operation: "run bounded SQLite-store compaction".to_owned(),
            })
    }
}
