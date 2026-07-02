use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::branch_meta;
use crate::global_db::{
    CodeProjectRecord, GlobalDb, GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert,
};
use crate::storage::{
    read_store_manifest, validate_project_id, StorageMode, StoreKind, STORE_MANIFEST_FILENAME,
    STORE_MANIFEST_SCHEMA_VERSION,
};

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RegistryReconstructionApplyReport {
    pub projects: usize,
    pub aliases: usize,
    pub stores: usize,
    pub graph_scopes: usize,
    pub artifacts: usize,
}

pub async fn apply_registry_reconstruction_report(
    db: &GlobalDb,
    report: &RegistryReconstructionReport,
) -> std::result::Result<RegistryReconstructionApplyReport, Vec<String>> {
    if !report.issues.is_empty() {
        return Err(report.issues.clone());
    }

    let mut applied = RegistryReconstructionApplyReport::default();
    let mut issues = Vec::new();
    for plan in &report.plans {
        let project = &plan.project;
        if db
            .upsert_code_project(
                &project.project_id,
                &project.project_root,
                None,
                None,
                project.default_branch.as_deref(),
            )
            .await
            .is_none()
        {
            issues.push(format!(
                "failed to upsert code project '{}'",
                project.project_id
            ));
            continue;
        }
        applied.projects += 1;

        for alias in &project.aliases {
            if db
                .upsert_project_alias(alias, &project.project_id)
                .await
                .is_some()
            {
                applied.aliases += 1;
            } else {
                issues.push(format!(
                    "failed to upsert alias '{}' for project '{}'",
                    alias.display(),
                    project.project_id
                ));
            }
        }

        if db.upsert_store_instance(plan.store.clone()).await.is_some() {
            applied.stores += 1;
        } else {
            issues.push(format!(
                "failed to upsert store '{}' for project '{}'",
                plan.store.store_id, project.project_id
            ));
            continue;
        }

        for scope in &plan.graph_scopes {
            if db.upsert_graph_scope(scope.clone()).await.is_some() {
                applied.graph_scopes += 1;
            } else {
                issues.push(format!(
                    "failed to upsert graph scope '{}'",
                    scope.graph_scope_id
                ));
            }
        }
        for artifact in &plan.artifacts {
            if db.upsert_store_artifact(artifact.clone()).await.is_some() {
                applied.artifacts += 1;
            } else {
                issues.push(format!(
                    "failed to upsert store artifact '{}:{}'",
                    artifact.artifact_kind, artifact.relpath
                ));
            }
        }
    }

    if issues.is_empty() {
        Ok(applied)
    } else {
        Err(issues)
    }
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
    let Ok(entries) = fs::read_dir(&projects_root) else {
        return report;
    };
    let mut manifest_paths = entries
        .flatten()
        .map(|entry| entry.path().join(STORE_MANIFEST_FILENAME))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifest_paths.sort();

    for manifest_path in manifest_paths {
        let manifest_report =
            reconstruct_registry_from_store_manifest(&manifest_path, profile_root, verified_at);
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
    let (default_branch, graph_scopes) =
        reconstruct_graph_scopes(&branch_meta_path, &store_id, &project_id, profile_root);

    report.plans.push(RegistryReconstructionPlan {
        manifest_path: manifest_path.to_path_buf(),
        project: RegistryProjectPlan {
            project_id: project_id.clone(),
            project_root: manifest.project_root.clone(),
            aliases: vec![manifest.project_root],
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
    if !manifest.data_root.starts_with(profile_root) {
        issues.push(format!(
            "store data root '{}' is outside profile root '{}'",
            manifest.data_root.display(),
            profile_root.display()
        ));
    }
    if manifest_path.parent() != Some(manifest.data_root.as_path()) {
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
) -> (Option<String>, Vec<GraphScopeUpsert>) {
    let Some(meta) = branch_meta_path
        .parent()
        .and_then(branch_meta::load_branch_meta)
    else {
        return (None, Vec::new());
    };
    let mut scopes = meta
        .branches
        .iter()
        .filter_map(|(branch_name, entry)| {
            let db_relpath = Path::new(&entry.db_file);
            if !is_safe_relpath(db_relpath) {
                return None;
            }
            let absolute_db_path = branch_meta_path.parent()?.join(db_relpath);
            let profile_db_relpath = strip_profile_root(profile_root, &absolute_db_path)?;
            Some(GraphScopeUpsert {
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
            })
        })
        .collect::<Vec<_>>();
    scopes.sort_by(|a, b| a.branch_name.cmp(&b.branch_name));
    (Some(meta.default_branch), scopes)
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
    path.to_string_lossy().to_string()
}
