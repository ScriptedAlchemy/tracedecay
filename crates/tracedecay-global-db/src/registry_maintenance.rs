use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::{
    CodeProjectRecord, GraphScopeUpsert, ProjectRegistryContext, RegisteredGlobalDb,
    RegisteredGlobalDbWriteTransaction, StoreArtifactUpsert, StoreInstanceUpsert,
};
use tracedecay_runtime_core::branch_meta;
use tracedecay_runtime_core::storage::{
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StoreKind,
    read_repository_identity_marker, read_store_manifest, validate_project_id,
};

mod lifecycle;
mod orphan;

pub use lifecycle::*;
pub use orphan::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryOrphanRelinkStatus {
    Eligible,
    Blocked,
    Stale,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryOrphanProjectPlan {
    pub project_id: String,
    pub project_root: PathBuf,
    pub aliases: Vec<PathBuf>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryOrphanRelinkPlan {
    pub manifest_path: PathBuf,
    pub status: RegistryOrphanRelinkStatus,
    pub status_reason: Option<String>,
    pub project: RegistryOrphanProjectPlan,
    pub store: StoreInstanceUpsert,
    pub graph_scopes: Vec<GraphScopeUpsert>,
    pub artifacts: Vec<StoreArtifactUpsert>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RegistryOrphanRelinkReport {
    pub plans: Vec<RegistryOrphanRelinkPlan>,
    pub issues: Vec<String>,
}

/// Canonical read-only plan returned by the daemon and consumed by the
/// `registry-gc` CLI. Apply fills only the deletion counters after executing
/// the same plan under the active database mutation authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryGcReport {
    pub apply: bool,
    pub prefix: Option<String>,
    pub candidate_count: usize,
    pub metadata_candidate_count: usize,
    pub code_project_candidate_count: usize,
    pub storage_project_candidate_count: usize,
    pub protected_code_project_count: usize,
    pub deleted_count: usize,
    pub deleted_code_project_count: usize,
    pub deleted_storage_project_count: usize,
    pub candidate_paths: Vec<String>,
    pub candidates: Vec<CodeProjectRecord>,
    pub protected_code_projects: Vec<CodeProjectRecord>,
    pub storage_project_candidates: Vec<PathBuf>,
}

impl RegistryGcReport {
    pub fn record_deletions(&mut self, code_projects: usize, storage_projects: usize) {
        self.apply = true;
        self.deleted_code_project_count = code_projects;
        self.deleted_storage_project_count = storage_projects;
        self.deleted_count = code_projects.saturating_add(storage_projects);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

#[cfg(test)]
fn graph_scope_location_drift_is_repairable(existing: &str, expected: &GraphScopeUpsert) -> bool {
    serde_json::from_str::<(String, String, String, String, Option<String>)>(existing).is_ok_and(
        |(project_id, store_id, branch_name, _, _)| {
            project_id == expected.project_id
                && store_id == expected.store_id
                && branch_name == expected.branch_name
        },
    )
}
