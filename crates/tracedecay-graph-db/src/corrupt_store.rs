//! Deletion of a deterministically corrupt registry-owned graph container.
//!
//! The registry-owned `.grafeo` container is a derived index in every
//! namespace it serves: verified code and memory projections replay from the
//! relational publication journal and canonical sealed-generation seals, and
//! session relation projections re-materialize from the relational session
//! store. Permanent container corruption (a torn WAL write, a CRC fault in a
//! serialized block) therefore never destroys canonical data, but left alone
//! it permanently disables the mount: every open of the same bytes fails with
//! the identical typed [`GraphDbError::Corrupt`] and the store never heals.
//!
//! This module turns that deterministic verdict into a bounded recovery:
//!
//! 1. The corruption decision is serialized across incarnations by an
//!    exclusive advisory lock on a sibling lock file. A holder elsewhere
//!    means another authority is mid-decision, so this attempt reports a
//!    retryable unavailable state and touches nothing.
//! 2. Under the lock, the deciding authority re-runs the identical failing
//!    open itself. Only a second corruption verdict with the byte-identical
//!    fault message, same GRAFEO code, same block, same CRC pair, proves
//!    the fault deterministic. A successful reopen is served; a drifting
//!    fault stays a terminal typed `Corrupt` for the operator, because a
//!    fault that changes between attempts is hardware-shaped and a rebuild
//!    onto the same medium would only re-corrupt.
//! 3. The container family is deleted: WAL sidecar, verified marker, spill
//!    directory, then the container itself, and the `store_corrupt_deleted`
//!    event records the fault fingerprint. No copy is kept.
//!
//! The caller then reopens the now-vacant path as a fresh store and the
//! ordinary publication and reconcile paths re-project every generation from
//! their canonical replay authorities.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_private_fs::FileLease;
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_directory};

use crate::{GraphDb, GraphDbError};

const CORRUPTION_DECISION_LOCK_SUFFIX: &str = ".corruption-lock";

/// Outcome of the corruption recovery protocol for one mount attempt.
#[derive(Debug)]
pub(crate) enum CorruptStoreRecovery<T = Arc<GraphDb>> {
    /// The verification reopen succeeded: the first verdict did not
    /// reproduce, so the live database is served and nothing was touched.
    Reopened(T),
    /// The fault reproduced byte-identically and the container family was
    /// deleted. The live path is vacant and the caller reopens it fresh.
    Deleted,
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
    let _decision_lock = acquire_corruption_decision_lock(container)?;

    // Re-verify under the decision lock: deletion only adopts a store whose
    // corruption this exact authority reproduced.
    let second_fault = match verification_open() {
        Ok(database) => return Ok(CorruptStoreRecovery::Reopened(database)),
        Err(GraphDbError::Corrupt { message }) => message,
        Err(other) => return Err(other),
    };
    if second_fault != first_fault {
        return Err(GraphDbError::Corrupt {
            message: format!(
                "graph container corruption is not deterministic; refusing deletion: \
                 first fault `{first_fault}`, second fault `{second_fault}`"
            ),
        });
    }

    delete_container_family(container, first_fault)?;
    Ok(CorruptStoreRecovery::Deleted)
}

/// Holds the exclusive cross-incarnation corruption-decision lock while the
/// verdict is re-proven and the family deleted. The lock file persists after
/// release: unlinking a held advisory lock would let a racer lock a fresh
/// inode while this holder still believes it owns the decision.
struct CorruptionDecisionLock {
    _file: FileLease,
}

fn corruption_decision_lock_path(container: &Path) -> Result<PathBuf, GraphDbError> {
    let file_name = container_file_name(container)?;
    Ok(container.with_file_name(format!("{file_name}{CORRUPTION_DECISION_LOCK_SUFFIX}")))
}

fn acquire_corruption_decision_lock(
    container: &Path,
) -> Result<CorruptionDecisionLock, GraphDbError> {
    let path = corruption_decision_lock_path(container)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            GraphDbError::unavailable(format!(
                "graph store corruption decision lock is unavailable at {}: {error}",
                path.display()
            ))
        })?;
    match file.try_lock().map_err(std::io::Error::from) {
        Ok(()) => Ok(CorruptionDecisionLock {
            _file: FileLease::held(file, "graph_db.corruption_decision"),
        }),
        // Windows LockFileEx reports ERROR_LOCK_VIOLATION (33) instead of
        // WouldBlock. AccessDenied and sharing violations stay generic
        // unavailable, not "another authority holds".
        Err(error) if tracedecay_private_fs::is_lock_contended(&error) => {
            Err(GraphDbError::unavailable(format!(
                "another authority holds the graph store corruption decision for {}; \
                 leaving the store untouched",
                container.display()
            )))
        }
        Err(error) => Err(GraphDbError::unavailable(format!(
            "graph store corruption decision lock failed for {}: {error}",
            container.display()
        ))),
    }
}

/// Deletes the container family. The container goes last: it is the fault
/// authority, so an interruption mid-delete leaves the corrupt container in
/// place for the next deciding authority rather than a vacant path beside
/// stranded sidecars.
fn delete_container_family(container: &Path, fault: &str) -> Result<(), GraphDbError> {
    match container.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            return Err(GraphDbError::unavailable(format!(
                "graph container at {} is no longer a regular file; refusing deletion",
                container.display()
            )));
        }
        Err(error) => {
            return Err(GraphDbError::unavailable(format!(
                "graph container at {} disappeared during the corruption decision: {error}",
                container.display()
            )));
        }
    }

    for sidecar in [
        wal_sidecar_path(container),
        container.with_extension("verified"),
        container.with_extension("spill"),
    ] {
        remove_family_member(&sidecar)?;
    }
    remove_family_member(container)?;
    if let Some(parent) = container.parent() {
        sync_directory(parent, DirectorySyncPolicy::Strict).map_err(|error| {
            GraphDbError::DurabilityUncertain {
                message: format!(
                    "corrupt graph container {} was deleted but its directory sync failed: \
                     {error}",
                    container.display()
                ),
            }
        })?;
    }

    tracing::warn!(
        event = "store_corrupt_deleted",
        container = %container.display(),
        fault_fingerprint = %format!("sha256:{}", sha256_hex(fault.as_bytes())),
        fault = %fault,
        "deterministically corrupt graph container deleted; \
         a fresh store rebuilds from the canonical replay authorities"
    );
    Ok(())
}

/// Removes one family member without following a symlink. Every failure is
/// retryable: the container is removed last, so it still carries the verdict.
fn remove_family_member(member: &Path) -> Result<(), GraphDbError> {
    let removed = match member.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(member),
        Ok(_) => std::fs::remove_file(member),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => Err(error),
    };
    removed.map_err(|error| {
        GraphDbError::unavailable(format!(
            "corrupt graph store member {} could not be deleted: {error}",
            member.display()
        ))
    })
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

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

    fn directory_names(root: &Path) -> Vec<String> {
        let mut names = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn identical_second_verdict_deletes_the_whole_family_without_a_copy() {
        let temp = tempfile::tempdir().unwrap();
        let container = seeded_family(temp.path());
        let neighbour = temp.path().join("sessions.db");
        std::fs::write(&neighbour, b"live sibling").unwrap();
        let fault = "GRAFEO-X002: Serialization error: block 18 CRC mismatch: \
                     expected 7d877cc5, got 5a475db3";

        let outcome =
            recover_deterministically_corrupt_container(&container, fault, &|| Err(corrupt(fault)))
                .unwrap();

        assert!(matches!(outcome, CorruptStoreRecovery::Deleted));
        assert_eq!(
            directory_names(temp.path()),
            vec![
                "graph.grafeo.corruption-lock".to_owned(),
                "sessions.db".to_owned()
            ],
            "only the decision lock and unrelated siblings may remain"
        );
        assert_eq!(std::fs::read(&neighbour).unwrap(), b"live sibling");
    }

    #[test]
    fn successful_verification_reopen_touches_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let container = seeded_family(temp.path());

        let outcome =
            recover_deterministically_corrupt_container_with(&container, "fault", &|| Ok(()))
                .unwrap();

        assert!(matches!(outcome, CorruptStoreRecovery::Reopened(())));
        assert_eq!(std::fs::read(&container).unwrap(), b"torn container bytes");
        assert!(wal_sidecar_path(&container).is_dir());
        assert!(container.with_extension("verified").is_file());
    }

    #[test]
    fn drifting_fault_refuses_deletion_and_stays_typed_corrupt() {
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
        assert!(container.exists(), "a drifting fault must not delete bytes");
        assert!(wal_sidecar_path(&container).is_dir());
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
            .open(corruption_decision_lock_path(&container).unwrap())
            .unwrap();
        foreign_holder
            .try_lock()
            .map_err(std::io::Error::from)
            .unwrap();

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
        assert!(container.exists(), "a non-holder must not delete bytes");
        foreign_holder.unlock().unwrap();
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
                if message.contains("disappeared during the corruption decision")),
            "an already-recovered path must abort cleanly, got {error:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_sidecar_is_unlinked_without_touching_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let container = temp.path().join("graph.grafeo");
        std::fs::write(&container, b"torn").unwrap();
        std::fs::create_dir(outside.path().join("wal")).unwrap();
        std::fs::write(outside.path().join("wal/segment"), b"foreign").unwrap();
        std::os::unix::fs::symlink(outside.path().join("wal"), wal_sidecar_path(&container))
            .unwrap();

        recover_deterministically_corrupt_container(&container, "fault", &|| {
            Err(corrupt("fault"))
        })
        .unwrap();

        assert!(wal_sidecar_path(&container).symlink_metadata().is_err());
        assert_eq!(
            std::fs::read(outside.path().join("wal/segment")).unwrap(),
            b"foreign"
        );
    }
}
