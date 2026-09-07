use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grafeo_engine::Config;
use grafeo_engine::config::{DurabilityMode, StorageFormat};

use crate::{GraphCancellation, GraphDbError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphDbLocation {
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
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

    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
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
            #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
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
                let config = Config::persistent(&path)
                    .with_storage_format(StorageFormat::SingleFile)
                    .with_wal_durability(durability);
                let config = apply_tiered_storage(config, &path);
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

/// Feature-off: the persistent config is handed to Grafeo untouched, so the
/// default all-in-RAM LPG store and default buffer budget stay exactly as they
/// were before `graph-disk-tier` existed.
#[cfg(not(feature = "graph-disk-tier"))]
fn apply_tiered_storage(config: Config, _path: &Path) -> Config {
    config
}

/// Fraction of detected system RAM the graph buffer manager may hold before
/// it starts spilling eligible sections. Deliberately conservative: the point
/// of the tier is to bound the projection/publication peak, not to squeeze the
/// resident set as small as it will go.
#[cfg(feature = "graph-disk-tier")]
const TIERED_MEMORY_FRACTION: f64 = 0.25;

/// Feature-on: give Grafeo somewhere to put spilled sections and a bounded
/// memory budget to spill against.
///
/// Both knobs are prerequisites, not triggers. `with_spill_path` only decides
/// *where* a spill lands — grafeo's `SectionConsumer::spill` returns
/// `SpillError::NoSpillDirectory` without it — and `with_memory_fraction` sets
/// the budget the buffer manager measures pressure against. Section tiers are
/// left at grafeo's default `TierOverride::Auto` on purpose:
///
/// * `SectionType::LpgStore` is declared `mmap_able: false` in grafeo-common
///   (`src/storage/section.rs`), so its consumer reports `can_spill() == false`
///   and `ForceDisk` on it is a silent no-op.
/// * Forcing `VectorStore`/`TextIndex` to disk at open would change read
///   behaviour for every caller, which is not something an opt-in storage
///   feature should do implicitly.
///
/// The spill directory is named as a sibling of the `.grafeo` file
/// (`<name>.spill/`) so the tier's on-disk footprint lives with the database
/// it belongs to.
///
/// It is deliberately *not* created here. Grafeo's
/// `write_and_mmap_spill_file` (and the query-spill path in `session/mod.rs`)
/// already `create_dir_all` the parent on first use, so an open that never
/// spills leaves no trace on disk. Creating it eagerly would plant an empty
/// directory beside every database, including the short-lived
/// `*.tracedecay-restore-*.grafeo` staging stores whose parent directory
/// `backup_contract::assert_no_staging_residue` requires to be clean.
///
/// Note for whoever makes this default: once a section *does* spill, the
/// directory becomes real and nothing currently removes it when a staging
/// store closes. That cleanup is unwritten, because no section tracedecay
/// registers today is spillable (see above).
#[cfg(feature = "graph-disk-tier")]
fn apply_tiered_storage(config: Config, path: &Path) -> Config {
    config
        .with_spill_path(tiered_spill_directory(path))
        .with_memory_fraction(TIERED_MEMORY_FRACTION)
}

/// `/var/db/graph.grafeo` -> `/var/db/graph.spill`.
#[cfg(feature = "graph-disk-tier")]
fn tiered_spill_directory(path: &Path) -> PathBuf {
    // `validate_persistent_path` has already established a parent directory,
    // a UTF-8 file name, and a `.grafeo` extension by the time we get here.
    path.with_extension("spill")
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
