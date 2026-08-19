use std::path::{Path, PathBuf};

use tracedecay_runtime_core::db::DatabaseAuthorityRole;
use tracedecay_runtime_core::errors::TraceDecayError;

use crate::RegisteredGlobalDb;

/// WAL file size at or above which a completed checkpoint escalates to file
/// truncation (Plan 38 §6 storage reclaim).
///
/// SQLite's passive checkpoint lane backfills WAL frames into the main
/// database but never shrinks the `-wal` file, so a store that once ballooned
/// keeps its high-water file size forever. This bound matches the retained
/// writer's own soft checkpoint budget: below it the runtime deliberately
/// tolerates the warm WAL (a fresh schema install alone leaves several
/// mebibytes), so truncation there would only churn the file. A file at or
/// above it survived checkpoint pressure and is high-water debris worth
/// reclaiming.
pub const REGISTERED_WAL_RECLAIM_TRIGGER_BYTES: u64 = 32 * 1024 * 1024;

/// Measured outcome of one registered WAL checkpoint/compaction pass
/// (Plan 38 §6).
///
/// Byte figures are file-level measurements of the store's `-wal` sidecar,
/// matching Plan 38's rule that published size evidence stays file- and
/// directory-level. A missing sidecar measures zero bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisteredWalCheckpointReceiptV1 {
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
    pub reclaim: RegisteredWalReclaimV1,
}

/// How the pass disposed of the WAL file after the checkpoint drained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisteredWalReclaimV1 {
    /// The WAL file was below [`REGISTERED_WAL_RECLAIM_TRIGGER_BYTES`], so
    /// only the runtime checkpoint lane ran; no file reclaim was warranted.
    BelowTrigger { trigger_bytes: u64 },
    /// The drained WAL file was truncated under this client's exclusive
    /// maintenance authority.
    Truncated { trigger_bytes: u64 },
    /// The checkpoint drained, but the file keeps its high-water size: the
    /// runtime authorizes WAL file truncation only under the exclusive
    /// maintenance role, which this client (for example the live daemon)
    /// does not hold. Reclaim happens on the next maintenance-scoped pass
    /// over the same store.
    RequiresExclusiveMaintenance { trigger_bytes: u64 },
}

impl RegisteredGlobalDb {
    /// Runs one WAL checkpoint/compaction pass through this store's
    /// authorized writer and reports the measured result (Plan 38 §6).
    ///
    /// The checkpoint itself goes through the retained runtime's bounded
    /// checkpoint lane, so a WAL pinned by a live reader under size pressure
    /// surfaces as a typed error instead of a vacuous success. After the
    /// drain, a WAL file at or above [`REGISTERED_WAL_RECLAIM_TRIGGER_BYTES`]
    /// is truncated when this client holds the exclusive maintenance
    /// authority; otherwise the receipt records that reclaim is deferred to a
    /// maintenance-scoped pass.
    pub async fn checkpoint_result(
        &self,
    ) -> Result<RegisteredWalCheckpointReceiptV1, TraceDecayError> {
        let wal_path = registered_wal_path(self.db_path());
        let wal_bytes_before = wal_file_bytes(&wal_path)?;
        self.checkpoint_database().await?;
        // A successful checkpoint proves the writable scope, so the role read
        // cannot race a mode downgrade.
        let reclaim = match wal_reclaim_plan(wal_bytes_before, self.write_authority_role()?) {
            WalReclaimPlan::BelowTrigger => RegisteredWalReclaimV1::BelowTrigger {
                trigger_bytes: REGISTERED_WAL_RECLAIM_TRIGGER_BYTES,
            },
            WalReclaimPlan::Truncate => {
                self.truncate_database_wal().await?;
                RegisteredWalReclaimV1::Truncated {
                    trigger_bytes: REGISTERED_WAL_RECLAIM_TRIGGER_BYTES,
                }
            }
            WalReclaimPlan::RequiresExclusiveMaintenance => {
                RegisteredWalReclaimV1::RequiresExclusiveMaintenance {
                    trigger_bytes: REGISTERED_WAL_RECLAIM_TRIGGER_BYTES,
                }
            }
        };
        let wal_bytes_after = wal_file_bytes(&wal_path)?;
        Ok(RegisteredWalCheckpointReceiptV1 {
            wal_bytes_before,
            wal_bytes_after,
            reclaim,
        })
    }

    pub async fn checkpoint(&self) {
        if let Err(error) = self.checkpoint_result().await {
            eprintln!("[tracedecay] registered database WAL checkpoint failed: {error}");
        }
    }

    // Root-owned policy, deliberately not driven here: `prune_global_retention`
    // wraps `crate::retention::prune_global_tables` (root `src/retention.rs`,
    // keyed by the root `config::RetentionConfig`) in a write transaction.
    // Neither the table window policy nor the config type has moved down yet,
    // and reaching up for them would point this crate back at the composition
    // root.
    //
    // Root wiring: the wrapper is three lines over the public transaction API —
    //
    //     let tx = registered.begin_write_transaction().await?;
    //     let report = retention::prune_global_tables(&tx, config, now).await?;
    //     tx.commit().await?;
    //
    // Restore it here once `retention` + `config::RetentionConfig` land below
    // the composition root.
}

/// How one pass disposes of the WAL file, decided purely from the measured
/// size and the client's write-authority role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WalReclaimPlan {
    BelowTrigger,
    Truncate,
    RequiresExclusiveMaintenance,
}

const fn wal_reclaim_plan(wal_bytes_before: u64, role: DatabaseAuthorityRole) -> WalReclaimPlan {
    if wal_bytes_before < REGISTERED_WAL_RECLAIM_TRIGGER_BYTES {
        WalReclaimPlan::BelowTrigger
    } else if matches!(role, DatabaseAuthorityRole::Maintenance) {
        WalReclaimPlan::Truncate
    } else {
        WalReclaimPlan::RequiresExclusiveMaintenance
    }
}

/// The SQLite WAL sidecar for a database path: the full database file name
/// with `-wal` appended.
fn registered_wal_path(database_path: &Path) -> PathBuf {
    let mut wal = database_path.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// File-level size of the WAL sidecar. A missing sidecar is the typed
/// "no WAL exists" state and measures zero; any other filesystem failure
/// propagates instead of degrading to a fabricated figure.
fn wal_file_bytes(wal_path: &Path) -> Result<u64, TraceDecayError> {
    match std::fs::metadata(wal_path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(TraceDecayError::Database {
            message: format!(
                "failed to measure WAL file '{}': {error}",
                wal_path.display()
            ),
            operation: "measure registered WAL file".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{REGISTERED_WAL_RECLAIM_TRIGGER_BYTES, WalReclaimPlan, wal_reclaim_plan};
    use tracedecay_runtime_core::db::DatabaseAuthorityRole;

    #[test]
    fn wal_below_trigger_is_left_alone_for_every_role() {
        for role in [
            DatabaseAuthorityRole::Daemon,
            DatabaseAuthorityRole::Maintenance,
            DatabaseAuthorityRole::Test,
        ] {
            assert_eq!(
                wal_reclaim_plan(REGISTERED_WAL_RECLAIM_TRIGGER_BYTES - 1, role),
                WalReclaimPlan::BelowTrigger
            );
        }
    }

    #[test]
    fn triggered_wal_truncates_only_under_exclusive_maintenance() {
        assert_eq!(
            wal_reclaim_plan(
                REGISTERED_WAL_RECLAIM_TRIGGER_BYTES,
                DatabaseAuthorityRole::Maintenance
            ),
            WalReclaimPlan::Truncate
        );
        for role in [DatabaseAuthorityRole::Daemon, DatabaseAuthorityRole::Test] {
            assert_eq!(
                wal_reclaim_plan(REGISTERED_WAL_RECLAIM_TRIGGER_BYTES, role),
                WalReclaimPlan::RequiresExclusiveMaintenance
            );
        }
    }
}
