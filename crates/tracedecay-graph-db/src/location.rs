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
    Sync,
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

pub(crate) struct ValidatedOpen {
    pub(crate) config: Config,
    pub(crate) durability: GraphDurability,
    pub(crate) expected_format: GraphFormatVersion,
    pub(crate) preexisting_file: bool,
}

impl GraphDbOpenOptions {
    pub(crate) fn validate(self) -> Result<ValidatedOpen, GraphDbError> {
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
                    preexisting_file: false,
                })
            }
            GraphDbLocation::Persistent(path) => {
                if self.durability == GraphDurability::Memory {
                    return Err(GraphDbError::invalid(
                        "persistent graph databases require sync durability",
                    ));
                }
                validate_persistent_path(&path)?;
                let preexisting_file = path.try_exists().map_err(|error| {
                    GraphDbError::unavailable(format!(
                        "failed to inspect persistent path {}: {error}",
                        path.display()
                    ))
                })?;
                let durability = match self.durability {
                    GraphDurability::Sync => DurabilityMode::Sync,
                    GraphDurability::Memory => {
                        return Err(GraphDbError::invalid(
                            "persistent graph databases require durable storage",
                        ));
                    }
                };
                Ok(ValidatedOpen {
                    config: Config::persistent(path)
                        .with_storage_format(StorageFormat::SingleFile)
                        .with_wal_durability(durability),
                    durability: self.durability,
                    expected_format,
                    preexisting_file,
                })
            }
        }
    }
}

fn validate_persistent_path(path: &Path) -> Result<(), GraphDbError> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("grafeo") {
        return Err(GraphDbError::invalid(
            "persistent graph path must end in .grafeo",
        ));
    }
    let Some(parent) = path.parent() else {
        return Err(GraphDbError::invalid(
            "persistent graph path must have a parent directory",
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
            "persistent graph filename must be valid UTF-8",
        ));
    }
    Ok(())
}
