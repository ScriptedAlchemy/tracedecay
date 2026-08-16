use tracedecay_runtime_core::errors::TraceDecayError;

use crate::RegisteredGlobalDb;

impl RegisteredGlobalDb {
    /// Checkpoints the registered store's WAL through its authorized writer.
    pub async fn checkpoint_result(&self) -> Result<(), TraceDecayError> {
        self.checkpoint_database().await
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
    // the composition root.
}
