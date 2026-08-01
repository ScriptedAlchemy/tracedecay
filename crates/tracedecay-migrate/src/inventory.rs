//! Planning vocabulary for a migration preflight scan, plus the scanners that
//! produce it.
//!
//! The records below describe what a scan found — stores, roles, artifacts,
//! registry state, and integrity outcomes. [`scan`] performs the scan itself;
//! it reaches storage through the runtime seam re-exported by the root crate.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

mod artifacts;
mod hermes;
mod project;
mod scan;
mod sqlite;

pub use scan::*;

#[derive(Debug, Clone, Default)]
pub struct MigrationInventoryOptions {
    pub roots: Vec<PathBuf>,
    pub global_db_path: Option<PathBuf>,
    pub follow_symlinks: bool,
    pub include_all_registered: bool,
    pub integrity: InventoryIntegrityMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InventoryIntegrityMode {
    MetadataOnly,
    #[default]
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationInventory {
    pub stores: Vec<StoreInventory>,
    pub skipped: Vec<SkippedPath>,
    pub global_db: Option<GlobalDbInventory>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreBrand {
    TraceDecay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreRole {
    CodeProjectStore,
    GlobalDbStore,
    DiskOnlyOrphan,
    HermesProfileStore,
    HermesStateDbSource,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SqliteIntegrityOutcome {
    #[default]
    NotChecked,
    Verified,
    Damaged {
        details: Vec<String>,
    },
    Unavailable {
        reason: String,
    },
    NoData {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryStoreAuthority {
    Authoritative,
    StaleBranch,
    ExternalSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreStatus {
    Ok,
    MissingDb,
    Dirty,
    Locked,
    /// Legacy inventory manifests used this unscoped status. New scans emit
    /// `IntegrityIssue`, which identifies the exact database and its authority.
    Corrupt,
    IntegrityIssue {
        path: PathBuf,
        authority: InventoryStoreAuthority,
        outcome: SqliteIntegrityOutcome,
    },
    IntegrityUnchecked,
    NeedsManualReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryStatus {
    Registered,
    Unregistered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreInventory {
    pub project_root: PathBuf,
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub brand: StoreBrand,
    pub role: StoreRole,
    pub registry_status: RegistryStatus,
    pub size_bytes: u64,
    pub statuses: Vec<StoreStatus>,
    pub artifacts: Vec<StoreArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreArtifact {
    pub kind: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalDbInventory {
    pub path: PathBuf,
    pub exists: bool,
    pub path_overridden: bool,
    pub accounting_mode: String,
    pub legacy_home_fallback: bool,
    pub project_count: u64,
    pub session_count: u64,
    pub lcm_raw_message_count: u64,
    pub registered_project_paths: Vec<PathBuf>,
    #[serde(default)]
    pub integrity: SqliteIntegrityOutcome,
    pub warnings: Vec<String>,
}
