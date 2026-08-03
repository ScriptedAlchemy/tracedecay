use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[path = "inventory/artifacts.rs"]
mod artifacts;
#[path = "inventory/hermes.rs"]
mod hermes;
#[path = "inventory/project.rs"]
mod project;
#[path = "inventory/scan.rs"]
mod scan;
#[path = "inventory/sqlite.rs"]
mod sqlite;

pub use scan::build_inventory_with_global_db;

#[derive(Debug, Clone, Default)]
pub struct MigrationInventoryOptions {
    pub roots: Vec<PathBuf>,
    pub global_db_path: Option<PathBuf>,
    pub follow_symlinks: bool,
    pub include_all_registered: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreStatus {
    Ok,
    MissingDb,
    Dirty,
    Locked,
    Corrupt,
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
    pub token_cache_present: bool,
    pub registered_project_paths: Vec<PathBuf>,
    pub warnings: Vec<String>,
}
