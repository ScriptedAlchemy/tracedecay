//! Quarantine of a deterministically corrupt registry-owned graph container.
//!
//! The registry-owned `.grafeo` container is a derived index in every
//! namespace it serves: verified code and memory projections replay from the
//! relational publication journal and canonical sealed-generation seals, and
//! session relation projections re-materialize from the relational session
//! store. Permanent container corruption (a torn WAL write, a CRC fault in a
//! serialized block) therefore never destroys canonical data — but before
//! this module it permanently disabled the mount: every open of the same
//! bytes failed with the identical typed [`GraphDbError::Corrupt`], every
//! activation retried through the same fault, and the store never healed.
//!
//! This module turns that deterministic verdict into a bounded recovery:
//!
//! 1. The corruption decision is serialized across incarnations by an
//!    exclusive advisory lock on a sibling lock file. A holder elsewhere
//!    means another authority is mid-decision, so this attempt reports a
//!    retryable unavailable state and touches nothing.
//! 2. Under the lock, the deciding authority re-runs the identical failing
//!    open itself. Only a second corruption verdict with the byte-identical
//!    fault message — same GRAFEO code, same block, same CRC pair — proves
//!    the fault deterministic. A successful reopen is served; a drifting
//!    fault stays a terminal typed `Corrupt` for the operator, because a
//!    fault that changes between attempts is hardware-shaped and a rebuild
//!    onto the same medium would only re-corrupt.
//! 3. Quarantine moves the container family — WAL sidecar, verified marker,
//!    spill directory, then the container itself — into one timestamped
//!    sibling directory named with the canonical `.corrupt-` incident-debris
//!    segment, writes a durable `store-quarantined.json` receipt carrying
//!    the fault fingerprint, and emits the `store_quarantined` event.
//!    Nothing is ever deleted: the operator keeps the forensic bytes and
//!    retention may age the quarantine later.
//!
//! The caller then reopens the now-vacant path as a fresh store and the
//! ordinary publication and reconcile paths re-project every generation from
//! their canonical replay authorities.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write, sync_directory};

use crate::{GraphDb, GraphDbError};

const STORE_QUARANTINE_RECEIPT_VERSION: &str = "tracedecay.graph-store-quarantine.v1";
const STORE_QUARANTINE_RECEIPT_FILE: &str = "store-quarantined.json";
const STORE_QUARANTINE_LOCK_SUFFIX: &str = ".quarantine-lock";

/// Durable journal record written into the quarantine directory.
///
/// This is the `store_quarantined` event's durable form: it binds the exact
/// fault message and its fingerprint to the members that were moved, so a
/// doctor or an operator reading the quarantine later sees why the family
/// was adopted and what the deciding authority verified.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GraphStoreQuarantineReceiptV1 {
    version: String,
    /// The live container path the family was quarantined from.
    container: PathBuf,
    quarantined_at_micros: i64,
    /// The exact typed corruption message both open attempts reported.
    fault: String,
    /// `sha256:` fingerprint of the fault message.
    fault_fingerprint: String,
    /// Consecutive identical corruption verdicts the deciding authority
    /// observed before adopting the store.
    verification_attempts: u32,
    /// File names moved into the quarantine directory, in move order.
    members: Vec<String>,
}

/// Outcome of the corruption recovery protocol for one mount attempt.
#[derive(Debug)]
pub(crate) enum CorruptStoreRecovery<T = Arc<GraphDb>> {
    /// The verification reopen succeeded: the first verdict did not
    /// reproduce, so the live database is served and nothing was moved.
    Reopened(T),
    /// The fault reproduced byte-identically and the container family was
    /// moved into the returned quarantine directory. The live path is vacant
    /// and the caller reopens it as a fresh store.
    Quarantined { quarantine_directory: PathBuf },
}

/// Runs the deterministic-corruption recovery protocol for `container` after
/// a mount-time open failed with the typed corruption verdict `first_fault`.
///
/// `verification_open` must re-run the identical open the first verdict came
/// from; the protocol never trusts the first failure alone.
pub(crate) fn recover_deterministically_corrupt_container(
    container: &Path,
    first_fault: &str,
    verification_open: &dyn Fn() -> Result<Arc<GraphDb>, GraphDbError>,
) -> Result<CorruptStoreRecovery, GraphDbError> {
    recover_deterministically_corrupt_container_with(container, first_fault, verification_open)
}

pub(crate) fn recover_deterministically_corrupt_container_with<T>(
    container: &Path,
    first_fault: &str,
    verification_open: &dyn Fn() -> Result<T, GraphDbError>,
) -> Result<CorruptStoreRecovery<T>, GraphDbError> {
    let _decision_lock = acquire_quarantine_decision_lock(container)?;

    // Re-verify under the decision lock: quarantine only adopts a store
    // whose corruption this exact authority reproduced.
    let second_fault = match verification_open() {
        Ok(database) => return Ok(CorruptStoreRecovery::Reopened(database)),
        Err(GraphDbError::Corrupt { message }) => message,
        Err(other) => return Err(other),
    };
    if second_fault != first_fault {
        return Err(GraphDbError::Corrupt {
            message: format!(
                "graph container corruption is not deterministic; refusing quarantine: \
                 first fault `{first_fault}`, second fault `{second_fault}`"
            ),
        });
    }

    let quarantine_directory = quarantine_container_family(container, first_fault)?;
    Ok(CorruptStoreRecovery::Quarantined {
        quarantine_directory,
    })
}

/// Holds the exclusive cross-incarnation corruption-decision lock while the
/// verdict is re-proven and the family moved. The lock file persists after
/// release: unlinking a held advisory lock would let a racer lock a fresh
/// inode while this holder still believes it owns the decision.
struct QuarantineDecisionLock {
    file: File,
}

impl Drop for QuarantineDecisionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn quarantine_decision_lock_path(container: &Path) -> Result<PathBuf, GraphDbError> {
    let file_name = container_file_name(container)?;
    Ok(container.with_file_name(format!("{file_name}{STORE_QUARANTINE_LOCK_SUFFIX}")))
}

fn acquire_quarantine_decision_lock(
    container: &Path,
) -> Result<QuarantineDecisionLock, GraphDbError> {
    let path = quarantine_decision_lock_path(container)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "graph store quarantine decision lock is unavailable at {}: {error}",
                path.display()
            ))
        })?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(QuarantineDecisionLock { file }),
        // Windows LockFileEx reports ERROR_LOCK_VIOLATION (33) instead of
        // WouldBlock. AccessDenied and sharing violations stay generic
        // unavailable, not "another authority holds".
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => {
            Err(GraphDbError::unavailable(format!(
                "another authority holds the graph store corruption quarantine decision for {}; \
                 leaving the store untouched",
                container.display()
            )))
        }
        Err(error) => Err(GraphDbError::unavailable(format!(
            "graph store quarantine decision lock failed for {}: {error}",
            container.display()
        ))),
    }
}

/// Moves the container family into a fresh timestamped quarantine directory
/// and journals the receipt. The container moves last: it is the fault
/// authority, so an interruption mid-move leaves the corrupt container in
/// place for the next deciding authority rather than a vacant path beside
/// stranded sidecars.
fn quarantine_container_family(container: &Path, fault: &str) -> Result<PathBuf, GraphDbError> {
    let container_name = container_file_name(container)?.to_owned();
    match container.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(GraphDbError::unavailable(format!(
                "graph container at {} is no longer a regular file; refusing quarantine",
                container.display()
            )));
        }
        Err(error) => {
            return Err(GraphDbError::unavailable(format!(
                "graph container at {} disappeared during the quarantine decision: {error}",
                container.display()
            )));
        }
    }

    let quarantined_at_micros = current_wall_micros()?;
    let quarantine_directory =
        container.with_file_name(format!("{container_name}.corrupt-{quarantined_at_micros}"));
    std::fs::create_dir(&quarantine_directory).map_err(|error| {
        GraphDbError::unavailable(format!(
            "graph store quarantine directory {} could not be created: {error}",
            quarantine_directory.display()
        ))
    })?;

    let mut members = Vec::new();
    let mut sidecars = vec![wal_sidecar_path(container)];
    sidecars.push(container.with_extension("verified"));
    sidecars.push(container.with_extension("spill"));
    for sidecar in sidecars {
        move_family_member(&sidecar, &quarantine_directory, &mut members)?;
    }
    move_family_member(container, &quarantine_directory, &mut members)?;

    let fault_fingerprint = fault_fingerprint(fault);
    let receipt = GraphStoreQuarantineReceiptV1 {
        version: STORE_QUARANTINE_RECEIPT_VERSION.to_owned(),
        container: container.to_path_buf(),
        quarantined_at_micros,
        fault: fault.to_owned(),
        fault_fingerprint: fault_fingerprint.clone(),
        verification_attempts: 2,
        members,
    };
    let payload = serde_json::to_vec_pretty(&receipt).map_err(|error| {
        quarantine_durability_failure(&quarantine_directory, "receipt encoding failed", &error)
    })?;
    atomic_write(
        &quarantine_directory.join(STORE_QUARANTINE_RECEIPT_FILE),
        "graph store quarantine receipt",
        &payload,
        DirectorySyncPolicy::Strict,
    )
    .map_err(|error| {
        quarantine_durability_failure(&quarantine_directory, "receipt write failed", &error)
    })?;
    if let Some(parent) = container.parent() {
        sync_directory(parent, DirectorySyncPolicy::Strict).map_err(|error| {
            quarantine_durability_failure(
                &quarantine_directory,
                "store directory sync failed",
                &error,
            )
        })?;
    }

    tracing::warn!(
        event = "store_quarantined",
        container = %container.display(),
        quarantine = %quarantine_directory.display(),
        fault_fingerprint = %fault_fingerprint,
        fault = %fault,
        "deterministically corrupt graph container quarantined for forensics; \
         a fresh store rebuilds from the canonical replay authorities"
    );
    Ok(quarantine_directory)
}

fn move_family_member(
    source: &Path,
    quarantine_directory: &Path,
    members: &mut Vec<String>,
) -> Result<(), GraphDbError> {
    match source.symlink_metadata() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(member_move_failure(
                source,
                quarantine_directory,
                members,
                &format!("inspection failed: {error}"),
            ));
        }
    }
    let Some(file_name) = source.file_name() else {
        return Err(member_move_failure(
            source,
            quarantine_directory,
            members,
            "member has no file name",
        ));
    };
    std::fs::rename(source, quarantine_directory.join(file_name)).map_err(|error| {
        member_move_failure(
            source,
            quarantine_directory,
            members,
            &format!("rename failed: {error}"),
        )
    })?;
    members.push(file_name.to_string_lossy().into_owned());
    Ok(())
}

/// A move failure before anything moved is a clean retryable abort; after the
/// first member moved the family is split across two directories and the
/// durable outcome can no longer be described as either state.
fn member_move_failure(
    source: &Path,
    quarantine_directory: &Path,
    members: &[String],
    detail: &str,
) -> GraphDbError {
    let message = format!(
        "graph store quarantine could not move {} into {}: {detail}",
        source.display(),
        quarantine_directory.display()
    );
    if members.is_empty() {
        GraphDbError::unavailable(message)
    } else {
        GraphDbError::DurabilityUncertain {
            message: format!(
                "{message}; members already quarantined: {}",
                members.join(", ")
            ),
        }
    }
}

fn quarantine_durability_failure(
    quarantine_directory: &Path,
    context: &str,
    error: &dyn std::fmt::Display,
) -> GraphDbError {
    GraphDbError::DurabilityUncertain {
        message: format!(
            "graph store family moved into {} but the quarantine journal is incomplete: \
             {context}: {error}",
            quarantine_directory.display()
        ),
    }
}

fn fault_fingerprint(fault: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(fault.as_bytes())))
}

/// `graph.grafeo` -> `graph.grafeo.wal`, matching Grafeo's sidecar layout.
fn wal_sidecar_path(container: &Path) -> PathBuf {
    let mut sidecar = container.as_os_str().to_owned();
    sidecar.push(".wal");
    PathBuf::from(sidecar)
}

fn container_file_name(container: &Path) -> Result<&str, GraphDbError> {
    container
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            GraphDbError::invalid(format!(
                "graph container path {} has no UTF-8 file name",
                container.display()
            ))
        })
}

fn current_wall_micros() -> Result<i64, GraphDbError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "graph store quarantine cannot read the system clock: {error}"
            ))
        })?;
    i64::try_from(elapsed.as_micros()).map_err(|_| {
        GraphDbError::unavailable("graph store quarantine timestamp exceeds the journal range")
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{GraphCancellation, GraphDbLocation, GraphDbOpenOptions, GraphDurability};

    struct NeverCancelled;

    impl GraphCancellation for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn corrupt(message: &str) -> GraphDbError {
        GraphDbError::Corrupt {
            message: message.to_owned(),
        }
    }

    fn seeded_family(root: &Path) -> PathBuf {
        let container = root.join("graph.grafeo");
        std::fs::write(&container, b"torn container bytes").unwrap();
        std::fs::create_dir(wal_sidecar_path(&container)).unwrap();
        std::fs::write(
            wal_sidecar_path(&container).join("wal_00000001.log"),
            b"wal",
        )
        .unwrap();
        std::fs::write(container.with_extension("verified"), b"marker").unwrap();
        container
    }

    fn memory_database() -> Arc<GraphDb> {
        GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: crate::GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap()
    }

    #[test]
    fn identical_second_verdict_quarantines_the_whole_family_with_a_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let container = seeded_family(temp.path());
        let fault = "GRAFEO-X002: Serialization error: block 18 CRC mismatch: \
                     expected 7d877cc5, got 5a475db3";

        let outcome =
            recover_deterministically_corrupt_container(&container, fault, &|| Err(corrupt(fault)))
                .unwrap();
        let CorruptStoreRecovery::Quarantined {
            quarantine_directory,
        } = outcome
        else {
            panic!("identical deterministic verdicts must quarantine");
        };

        assert!(
            !container.exists(),
            "the live container path must be vacant"
        );
        assert!(!wal_sidecar_path(&container).exists());
        assert!(!container.with_extension("verified").exists());
        assert_eq!(
            std::fs::read(quarantine_directory.join("graph.grafeo")).unwrap(),
            b"torn container bytes",
            "forensic bytes must move, never be deleted or rewritten"
        );
        assert_eq!(
            std::fs::read(
                quarantine_directory
                    .join("graph.grafeo.wal")
                    .join("wal_00000001.log")
            )
            .unwrap(),
            b"wal"
        );
        assert!(quarantine_directory.join("graph.verified").is_file());

        let receipt: GraphStoreQuarantineReceiptV1 = serde_json::from_slice(
            &std::fs::read(quarantine_directory.join(STORE_QUARANTINE_RECEIPT_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt.version, STORE_QUARANTINE_RECEIPT_VERSION);
        assert_eq!(receipt.container, container);
        assert_eq!(receipt.fault, fault);
        assert_eq!(receipt.fault_fingerprint, fault_fingerprint(fault));
        assert_eq!(receipt.verification_attempts, 2);
        assert_eq!(
            receipt.members,
            vec![
                "graph.grafeo.wal".to_owned(),
                "graph.verified".to_owned(),
                "graph.grafeo".to_owned(),
            ]
        );
        let directory_name = quarantine_directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(
            directory_name.starts_with("graph.grafeo.corrupt-"),
            "quarantine directory must carry the canonical corrupt-incident segment: \
             {directory_name}"
        );
    }

    #[test]
    fn successful_verification_reopen_is_served_and_nothing_moves() {
        let temp = tempfile::tempdir().unwrap();
        let container = seeded_family(temp.path());

        let outcome =
            recover_deterministically_corrupt_container(&container, "transient verdict", &|| {
                Ok(memory_database())
            })
            .unwrap();

        assert!(matches!(outcome, CorruptStoreRecovery::Reopened(_)));
        assert!(container.exists(), "a reopened store must not be touched");
        assert!(wal_sidecar_path(&container).exists());
    }

    #[test]
    fn drifting_fault_refuses_quarantine_and_stays_typed_corrupt() {
        let temp = tempfile::tempdir().unwrap();
        let container = seeded_family(temp.path());

        let error = recover_deterministically_corrupt_container(
            &container,
            "block 18 CRC mismatch",
            &|| Err(corrupt("block 7 CRC mismatch")),
        )
        .unwrap_err();

        assert!(
            matches!(&error, GraphDbError::Corrupt { message }
                if message.contains("not deterministic")
                    && message.contains("block 18 CRC mismatch")
                    && message.contains("block 7 CRC mismatch")),
            "a drifting fault is terminal and names both verdicts, got {error:?}"
        );
        assert!(container.exists(), "a drifting fault must not move bytes");
    }

    #[test]
    fn non_corrupt_verification_failure_propagates_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let container = seeded_family(temp.path());

        let error = recover_deterministically_corrupt_container(&container, "fault", &|| {
            Err(GraphDbError::Cancelled)
        })
        .unwrap_err();

        assert_eq!(error, GraphDbError::Cancelled);
        assert!(container.exists());
    }

    #[test]
    fn held_decision_lock_reports_retryable_unavailable_without_verifying() {
        let temp = tempfile::tempdir().unwrap();
        let container = seeded_family(temp.path());
        let foreign_holder = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(quarantine_decision_lock_path(&container).unwrap())
            .unwrap();
        foreign_holder.try_lock_exclusive().unwrap();

        let verification_attempts = AtomicUsize::new(0);
        let error = recover_deterministically_corrupt_container(&container, "fault", &|| {
            verification_attempts.fetch_add(1, Ordering::SeqCst);
            Err(corrupt("fault"))
        })
        .unwrap_err();

        assert!(
            matches!(&error, GraphDbError::Unavailable { message }
                if message.contains("another authority holds")),
            "a held decision lock is a retryable typed state, got {error:?}"
        );
        assert_eq!(
            verification_attempts.load(Ordering::SeqCst),
            0,
            "a non-holder must not re-open the store it does not own"
        );
        assert!(container.exists(), "a non-holder must not move bytes");
        FileExt::unlock(&foreign_holder).unwrap();
    }

    #[test]
    fn vanished_container_under_the_lock_is_a_retryable_abort() {
        let temp = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");

        let error = recover_deterministically_corrupt_container(&container, "fault", &|| {
            Err(corrupt("fault"))
        })
        .unwrap_err();

        assert!(
            matches!(&error, GraphDbError::Unavailable { message }
                if message.contains("disappeared during the quarantine decision")),
            "an already-recovered path must abort cleanly, got {error:?}"
        );
    }
}
