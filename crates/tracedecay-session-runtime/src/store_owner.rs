use std::path::PathBuf;

/// Identity key for one project store owner: the canonicalized profile,
/// global-DB, store, and graph paths (plus registered project id) that
/// distinguish one owned project store from every other mount.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StoreOwnerKey {
    pub profile_root: PathBuf,
    pub global_db_path: PathBuf,
    pub project_id: Option<String>,
    pub store_root: PathBuf,
    pub graph_db_path: PathBuf,
}
