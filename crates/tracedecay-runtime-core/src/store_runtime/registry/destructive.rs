//! Registry-owned exclusion for destructive store maintenance.

use std::path::{Path, PathBuf};

use super::{
    DestructivePathReservation, RegistryEntry, StoreRuntimeRegistry, StoreRuntimeRegistryFailure,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestructiveMaintenanceTarget {
    root: PathBuf,
    database_paths: Vec<PathBuf>,
    initial_file_identities: Vec<(PathBuf, u64)>,
}

impl DestructiveMaintenanceTarget {
    pub fn new(
        root: impl Into<PathBuf>,
        database_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, StoreRuntimeRegistryFailure> {
        let root = canonical_existing_directory(root.into())?;
        let mut database_paths = database_paths
            .into_iter()
            .map(|path| canonical_database_under_root(&root, &path))
            .collect::<Result<Vec<_>, _>>()?;
        database_paths.sort();
        database_paths.dedup();
        if database_paths.is_empty() {
            return Err(
                StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                    message: "destructive maintenance requires at least one SQLite database"
                        .to_owned(),
                },
            );
        }
        let initial_file_identities = database_paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| {
                crate::db::sqlite_generation_identity(path)
                    .map(|identity| (path.clone(), identity))
                    .map_err(
                        |_| StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                            message: format!(
                                "database '{}' has no stable file identity",
                                path.display()
                            ),
                        },
                    )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            root,
            database_paths,
            initial_file_identities,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn database_paths(&self) -> &[PathBuf] {
        &self.database_paths
    }
}

pub struct DestructiveMaintenanceReservation {
    registry: StoreRuntimeRegistry,
    attempt: u64,
    target: DestructiveMaintenanceTarget,
    closed: Vec<super::ClosedStoreRuntime>,
    released: bool,
}

impl DestructiveMaintenanceReservation {
    pub fn target(&self) -> &DestructiveMaintenanceTarget {
        &self.target
    }

    pub fn closed(&self) -> &[super::ClosedStoreRuntime] {
        &self.closed
    }

    pub fn finish_deleted(mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        self.registry.release_destructive(self.attempt)?;
        self.released = true;
        Ok(())
    }

    pub fn abort_preserved(mut self) -> Result<(), StoreRuntimeRegistryFailure> {
        for (path, initial_identity) in &self.target.initial_file_identities {
            let current = crate::db::sqlite_generation_identity(path).map_err(|_| {
                StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "verify preserved destructive-maintenance database",
                    message: format!("database '{}' is no longer intact", path.display()),
                }
            })?;
            if current != *initial_identity {
                return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                    operation: "verify preserved destructive-maintenance database",
                    message: format!(
                        "database '{}' changed during destructive maintenance",
                        path.display()
                    ),
                });
            }
        }
        self.registry.release_destructive(self.attempt)?;
        self.released = true;
        Ok(())
    }
}

impl Drop for DestructiveMaintenanceReservation {
    fn drop(&mut self) {
        if !self.released {
            // Abandonment intentionally remains fail-closed until process exit.
        }
    }
}

impl StoreRuntimeRegistry {
    pub async fn begin_destructive_maintenance(
        &self,
        target: DestructiveMaintenanceTarget,
    ) -> Result<DestructiveMaintenanceReservation, StoreRuntimeRegistryFailure> {
        let (attempt, closes) =
            {
                let mut state = self.lock_state();
                if state.destructive_paths.values().any(|reservation| {
                    paths_overlap(&target.root, &reservation.root)
                        || target
                            .database_paths
                            .iter()
                            .any(|path| reservation.database_paths.binary_search(path).is_ok())
                }) {
                    return Err(
                        StoreRuntimeRegistryFailure::DestructiveMaintenanceInProgress {
                            root: target.root.clone(),
                        },
                    );
                }
                if state.entries.values().any(|entry| match entry {
                    RegistryEntry::Opening(opening) => opening
                        .database_authority
                        .as_ref()
                        .is_some_and(|authority| {
                            authority
                                .canonical_database_path()
                                .starts_with(&target.root)
                        }),
                    RegistryEntry::Ready(_) | RegistryEntry::Evicting(_) => false,
                }) {
                    return Err(StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                        operation: "reserve destructive store maintenance",
                        message: format!(
                            "a runtime under '{}' is still opening",
                            target.root.display()
                        ),
                    });
                }
                let attempt = state
                    .next_destructive_attempt
                    .checked_add(1)
                    .ok_or(StoreRuntimeRegistryFailure::EvictionAttemptExhausted)?;
                state.next_destructive_attempt = attempt;
                let closes = state
                    .entries
                    .values()
                    .filter_map(|entry| match entry {
                        RegistryEntry::Ready(ready)
                            if ready.handle.locator().path().starts_with(&target.root) =>
                        {
                            ready
                                .handle
                                .inner
                                .database_authority
                                .clone()
                                .map(|authority| (ready.handle.binding().clone(), authority))
                        }
                        RegistryEntry::Opening(_)
                        | RegistryEntry::Ready(_)
                        | RegistryEntry::Evicting(_) => None,
                    })
                    .collect::<Vec<_>>();
                let (released, _) = tokio::sync::watch::channel(false);
                state.destructive_paths.insert(
                    attempt,
                    DestructivePathReservation {
                        root: target.root.clone(),
                        database_paths: target.database_paths.clone(),
                        released,
                    },
                );
                (attempt, closes)
            };

        let mut closed = Vec::with_capacity(closes.len());
        for (binding, authority) in closes {
            match self.close_exact(&binding, &authority).await {
                Ok(proof) => closed.push(proof),
                Err(error) => return Err(error),
            }
        }
        Ok(DestructiveMaintenanceReservation {
            registry: self.clone(),
            attempt,
            target,
            closed,
            released: false,
        })
    }

    pub(super) fn destructive_wait(
        &self,
        path: &Path,
    ) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.lock_state()
            .destructive_paths
            .values()
            .find(|reservation| reservation_matches(reservation, path))
            .map(|reservation| reservation.released.subscribe())
    }

    fn release_destructive(&self, attempt: u64) -> Result<(), StoreRuntimeRegistryFailure> {
        let reservation = self
            .lock_state()
            .destructive_paths
            .remove(&attempt)
            .ok_or_else(|| StoreRuntimeRegistryFailure::PhysicalRuntimeFailed {
                operation: "release destructive store maintenance",
                message: "destructive maintenance reservation was lost".to_owned(),
            })?;
        reservation.released.send_replace(true);
        Ok(())
    }
}

fn reservation_matches(reservation: &DestructivePathReservation, path: &Path) -> bool {
    path.starts_with(&reservation.root)
        || reservation
            .database_paths
            .binary_search_by(|candidate| candidate.as_path().cmp(path))
            .is_ok()
}

fn canonical_existing_directory(root: PathBuf) -> Result<PathBuf, StoreRuntimeRegistryFailure> {
    let canonical = root.canonicalize().map_err(|error| {
        StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
            message: format!("canonicalize store root '{}': {error}", root.display()),
        }
    })?;
    if !canonical.is_dir() {
        return Err(
            StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                message: format!("store root '{}' is not a directory", canonical.display()),
            },
        );
    }
    Ok(canonical)
}

fn canonical_database_under_root(
    root: &Path,
    path: &Path,
) -> Result<PathBuf, StoreRuntimeRegistryFailure> {
    let canonical = crate::path_safety::canonicalize_path_or_existing_parent(path);
    if !canonical.starts_with(root) {
        return Err(
            StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                message: format!(
                    "database '{}' is outside reserved store root '{}'",
                    canonical.display(),
                    root.display()
                ),
            },
        );
    }
    if canonical.exists() && !canonical.is_file() {
        return Err(
            StoreRuntimeRegistryFailure::DestructiveMaintenanceInvalidTarget {
                message: format!("database '{}' is not a regular file", canonical.display()),
            },
        );
    }
    Ok(canonical)
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}
