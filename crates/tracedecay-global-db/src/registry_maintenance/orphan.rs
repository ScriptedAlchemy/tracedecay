use super::*;

pub fn inspect_profile_store_orphans(
    profile_root: &Path,
    verified_at: i64,
) -> RegistryOrphanRelinkReport {
    let mut report = RegistryOrphanRelinkReport::default();
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
        let manifest_report =
            inspect_registry_orphan_manifest_inner(&manifest_path, profile_root, verified_at, true);
        report.plans.extend(manifest_report.plans);
        report.issues.extend(manifest_report.issues);
    }

    report
}

pub fn inspect_registry_orphan_manifest(
    manifest_path: &Path,
    profile_root: &Path,
    verified_at: i64,
) -> RegistryOrphanRelinkReport {
    inspect_registry_orphan_manifest_inner(manifest_path, profile_root, verified_at, false)
}

fn inspect_registry_orphan_manifest_inner(
    manifest_path: &Path,
    profile_root: &Path,
    verified_at: i64,
    reject_ephemeral_root: bool,
) -> RegistryOrphanRelinkReport {
    let mut report = RegistryOrphanRelinkReport::default();
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
    if status == RegistryOrphanRelinkStatus::Eligible {
        report.issues.extend(graph_scope_issues);
    }

    report.plans.push(RegistryOrphanRelinkPlan {
        manifest_path: manifest_path.to_path_buf(),
        status,
        status_reason,
        project: RegistryOrphanProjectPlan {
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
) -> (RegistryOrphanRelinkStatus, Option<String>, PathBuf) {
    let canonical_root = match project_root.canonicalize() {
        Ok(root) if root.is_dir() => root,
        Ok(root) => {
            return (
                RegistryOrphanRelinkStatus::Blocked,
                Some(format!(
                    "project root '{}' is not a directory",
                    root.display()
                )),
                root,
            );
        }
        Err(error) => {
            return (
                RegistryOrphanRelinkStatus::Stale,
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
            RegistryOrphanRelinkStatus::Stale,
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
                RegistryOrphanRelinkStatus::Blocked,
                Some(format!("could not validate repository identity: {error}")),
                canonical_root,
            );
        }
    };
    let enrollment = match read_enrollment_marker(&canonical_root) {
        Ok(marker) => marker.map(|marker| marker.project_id),
        Err(error) => {
            return (
                RegistryOrphanRelinkStatus::Blocked,
                Some(format!("could not validate enrollment marker: {error}")),
                canonical_root,
            );
        }
    };
    let identity = match (repository_identity.as_deref(), enrollment.as_deref()) {
        (Some(repository), Some(enrolled)) if repository != enrolled => (
            RegistryOrphanRelinkStatus::Blocked,
            Some(format!(
                "repository identity project '{repository}' disagrees with enrollment project '{enrolled}'"
            )),
        ),
        (Some(repository), Some(_)) if repository == project_id => {
            (RegistryOrphanRelinkStatus::Eligible, None)
        }
        (Some(repository), Some(_)) => (
            RegistryOrphanRelinkStatus::Retired,
            Some(format!(
                "repository identity and enrollment name retired project '{repository}' instead of manifest project '{project_id}'"
            )),
        ),
        (Some(owner), None) | (None, Some(owner)) if owner == project_id => {
            (RegistryOrphanRelinkStatus::Eligible, None)
        }
        (Some(owner), None) | (None, Some(owner)) => (
            RegistryOrphanRelinkStatus::Retired,
            Some(format!(
                "project marker names retired project '{owner}' instead of manifest project '{project_id}'"
            )),
        ),
        (None, None) if reject_ephemeral_root => (
            RegistryOrphanRelinkStatus::Blocked,
            Some("project has no repository identity or enrollment marker".to_string()),
        ),
        (None, None) => (RegistryOrphanRelinkStatus::Eligible, None),
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
