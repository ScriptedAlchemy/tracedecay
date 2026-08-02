use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::root_seam::global_db::{
    CodeProjectRecord, GraphScopeUpsert, ProjectRegistryContext, RegisteredGlobalDb,
    StoreArtifactUpsert, StoreInstanceUpsert,
};
use crate::root_seam::storage_adapters::try_classify_project_storage_with_registry;
use tracedecay_runtime_core::branch_meta;
use tracedecay_runtime_core::db::engine::{Executor, IntoParams, QueryExecutor, params};
use tracedecay_runtime_core::storage::{
    ProjectStorageLocation, STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode,
    StoreKind, read_enrollment_marker, read_repository_identity_marker, read_store_manifest,
    validate_project_id,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryReconstructionStatus {
    Eligible,
    Blocked,
    Stale,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryProjectPlan {
    pub project_id: String,
    pub project_root: PathBuf,
    pub aliases: Vec<PathBuf>,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegistryReconstructionPlan {
    pub manifest_path: PathBuf,
    pub status: RegistryReconstructionStatus,
    pub status_reason: Option<String>,
    pub project: RegistryProjectPlan,
    pub store: StoreInstanceUpsert,
    pub graph_scopes: Vec<GraphScopeUpsert>,
    pub artifacts: Vec<StoreArtifactUpsert>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RegistryReconstructionReport {
    pub plans: Vec<RegistryReconstructionPlan>,
    pub issues: Vec<String>,
}

impl RegistryReconstructionReport {
    pub fn status_count(&self, status: RegistryReconstructionStatus) -> usize {
        self.plans
            .iter()
            .filter(|plan| plan.status == status)
            .count()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RegistryReconstructionApplyReport {
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
pub struct RegistryReconstructionDiffReport {
    pub missing_plans: usize,
    pub issues: Vec<String>,
}

/// Opaque retained runtime for offline migration commands.
///
/// The command crate can request exact registry/session operations without
/// receiving database handles or reopening owned paths.
pub struct MigrationRegistryRuntime {
    profile_database: std::sync::Arc<RegisteredGlobalDb>,
}

impl MigrationRegistryRuntime {
    /// Mounts an existing profile registry without creating a database.
    ///
    /// Callers hold the profile's exclusive maintenance lease, so this
    /// existence check cannot race another authorized lifecycle mutation.
    pub async fn try_open_existing(
        profile_root: &Path,
    ) -> tracedecay_runtime_core::errors::Result<Option<Self>> {
        if !profile_root.try_exists().map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "inspect existing profile root".to_string(),
                message: error.to_string(),
            }
        })? {
            return Ok(None);
        }
        let profile_root = profile_root.canonicalize().map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "resolve existing profile registry".to_string(),
                message: error.to_string(),
            }
        })?;
        if !profile_root
            .join("global.db")
            .try_exists()
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "inspect existing profile registry".to_string(),
                    message: error.to_string(),
                },
            )?
        {
            return Ok(None);
        }
        Self::open(&profile_root).await.map(Some)
    }

    pub async fn open(profile_root: &Path) -> tracedecay_runtime_core::errors::Result<Self> {
        let identity = crate::root_seam::daemon::profile_identity::load_or_create(profile_root)?;
        let registry =
            crate::root_seam::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity,
            )
            .await?;
        let profile_database = registry.profile_database().await?;
        Ok(Self { profile_database })
    }

    pub async fn registered_project_paths(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
        self.profile_database
            .try_list_code_project_paths(usize::MAX)
            .await
    }

    pub async fn classify_project_storage(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> tracedecay_runtime_core::errors::Result<ProjectStorageLocation> {
        try_classify_project_storage_with_registry(
            project_root,
            self.profile_database.as_ref(),
            profile_root,
        )
        .await
    }

    pub fn canonical_project_key(project_root: &Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_root)
    }

    pub async fn delete_project_paths(
        &self,
        project_paths: &[PathBuf],
    ) -> tracedecay_runtime_core::errors::Result<usize> {
        let transaction = self.profile_database.begin_write_transaction().await?;
        let (_, deleted) =
            delete_registry_gc_candidates_in_transaction(&transaction, &[], project_paths).await?;
        transaction.commit().await?;
        Ok(deleted)
    }

    pub async fn apply_reconstruction(
        &self,
        report: &RegistryReconstructionReport,
    ) -> std::result::Result<RegistryReconstructionApplyReport, Vec<String>> {
        apply_registry_reconstruction_report(self.profile_database.as_ref(), report).await
    }

    pub async fn apply_single_reconstruction(
        &self,
        report: &RegistryReconstructionReport,
    ) -> std::result::Result<RegistryReconstructionApplyReport, Vec<String>> {
        apply_single_registry_reconstruction_report(self.profile_database.as_ref(), report).await
    }

    pub async fn registry_gc(
        &self,
        profile_root: &Path,
        prefix: Option<String>,
        apply: bool,
    ) -> tracedecay_runtime_core::errors::Result<RegistryGcReport> {
        if apply {
            apply_registry_gc(self.profile_database.as_ref(), profile_root, prefix).await
        } else {
            registry_gc_report(self.profile_database.as_ref(), profile_root, prefix).await
        }
    }
}

pub async fn diff_registry_reconstruction_report(
    db: &RegisteredGlobalDb,
    report: &RegistryReconstructionReport,
) -> RegistryReconstructionDiffReport {
    let mut diff = RegistryReconstructionDiffReport {
        issues: report.issues.clone(),
        ..RegistryReconstructionDiffReport::default()
    };
    let mut eligible = Vec::new();
    let snapshot = match db.read_snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            diff.issues.push(format!(
                "could not snapshot registry reconstruction state: {error}"
            ));
            return diff;
        }
    };

    for plan in report
        .plans
        .iter()
        .filter(|plan| plan.status == RegistryReconstructionStatus::Eligible)
    {
        let single = RegistryReconstructionReport {
            plans: vec![plan.clone()],
            issues: Vec::new(),
        };
        let issues = preflight_registry_reconstruction(&snapshot, &single).await;
        if !issues.is_empty() {
            diff.issues.extend(issues);
            continue;
        }
        eligible.push(plan);
    }

    let mut conflicts = vec![false; eligible.len()];
    for left in 0..eligible.len() {
        for right in (left + 1)..eligible.len() {
            let pair = RegistryReconstructionReport {
                plans: vec![eligible[left].clone(), eligible[right].clone()],
                issues: Vec::new(),
            };
            let issues = preflight_registry_reconstruction(&snapshot, &pair).await;
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
        match registry_plan_has_missing_rows(&snapshot, plan).await {
            Ok(true) => diff.missing_plans += 1,
            Ok(false) => {}
            Err(issue) => diff.issues.push(issue),
        }
    }
    diff
}

async fn registry_plan_has_missing_rows<Q>(
    conn: &Q,
    plan: &RegistryReconstructionPlan,
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

pub async fn apply_registry_reconstruction_report(
    db: &RegisteredGlobalDb,
    report: &RegistryReconstructionReport,
) -> std::result::Result<RegistryReconstructionApplyReport, Vec<String>> {
    let transaction = db.begin_write_transaction().await.map_err(|error| {
        vec![format!(
            "could not start atomic registry reconstruction: {error}"
        )]
    })?;
    let issues = preflight_registry_reconstruction(&transaction, report).await;
    if !issues.is_empty() {
        return Err(issues);
    }
    let applied = insert_missing_registry_rows(&transaction, report)
        .await
        .map_err(|issue| vec![issue])?;
    transaction.commit().await.map_err(|error| {
        vec![format!(
            "could not commit atomic registry reconstruction: {error}"
        )]
    })?;
    Ok(applied)
}

pub async fn apply_single_registry_reconstruction_report(
    db: &RegisteredGlobalDb,
    report: &RegistryReconstructionReport,
) -> std::result::Result<RegistryReconstructionApplyReport, Vec<String>> {
    let [plan] = report.plans.as_slice() else {
        return Err(vec![format!(
            "migration cutover requires exactly one registry reconstruction plan, found {}",
            report.plans.len()
        )]);
    };
    if plan.status != RegistryReconstructionStatus::Eligible {
        return Err(vec![format!(
            "migration cutover registry reconstruction plan for '{}' is {:?}: {}",
            plan.project.project_id,
            plan.status,
            plan.status_reason.as_deref().unwrap_or("not eligible")
        )]);
    }
    apply_registry_reconstruction_report(db, report).await
}

async fn preflight_registry_reconstruction<Q>(
    conn: &Q,
    report: &RegistryReconstructionReport,
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
            RegistryReconstructionStatus::Eligible => {}
            RegistryReconstructionStatus::Stale | RegistryReconstructionStatus::Retired => {
                continue;
            }
            RegistryReconstructionStatus::Blocked => {
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
        .map_err(|error| format!("registry reconstruction preflight query failed: {error}"))?;
    rows.next()
        .await
        .map_err(|error| format!("registry reconstruction preflight row failed: {error}"))?
        .map(|row| {
            row.get::<String>(0)
                .map_err(|error| format!("registry reconstruction preflight value failed: {error}"))
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
        .map_err(|error| format!("registry reconstruction preflight query failed: {error}"))?;
    let mut values = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("registry reconstruction preflight row failed: {error}"))?
    {
        values.push(
            row.get::<String>(0).map_err(|error| {
                format!("registry reconstruction preflight value failed: {error}")
            })?,
        );
    }
    Ok(values)
}

async fn insert_missing_registry_rows<E>(
    conn: &E,
    report: &RegistryReconstructionReport,
) -> std::result::Result<RegistryReconstructionApplyReport, String>
where
    E: Executor + ?Sized,
{
    let mut applied = RegistryReconstructionApplyReport::default();
    let now = tracedecay_runtime_core::tracedecay::current_timestamp();
    for plan in &report.plans {
        if plan.status != RegistryReconstructionStatus::Eligible {
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

/// How dead a registry row's project root must be before the row counts as
/// stale. This is the single definition of both GC scopes, so a reader never
/// has to reassemble the effective condition from scattered half-checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleRootScope {
    /// Manual `tracedecay migrate registry-gc` scope: the canonical root is
    /// gone (the user reviews candidates before applying).
    CanonicalRootMissing,
    /// Post-update auto-GC scope: both the canonical and display roots are
    /// gone — stricter, because nobody reviews the deletion.
    AllRootsMissing,
}

/// Whether a recorded root could be proven present or absent.
///
/// Deletion authority requires proof of *absence*. An inspection that fails —
/// an unreadable parent directory, a stale mount, any I/O error — proves
/// nothing, so it is [`RootLivenessV1::Unverifiable`] and never absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootLivenessV1 {
    Live,
    Absent,
    Unverifiable,
}

impl RootLivenessV1 {
    /// Whether this liveness permits retiring the identity that owns the root.
    /// Only proven absence does.
    pub fn permits_retirement(self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Combine sibling roots of one identity: any live root wins, then any
    /// unverifiable root, and absence only when every root was proven absent.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Live, _) | (_, Self::Live) => Self::Live,
            (Self::Unverifiable, _) | (_, Self::Unverifiable) => Self::Unverifiable,
            (Self::Absent, Self::Absent) => Self::Absent,
        }
    }
}

/// Probe one root with typed existence, so a permission or I/O failure is
/// reported as unverifiable rather than silently read as "gone".
pub fn probe_root(root: &Path) -> RootLivenessV1 {
    match root.try_exists() {
        Ok(true) => RootLivenessV1::Live,
        Ok(false) => RootLivenessV1::Absent,
        Err(_) => RootLivenessV1::Unverifiable,
    }
}

/// Liveness across every root one registry row records: canonical, display, and
/// the git common directory. A linked worktree shares its common directory with
/// the primary checkout, so a live common directory means the repository
/// identity is still in use even when this row's working tree is gone.
pub fn code_project_root_liveness(project: &CodeProjectRecord) -> RootLivenessV1 {
    let mut liveness = probe_root(Path::new(&project.canonical_root))
        .merge(probe_root(Path::new(&project.display_root)));
    if let Some(git_common_dir) = project.git_common_dir.as_deref() {
        liveness = liveness.merge(probe_root(Path::new(git_common_dir)));
    }
    liveness
}

/// Returns true unless every root this row records was *proven* absent.
pub fn code_project_root_exists(project: &CodeProjectRecord) -> bool {
    !code_project_root_liveness(project).permits_retirement()
}

/// Liveness of a fully-resolved registry identity: every root of the project
/// row plus every registered alias path.
///
/// A registered store instance keeps the identity live on its own. Deleting the
/// `code_projects` row cascades its aliases and store instances away, so a row
/// that still owns a store must never be retired by an unreviewed pass.
pub fn project_context_liveness(context: &ProjectRegistryContext) -> RootLivenessV1 {
    if !context.stores.is_empty() {
        return RootLivenessV1::Live;
    }
    context.aliases.iter().fold(
        code_project_root_liveness(&context.project),
        |liveness, alias| liveness.merge(probe_root(Path::new(&alias.alias_path))),
    )
}

/// Registry identities that are stale under `scope` across every root and alias
/// they record, restricted to canonical roots under one of `prefixes`.
///
/// This is the context-aware counterpart of [`stale_code_projects`]: an
/// unreviewed pass must resolve aliases and store instances before retiring an
/// identity, or it will retire a project another checkout is still using.
pub fn stale_project_contexts<'a>(
    contexts: &'a [ProjectRegistryContext],
    prefixes: &[PathBuf],
    scope: StaleRootScope,
) -> Vec<&'a ProjectRegistryContext> {
    contexts
        .iter()
        .filter(|context| {
            let canonical_root = Path::new(&context.project.canonical_root);
            prefixes.is_empty()
                || prefixes
                    .iter()
                    .any(|prefix| canonical_root.starts_with(prefix))
        })
        .filter(|context| match scope {
            StaleRootScope::CanonicalRootMissing => {
                probe_root(Path::new(&context.project.canonical_root)).permits_retirement()
                    && project_context_liveness(context).permits_retirement()
            }
            StaleRootScope::AllRootsMissing => {
                project_context_liveness(context).permits_retirement()
            }
        })
        .collect()
}

/// Filters registry rows that are stale under `scope`, restricted to
/// canonical roots under one of `prefixes` (an empty slice means no
/// restriction). Shared by `tracedecay migrate registry-gc` and the
/// post-update health pass so both agree on what counts as a GC candidate.
pub fn stale_code_projects<'a>(
    projects: &'a [CodeProjectRecord],
    prefixes: &[PathBuf],
    scope: StaleRootScope,
) -> Vec<&'a CodeProjectRecord> {
    projects
        .iter()
        .filter(|project| {
            let canonical_root = Path::new(&project.canonical_root);
            prefixes.is_empty()
                || prefixes
                    .iter()
                    .any(|prefix| canonical_root.starts_with(prefix))
        })
        .filter(|project| match scope {
            StaleRootScope::CanonicalRootMissing => {
                probe_root(Path::new(&project.canonical_root)).permits_retirement()
            }
            StaleRootScope::AllRootsMissing => !code_project_root_exists(project),
        })
        .collect()
}

/// Builds the exact registry cleanup plan without mutating registry state.
/// Both daemon-owned and offline maintenance paths must hold their mutation
/// authority before applying this plan.
pub async fn registry_gc_report(
    db: &RegisteredGlobalDb,
    _profile_root: &Path,
    prefix: Option<String>,
) -> tracedecay_runtime_core::errors::Result<RegistryGcReport> {
    let prefixes = prefix.iter().map(PathBuf::from).collect::<Vec<_>>();
    let projects = db.list_code_projects(usize::MAX).await?;
    let mut candidates = Vec::new();
    let mut protected_code_projects = Vec::new();
    for project in stale_code_projects(&projects, &prefixes, StaleRootScope::CanonicalRootMissing) {
        if db
            .try_list_store_instances_for_project(&project.project_id)
            .await?
            .is_empty()
        {
            candidates.push(project.clone());
        } else {
            protected_code_projects.push(project.clone());
        }
    }

    let mut storage_project_candidates = Vec::new();
    for project_path in db.try_list_project_paths().await? {
        if !prefixes.is_empty()
            && !prefixes
                .iter()
                .any(|prefix| project_path.starts_with(prefix))
        {
            continue;
        }
        if !project_path.exists() {
            storage_project_candidates.push(project_path);
        }
    }

    let candidate_paths = candidates
        .iter()
        .map(|project| {
            RegisteredGlobalDb::canonical_project_key(Path::new(&project.canonical_root))
        })
        .chain(
            storage_project_candidates
                .iter()
                .map(|path| RegisteredGlobalDb::canonical_project_key(path)),
        )
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    Ok(RegistryGcReport {
        apply: false,
        prefix,
        candidate_count: candidate_paths.len(),
        metadata_candidate_count: candidates.len() + storage_project_candidates.len(),
        code_project_candidate_count: candidates.len(),
        storage_project_candidate_count: storage_project_candidates.len(),
        protected_code_project_count: protected_code_projects.len(),
        deleted_count: 0,
        deleted_code_project_count: 0,
        deleted_storage_project_count: 0,
        candidate_paths,
        candidates,
        protected_code_projects,
        storage_project_candidates,
    })
}

pub async fn apply_registry_gc(
    db: &RegisteredGlobalDb,
    profile_root: &Path,
    prefix: Option<String>,
) -> tracedecay_runtime_core::errors::Result<RegistryGcReport> {
    let transaction = db.begin_write_transaction().await?;
    let mut report = registry_gc_report(db, profile_root, prefix).await?;
    for project in &report.candidates {
        if Path::new(&project.canonical_root).exists() {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!(
                    "registry cleanup candidate '{}' became live while applying the plan",
                    project.project_id
                ),
            });
        }
    }
    for project_path in &report.storage_project_candidates {
        if project_path.exists() {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Config {
                message: format!(
                    "registry cleanup candidate '{}' became live while applying the plan",
                    project_path.display()
                ),
            });
        }
    }
    let project_ids = report
        .candidates
        .iter()
        .map(|project| project.project_id.clone())
        .collect::<Vec<_>>();
    let (deleted_code_projects, deleted_storage_projects) =
        delete_registry_gc_candidates_in_transaction(
            &transaction,
            &project_ids,
            &report.storage_project_candidates,
        )
        .await?;
    transaction.commit().await?;
    report.record_deletions(deleted_code_projects, deleted_storage_projects);
    Ok(report)
}

async fn delete_registry_gc_candidates_in_transaction(
    transaction: &crate::root_seam::global_db::RegisteredGlobalDbWriteTransaction<'_>,
    project_ids: &[String],
    project_paths: &[PathBuf],
) -> tracedecay_runtime_core::errors::Result<(usize, usize)> {
    const CHUNK: usize = 256;
    let mut code_projects = 0_usize;
    for chunk in project_ids.chunks(CHUNK) {
        let sql = format!(
            "DELETE FROM code_projects WHERE project_id IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let values = chunk
            .iter()
            .cloned()
            .map(tracedecay_runtime_core::db::engine::Value::Text)
            .collect::<Vec<_>>();
        code_projects =
            code_projects.saturating_add(transaction.execute(&sql, values).await? as usize);
    }

    let mut storage_projects = 0_usize;
    for chunk in project_paths.chunks(CHUNK) {
        let sql = format!(
            "DELETE FROM projects WHERE path IN ({})",
            vec!["?"; chunk.len()].join(",")
        );
        let values = chunk
            .iter()
            .map(|path| {
                tracedecay_runtime_core::db::engine::Value::Text(
                    RegisteredGlobalDb::project_path_alias_key(path),
                )
            })
            .collect::<Vec<_>>();
        storage_projects =
            storage_projects.saturating_add(transaction.execute(&sql, values).await? as usize);
    }
    Ok((code_projects, storage_projects))
}

pub fn scan_profile_store_manifests(
    profile_root: &Path,
    verified_at: i64,
) -> RegistryReconstructionReport {
    let mut report = RegistryReconstructionReport::default();
    let projects_root = profile_root.join("projects");
    let entries = match fs::read_dir(&projects_root) {
        Ok(entries) => entries,
        Err(error) => {
            report.issues.push(format!(
                "could not read profile projects directory '{}': {error}",
                projects_root.display()
            ));
            return report;
        }
    };
    let mut manifest_paths = entries
        .flatten()
        .map(|entry| entry.path().join(STORE_MANIFEST_FILENAME))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifest_paths.sort();

    for manifest_path in manifest_paths {
        let manifest_report = reconstruct_registry_from_store_manifest_inner(
            &manifest_path,
            profile_root,
            verified_at,
            true,
        );
        report.plans.extend(manifest_report.plans);
        report.issues.extend(manifest_report.issues);
    }

    report
}

pub fn reconstruct_registry_from_store_manifest(
    manifest_path: &Path,
    profile_root: &Path,
    verified_at: i64,
) -> RegistryReconstructionReport {
    reconstruct_registry_from_store_manifest_inner(manifest_path, profile_root, verified_at, false)
}

fn reconstruct_registry_from_store_manifest_inner(
    manifest_path: &Path,
    profile_root: &Path,
    verified_at: i64,
    reject_ephemeral_root: bool,
) -> RegistryReconstructionReport {
    let mut report = RegistryReconstructionReport::default();
    let manifest = match read_store_manifest(manifest_path) {
        Ok(manifest) => manifest,
        Err(err) => {
            report.issues.push(format!(
                "could not read store manifest '{}': {err}",
                manifest_path.display()
            ));
            return report;
        }
    };

    let mut issues = validate_manifest_shape(manifest_path, profile_root, &manifest);
    let Some(project_id) = manifest.project_id.clone() else {
        issues.push(format!(
            "store manifest '{}' has no project_id",
            manifest_path.display()
        ));
        report.issues = issues;
        return report;
    };
    if let Err(message) = validate_project_id(&project_id) {
        issues.push(format!(
            "store manifest '{}' has invalid project_id '{}': {message}",
            manifest_path.display(),
            project_id
        ));
    }
    for (field, relpath) in [
        ("graph_db_relpath", &manifest.graph_db_relpath),
        ("sessions_db_relpath", &manifest.sessions_db_relpath),
        ("branch_meta_relpath", &manifest.branch_meta_relpath),
    ] {
        if !is_safe_relpath(relpath) {
            issues.push(format!(
                "store manifest '{}' has unsafe {field}: {}",
                manifest_path.display(),
                relpath.display()
            ));
        }
    }
    if !issues.is_empty() {
        report.issues = issues;
        return report;
    }

    let Some(store_relpath) = strip_profile_root(profile_root, &manifest.data_root) else {
        report.issues.push(format!(
            "store data root '{}' is outside profile root '{}'",
            manifest.data_root.display(),
            profile_root.display()
        ));
        return report;
    };
    let Some(manifest_relpath) = strip_profile_root(profile_root, manifest_path) else {
        report.issues.push(format!(
            "store manifest '{}' is outside profile root '{}'",
            manifest_path.display(),
            profile_root.display()
        ));
        return report;
    };

    let (status, status_reason, project_root) =
        classify_project_root(&manifest.project_root, &project_id, reject_ephemeral_root);
    let store_id = format!("store:{project_id}:profile_sharded");
    let mut artifacts = Vec::new();
    push_artifact_if_present(
        &mut artifacts,
        &store_id,
        "graph_db",
        &manifest.data_root.join(&manifest.graph_db_relpath),
        profile_root,
        None,
        verified_at,
    );
    push_artifact_if_present(
        &mut artifacts,
        &store_id,
        "sessions_db",
        &manifest.data_root.join(&manifest.sessions_db_relpath),
        profile_root,
        None,
        verified_at,
    );
    push_artifact_if_present(
        &mut artifacts,
        &store_id,
        "branch_meta",
        &manifest.data_root.join(&manifest.branch_meta_relpath),
        profile_root,
        None,
        verified_at,
    );
    push_artifact_if_present(
        &mut artifacts,
        &store_id,
        "store_manifest",
        manifest_path,
        profile_root,
        Some(manifest.schema_version.to_string()),
        verified_at,
    );

    let branch_meta_path = manifest.data_root.join(&manifest.branch_meta_relpath);
    let (default_branch, graph_scopes, graph_scope_issues) =
        reconstruct_graph_scopes(&branch_meta_path, &store_id, &project_id, profile_root);
    if status == RegistryReconstructionStatus::Eligible {
        report.issues.extend(graph_scope_issues);
    }

    report.plans.push(RegistryReconstructionPlan {
        manifest_path: manifest_path.to_path_buf(),
        status,
        status_reason,
        project: RegistryProjectPlan {
            project_id: project_id.clone(),
            project_root: project_root.clone(),
            aliases: vec![project_root],
            default_branch,
        },
        store: StoreInstanceUpsert {
            store_id,
            project_id,
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: path_string(&store_relpath),
            manifest_relpath: Some(path_string(&manifest_relpath)),
            last_verified_at: Some(verified_at),
            last_write_at: None,
        },
        graph_scopes,
        artifacts,
    });
    report
}

fn classify_project_root(
    project_root: &Path,
    project_id: &str,
    reject_ephemeral_root: bool,
) -> (RegistryReconstructionStatus, Option<String>, PathBuf) {
    let canonical_root = match project_root.canonicalize() {
        Ok(root) if root.is_dir() => root,
        Ok(root) => {
            return (
                RegistryReconstructionStatus::Blocked,
                Some(format!(
                    "project root '{}' is not a directory",
                    root.display()
                )),
                root,
            );
        }
        Err(error) => {
            return (
                RegistryReconstructionStatus::Stale,
                Some(format!(
                    "project root '{}' is unavailable: {error}",
                    project_root.display()
                )),
                project_root.to_path_buf(),
            );
        }
    };
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir());
    if reject_ephemeral_root && canonical_root.starts_with(temp_root) {
        return (
            RegistryReconstructionStatus::Stale,
            Some(format!(
                "project root '{}' is under the temporary directory",
                canonical_root.display()
            )),
            canonical_root,
        );
    }
    let repository_identity = match read_repository_identity_marker(&canonical_root) {
        Ok(marker) => marker.map(|marker| marker.project_id),
        Err(error) => {
            return (
                RegistryReconstructionStatus::Blocked,
                Some(format!("could not validate repository identity: {error}")),
                canonical_root,
            );
        }
    };
    let enrollment = match read_enrollment_marker(&canonical_root) {
        Ok(marker) => marker.map(|marker| marker.project_id),
        Err(error) => {
            return (
                RegistryReconstructionStatus::Blocked,
                Some(format!("could not validate enrollment marker: {error}")),
                canonical_root,
            );
        }
    };
    let identity = match (repository_identity.as_deref(), enrollment.as_deref()) {
        (Some(repository), Some(enrolled)) if repository != enrolled => (
            RegistryReconstructionStatus::Blocked,
            Some(format!(
                "repository identity project '{repository}' disagrees with enrollment project '{enrolled}'"
            )),
        ),
        (Some(repository), Some(_)) if repository == project_id => {
            (RegistryReconstructionStatus::Eligible, None)
        }
        (Some(repository), Some(_)) => (
            RegistryReconstructionStatus::Retired,
            Some(format!(
                "repository identity and enrollment name retired project '{repository}' instead of manifest project '{project_id}'"
            )),
        ),
        (Some(owner), None) | (None, Some(owner)) if owner == project_id => {
            (RegistryReconstructionStatus::Eligible, None)
        }
        (Some(owner), None) | (None, Some(owner)) => (
            RegistryReconstructionStatus::Retired,
            Some(format!(
                "project marker names retired project '{owner}' instead of manifest project '{project_id}'"
            )),
        ),
        (None, None) if reject_ephemeral_root => (
            RegistryReconstructionStatus::Blocked,
            Some("project has no repository identity or enrollment marker".to_string()),
        ),
        (None, None) => (RegistryReconstructionStatus::Eligible, None),
    };
    (identity.0, identity.1, canonical_root)
}

fn validate_manifest_shape(
    manifest_path: &Path,
    profile_root: &Path,
    manifest: &tracedecay_runtime_core::storage::StoreManifest,
) -> Vec<String> {
    let mut issues = Vec::new();
    if manifest.schema_version != STORE_MANIFEST_SCHEMA_VERSION {
        issues.push(format!(
            "store manifest '{}' uses unsupported schema_version {}",
            manifest_path.display(),
            manifest.schema_version
        ));
    }
    if manifest.store_kind != StoreKind::CodeProject {
        issues.push(format!(
            "store manifest '{}' is {:?}, not code_project",
            manifest_path.display(),
            manifest.store_kind
        ));
    }
    if manifest.storage_mode != StorageMode::ProfileSharded {
        issues.push(format!(
            "store manifest '{}' is {:?}, not profile_sharded",
            manifest_path.display(),
            manifest.storage_mode
        ));
    }
    if strip_profile_root(profile_root, &manifest.data_root).is_none() {
        issues.push(format!(
            "store data root '{}' is outside profile root '{}'",
            manifest.data_root.display(),
            profile_root.display()
        ));
    }
    if manifest_path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        != manifest.data_root.canonicalize().ok()
    {
        issues.push(format!(
            "store manifest '{}' is not inside its data root '{}'",
            manifest_path.display(),
            manifest.data_root.display()
        ));
    }
    issues
}

fn reconstruct_graph_scopes(
    branch_meta_path: &Path,
    store_id: &str,
    project_id: &str,
    profile_root: &Path,
) -> (Option<String>, Vec<GraphScopeUpsert>, Vec<String>) {
    let Some(branch_dir) = branch_meta_path.parent() else {
        return (None, Vec::new(), Vec::new());
    };
    let invalid = |message| (None, Vec::new(), vec![message]);
    let metadata = match fs::symlink_metadata(branch_meta_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (None, Vec::new(), Vec::new());
        }
        Err(error) => {
            return invalid(format!(
                "could not inspect branch metadata '{}': {error}",
                branch_meta_path.display()
            ));
        }
    };
    if !metadata.file_type().is_file() {
        return invalid(format!(
            "branch metadata '{}' is not a regular file",
            branch_meta_path.display()
        ));
    }
    let content = match fs::read_to_string(branch_meta_path) {
        Ok(content) => content,
        Err(error) => {
            return invalid(format!(
                "could not read branch metadata '{}': {error}",
                branch_meta_path.display()
            ));
        }
    };
    let meta = match branch_meta::parse(&content) {
        Ok(meta) => meta,
        Err(error) => {
            return invalid(format!(
                "branch metadata '{}' is invalid: {error}",
                branch_meta_path.display()
            ));
        }
    };
    let mut scopes = Vec::new();
    let mut issues = Vec::new();
    for (branch_name, entry) in &meta.branches {
        let db_relpath = Path::new(&entry.db_file);
        if !is_safe_relpath(db_relpath) {
            issues.push(format!(
                "branch '{branch_name}' has unsafe database path '{}'",
                entry.db_file
            ));
            continue;
        }
        let absolute_db_path = branch_dir.join(db_relpath);
        let Some(profile_db_relpath) = strip_profile_root(profile_root, &absolute_db_path) else {
            issues.push(format!(
                "branch '{branch_name}' database '{}' is missing or escapes profile root",
                absolute_db_path.display()
            ));
            continue;
        };
        scopes.push(GraphScopeUpsert {
            graph_scope_id: graph_scope_id(store_id, branch_name),
            project_id: project_id.to_string(),
            store_id: store_id.to_string(),
            branch_name: branch_name.clone(),
            db_relpath: path_string(&profile_db_relpath),
            parent_scope_id: entry
                .parent
                .as_ref()
                .map(|parent| graph_scope_id(store_id, parent)),
            last_synced_at: entry.last_synced_at.parse::<i64>().ok(),
            writable: true,
        });
    }
    scopes.sort_by(|a, b| a.branch_name.cmp(&b.branch_name));
    (Some(meta.default_branch), scopes, issues)
}

fn graph_scope_id(store_id: &str, branch_name: &str) -> String {
    format!("{store_id}:branch:{branch_name}")
}

fn push_artifact_if_present(
    artifacts: &mut Vec<StoreArtifactUpsert>,
    store_id: &str,
    artifact_kind: &str,
    path: &Path,
    profile_root: &Path,
    schema_version: Option<String>,
    updated_at: i64,
) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let Some(relpath) = strip_profile_root(profile_root, path) else {
        return;
    };
    artifacts.push(StoreArtifactUpsert {
        store_id: store_id.to_string(),
        artifact_kind: artifact_kind.to_string(),
        relpath: path_string(&relpath),
        size_bytes: i64::try_from(meta.len()).ok(),
        schema_version,
        updated_at: Some(updated_at),
    });
}

fn strip_profile_root(profile_root: &Path, path: &Path) -> Option<PathBuf> {
    let profile_root = profile_root.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    path.strip_prefix(profile_root).ok().map(PathBuf::from)
}

fn is_safe_relpath(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
