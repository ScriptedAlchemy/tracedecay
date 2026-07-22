use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
};

use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

/// An existing file whose canonical identity was verified by the daemon.
///
/// The path is transport only. It is never normalized or used to derive store
/// identity, and the reader worker opens it without `CREATE`.
#[derive(Clone, Debug)]
pub struct ExistingReaderLocator {
    binding: StoreRuntimeBindingV1,
    locator: VerifiedStoreLocatorV1,
    path: PathBuf,
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
        }
    }
}

impl Error for ReaderStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidReaderBudget(error) => Some(error),
            Self::ThreadSpawn(error) => Some(error),
            _ => None,
        }
    }
}
