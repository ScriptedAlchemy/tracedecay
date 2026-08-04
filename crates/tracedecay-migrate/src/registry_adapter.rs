//! Narrow root-owned global-registry boundary.

use libsql::Connection;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodeProjectRecord {
    pub project_id: String,
    pub canonical_root: String,
    pub display_root: String,
    pub git_common_dir: Option<String>,
    pub git_remote_url: Option<String>,
    pub default_branch: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectAliasRecord {
    pub alias_path: String,
    pub project_id: String,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectRegistryContext {
    pub project: CodeProjectRecord,
    pub aliases: Vec<ProjectAliasRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreInstanceUpsert {
    pub store_id: String,
    pub project_id: String,
    pub store_kind: String,
    pub storage_mode: String,
    pub store_relpath: String,
    pub manifest_relpath: Option<String>,
    pub last_verified_at: Option<i64>,
    pub last_write_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GraphScopeUpsert {
    pub graph_scope_id: String,
    pub project_id: String,
    pub store_id: String,
    pub branch_name: String,
    pub db_relpath: String,
    pub parent_scope_id: Option<String>,
    pub last_synced_at: Option<i64>,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StoreArtifactUpsert {
    pub store_id: String,
    pub artifact_kind: String,
    pub relpath: String,
    pub size_bytes: Option<i64>,
    pub schema_version: Option<String>,
    pub updated_at: Option<i64>,
}

#[allow(async_fn_in_trait)]
pub trait RegistryDatabase {
    fn conn(&self) -> &Connection;

    async fn get_code_project(&self, project_id: &str) -> Option<CodeProjectRecord>;

    async fn delete_code_projects(&self, project_ids: &[String]) -> usize;

    async fn project_registry_context_by_alias(
        &self,
        alias_path: &Path,
    ) -> Option<ProjectRegistryContext>;

    async fn upsert_code_project(
        &self,
        project_id: &str,
        project_root: &Path,
        git_common_dir: Option<&Path>,
        git_remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Option<CodeProjectRecord>;

    async fn upsert_project_alias(&self, alias_path: &Path, project_id: &str) -> bool;

    async fn upsert_store_instance(&self, upsert: StoreInstanceUpsert) -> bool;

    async fn upsert_graph_scope(&self, upsert: GraphScopeUpsert) -> bool;

    async fn upsert_store_artifact(&self, upsert: StoreArtifactUpsert) -> bool;

    async fn ensure_token_count_cache(&self) -> bool;

    async fn checkpoint(&self);
}

#[allow(async_fn_in_trait)]
pub trait RegistryRuntime {
    type Database: RegistryDatabase;

    async fn open_at(&self, path: &Path) -> Option<Self::Database>;

    async fn open_read_only_at(&self, path: &Path) -> Option<Self::Database>;

    fn fail_registry_retirement_once(&self, _profile_root: &Path) -> bool {
        false
    }
}

pub fn canonical_project_key(project_path: &Path) -> String {
    std::fs::canonicalize(project_path)
        .unwrap_or_else(|_| project_path.to_path_buf())
        .to_string_lossy()
        .to_string()
}
