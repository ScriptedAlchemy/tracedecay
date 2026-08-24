use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::{
    CodeProjectRecord, GraphScopeUpsert, ProjectRegistryContext, RegisteredGlobalDb,
    RegisteredGlobalDbWriteTransaction, StoreArtifactUpsert, StoreInstanceUpsert,
};
use tracedecay_runtime_core::branch_meta;
use tracedecay_runtime_core::db::engine::{Executor, IntoParams, QueryExecutor, params};
use tracedecay_runtime_core::storage::{
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind,
    read_legacy_enrollment_marker, read_repository_identity_marker, read_store_manifest,
    validate_project_id,
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

impl RegistryOrphanRelinkReport {
    pub fn status_count(&self, status: RegistryOrphanRelinkStatus) -> usize {
        self.plans
            .iter()
            .filter(|plan| plan.status == status)
            .count()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RegistryOrphanRelinkApplyReport {
    pub projects: usize,
    pub aliases: usize,
    pub stores: usize,
    pub graph_scopes: usize,
    pub artifacts: usize,
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

/// Read-only view of eligible reconstruction plans that would insert at least
/// one registry row. This uses the same conflict preflight as apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryOrphanRelinkDiffReport {
    pub missing_plans: usize,
    pub issues: Vec<String>,
}

pub async fn diff_registry_orphan_relink_report(
    db: &RegisteredGlobalDb,
    report: &RegistryOrphanRelinkReport,
) -> RegistryOrphanRelinkDiffReport {
    let mut diff = RegistryOrphanRelinkDiffReport {
        issues: report.issues.clone(),
        ..RegistryOrphanRelinkDiffReport::default()
    };
    let mut eligible = Vec::new();
    let snapshot = match db.read_snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            diff.issues.push(format!(
                "could not snapshot registry orphan relink state: {error}"
            ));
            return diff;
        }
    };

    for plan in report
        .plans
        .iter()
        .filter(|plan| plan.status == RegistryOrphanRelinkStatus::Eligible)
    {
        let single = RegistryOrphanRelinkReport {
            plans: vec![plan.clone()],
            issues: Vec::new(),
        };
        let issues = preflight_registry_orphan_relink(&snapshot, &single).await;
        if !issues.is_empty() {
            diff.issues.extend(issues);
            continue;
        }
        eligible.push(plan);
    }

    let mut conflicts = vec![false; eligible.len()];
    for left in 0..eligible.len() {
        for right in (left + 1)..eligible.len() {
            let pair = RegistryOrphanRelinkReport {
                plans: vec![eligible[left].clone(), eligible[right].clone()],
                issues: Vec::new(),
            };
            let issues = preflight_registry_orphan_relink(&snapshot, &pair).await;
            if issues.is_empty() {
                continue;
            }
            conflicts[left] = true;
            conflicts[right] = true;
            for issue in issues {
                if !diff.issues.contains(&issue) {
                    diff.issues.push(issue);
                }
            }
        }
    }

    for (index, plan) in eligible.into_iter().enumerate() {
        if conflicts[index] {
            continue;
        }
        match registry_orphan_relink_has_missing_rows(&snapshot, plan).await {
            Ok(true) => diff.missing_plans += 1,
            Ok(false) => {}
            Err(issue) => diff.issues.push(issue),
        }
    }
    diff
}

async fn registry_orphan_relink_has_missing_rows<Q>(
    conn: &Q,
    plan: &RegistryOrphanRelinkPlan,
) -> std::result::Result<bool, String>
where
    Q: QueryExecutor + ?Sized,
{
    let project = &plan.project;
    let root = RegisteredGlobalDb::canonical_project_key(&project.project_root);
    if query_optional_text(
        conn,
        "SELECT canonical_root FROM code_projects WHERE project_id=?1",
        params![project.project_id.as_str()],
    )
    .await?
    .as_deref()
        != Some(root.as_str())
    {
        return Ok(true);
    }
    for alias in &project.aliases {
        if query_optional_text(
            conn,
            "SELECT project_id FROM project_aliases WHERE alias_path=?1",
            params![RegisteredGlobalDb::project_path_alias_key(alias)],
        )
        .await?
        .as_deref()
            != Some(project.project_id.as_str())
        {
            return Ok(true);
        }
    }

    let store_identity = serde_json::to_string(&(
        &plan.store.project_id,
        &plan.store.store_kind,
        &plan.store.storage_mode,
        &plan.store.store_relpath,
        &plan.store.manifest_relpath,
    ))
    .unwrap_or_default();
    if query_optional_text(
        conn,
        "SELECT json_array(project_id, store_kind, storage_mode, store_relpath, manifest_relpath)
         FROM store_instances WHERE store_id=?1",
        params![plan.store.store_id.as_str()],
    )
    .await?
    .as_deref()
        != Some(store_identity.as_str())
    {
        return Ok(true);
    }

    for scope in &plan.graph_scopes {
        let scope_identity = serde_json::to_string(&(
            &scope.project_id,
            &scope.store_id,
            &scope.branch_name,
            &scope.db_relpath,
            &scope.parent_scope_id,
        ))
        .unwrap_or_default();
        if query_optional_text(
            conn,
            "SELECT json_array(project_id, store_id, branch_name, db_relpath, parent_scope_id)
             FROM graph_scopes WHERE graph_scope_id=?1",
            params![scope.graph_scope_id.as_str()],
        )
        .await?
        .as_deref()
            != Some(scope_identity.as_str())
        {
            return Ok(true);
        }
    }
    for artifact in &plan.artifacts {
        if query_optional_text(
            conn,
            "SELECT store_id FROM store_artifacts
             WHERE store_id=?1 AND artifact_kind=?2 AND relpath=?3",
            params![
                artifact.store_id.as_str(),
                artifact.artifact_kind.as_str(),
                artifact.relpath.as_str(),
            ],
        )
        .await?
        .is_none()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn apply_registry_orphan_relink_report(
    db: &RegisteredGlobalDb,
    report: &RegistryOrphanRelinkReport,
) -> std::result::Result<RegistryOrphanRelinkApplyReport, Vec<String>> {
    let transaction = db.begin_write_transaction().await.map_err(|error| {
        vec![format!(
            "could not start atomic registry orphan relink: {error}"
        )]
    })?;
    let issues = preflight_registry_orphan_relink(&transaction, report).await;
    if !issues.is_empty() {
        return Err(issues);
    }
    let applied = apply_registry_orphan_relink_rows(&transaction, report)
        .await
        .map_err(|issue| vec![issue])?;
    transaction.commit().await.map_err(|error| {
        vec![format!(
            "could not commit atomic registry orphan relink: {error}"
        )]
    })?;
    Ok(applied)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;

pub async fn apply_single_registry_orphan_relink_report(
    db: &RegisteredGlobalDb,
    report: &RegistryOrphanRelinkReport,
) -> std::result::Result<RegistryOrphanRelinkApplyReport, Vec<String>> {
    let [plan] = report.plans.as_slice() else {
        return Err(vec![format!(
            "migration cutover requires exactly one registry orphan relink plan, found {}",
            report.plans.len()
        )]);
    };
    if plan.status != RegistryOrphanRelinkStatus::Eligible {
        return Err(vec![format!(
            "migration cutover registry orphan relink plan for '{}' is {:?}: {}",
            plan.project.project_id,
            plan.status,
            plan.status_reason.as_deref().unwrap_or("not eligible")
        )]);
    }
    apply_registry_orphan_relink_report(db, report).await
}

async fn preflight_registry_orphan_relink<Q>(
    conn: &Q,
    report: &RegistryOrphanRelinkReport,
) -> Vec<String>
where
    Q: QueryExecutor + ?Sized,
{
    let mut issues = report.issues.clone();
    let mut project_roots = BTreeMap::<String, String>::new();
    let mut aliases = BTreeMap::<String, String>::new();
    let mut stores = BTreeMap::<String, String>::new();
    let mut store_paths = BTreeMap::<String, String>::new();
    let mut scopes = BTreeMap::<String, String>::new();
    let mut scope_paths = BTreeMap::<String, String>::new();

    for plan in &report.plans {
        match plan.status {
            RegistryOrphanRelinkStatus::Eligible => {}
            RegistryOrphanRelinkStatus::Stale | RegistryOrphanRelinkStatus::Retired => {
                continue;
            }
            RegistryOrphanRelinkStatus::Blocked => {
                issues.push(format!(
                    "{} reconstruction plan for '{}' is blocked: {}",
                    plan.manifest_path.display(),
                    plan.project.project_id,
                    plan.status_reason.as_deref().unwrap_or("not eligible")
                ));
                continue;
            }
        }
        let project = &plan.project;
        let root = RegisteredGlobalDb::canonical_project_key(&project.project_root);
        let root_alias = RegisteredGlobalDb::project_path_alias_key(&project.project_root);
        record_batch_owner(
            &mut project_roots,
            &root_alias,
            &project.project_id,
            "canonical project root",
            &mut issues,
        );
        match query_optional_text(
            conn,
            "SELECT canonical_root FROM code_projects WHERE project_id=?1",
            params![project.project_id.as_str()],
        )
        .await
        {
            Ok(Some(existing)) if existing != root => issues.push(format!(
                "project '{}' already owns canonical root '{}' instead of '{}'",
                project.project_id, existing, root
            )),
            Err(error) => issues.push(error),
            _ => {}
        }
        match query_all_text(
            conn,
            "SELECT project_id FROM project_aliases WHERE alias_path=?1",
            params![root_alias.as_str()],
        )
        .await
        {
            Ok(owners) => {
                for owner in owners {
                    if owner != project.project_id {
                        issues.push(format!(
                            "canonical root '{root}' is already owned by project '{owner}'"
                        ));
                    }
                }
            }
            Err(error) => issues.push(error),
        }
        for alias in &project.aliases {
            let alias = RegisteredGlobalDb::project_path_alias_key(alias);
            record_batch_owner(
                &mut aliases,
                &alias,
                &project.project_id,
                "project alias",
                &mut issues,
            );
            match query_optional_text(
                conn,
                "SELECT project_id FROM project_aliases WHERE alias_path=?1",
                params![alias.as_str()],
            )
            .await
            {
                Ok(Some(owner)) if owner != project.project_id => issues.push(format!(
                    "alias '{alias}' is already owned by project '{owner}'"
                )),
                Err(error) => issues.push(error),
                _ => {}
            }
        }

        let store_identity = serde_json::to_string(&(
            &plan.store.project_id,
            &plan.store.store_kind,
            &plan.store.storage_mode,
            &plan.store.store_relpath,
            &plan.store.manifest_relpath,
        ))
        .unwrap_or_default();
        record_batch_owner(
            &mut stores,
            &plan.store.store_id,
            &store_identity,
            "store id",
            &mut issues,
        );
        match query_optional_text(
            conn,
            "SELECT json_array(project_id, store_kind, storage_mode, store_relpath, manifest_relpath)
             FROM store_instances WHERE store_id=?1",
            params![plan.store.store_id.as_str()],
        )
        .await
        {
            Ok(Some(existing)) if existing != store_identity => issues.push(format!(
                "store '{}' already has conflicting ownership or location",
                plan.store.store_id
            )),
            Err(error) => issues.push(error),
            _ => {}
        }
        for physical_path in std::iter::once(plan.store.store_relpath.as_str())
            .chain(plan.store.manifest_relpath.as_deref())
        {
            record_batch_owner(
                &mut store_paths,
                physical_path,
                &plan.store.store_id,
                "physical store path",
                &mut issues,
            );
            match query_all_text(
                conn,
                "SELECT store_id FROM store_instances
                 WHERE store_relpath=?1 OR manifest_relpath=?1",
                params![physical_path],
            )
            .await
            {
                Ok(owners) => {
                    for owner in owners {
                        if owner != plan.store.store_id {
                            issues.push(format!(
                                "physical store path '{physical_path}' is already owned by store '{owner}'"
                            ));
                        }
                    }
                }
                Err(error) => issues.push(error),
            }
        }

        for scope in &plan.graph_scopes {
            let scope_identity = serde_json::to_string(&(
                &scope.project_id,
                &scope.store_id,
                &scope.branch_name,
                &scope.db_relpath,
                &scope.parent_scope_id,
            ))
            .unwrap_or_default();
            record_batch_owner(
                &mut scopes,
                &scope.graph_scope_id,
                &scope_identity,
                "graph scope id",
                &mut issues,
            );
            match query_optional_text(
                conn,
                "SELECT json_array(project_id, store_id, branch_name, db_relpath, parent_scope_id)
                 FROM graph_scopes WHERE graph_scope_id=?1",
                params![scope.graph_scope_id.as_str()],
            )
            .await
            {
                Ok(Some(existing))
                    if existing != scope_identity
                        && !graph_scope_location_drift_is_repairable(&existing, scope) =>
                {
                    issues.push(format!(
                        "graph scope '{}' already has conflicting ownership",
                        scope.graph_scope_id
                    ));
                }
                Err(error) => issues.push(error),
                _ => {}
            }
            record_batch_owner(
                &mut scope_paths,
                &scope.db_relpath,
                &scope.graph_scope_id,
                "physical graph database path",
                &mut issues,
            );
            match query_all_text(
                conn,
                "SELECT graph_scope_id FROM graph_scopes WHERE db_relpath=?1",
                params![scope.db_relpath.as_str()],
            )
            .await
            {
                Ok(owners) => {
                    for owner in owners {
                        if owner != scope.graph_scope_id {
                            issues.push(format!(
                                "physical graph database path '{}' is already owned by scope '{}'",
                                scope.db_relpath, owner
                            ));
                        }
                    }
                }
                Err(error) => issues.push(error),
            }
        }
    }
    issues
}

fn record_batch_owner(
    owners: &mut BTreeMap<String, String>,
    key: &str,
    owner: &str,
    label: &str,
    issues: &mut Vec<String>,
) {
    if let Some(existing) = owners.insert(key.to_string(), owner.to_string())
        && existing != owner
    {
        issues.push(format!(
            "{label} '{key}' has conflicting batch owners '{existing}' and '{owner}'"
        ));
    }
}

fn graph_scope_location_drift_is_repairable(existing: &str, expected: &GraphScopeUpsert) -> bool {
    serde_json::from_str::<(String, String, String, String, Option<String>)>(existing).is_ok_and(
        |(project_id, store_id, branch_name, _, _)| {
            project_id == expected.project_id
                && store_id == expected.store_id
                && branch_name == expected.branch_name
        },
    )
}

async fn query_optional_text<Q, P>(
    conn: &Q,
    sql: &str,
    params: P,
) -> std::result::Result<Option<String>, String>
where
    Q: QueryExecutor + ?Sized,
    P: IntoParams,
{
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| format!("registry orphan relink preflight query failed: {error}"))?;
    rows.next()
        .await
        .map_err(|error| format!("registry orphan relink preflight row failed: {error}"))?
        .map(|row| {
            row.get::<String>(0)
                .map_err(|error| format!("registry orphan relink preflight value failed: {error}"))
        })
        .transpose()
}

async fn query_all_text<Q, P>(
    conn: &Q,
    sql: &str,
    params: P,
) -> std::result::Result<Vec<String>, String>
where
    Q: QueryExecutor + ?Sized,
    P: IntoParams,
{
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|error| format!("registry orphan relink preflight query failed: {error}"))?;
    let mut values = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("registry orphan relink preflight row failed: {error}"))?
    {
        values.push(
            row.get::<String>(0).map_err(|error| {
                format!("registry orphan relink preflight value failed: {error}")
            })?,
        );
    }
    Ok(values)
}

async fn apply_registry_orphan_relink_rows<E>(
    conn: &E,
    report: &RegistryOrphanRelinkReport,
) -> std::result::Result<RegistryOrphanRelinkApplyReport, String>
where
    E: Executor + ?Sized,
{
    let mut applied = RegistryOrphanRelinkApplyReport::default();
    let now = tracedecay_runtime_core::tracedecay::current_timestamp();
    for plan in &report.plans {
        if plan.status != RegistryOrphanRelinkStatus::Eligible {
            continue;
        }
        let project = &plan.project;
        let canonical_root = RegisteredGlobalDb::canonical_project_key(&project.project_root);
        applied.projects += usize::try_from(
            conn.execute(
                "INSERT OR IGNORE INTO code_projects(
                     project_id, canonical_root, display_root, git_common_dir, git_remote_url,
                     default_branch, created_at, last_seen_at
                 ) VALUES(?1, ?2, ?3, NULL, NULL, ?4, ?5, ?5)",
                params![
                    project.project_id.as_str(),
                    canonical_root,
                    project.project_root.to_string_lossy().to_string(),
                    project.default_branch.as_deref(),
                    now,
                ],
            )
            .await
            .map_err(|error| format!("failed to insert code project: {error}"))?,
        )
        .unwrap_or(usize::MAX);
        for alias in &project.aliases {
            applied.aliases += usize::try_from(
                conn.execute(
                    "INSERT OR IGNORE INTO project_aliases(alias_path, project_id, last_seen_at)
                     VALUES(?1, ?2, ?3)",
                    params![
                        RegisteredGlobalDb::project_path_alias_key(alias),
                        project.project_id.as_str(),
                        now,
                    ],
                )
                .await
                .map_err(|error| format!("failed to insert project alias: {error}"))?,
            )
            .unwrap_or(usize::MAX);
        }
        applied.stores += usize::try_from(
            conn.execute(
                "INSERT OR IGNORE INTO store_instances(
                     store_id, project_id, store_kind, storage_mode, store_relpath,
                     manifest_relpath, created_at, last_verified_at, last_write_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    plan.store.store_id.as_str(),
                    plan.store.project_id.as_str(),
                    plan.store.store_kind.as_str(),
                    plan.store.storage_mode.as_str(),
                    plan.store.store_relpath.as_str(),
                    plan.store.manifest_relpath.as_deref(),
                    now,
                    plan.store.last_verified_at,
                    plan.store.last_write_at,
                ],
            )
            .await
            .map_err(|error| format!("failed to insert store instance: {error}"))?,
        )
        .unwrap_or(usize::MAX);
        for scope in &plan.graph_scopes {
            applied.graph_scopes += usize::try_from(
                conn.execute(
                    "INSERT INTO graph_scopes(
                         graph_scope_id, project_id, store_id, branch_name, db_relpath,
                         parent_scope_id, last_synced_at, writable
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(graph_scope_id) DO UPDATE SET
                         project_id = excluded.project_id,
                         store_id = excluded.store_id,
                         branch_name = excluded.branch_name,
                         db_relpath = excluded.db_relpath,
                         parent_scope_id = excluded.parent_scope_id,
                         last_synced_at = excluded.last_synced_at,
                         writable = excluded.writable",
                    params![
                        scope.graph_scope_id.as_str(),
                        scope.project_id.as_str(),
                        scope.store_id.as_str(),
                        scope.branch_name.as_str(),
                        scope.db_relpath.as_str(),
                        scope.parent_scope_id.as_deref(),
                        scope.last_synced_at,
                        i64::from(scope.writable),
                    ],
                )
                .await
                .map_err(|error| format!("failed to insert graph scope: {error}"))?,
            )
            .unwrap_or(usize::MAX);
        }
        for artifact in &plan.artifacts {
            applied.artifacts += usize::try_from(
                conn.execute(
                    "INSERT OR IGNORE INTO store_artifacts(
                         store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        artifact.store_id.as_str(),
                        artifact.artifact_kind.as_str(),
                        artifact.relpath.as_str(),
                        artifact.size_bytes,
                        artifact.schema_version.as_deref(),
                        artifact.updated_at,
                    ],
                )
                .await
                .map_err(|error| format!("failed to insert store artifact: {error}"))?,
            )
            .unwrap_or(usize::MAX);
        }
    }
    Ok(applied)
}
