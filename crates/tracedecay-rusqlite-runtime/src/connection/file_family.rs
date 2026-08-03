use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteFamilyComponent {
    Main,
    Wal,
    Shm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteFamilyViolation {
    Missing,
    Replaced,
    Unlinked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteFamilyIntegrityError {
    Quarantined {
        component: SqliteFamilyComponent,
        violation: SqliteFamilyViolation,
    },
    ProbeUnavailable {
        component: SqliteFamilyComponent,
    },
}

impl fmt::Display for SqliteFamilyIntegrityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quarantined {
                component,
                violation,
            } => write!(
                formatter,
                "SQLite family is quarantined: {component:?} is {violation:?}"
            ),
            Self::ProbeUnavailable { component } => {
                write!(
                    formatter,
                    "could not inspect SQLite {component:?} file identity"
                )
            }
        }
    }
}

impl std::error::Error for SqliteFamilyIntegrityError {}

pub(crate) struct SqliteFamilyGuard {
    path: PathBuf,
    state: Mutex<SqliteFamilyState>,
}

impl fmt::Debug for SqliteFamilyGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqliteFamilyGuard")
            .field("path", &self.path)
            .field("quarantine", &self.quarantine())
            .finish_non_exhaustive()
    }
}

struct SqliteFamilyState {
    main: PinnedFamilyFile,
    wal: Option<PinnedFamilyFile>,
    shm: Option<PinnedFamilyFile>,
    quarantine: Option<SqliteFamilyIntegrityError>,
    disarmed: bool,
}

struct PinnedFamilyFile {
    file: File,
    identity: u64,
}

impl SqliteFamilyGuard {
    pub(crate) fn new(path: PathBuf, main: File) -> Result<Self, SqliteFamilyIntegrityError> {
        let identity = super::opened_file_identity(&main).map_err(|_| {
            SqliteFamilyIntegrityError::ProbeUnavailable {
                component: SqliteFamilyComponent::Main,
            }
        })?;
        Ok(Self {
            path,
            state: Mutex::new(SqliteFamilyState {
                main: PinnedFamilyFile {
                    file: main,
                    identity,
                },
                wal: None,
                shm: None,
                quarantine: None,
                disarmed: false,
            }),
        })
    }

    pub(crate) fn observe_visible_sidecars(&self) -> Result<(), SqliteFamilyIntegrityError> {
        let mut state = self.lock_state();
        if let Some(error) = state.quarantine {
            return Err(error);
        }
        if state.disarmed {
            return Ok(());
        }
        observe_component(&self.path, SqliteFamilyComponent::Wal, &mut state.wal)?;
        observe_component(&self.path, SqliteFamilyComponent::Shm, &mut state.shm)
    }

    pub(crate) fn probe(&self) -> Result<(), SqliteFamilyIntegrityError> {
        self.probe_with_sidecar_policy(false)
    }

    pub(crate) fn probe_after_write(&self) -> Result<(), SqliteFamilyIntegrityError> {
        self.probe_with_sidecar_policy(true)
    }

    fn probe_with_sidecar_policy(
        &self,
        require_live_sidecars: bool,
    ) -> Result<(), SqliteFamilyIntegrityError> {
        let mut state = self.lock_state();
        if let Some(error) = state.quarantine {
            return Err(error);
        }
        if state.disarmed {
            return Ok(());
        }

        if let Err(error) = probe_pinned(&self.path, SqliteFamilyComponent::Main, &state.main) {
            return quarantine_if_definitive(&mut state, error);
        }
        for component in [SqliteFamilyComponent::Wal, SqliteFamilyComponent::Shm] {
            let path = component_path(&self.path, component);
            let slot = match component {
                SqliteFamilyComponent::Wal => &mut state.wal,
                SqliteFamilyComponent::Shm => &mut state.shm,
                SqliteFamilyComponent::Main => unreachable!("main is always pinned"),
            };
            if slot.is_none() {
                if let Err(error) = observe_component(&self.path, component, slot) {
                    return quarantine_if_definitive(&mut state, error);
                }
                if require_live_sidecars && slot.is_none() {
                    return quarantine_if_definitive(
                        &mut state,
                        SqliteFamilyIntegrityError::Quarantined {
                            component,
                            violation: SqliteFamilyViolation::Missing,
                        },
                    );
                }
            }
            if let Some(pinned) = slot.as_ref()
                && let Err(error) = probe_pinned(&path, component, pinned)
            {
                return quarantine_if_definitive(&mut state, error);
            }
        }
        Ok(())
    }

    pub(crate) fn disarm(&self) {
        let mut state = self.lock_state();
        state.disarmed = true;
    }

    pub(crate) fn remove_closed_sidecars(&self) -> Result<(), SqliteFamilyIntegrityError> {
        let mut state = self.lock_state();
        if let Some(error) = state.quarantine {
            return Err(error);
        }
        probe_pinned(&self.path, SqliteFamilyComponent::Main, &state.main)?;
        for component in [SqliteFamilyComponent::Wal, SqliteFamilyComponent::Shm] {
            let path = component_path(&self.path, component);
            let slot = match component {
                SqliteFamilyComponent::Wal => &mut state.wal,
                SqliteFamilyComponent::Shm => &mut state.shm,
                SqliteFamilyComponent::Main => unreachable!("main is always pinned"),
            };
            if let Some(pinned) = slot.as_ref() {
                match probe_pinned(&path, component, pinned) {
                    Ok(()) => std::fs::remove_file(&path)
                        .map_err(|_| SqliteFamilyIntegrityError::ProbeUnavailable { component })?,
                    Err(SqliteFamilyIntegrityError::Quarantined {
                        violation: SqliteFamilyViolation::Missing | SqliteFamilyViolation::Unlinked,
                        ..
                    }) => {}
                    Err(error) => return Err(error),
                }
            } else if path.exists() {
                return Err(SqliteFamilyIntegrityError::ProbeUnavailable { component });
            }
            *slot = None;
        }
        state.disarmed = true;
        Ok(())
    }

    pub(crate) fn quarantine(&self) -> Option<SqliteFamilyIntegrityError> {
        self.lock_state().quarantine
    }

    fn lock_state(&self) -> MutexGuard<'_, SqliteFamilyState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn observe_component(
    database_path: &Path,
    component: SqliteFamilyComponent,
    slot: &mut Option<PinnedFamilyFile>,
) -> Result<(), SqliteFamilyIntegrityError> {
    if slot.is_some() {
        return Ok(());
    }
    let path = component_path(database_path, component);
    let file = match open_sidecar(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(SqliteFamilyIntegrityError::ProbeUnavailable { component }),
    };
    let identity = super::opened_file_identity(&file)
        .map_err(|_| SqliteFamilyIntegrityError::ProbeUnavailable { component })?;
    *slot = Some(PinnedFamilyFile { file, identity });
    Ok(())
}

fn probe_pinned(
    path: &Path,
    component: SqliteFamilyComponent,
    pinned: &PinnedFamilyFile,
) -> Result<(), SqliteFamilyIntegrityError> {
    match descriptor_link_state(&pinned.file) {
        Ok(DescriptorLinkState::Linked) => {}
        Ok(DescriptorLinkState::Unlinked) => {
            return Err(SqliteFamilyIntegrityError::Quarantined {
                component,
                violation: SqliteFamilyViolation::Unlinked,
            });
        }
        Err(()) => return Err(SqliteFamilyIntegrityError::ProbeUnavailable { component }),
    }
    let current = match open_component(path, component) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(SqliteFamilyIntegrityError::Quarantined {
                component,
                violation: SqliteFamilyViolation::Missing,
            });
        }
        Err(_) => return Err(SqliteFamilyIntegrityError::ProbeUnavailable { component }),
    };
    let current_identity = super::opened_file_identity(&current)
        .map_err(|_| SqliteFamilyIntegrityError::ProbeUnavailable { component })?;
    if current_identity != pinned.identity {
        return Err(SqliteFamilyIntegrityError::Quarantined {
            component,
            violation: SqliteFamilyViolation::Replaced,
        });
    }
    Ok(())
}

fn quarantine_if_definitive(
    state: &mut SqliteFamilyState,
    error: SqliteFamilyIntegrityError,
) -> Result<(), SqliteFamilyIntegrityError> {
    if matches!(error, SqliteFamilyIntegrityError::Quarantined { .. }) {
        state.quarantine = Some(error);
    }
    Err(error)
}

fn component_path(database_path: &Path, component: SqliteFamilyComponent) -> PathBuf {
    let suffix = match component {
        SqliteFamilyComponent::Main => return database_path.to_path_buf(),
        SqliteFamilyComponent::Wal => "-wal",
        SqliteFamilyComponent::Shm => "-shm",
    };
    let mut value = database_path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

fn open_component(path: &Path, component: SqliteFamilyComponent) -> io::Result<File> {
    match component {
        SqliteFamilyComponent::Main => File::open(path),
        SqliteFamilyComponent::Wal | SqliteFamilyComponent::Shm => open_sidecar(path),
    }
}

#[cfg(not(windows))]
fn open_sidecar(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

#[cfg(windows)]
fn open_sidecar(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;

    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
}

enum DescriptorLinkState {
    Linked,
    Unlinked,
}

#[cfg(unix)]
fn descriptor_link_state(file: &File) -> Result<DescriptorLinkState, ()> {
    use std::os::unix::fs::MetadataExt;

    file.metadata()
        .map(|metadata| {
            if metadata.nlink() == 0 {
                DescriptorLinkState::Unlinked
            } else {
                DescriptorLinkState::Linked
            }
        })
        .map_err(|_| ())
}

#[cfg(windows)]
fn descriptor_link_state(file: &File) -> Result<DescriptorLinkState, ()> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct FileStandardInfo {
        allocation_size: i64,
        end_of_file: i64,
        number_of_links: u32,
        delete_pending: u8,
        directory: u8,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "GetFileInformationByHandleEx"]
        fn get_file_information_by_handle_ex(
            handle: *mut std::ffi::c_void,
            class: i32,
            information: *mut std::ffi::c_void,
            size: u32,
        ) -> i32;
    }

    const FILE_STANDARD_INFO_CLASS: i32 = 1;
    let mut information = MaybeUninit::<FileStandardInfo>::uninit();
    let size = u32::try_from(std::mem::size_of::<FileStandardInfo>()).map_err(|_| ())?;
    // SAFETY: `file` owns a valid handle and the output buffer has the exact
    // size required for FILE_STANDARD_INFO.
    let succeeded = unsafe {
        get_file_information_by_handle_ex(
            file.as_raw_handle(),
            FILE_STANDARD_INFO_CLASS,
            information.as_mut_ptr().cast(),
            size,
        )
    };
    if succeeded == 0 {
        return Err(());
    }
    // SAFETY: A nonzero result initializes the complete output structure.
    let information = unsafe { information.assume_init() };
    if information.number_of_links == 0 || information.delete_pending != 0 {
        Ok(DescriptorLinkState::Unlinked)
    } else {
        Ok(DescriptorLinkState::Linked)
    }
}

#[cfg(not(any(unix, windows)))]
fn descriptor_link_state(_file: &File) -> Result<DescriptorLinkState, ()> {
    Err(())
}

#[cfg(test)]
mod tests {
    use std::fs::{File, OpenOptions};
    use std::io::Write;

    use tempfile::TempDir;

    use super::{
        SqliteFamilyComponent, SqliteFamilyGuard, SqliteFamilyIntegrityError,
        SqliteFamilyViolation, component_path,
    };

    fn create_file(path: &std::path::Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn armed_wal_unlink_terminally_quarantines_the_family() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        let main = create_file(&database);
        let guard = SqliteFamilyGuard::new(database.clone(), main).unwrap();
        let wal = directory.path().join("store.sqlite3-wal");
        File::create(&wal).unwrap().write_all(b"frame").unwrap();

        guard.observe_visible_sidecars().unwrap();
        std::fs::remove_file(&wal).unwrap();

        let error = guard.probe().unwrap_err();
        assert_eq!(
            error,
            SqliteFamilyIntegrityError::Quarantined {
                component: SqliteFamilyComponent::Wal,
                violation: SqliteFamilyViolation::Unlinked,
            }
        );
        assert_eq!(guard.probe().unwrap_err(), error);
    }

    #[test]
    fn absent_sidecar_is_pending_until_observed() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        let main = create_file(&database);
        let guard = SqliteFamilyGuard::new(database.clone(), main).unwrap();

        guard.observe_visible_sidecars().unwrap();
        guard.probe().unwrap();

        let wal = directory.path().join("store.sqlite3-wal");
        File::create(&wal).unwrap().write_all(b"frame").unwrap();
        guard.observe_visible_sidecars().unwrap();
        guard.probe().unwrap();
    }

    #[test]
    fn committed_write_requires_the_previously_unobserved_wal_family() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        let main = create_file(&database);
        let guard = SqliteFamilyGuard::new(database, main).unwrap();

        assert_eq!(
            guard.probe_after_write(),
            Err(SqliteFamilyIntegrityError::Quarantined {
                component: SqliteFamilyComponent::Wal,
                violation: SqliteFamilyViolation::Missing,
            })
        );
        assert_eq!(
            guard.quarantine(),
            Some(SqliteFamilyIntegrityError::Quarantined {
                component: SqliteFamilyComponent::Wal,
                violation: SqliteFamilyViolation::Missing,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn first_wal_unlink_during_commit_is_not_accepted_as_an_absent_pending_sidecar() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        {
            let connection = rusqlite::Connection::open(&database).unwrap();
            connection
                .execute_batch("CREATE TABLE markers(value INTEGER NOT NULL)")
                .unwrap();
        }
        let main = File::open(&database).unwrap();
        let guard = SqliteFamilyGuard::new(database.clone(), main).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let wal = component_path(&database, SqliteFamilyComponent::Wal);
        let removed = Arc::new(AtomicBool::new(false));
        let hook_removed = Arc::clone(&removed);
        connection
            .commit_hook(Some(move || {
                hook_removed.store(std::fs::remove_file(&wal).is_ok(), Ordering::SeqCst);
                false
            }))
            .unwrap();

        connection
            .execute("INSERT INTO markers(value) VALUES (1)", [])
            .unwrap();
        assert!(removed.load(Ordering::SeqCst));
        assert_eq!(
            guard.probe_after_write(),
            Err(SqliteFamilyIntegrityError::Quarantined {
                component: SqliteFamilyComponent::Wal,
                violation: SqliteFamilyViolation::Missing,
            })
        );
    }

    #[test]
    fn wal_truncation_preserves_the_armed_identity() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        let main = create_file(&database);
        let guard = SqliteFamilyGuard::new(database.clone(), main).unwrap();
        let wal = directory.path().join("store.sqlite3-wal");
        File::create(&wal).unwrap().write_all(b"frame").unwrap();
        guard.observe_visible_sidecars().unwrap();

        OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&wal)
            .unwrap();

        guard.probe().unwrap();
    }

    #[test]
    fn armed_wal_replacement_terminally_quarantines_the_family() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        let main = create_file(&database);
        let guard = SqliteFamilyGuard::new(database.clone(), main).unwrap();
        let wal = directory.path().join("store.sqlite3-wal");
        File::create(&wal).unwrap().write_all(b"original").unwrap();
        guard.observe_visible_sidecars().unwrap();
        let displaced = directory.path().join("displaced-wal");
        std::fs::rename(&wal, displaced).unwrap();
        File::create(&wal)
            .unwrap()
            .write_all(b"replacement")
            .unwrap();

        assert_eq!(
            guard.probe(),
            Err(SqliteFamilyIntegrityError::Quarantined {
                component: SqliteFamilyComponent::Wal,
                violation: SqliteFamilyViolation::Replaced,
            })
        );
    }

    #[test]
    fn disarm_allows_lifecycle_cleanup() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        let main = create_file(&database);
        let guard = SqliteFamilyGuard::new(database.clone(), main).unwrap();
        let wal = directory.path().join("store.sqlite3-wal");
        File::create(&wal).unwrap().write_all(b"frame").unwrap();
        guard.observe_visible_sidecars().unwrap();

        guard.disarm();
        std::fs::remove_file(wal).unwrap();
        std::fs::remove_file(database).unwrap();

        guard.probe().unwrap();
        assert_eq!(guard.quarantine(), None);
    }

    #[cfg(unix)]
    #[test]
    fn opened_database_clones_share_terminal_quarantine() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("store.sqlite3");
        let opened = super::super::OpenedDatabaseFile::create_new(&database).unwrap();
        let clone = opened.try_clone().unwrap();
        let wal = directory.path().join("store.sqlite3-wal");
        File::create(&wal).unwrap().write_all(b"frame").unwrap();

        opened.family_guard().observe_visible_sidecars().unwrap();
        std::fs::remove_file(wal).unwrap();

        assert!(matches!(
            clone.family_guard().probe(),
            Err(SqliteFamilyIntegrityError::Quarantined {
                component: SqliteFamilyComponent::Wal,
                violation: SqliteFamilyViolation::Unlinked,
            })
        ));
        assert_eq!(
            opened.family_guard().quarantine(),
            clone.family_guard().quarantine()
        );
    }
}
