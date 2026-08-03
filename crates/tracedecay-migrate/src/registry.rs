use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use libsql::{Connection, params, params::IntoParams};
use serde::Serialize;

use crate::branch_meta;
use crate::registry_adapter::{
    CodeProjectRecord, GraphScopeUpsert, RegistryDatabase, StoreArtifactUpsert,
    StoreInstanceUpsert, canonical_project_key,
};
use crate::storage::{
    STORE_MANIFEST_FILENAME, STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind,
    read_enrollment_marker, read_repository_identity_marker, read_store_manifest,
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

/// Read-only view of eligible reconstruction plans that would insert at least
/// one registry row. This uses the same conflict preflight as apply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryReconstructionDiffReport {
    pub missing_plans: usize,
    pub issues: Vec<String>,
}

pub async fn diff_registry_reconstruction_report<D: RegistryDatabase>(
    db: &D,
    report: &RegistryReconstructionReport,
) -> RegistryReconstructionDiffReport {
    let mut diff = RegistryReconstructionDiffReport {
        issues: report.issues.clone(),
        ..RegistryReconstructionDiffReport::default()
    };
    let mut eligible = Vec::new();

    for plan in report
        .plans
        .iter()
        .filter(|plan| plan.status == RegistryReconstructionStatus::Eligible)
    {
        let single = RegistryReconstructionReport {
            plans: vec![plan.clone()],
            issues: Vec::new(),
        };
        let issues = preflight_registry_reconstruction(db.conn(), &single).await;
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
            let issues = preflight_registry_reconstruction(db.conn(), &pair).await;
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
        match registry_plan_has_missing_rows(db.conn(), plan).await {
            Ok(true) => diff.missing_plans += 1,
            Ok(false) => {}
            Err(issue) => diff.issues.push(issue),
        }
    }
    diff
}

async fn registry_plan_has_missing_rows(
    conn: &Connection,
    plan: &RegistryReconstructionPlan,
) -> std::result::Result<bool, String> {
    let project = &plan.project;
    let root = canonical_project_key(&project.project_root);
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
            params![canonical_project_key(alias)],
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

pub async fn apply_registry_reconstruction_report<D: RegistryDatabase>(
    db: &D,
    report: &RegistryReconstructionReport,
) -> std::result::Result<RegistryReconstructionApplyReport, Vec<String>> {
    let conn = db.conn();
    conn.execute("BEGIN IMMEDIATE", ()).await.map_err(|error| {
        vec![format!(
            "could not start atomic registry reconstruction: {error}"
        )]
    })?;
    let issues = preflight_registry_reconstruction(conn, report).await;
    if !issues.is_empty() {
        let _ = conn.execute("ROLLBACK", ()).await;
        return Err(issues);
    }
    match insert_missing_registry_rows(conn, report).await {
        Ok(applied) => match conn.execute("COMMIT", ()).await {
            Ok(_) => Ok(applied),
            Err(error) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(vec![format!(
                    "could not commit atomic registry reconstruction: {error}"
                )])
            }
        },
        Err(issue) => {
            let _ = conn.execute("ROLLBACK", ()).await;
            Err(vec![issue])
        }
    }
}

pub async fn apply_single_registry_reconstruction_report<D: RegistryDatabase>(
    db: &D,
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

async fn preflight_registry_reconstruction(
    conn: &Connection,
    report: &RegistryReconstructionReport,
) -> Vec<String> {
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
        let root = canonical_project_key(&project.project_root);
        record_batch_owner(
            &mut project_roots,
            &root,
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
            "SELECT project_id FROM code_projects WHERE canonical_root=?1",
            params![root.as_str()],
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
            let alias = canonical_project_key(alias);
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
                Ok(Some(existing)) if existing != scope_identity => issues.push(format!(
                    "graph scope '{}' already has conflicting ownership or location",
                    scope.graph_scope_id
                )),
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

async fn query_optional_text(
    conn: &Connection,
    sql: &str,
    params: impl IntoParams,
) -> std::result::Result<Option<String>, String> {
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

async fn query_all_text(
    conn: &Connection,
    sql: &str,
    params: impl IntoParams,
) -> std::result::Result<Vec<String>, String> {
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

async fn insert_missing_registry_rows(
    conn: &Connection,
    report: &RegistryReconstructionReport,
) -> std::result::Result<RegistryReconstructionApplyReport, String> {
    let mut applied = RegistryReconstructionApplyReport::default();
    let now = crate::tracedecay::current_timestamp();
    for plan in &report.plans {
        if plan.status != RegistryReconstructionStatus::Eligible {
            continue;
        }
        let project = &plan.project;
        let canonical_root = canonical_project_key(&project.project_root);
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
                        canonical_project_key(alias),
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
                    "INSERT OR IGNORE INTO graph_scopes(
                         graph_scope_id, project_id, store_id, branch_name, db_relpath,
                         parent_scope_id, last_synced_at, writable
                     ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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

/// Returns true if the project's canonical or display root still exists.
pub fn code_project_root_exists(project: &CodeProjectRecord) -> bool {
    Path::new(&project.canonical_root).exists() || Path::new(&project.display_root).exists()
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
            StaleRootScope::CanonicalRootMissing => !Path::new(&project.canonical_root).exists(),
            StaleRootScope::AllRootsMissing => !code_project_root_exists(project),
        })
        .collect()
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
    manifest: &crate::storage::StoreManifest,
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
