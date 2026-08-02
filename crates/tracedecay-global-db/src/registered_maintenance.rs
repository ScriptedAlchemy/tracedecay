use tracedecay_runtime_core::db::engine::WalCheckpointExecutor;
use tracedecay_runtime_core::errors::TraceDecayError;

use crate::{RegisteredGlobalDb, global_db_operation_error, global_db_operation_message};

impl RegisteredGlobalDb {
    /// Checkpoints the registered store's WAL through its authorized writer.
    pub async fn checkpoint_result(&self) -> Result<(), TraceDecayError> {
        let writer = self.writer_connection().map_err(|error| {
            global_db_operation_error("open registered WAL checkpoint writer", error)
        })?;
        let mut rows = writer
            .checkpoint_wal_truncate()
            .await
            .map_err(|error| global_db_operation_error("checkpoint registered WAL", error))?;
        let row = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?
            .ok_or_else(|| {
                global_db_operation_message(
                    "checkpoint registered WAL",
                    "WAL checkpoint returned no status row",
                )
            })?;
        let busy: i64 = row
            .get(0)
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?;
        let log_frames: i64 = row
            .get(1)
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?;
        let checkpointed_frames: i64 = row
            .get(2)
            .map_err(|error| global_db_operation_error("read registered WAL checkpoint", error))?;
        if busy != 0 || checkpointed_frames < log_frames {
            return Err(global_db_operation_message(
                "checkpoint registered WAL",
                format!(
                    "WAL checkpoint incomplete: busy={busy}, log_frames={log_frames}, checkpointed_frames={checkpointed_frames}"
                ),
            ));
        }
        Ok(())
    }

    pub async fn checkpoint(&self) {
        if let Err(error) = self.checkpoint_result().await {
            eprintln!("[tracedecay] registered database WAL checkpoint failed: {error}");
        }
    }

    // Root-owned policy, deliberately not driven here: `prune_global_retention`
    // and `global_retention_report` wrapped `crate::retention::
    // prune_global_tables` (root `src/retention.rs`, keyed by the root
    // `config::RetentionConfig`) in an apply/dry-run transaction. Neither the
    // table window policy nor the config type has moved down yet, and reaching
    // up for them would point this crate back at the composition root.
    //
    // Root wiring: the two wrappers are three lines each over the public
    // transaction API —
    //
    //     let tx = registered.begin_write_transaction().await?;
    //     let report = retention::prune_global_tables(&tx, config, mode, now).await?;
    //     tx.commit().await?;   // or tx.rollback() for the dry run
    //
    // Restore them here once `retention` + `config::RetentionConfig` land below
    // the composition root. See `SEAMS.md`.
}
