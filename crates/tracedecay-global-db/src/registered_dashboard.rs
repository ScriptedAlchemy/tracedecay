use std::collections::BTreeMap;
use std::path::Path;

use tracedecay_runtime_core::db::engine::{ReadSnapshot, Row, Value};
use tracedecay_runtime_core::errors::TraceDecayError;

use super::{
    CodeProjectRecord, GraphScopeRecord, ProjectAliasRecord, ProjectRegistryContext,
    ProjectStoreContext, RegisteredGlobalDb, StoreArtifactRecord, StoreInstanceRecord,
};

type Result<T> = std::result::Result<T, TraceDecayError>;

fn profile_store_path_is_contained(
    profile_root: &Path,
    store_relpath: &str,
    data_root: &Path,
) -> bool {
    let relpath = Path::new(store_relpath);
    if relpath.as_os_str().is_empty()
        || relpath
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || profile_root.join(relpath) != data_root
    {
        return false;
    }
    let Ok(canonical_profile) = profile_root.canonicalize() else {
        return false;
    };
    let target_exists = data_root.exists();
    let mut existing = data_root;
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return false;
        };
        existing = parent;
    }
    existing.canonicalize().is_ok_and(|path| {
        path.starts_with(&canonical_profile) && (!target_exists || path != canonical_profile)
    })
}

impl RegisteredGlobalDb {
    pub fn canonical_project_key(project_path: &Path) -> String {
        super::project_registry::canonical_project_path(project_path)
            .to_string_lossy()
            .into_owned()
    }

    pub fn project_path_alias_key(project_path: &Path) -> String {
        super::project_registry::project_path_alias_key(project_path)
    }

    /// Reads the dashboard project registry through the retained registered
    /// runtime. Query failures stay typed so callers never mistake an
    /// unavailable registry for an empty one.
    pub async fn list_code_projects(&self, limit: usize) -> Result<Vec<CodeProjectRecord>> {
        let snapshot = self.dashboard_snapshot("list code projects").await?;
        let mut rows = snapshot
            .query(
                "SELECT project_id, canonical_root, display_root, git_common_dir,
                        git_remote_url, default_branch, created_at, last_seen_at
                 FROM code_projects
                 ORDER BY last_seen_at DESC, project_id
                 LIMIT ?1",
                [i64::try_from(limit).unwrap_or(i64::MAX)],
            )
            .await
            .map_err(|error| dashboard_error("list code projects", error))?;
        let mut projects = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| dashboard_error("read code project", error))?
        {
            projects.push(
                decode_code_project(&row)
                    .ok_or_else(|| dashboard_decode_error("decode code project registry row"))?,
            );
        }
        Ok(projects)
    }

    pub async fn list_code_projects_after(
        &self,
        after_project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeProjectRecord>> {
        let snapshot = self.dashboard_snapshot("list code projects page").await?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut rows = match after_project_id {
            Some(after_project_id) => {
                snapshot
                    .query(
                        "SELECT project_id, canonical_root, display_root, git_common_dir,
                                git_remote_url, default_branch, created_at, last_seen_at
                         FROM code_projects
                         WHERE project_id > ?1
                         ORDER BY project_id
                         LIMIT ?2",
                        tracedecay_runtime_core::db::engine::params![after_project_id, limit],
                    )
                    .await
            }
            None => {
                snapshot
                    .query(
                        "SELECT project_id, canonical_root, display_root, git_common_dir,
                                git_remote_url, default_branch, created_at, last_seen_at
                         FROM code_projects
                         ORDER BY project_id
                         LIMIT ?1",
                        [limit],
                    )
                    .await
            }
        }
        .map_err(|error| dashboard_error("list code projects page", error))?;
        let mut projects = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| dashboard_error("read code project page", error))?
        {
            projects.push(
                decode_code_project(&row)
                    .ok_or_else(|| dashboard_decode_error("decode code project registry row"))?,
            );
        }
        Ok(projects)
    }

    pub async fn code_project_exists(&self, project_id: &str) -> Result<bool> {
        let snapshot = self
            .dashboard_snapshot("check code project registration")
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT 1 FROM code_projects WHERE project_id = ?1 LIMIT 1",
                [project_id],
            )
            .await
            .map_err(|error| dashboard_error("check code project registration", error))?;
        rows.next()
            .await
            .map(|row| row.is_some())
            .map_err(|error| dashboard_error("read code project registration", error))
    }

    pub async fn project_registry_context_by_id(
        &self,
        project_id: &str,
    ) -> Result<Option<ProjectRegistryContext>> {
        let snapshot = self
            .dashboard_snapshot("read project registry context")
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT project_id, canonical_root, display_root, git_common_dir,
                        git_remote_url, default_branch, created_at, last_seen_at
                 FROM code_projects
                 WHERE project_id = ?1",
                tracedecay_runtime_core::db::engine::params![project_id],
            )
            .await
            .map_err(|error| dashboard_error("read project registry context", error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| dashboard_error("read project registry row", error))?
        else {
            return Ok(None);
        };
        let project = decode_code_project(&row)
            .ok_or_else(|| dashboard_decode_error("decode project registry row"))?;
        let mut contexts = contexts_for_projects(&snapshot, std::slice::from_ref(&project)).await?;
        Ok(contexts.pop())
    }

    pub async fn project_registry_contexts_for_projects(
        &self,
        projects: &[CodeProjectRecord],
    ) -> Result<Vec<ProjectRegistryContext>> {
        let snapshot = self
            .dashboard_snapshot("read project registry contexts")
            .await?;
        contexts_for_projects(&snapshot, projects).await
    }

    pub async fn try_list_store_instances_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<StoreInstanceRecord>> {
        let snapshot = self
            .dashboard_snapshot("list project store instances")
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT store_id, project_id, store_kind, storage_mode, store_relpath,
                        manifest_relpath, created_at, last_verified_at, last_write_at
                 FROM store_instances
                 WHERE project_id = ?1
                 ORDER BY store_id",
                tracedecay_runtime_core::db::engine::params![project_id],
            )
            .await
            .map_err(|error| dashboard_error("list project store instances", error))?;
        let mut stores = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| dashboard_error("read project store instance", error))?
        {
            stores.push(
                decode_store(&row)
                    .ok_or_else(|| dashboard_decode_error("decode project store instance"))?,
            );
        }
        Ok(stores)
    }

    /// Transfers one exact store identity to the registered project at
    /// `live_root`. The database move is one transaction; the manifest is
    /// written before commit and restored if commit fails, so retries can
    /// safely resume either side of an interrupted filesystem/database pair.
    #[allow(clippy::too_many_arguments)]
    pub async fn relink_orphan_store_instance(
        &self,
        source_project_id: &str,
        store_id: &str,
        live_root: &Path,
        profile_root: &Path,
        data_root: &Path,
        expected_store_relpath: &str,
        expected_created_at: i64,
        expected_last_write_at: Option<i64>,
        expected_manifest_bytes: Option<&[u8]>,
    ) -> Result<bool> {
        if !profile_store_path_is_contained(profile_root, expected_store_relpath, data_root) {
            return Err(dashboard_message(
                "relink orphan store instance",
                "store data root is outside the registered profile",
            ));
        }
        if !live_root.exists() {
            return Err(dashboard_message(
                "relink orphan store instance",
                "registered relink target is no longer live",
            ));
        }
        let manifest_path =
            data_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
        let original_manifest_bytes =
            std::fs::read(&manifest_path).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to snapshot store manifest '{}': {error}",
                    manifest_path.display()
                ),
            })?;
        if expected_manifest_bytes != Some(original_manifest_bytes.as_slice()) {
            return Err(dashboard_message(
                "relink orphan store instance",
                "store manifest changed after orphan classification",
            ));
        }
        let original_manifest = serde_json::from_slice::<
            tracedecay_runtime_core::storage::StoreManifest,
        >(&original_manifest_bytes)
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to parse store manifest '{}': {error}",
                manifest_path.display()
            ),
        })?;
        if original_manifest.data_root != data_root {
            return Err(dashboard_message(
                "relink orphan store instance",
                "store manifest data root does not match the requested store",
            ));
        }
        if original_manifest.project_root != live_root {
            return Err(dashboard_message(
                "relink orphan store instance",
                "store manifest project root changed before relink",
            ));
        }

        let transaction = self.begin_write_transaction().await?;
        let target_project_id = resolve_exact_project_at_root(&transaction, live_root).await?;
        let Some(target_project_id) = target_project_id else {
            transaction
                .rollback()
                .await
                .map_err(|error| dashboard_error("rollback unregistered orphan relink", error))?;
            return Ok(false);
        };
        if target_project_id == source_project_id {
            transaction
                .rollback()
                .await
                .map_err(|error| dashboard_error("rollback no-op orphan relink", error))?;
            return Ok(false);
        }
        if original_manifest.project_id.as_deref() != Some(source_project_id)
            && original_manifest.project_id.as_deref() != Some(target_project_id.as_str())
        {
            transaction
                .rollback()
                .await
                .map_err(|error| dashboard_error("rollback stale orphan relink", error))?;
            return Err(dashboard_message(
                "relink orphan store instance",
                "store manifest project identity changed before relink",
            ));
        }

        let Some(store) = load_exact_store(&transaction, source_project_id, store_id).await? else {
            transaction
                .rollback()
                .await
                .map_err(|error| dashboard_error("rollback raced orphan relink", error))?;
            return Ok(false);
        };
        if store.store_relpath != expected_store_relpath
            || store.created_at != expected_created_at
            || store.last_write_at != expected_last_write_at
        {
            transaction
                .rollback()
                .await
                .map_err(|error| dashboard_error("rollback changed orphan relink", error))?;
            return Ok(false);
        }
        let graph_scopes = load_store_graph_scopes(&transaction, store_id).await?;
        let artifacts = load_store_artifacts(&transaction, store_id).await?;

        let deleted = transaction
            .execute(
                "DELETE FROM store_instances
                 WHERE store_id = ?1 AND project_id = ?2",
                tracedecay_runtime_core::db::engine::params![store_id, source_project_id],
            )
            .await
            .map_err(|error| dashboard_error("remove prior orphan store identity", error))?;
        if deleted != 1 {
            transaction
                .rollback()
                .await
                .map_err(|error| dashboard_error("rollback ambiguous orphan relink", error))?;
            return Err(dashboard_message(
                "relink orphan store instance",
                "orphan store identity was not unique",
            ));
        }
        transaction
            .execute(
                "INSERT INTO store_instances (
                    store_id, project_id, store_kind, storage_mode, store_relpath,
                    manifest_relpath, created_at, last_verified_at, last_write_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                tracedecay_runtime_core::db::engine::params![
                    store.store_id,
                    target_project_id.as_str(),
                    store.store_kind,
                    store.storage_mode,
                    store.store_relpath,
                    store.manifest_relpath,
                    store.created_at,
                    store.last_verified_at,
                    store.last_write_at
                ],
            )
            .await
            .map_err(|error| dashboard_error("insert transferred orphan store identity", error))?;
        for scope in graph_scopes {
            transaction
                .execute(
                    "INSERT INTO graph_scopes (
                        graph_scope_id, project_id, store_id, branch_name, db_relpath,
                        parent_scope_id, last_synced_at, writable
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    tracedecay_runtime_core::db::engine::params![
                        scope.graph_scope_id,
                        target_project_id.as_str(),
                        scope.store_id,
                        scope.branch_name,
                        scope.db_relpath,
                        scope.parent_scope_id,
                        scope.last_synced_at,
                        i64::from(scope.writable)
                    ],
                )
                .await
                .map_err(|error| dashboard_error("restore transferred graph scope", error))?;
        }
        for artifact in artifacts {
            transaction
                .execute(
                    "INSERT INTO store_artifacts (
                        store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    tracedecay_runtime_core::db::engine::params![
                        artifact.store_id,
                        artifact.artifact_kind,
                        artifact.relpath,
                        artifact.size_bytes,
                        artifact.schema_version,
                        artifact.updated_at
                    ],
                )
                .await
                .map_err(|error| dashboard_error("restore transferred store artifact", error))?;
        }
        transaction
            .execute(
                "DELETE FROM code_projects
                 WHERE project_id = ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM store_instances WHERE project_id = ?1
                   )",
                tracedecay_runtime_core::db::engine::params![source_project_id],
            )
            .await
            .map_err(|error| dashboard_error("retire empty orphan project identity", error))?;

        let mut relinked_manifest = original_manifest.clone();
        relinked_manifest.project_id = Some(target_project_id);
        relinked_manifest.project_root = live_root.to_path_buf();
        let current_manifest_bytes =
            std::fs::read(&manifest_path).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to recheck store manifest '{}': {error}",
                    manifest_path.display()
                ),
            })?;
        if current_manifest_bytes != original_manifest_bytes {
            transaction
                .rollback()
                .await
                .map_err(|error| dashboard_error("rollback raced orphan manifest relink", error))?;
            return Err(dashboard_message(
                "relink orphan store instance",
                "store manifest changed before relink",
            ));
        }
        let relinked_manifest_bytes =
            serde_json::to_string_pretty(&relinked_manifest).map_err(|error| {
                TraceDecayError::Config {
                    message: format!(
                        "failed to serialize store manifest '{}': {error}",
                        manifest_path.display()
                    ),
                }
            })?;
        if let Err(error) = tracedecay_runtime_core::storage::write_store_manifest_to_path(
            &manifest_path,
            &relinked_manifest,
        ) {
            transaction.rollback().await.map_err(|rollback_error| {
                dashboard_message(
                    "rollback orphan relink after manifest failure",
                    format!("{error}; rollback failed: {rollback_error}"),
                )
            })?;
            return Err(error);
        }

        if let Err(error) = transaction.commit().await {
            return match std::fs::read(&manifest_path) {
                Ok(current) if current == relinked_manifest_bytes.as_bytes() => {
                    match tracedecay_runtime_core::storage::write_store_manifest_to_path(
                        &manifest_path,
                        &original_manifest,
                    ) {
                        Ok(()) => Err(dashboard_error("commit orphan store relink", error)),
                        Err(restore_error) => Err(dashboard_message(
                            "commit orphan store relink",
                            format!(
                                "{error}; restoring the prior manifest failed: {restore_error}"
                            ),
                        )),
                    }
                }
                Ok(_) => Err(dashboard_message(
                    "commit orphan store relink",
                    format!("{error}; manifest changed concurrently and was not restored"),
                )),
                Err(read_error) => Err(dashboard_message(
                    "commit orphan store relink",
                    format!("{error}; reading the manifest for rollback failed: {read_error}"),
                )),
            };
        }
        Ok(true)
    }

    async fn dashboard_snapshot(&self, operation: &'static str) -> Result<ReadSnapshot> {
        self.read_snapshot()
            .await
            .map_err(|error| dashboard_error(operation, error))
    }
}

async fn resolve_exact_project_at_root(
    transaction: &super::RegisteredGlobalDbWriteTransaction<'_>,
    live_root: &Path,
) -> Result<Option<String>> {
    let root = RegisteredGlobalDb::canonical_project_key(live_root);
    let alias = super::project_path_alias_key(live_root);
    let mut rows = transaction
        .query(
            "SELECT DISTINCT code_projects.project_id
             FROM code_projects
             LEFT JOIN project_aliases
               ON project_aliases.project_id = code_projects.project_id
             WHERE code_projects.canonical_root = ?1
                OR code_projects.display_root = ?1
                OR (
                    project_aliases.alias_path = ?2
                    AND project_aliases.last_seen_at = code_projects.last_seen_at
                )
             ORDER BY code_projects.project_id",
            tracedecay_runtime_core::db::engine::params![root, alias],
        )
        .await
        .map_err(|error| dashboard_error("resolve orphan relink target", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| dashboard_error("read orphan relink target", error))?
    else {
        return Ok(None);
    };
    let project_id = row
        .get::<String>(0)
        .map_err(|error| dashboard_error("decode orphan relink target", error))?;
    if rows
        .next()
        .await
        .map_err(|error| dashboard_error("check orphan relink target uniqueness", error))?
        .is_some()
    {
        return Err(dashboard_message(
            "resolve orphan relink target",
            "multiple registered projects match the requested live root",
        ));
    }
    Ok(Some(project_id))
}

async fn load_exact_store(
    transaction: &super::RegisteredGlobalDbWriteTransaction<'_>,
    project_id: &str,
    store_id: &str,
) -> Result<Option<StoreInstanceRecord>> {
    let mut rows = transaction
        .query(
            "SELECT store_id, project_id, store_kind, storage_mode, store_relpath,
                    manifest_relpath, created_at, last_verified_at, last_write_at
             FROM store_instances
             WHERE project_id = ?1 AND store_id = ?2",
            tracedecay_runtime_core::db::engine::params![project_id, store_id],
        )
        .await
        .map_err(|error| dashboard_error("read exact orphan store instance", error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| dashboard_error("read exact orphan store row", error))?
    else {
        return Ok(None);
    };
    decode_store(&row)
        .ok_or_else(|| dashboard_decode_error("decode exact orphan store instance"))
        .map(Some)
}

async fn load_store_graph_scopes(
    transaction: &super::RegisteredGlobalDbWriteTransaction<'_>,
    store_id: &str,
) -> Result<Vec<GraphScopeRecord>> {
    let mut rows = transaction
        .query(
            "SELECT graph_scope_id, project_id, store_id, branch_name, db_relpath,
                    parent_scope_id, last_synced_at, writable
             FROM graph_scopes
             WHERE store_id = ?1
             ORDER BY graph_scope_id",
            tracedecay_runtime_core::db::engine::params![store_id],
        )
        .await
        .map_err(|error| dashboard_error("read orphan store graph scopes", error))?;
    let mut scopes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| dashboard_error("read orphan store graph scope", error))?
    {
        scopes.push(
            decode_graph_scope(&row)
                .ok_or_else(|| dashboard_decode_error("decode orphan store graph scope"))?,
        );
    }
    Ok(scopes)
}

async fn load_store_artifacts(
    transaction: &super::RegisteredGlobalDbWriteTransaction<'_>,
    store_id: &str,
) -> Result<Vec<StoreArtifactRecord>> {
    let mut rows = transaction
        .query(
            "SELECT store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
             FROM store_artifacts
             WHERE store_id = ?1
             ORDER BY artifact_kind, relpath",
            tracedecay_runtime_core::db::engine::params![store_id],
        )
        .await
        .map_err(|error| dashboard_error("read orphan store artifacts", error))?;
    let mut artifacts = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| dashboard_error("read orphan store artifact", error))?
    {
        artifacts.push(
            decode_store_artifact(&row)
                .ok_or_else(|| dashboard_decode_error("decode orphan store artifact"))?,
        );
    }
    Ok(artifacts)
}

async fn contexts_for_projects(
    snapshot: &ReadSnapshot,
    projects: &[CodeProjectRecord],
) -> Result<Vec<ProjectRegistryContext>> {
    if projects.is_empty() {
        return Ok(Vec::new());
    }
    let project_ids = projects
        .iter()
        .map(|project| project.project_id.clone())
        .collect::<Vec<_>>();
    let mut aliases_by_project = BTreeMap::<String, Vec<ProjectAliasRecord>>::new();
    let mut rows = query_ids(
        snapshot,
        "SELECT alias_path, project_id, last_seen_at
         FROM project_aliases
         WHERE project_id IN ({})
         ORDER BY alias_path",
        &project_ids,
        "read project aliases",
    )
    .await?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| dashboard_error("read project alias", error))?
    {
        let alias = decode_project_alias(&row)
            .ok_or_else(|| dashboard_decode_error("decode project alias"))?;
        aliases_by_project
            .entry(alias.project_id.clone())
            .or_default()
            .push(alias);
    }

    let mut stores = Vec::new();
    let mut rows = query_ids(
        snapshot,
        "SELECT store_id, project_id, store_kind, storage_mode, store_relpath,
                manifest_relpath, created_at, last_verified_at, last_write_at
         FROM store_instances
         WHERE project_id IN ({})
         ORDER BY COALESCE(last_verified_at, created_at) DESC, store_id",
        &project_ids,
        "read project stores",
    )
    .await?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| dashboard_error("read project store", error))?
    {
        stores.push(
            decode_store(&row).ok_or_else(|| dashboard_decode_error("decode project store"))?,
        );
    }

    let store_ids = stores
        .iter()
        .map(|store| store.store_id.clone())
        .collect::<Vec<_>>();
    let mut graph_scopes_by_store = BTreeMap::<String, Vec<GraphScopeRecord>>::new();
    let mut artifacts_by_store = BTreeMap::<String, Vec<StoreArtifactRecord>>::new();
    if !store_ids.is_empty() {
        let mut rows = query_ids(
            snapshot,
            "SELECT graph_scope_id, project_id, store_id, branch_name, db_relpath,
                    parent_scope_id, last_synced_at, writable
             FROM graph_scopes
             WHERE store_id IN ({})
             ORDER BY branch_name, graph_scope_id",
            &store_ids,
            "read project graph scopes",
        )
        .await?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| dashboard_error("read project graph scope", error))?
        {
            let scope = decode_graph_scope(&row)
                .ok_or_else(|| dashboard_decode_error("decode project graph scope"))?;
            graph_scopes_by_store
                .entry(scope.store_id.clone())
                .or_default()
                .push(scope);
        }

        let mut rows = query_ids(
            snapshot,
            "SELECT store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
             FROM store_artifacts
             WHERE store_id IN ({})
             ORDER BY artifact_kind, relpath",
            &store_ids,
            "read project store artifacts",
        )
        .await?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| dashboard_error("read project store artifact", error))?
        {
            let artifact = decode_store_artifact(&row)
                .ok_or_else(|| dashboard_decode_error("decode project store artifact"))?;
            artifacts_by_store
                .entry(artifact.store_id.clone())
                .or_default()
                .push(artifact);
        }
    }

    let mut stores_by_project = BTreeMap::<String, Vec<ProjectStoreContext>>::new();
    for store in stores {
        stores_by_project
            .entry(store.project_id.clone())
            .or_default()
            .push(ProjectStoreContext {
                graph_scopes: graph_scopes_by_store
                    .remove(&store.store_id)
                    .unwrap_or_default(),
                artifacts: artifacts_by_store
                    .remove(&store.store_id)
                    .unwrap_or_default(),
                store,
            });
    }

    Ok(projects
        .iter()
        .cloned()
        .map(|project| ProjectRegistryContext {
            aliases: aliases_by_project
                .remove(&project.project_id)
                .unwrap_or_default(),
            stores: stores_by_project
                .remove(&project.project_id)
                .unwrap_or_default(),
            project,
        })
        .collect())
}

async fn query_ids(
    snapshot: &ReadSnapshot,
    sql_template: &str,
    ids: &[String],
    operation: &'static str,
) -> Result<tracedecay_runtime_core::db::engine::Rows> {
    let sql = sql_template.replace("{}", &vec!["?"; ids.len()].join(","));
    let params = ids.iter().cloned().map(Value::Text).collect::<Vec<_>>();
    snapshot
        .query(&sql, params)
        .await
        .map_err(|error| dashboard_error(operation, error))
}

fn decode_code_project(row: &Row) -> Option<CodeProjectRecord> {
    Some(CodeProjectRecord {
        project_id: row.get(0).ok()?,
        canonical_root: row.get(1).ok()?,
        display_root: row.get(2).ok()?,
        git_common_dir: row.get(3).ok()?,
        git_remote_url: row.get(4).ok()?,
        default_branch: row.get(5).ok()?,
        created_at: row.get(6).ok()?,
        last_seen_at: row.get(7).ok()?,
    })
}

fn decode_project_alias(row: &Row) -> Option<ProjectAliasRecord> {
    Some(ProjectAliasRecord {
        alias_path: row.get(0).ok()?,
        project_id: row.get(1).ok()?,
        last_seen_at: row.get(2).ok()?,
    })
}

fn decode_store(row: &Row) -> Option<StoreInstanceRecord> {
    Some(StoreInstanceRecord {
        store_id: row.get(0).ok()?,
        project_id: row.get(1).ok()?,
        store_kind: row.get(2).ok()?,
        storage_mode: row.get(3).ok()?,
        store_relpath: row.get(4).ok()?,
        manifest_relpath: row.get(5).ok()?,
        created_at: row.get(6).ok()?,
        last_verified_at: row.get(7).ok()?,
        last_write_at: row.get(8).ok()?,
    })
}

fn decode_graph_scope(row: &Row) -> Option<GraphScopeRecord> {
    Some(GraphScopeRecord {
        graph_scope_id: row.get(0).ok()?,
        project_id: row.get(1).ok()?,
        store_id: row.get(2).ok()?,
        branch_name: row.get(3).ok()?,
        db_relpath: row.get(4).ok()?,
        parent_scope_id: row.get(5).ok()?,
        last_synced_at: row.get(6).ok()?,
        writable: row.get::<i64>(7).ok()? != 0,
    })
}

fn decode_store_artifact(row: &Row) -> Option<StoreArtifactRecord> {
    Some(StoreArtifactRecord {
        store_id: row.get(0).ok()?,
        artifact_kind: row.get(1).ok()?,
        relpath: row.get(2).ok()?,
        size_bytes: row.get(3).ok()?,
        schema_version: row.get(4).ok()?,
        updated_at: row.get(5).ok()?,
    })
}

fn dashboard_error(operation: &'static str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_string(),
        message: error.to_string(),
    }
}

fn dashboard_decode_error(operation: &'static str) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_string(),
        message: "registered dashboard row did not match the expected schema".to_string(),
    }
}

fn dashboard_message(operation: &'static str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_string(),
        message: message.into(),
    }
}
