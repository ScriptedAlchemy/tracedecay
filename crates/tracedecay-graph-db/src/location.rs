use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grafeo_engine::Config;
use grafeo_engine::config::{DurabilityMode, StorageFormat};

use crate::{GraphCancellation, GraphDbError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphDbLocation {
    Memory,
    Persistent(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphFormatVersion(u32);

impl GraphFormatVersion {
    #[must_use]
    pub const fn current() -> Self {
        Self(2)
    }

    pub fn new(value: u32) -> Result<Self, GraphDbError> {
        if value == 0 {
            return Err(GraphDbError::invalid(
                "graph format version must be greater than zero",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphDurability {
    Memory,
    /// Requests Grafeo's synchronous WAL mode for a persistent database file.
    /// Grafeo does not surface every WAL append failure from session commit, so
    /// this is a configuration request rather than a proof of durable commit.
    WalSync,
}

#[derive(Clone)]
pub struct GraphDbOpenOptions {
    pub location: GraphDbLocation,
    pub expected_format: GraphFormatVersion,
    pub durability: GraphDurability,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for GraphDbOpenOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphDbOpenOptions")
            .field("location", &self.location)
            .field("expected_format", &self.expected_format)
            .field("durability", &self.durability)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct ValidatedOpen {
    pub(crate) config: Config,
    pub(crate) durability: GraphDurability,
    pub(crate) expected_format: GraphFormatVersion,
    pub(crate) preexisting_store: bool,
}

/// The registry inspects the database-file leaf before Grafeo opens it.
/// The retained daemon store authority excludes a competing creator while
/// Grafeo atomically creates and exclusively locks a prospective file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistentGraphStoreState {
    Prospective,
    Existing,
}

impl GraphDbOpenOptions {
    pub(crate) fn validate(
        self,
        persistent_store_state: Option<PersistentGraphStoreState>,
    ) -> Result<ValidatedOpen, GraphDbError> {
        if self.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let expected_format = self.expected_format;
        match self.location {
            GraphDbLocation::Memory => {
                if self.durability != GraphDurability::Memory {
                    return Err(GraphDbError::invalid(
                        "in-memory graph databases require memory durability",
                    ));
                }
                Ok(ValidatedOpen {
                    config: Config::in_memory(),
                    durability: self.durability,
                    expected_format,
                    preexisting_store: false,
                })
            }
            GraphDbLocation::Persistent(path) => {
                if self.durability == GraphDurability::Memory {
                    return Err(GraphDbError::invalid(
                        "persistent graph databases require sync durability",
                    ));
                }
                validate_persistent_path(&path)?;
                let preexisting_store = match persistent_store_state {
                    Some(PersistentGraphStoreState::Prospective) => false,
                    Some(PersistentGraphStoreState::Existing) => true,
                    None => path.try_exists().map_err(|error| {
                        GraphDbError::unavailable(format!(
                            "failed to inspect persistent path {}: {error}",
                            path.display()
                        ))
                    })?,
                };
                let durability = match self.durability {
                    GraphDurability::WalSync => DurabilityMode::Sync,
                    GraphDurability::Memory => {
                        return Err(GraphDbError::invalid(
                            "persistent graph databases require durable storage",
                        ));
                    }
                };
                let config = Config::persistent(path)
                    .with_storage_format(StorageFormat::SingleFile)
                    .with_wal_durability(durability);
                Ok(ValidatedOpen {
                    config,
                    durability: self.durability,
                    expected_format,
                    preexisting_store,
                })
            }
        }
    }
}

fn validate_persistent_path(path: &Path) -> Result<(), GraphDbError> {
    let Some(parent) = path.parent() else {
        return Err(GraphDbError::invalid(
            "persistent graph database file must have a parent directory",
        ));
    };
    if !parent.is_dir() {
        return Err(GraphDbError::invalid(format!(
            "persistent graph parent does not exist: {}",
            parent.display()
        )));
    }
    if path.file_name().and_then(|name| name.to_str()).is_none() {
        return Err(GraphDbError::invalid(
            "persistent graph database filename must be valid UTF-8",
        ));
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("grafeo") {
        return Err(GraphDbError::invalid(
            "persistent graph database filename must end in .grafeo",
        ));
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => Err(
            GraphDbError::invalid("persistent graph path must be a regular file"),
        ),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(GraphDbError::unavailable(format!(
            "failed to inspect persistent path {}: {error}",
            path.display()
        ))),
    }
}
