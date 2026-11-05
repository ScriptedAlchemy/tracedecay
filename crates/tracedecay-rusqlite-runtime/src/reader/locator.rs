use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use crate::connection::{ConnectionPolicyError, OpenedDatabaseFile, OpenedDatabaseFileError};

/// An existing file whose canonical identity was verified by the daemon.
///
/// The path is transport only. It is never normalized or used to derive store
/// identity, and the reader worker opens it without `CREATE`.
#[derive(Clone, Debug)]
pub struct ExistingReaderLocator {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    path: PathBuf,
    opened_database: Option<Arc<OpenedDatabaseFile>>,
}

impl ExistingReaderLocator {
    pub fn new(
        binding: StoreRuntimeBindingV1,
        locator: VerifiedStoreLocatorV1,
        path: PathBuf,
    ) -> Result<Self, ReaderStartError> {
        if locator.shard_id != binding.shard_id || locator.incarnation != binding.incarnation {
            return Err(ReaderStartError::LocatorBindingMismatch);
        }
        if !path.is_absolute() {
            return Err(ReaderStartError::LocatorPathIsNotAbsolute);
        }
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(Self {
                binding,
                locator,
                path,
                opened_database: None,
            }),
            Ok(_) => Err(ReaderStartError::LocatorPathIsNotFile),
            Err(_) => Err(ReaderStartError::LocatorPathMissing),
        }
    }

    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }
    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.locator
    }
    pub(crate) fn with_opened_database(mut self, opened_database: OpenedDatabaseFile) -> Self {
        self.opened_database = Some(Arc::new(opened_database));
        self
    }
    pub(crate) fn expected_file_identity(&self) -> Option<u64> {
        self.opened_database
            .as_deref()
            .map(OpenedDatabaseFile::identity)
    }
    pub(crate) fn verify_connection(
        &self,
        connection: &rusqlite::Connection,
    ) -> Result<(), ReaderStartError> {
        self.opened_database.as_deref().map_or(Ok(()), |opened| {
            // A released pin cannot be replaced by a pathname-only proof,
            // even when that pathname still names the original inode.
            opened
                .try_clone()
                .and_then(|opened| opened.verify_connection(connection, &self.path))
                .map_err(ReaderStartError::OpenedDatabaseIdentity)
        })
    }
    pub(crate) fn worker_open_path(&self) -> Result<PathBuf, ReaderStartError> {
        self.opened_database.as_deref().map_or_else(
            || Ok(self.path.clone()),
            |opened| {
                // Canonical-path platforms do not otherwise touch the pin
                // while choosing the worker path. Refuse a released owner
                // before opening another SQLite connection.
                opened
                    .try_clone()
                    .and_then(|opened| opened.reader_open_path(&self.path))
                    .map_err(ReaderStartError::OpenedDatabaseIdentity)
            },
        )
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub enum ReaderStartError {
    InvalidReaderBudget(tracedecay_store::StorageRuntimeContractErrorV1),
    LocatorBindingMismatch,
    LocatorPathIsNotAbsolute,
    LocatorPathMissing,
    LocatorPathIsNotFile,
    RuntimeUnavailable,
    StartupChannelClosed,
    OpenFailed(ConnectionPolicyError),
    ReadOnlySetupFailed(ConnectionPolicyError),
    SchemaProbeFailed(rusqlite::Error),
    OpenedDatabaseIdentity(OpenedDatabaseFileError),
    OpenedDatabaseIdentityMismatch { expected: u64, actual: u64 },
}

impl fmt::Display for ReaderStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReaderBudget(error) => write!(f, "invalid reader budget: {error}"),
            Self::LocatorBindingMismatch => {
                f.write_str("verified SQLite locator does not bind to the reader runtime")
            }
            Self::LocatorPathIsNotAbsolute => {
                f.write_str("reader requires an explicit absolute SQLite path")
            }
            Self::LocatorPathMissing => f.write_str("verified SQLite path is missing"),
            Self::LocatorPathIsNotFile => f.write_str("verified SQLite path is not a regular file"),
            Self::RuntimeUnavailable => f.write_str("storage runtime is unavailable"),
            Self::StartupChannelClosed => {
                f.write_str("SQLite reader exited before reporting startup")
            }
            Self::OpenFailed(error) => {
                write!(f, "failed to open verified SQLite store read-only: {error}")
            }
            Self::ReadOnlySetupFailed(error) => {
                write!(f, "failed to establish query-only SQLite reader: {error}")
            }
            Self::SchemaProbeFailed(error) => {
                write!(
                    f,
                    "SQLite reader could not read the database header during startup: {error}"
                )
            }
            Self::OpenedDatabaseIdentity(error) => {
                write!(f, "failed to identify opened SQLite reader file: {error}")
            }
            Self::OpenedDatabaseIdentityMismatch { expected, actual } => write!(
                f,
                "SQLite reader opened file identity {actual}, expected {expected}"
            ),
        }
    }
}

impl Error for ReaderStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidReaderBudget(error) => Some(error),
            Self::OpenFailed(error) | Self::ReadOnlySetupFailed(error) => Some(error),
            Self::SchemaProbeFailed(error) => Some(error),
            Self::OpenedDatabaseIdentity(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExistingReaderLocator, OpenedDatabaseFile, OpenedDatabaseFileError, ReaderStartError,
    };
    use crate::reader::tests::TestStore;

    #[test]
    fn released_pin_refuses_retained_reader_locators() {
        let store = TestStore::new();
        let locator = store.locator();
        let path = locator.path().to_path_buf();
        let pinned = OpenedDatabaseFile::pin(&path).expect("pin reader database");
        let attachment_pin = pinned.try_clone().expect("share attachment pin");
        let locator: ExistingReaderLocator = locator.with_opened_database(pinned);
        let retained = locator.clone();
        let identity = locator.expected_file_identity();
        assert!(identity.is_some());

        let connection = rusqlite::Connection::open(&path).expect("open original database");
        locator
            .verify_connection(&connection)
            .expect("live pin verifies the original database");
        assert!(retained.worker_open_path().is_ok());
        // Release only after the SQLite worker has exited. A retained
        // locator must not become reusable merely because the file remains.
        drop(connection);
        attachment_pin.release_after_workers_exit();
        assert!(path.is_file());

        let connection = rusqlite::Connection::open(&path).expect("reopen same inode for probe");
        for closed in [&locator, &retained] {
            assert_eq!(closed.expected_file_identity(), identity);
            assert!(matches!(
                closed.worker_open_path(),
                Err(ReaderStartError::OpenedDatabaseIdentity(
                    OpenedDatabaseFileError::Replaced
                ))
            ));
            assert!(matches!(
                closed.verify_connection(&connection),
                Err(ReaderStartError::OpenedDatabaseIdentity(
                    OpenedDatabaseFileError::Replaced
                ))
            ));
        }
    }
}
