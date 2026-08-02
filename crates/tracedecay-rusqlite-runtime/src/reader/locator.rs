use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use crate::connection::{OpenedDatabaseFile, OpenedDatabaseFileError};

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
            opened
                .verify_connection(connection, &self.path)
                .map_err(ReaderStartError::OpenedDatabaseIdentity)
        })
    }
    pub(crate) fn worker_open_path(&self) -> Result<PathBuf, ReaderStartError> {
        self.opened_database.as_deref().map_or_else(
            || Ok(self.path.clone()),
            |opened| {
                opened
                    .worker_open_path(&self.path)
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
    ThreadSpawn(std::io::Error),
    StartupChannelClosed,
    OpenFailed,
    ReadOnlySetupFailed,
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
            Self::ThreadSpawn(error) => write!(f, "failed to start SQLite reader thread: {error}"),
            Self::StartupChannelClosed => {
                f.write_str("SQLite reader exited before reporting startup")
            }
            Self::OpenFailed => f.write_str("failed to open verified SQLite store read-only"),
            Self::ReadOnlySetupFailed => {
                f.write_str("failed to establish query-only SQLite reader")
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
            Self::ThreadSpawn(error) => Some(error),
            Self::OpenedDatabaseIdentity(error) => Some(error),
            _ => None,
        }
    }
}
