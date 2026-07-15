use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegistryDriftFinding {
    pub(super) project_id: String,
    pub(super) store_id: String,
    pub(super) field: &'static str,
    pub(super) registry_value: String,
    pub(super) manifest_value: String,
    pub(super) manifest_path: PathBuf,
    manifest: crate::storage::StoreManifest,
    registry_path: Option<PathBuf>,
}

pub(super) async fn registry_drift_findings(
    global_db: &crate::global_db::GlobalDb,
    profile_root: &Path,
) -> Vec<RegistryDriftFinding> {
    let mut findings = Vec::new();
    for project in global_db.list_code_projects(usize::MAX).await {
        let Some(context) = global_db
            .project_registry_context_by_id(&project.project_id)
            .await
        else {
            continue;
        };
        for store_context in context.stores {
            let store = store_context.store;
            let Some(manifest_path) = resolve_registry_manifest_path(profile_root, &store) else {
                continue;
            };
            let Ok(manifest) = crate::storage::read_store_manifest(&manifest_path) else {
                continue;
            };
            let manifest_project_id = manifest
                .project_id
                .as_deref()
                .unwrap_or("<missing>")
                .to_string();
            if manifest_project_id != store.project_id {
                findings.push(RegistryDriftFinding {
                    project_id: project.project_id.clone(),
                    store_id: store.store_id.clone(),
                    field: "project_id",
                    registry_value: store.project_id.clone(),
                    manifest_value: manifest_project_id,
                    manifest_path: manifest_path.clone(),
                    manifest: manifest.clone(),
                    registry_path: None,
                });
            }

            let registry_project_root = PathBuf::from(&project.canonical_root);
            let manifest_project_root = manifest.project_root.clone();
            if registry_project_root != manifest_project_root {
                findings.push(RegistryDriftFinding {
                    project_id: project.project_id.clone(),
                    store_id: store.store_id.clone(),
                    field: "project_root",
                    registry_value: project.canonical_root.clone(),
                    manifest_value: manifest_project_root.display().to_string(),
                    manifest_path,
                    manifest,
                    registry_path: Some(registry_project_root),
                });
            }
        }
    }
    findings
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledStoreRoot {
    pub store_id: String,
    pub manifest_path: PathBuf,
    pub config_path: Option<PathBuf>,
}

#[cfg(test)]
async fn reconcile_drifted_store_roots(
    global_db: &crate::global_db::GlobalDb,
    profile_root: &Path,
) -> (Vec<ReconciledStoreRoot>, Vec<String>) {
    let findings = registry_drift_findings(global_db, profile_root).await;
    reconcile_drifted_store_roots_from_findings(&findings)
}

pub(super) fn reconcile_drifted_store_roots_from_findings(
    findings: &[RegistryDriftFinding],
) -> (Vec<ReconciledStoreRoot>, Vec<String>) {
    let mut reconciled = Vec::new();
    let mut warnings = Vec::new();

    for finding in findings {
        if finding.field != "project_root" {
            continue;
        }
        let Some(canonical) = finding.registry_path.as_deref() else {
            continue;
        };
        if !canonical.exists() {
            continue;
        }

        match reconcile_one_store_root(finding, canonical) {
            Ok((entry, warning)) => {
                if let Some(entry) = entry {
                    reconciled.push(entry);
                }
                if let Some(warning) = warning {
                    warnings.push(warning);
                }
            }
            Err(message) => warnings.push(message),
        }
    }

    (reconciled, warnings)
}

fn reconcile_one_store_root(
    finding: &RegistryDriftFinding,
    canonical: &Path,
) -> std::result::Result<(Option<ReconciledStoreRoot>, Option<String>), String> {
    let mut manifest = finding.manifest.clone();

    let mut manifest_rewritten = false;
    if manifest.project_root != canonical {
        manifest.project_root = canonical.to_path_buf();
        crate::storage::write_store_manifest_to_path(&finding.manifest_path, &manifest).map_err(
            |e| {
                format!(
                    "could not rewrite store manifest '{}': {e}",
                    finding.manifest_path.display()
                )
            },
        )?;
        manifest_rewritten = true;
    }

    let config_path = finding
        .manifest_path
        .parent()
        .map(|parent| parent.join(crate::config::CONFIG_FILENAME));
    let mut config_rewritten = None;
    let mut warning = None;
    if let Some(config_path) = config_path
        && config_path.is_file()
    {
        match reconcile_config_root_dir(&config_path, canonical) {
            Ok(true) => config_rewritten = Some(config_path),
            Ok(false) => {}
            Err(message) => {
                if manifest_rewritten {
                    warning = Some(message);
                } else {
                    return Err(message);
                }
            }
        }
    }

    if !manifest_rewritten && config_rewritten.is_none() {
        return Ok((None, warning));
    }

    Ok((
        Some(ReconciledStoreRoot {
            store_id: finding.store_id.clone(),
            manifest_path: finding.manifest_path.clone(),
            config_path: config_rewritten,
        }),
        warning,
    ))
}

fn reconcile_config_root_dir(
    config_path: &Path,
    canonical: &Path,
) -> std::result::Result<bool, String> {
    let mut config = crate::config::load_config_from_path(canonical, config_path)
        .map_err(|e| format!("could not read config '{}': {e}", config_path.display()))?;
    if Path::new(&config.root_dir) == canonical {
        return Ok(false);
    }
    config.root_dir = canonical.display().to_string();
    crate::config::save_config_to_path(config_path, &config)
        .map_err(|e| format!("could not rewrite config '{}': {e}", config_path.display()))?;
    Ok(true)
}

fn resolve_registry_manifest_path(
    profile_root: &Path,
    store: &crate::global_db::StoreInstanceRecord,
) -> Option<PathBuf> {
    if store.storage_mode != "profile_sharded" {
        return None;
    }
    let store_relpath = super::registry_relpath(&store.store_relpath);
    let manifest_relpath = store
        .manifest_relpath
        .as_ref()
        .map(|relpath| super::registry_relpath(relpath));
    for profile_root in super::registry_profile_roots(profile_root) {
        let Ok(data_root) =
            crate::storage::StoreArtifactPath::resolve(&profile_root, &store_relpath)
        else {
            continue;
        };
        let data_root = data_root.absolute_path();
        if let Some(relpath) = manifest_relpath.as_ref() {
            for root in [&profile_root, &data_root] {
                let Ok(path) = crate::storage::StoreArtifactPath::resolve(root, relpath) else {
                    continue;
                };
                let path = path.absolute_path();
                if path.is_file() {
                    return Some(path);
                }
            }
        } else {
            let path = data_root.join(crate::storage::STORE_MANIFEST_FILENAME);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::global_db::{GlobalDb, StoreInstanceUpsert};
    use crate::storage::{STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest};

    const STORE_ID: &str = "store_reconcile_test";
    const PROJECT_ID: &str = "proj_reconcile_test";

    struct Fixture {
        _tmp: tempfile::TempDir,
        profile_root: PathBuf,
        current_root: PathBuf,
        stale_root: PathBuf,
        manifest_path: PathBuf,
        config_path: PathBuf,
        global_db: GlobalDb,
    }

    async fn build_fixture() -> Fixture {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let current_root = tmp.path().join("current-name");
        let stale_root = tmp.path().join("old-name");
        std::fs::create_dir_all(&current_root).unwrap();
        let data_root = profile_root.join("stores").join(STORE_ID);
        std::fs::create_dir_all(&data_root).unwrap();
        let manifest_path = data_root.join(crate::storage::STORE_MANIFEST_FILENAME);
        let config_path = data_root.join(crate::config::CONFIG_FILENAME);

        let manifest = StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(PROJECT_ID.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: stale_root.clone(),
            data_root: data_root.clone(),
            graph_db_relpath: PathBuf::from(crate::config::DB_FILENAME),
            sessions_db_relpath: PathBuf::from("sessions.db"),
            branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
        };
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let config = crate::config::TraceDecayConfig {
            root_dir: stale_root.to_string_lossy().to_string(),
            ..crate::config::TraceDecayConfig::default()
        };
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        let global_db = GlobalDb::open_at(&profile_root.join("global.db"))
            .await
            .unwrap();
        global_db
            .upsert_code_project(PROJECT_ID, &current_root, None, None, None)
            .await
            .unwrap();
        global_db
            .upsert_store_instance(StoreInstanceUpsert {
                store_id: STORE_ID.to_string(),
                project_id: PROJECT_ID.to_string(),
                store_kind: "project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath: format!("stores/{STORE_ID}"),
                manifest_relpath: None,
                last_verified_at: None,
                last_write_at: None,
            })
            .await
            .unwrap();

        Fixture {
            _tmp: tmp,
            profile_root,
            current_root,
            stale_root,
            manifest_path,
            config_path,
            global_db,
        }
    }

    fn manifest_root(path: &Path) -> PathBuf {
        crate::storage::read_store_manifest(path)
            .unwrap()
            .project_root
    }

    fn config_root_dir(path: &Path) -> String {
        crate::config::load_config_from_path(Path::new("/"), path)
            .unwrap()
            .root_dir
    }

    fn comparable_path(path: &Path) -> String {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let text = normalized.to_string_lossy();
        text.strip_prefix(r"\\?\")
            .unwrap_or(&text)
            .replace('\\', "/")
    }

    #[tokio::test]
    async fn detection_does_not_mutate() {
        let fx = build_fixture().await;

        let findings = registry_drift_findings(&fx.global_db, &fx.profile_root).await;
        let root_drift: Vec<_> = findings
            .iter()
            .filter(|f| f.field == "project_root")
            .collect();
        assert_eq!(root_drift.len(), 1, "should detect project_root drift");

        assert_eq!(manifest_root(&fx.manifest_path), fx.stale_root);
        assert_eq!(
            config_root_dir(&fx.config_path),
            fx.stale_root.to_string_lossy()
        );
    }

    #[tokio::test]
    async fn reconcile_rewrites_then_is_idempotent() {
        let fx = build_fixture().await;
        let canonical = fx.current_root.canonicalize().unwrap();

        let (reconciled, warnings) =
            reconcile_drifted_store_roots(&fx.global_db, &fx.profile_root).await;
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(reconciled.len(), 1, "one store should be reconciled");
        assert_eq!(reconciled[0].store_id, STORE_ID);
        assert!(
            reconciled[0].config_path.is_some(),
            "stale config should be reconciled too"
        );

        assert_eq!(manifest_root(&fx.manifest_path), canonical);
        assert_eq!(
            config_root_dir(&fx.config_path),
            canonical.to_string_lossy()
        );

        let healed = crate::storage::read_store_manifest(&fx.manifest_path).unwrap();
        assert_eq!(healed.project_id.as_deref(), Some(PROJECT_ID));
        assert_eq!(
            healed.graph_db_relpath,
            PathBuf::from(crate::config::DB_FILENAME)
        );

        let post = registry_drift_findings(&fx.global_db, &fx.profile_root).await;
        assert!(
            post.iter().all(|f| f.field != "project_root"),
            "project_root drift should be resolved: {post:?}"
        );

        let (again, warnings) =
            reconcile_drifted_store_roots(&fx.global_db, &fx.profile_root).await;
        assert!(again.is_empty(), "second run must be a no-op: {again:?}");
        assert!(
            warnings.is_empty(),
            "no warnings on no-op run: {warnings:?}"
        );
    }

    #[tokio::test]
    async fn manifest_reconcile_is_reported_when_config_rewrite_fails() {
        let fx = build_fixture().await;
        let canonical = fx.current_root.canonicalize().unwrap();
        std::fs::write(&fx.config_path, "{ not json").unwrap();

        let (reconciled, warnings) =
            reconcile_drifted_store_roots(&fx.global_db, &fx.profile_root).await;
        assert_eq!(
            reconciled.len(),
            1,
            "manifest rewrite should still be reported"
        );
        assert_eq!(reconciled[0].store_id, STORE_ID);
        assert_eq!(
            comparable_path(&reconciled[0].manifest_path),
            comparable_path(&fx.manifest_path.canonicalize().unwrap())
        );
        assert_eq!(
            reconciled[0].config_path, None,
            "failed config rewrite must not be reported as reconciled"
        );
        assert_eq!(manifest_root(&fx.manifest_path), canonical);
        assert_eq!(warnings.len(), 1, "config failure should still be surfaced");
        assert!(
            warnings[0].contains("could not read config"),
            "unexpected warning: {warnings:?}"
        );
    }

    #[tokio::test]
    async fn missing_canonical_root_is_not_healed() {
        let fx = build_fixture().await;
        std::fs::remove_dir_all(&fx.current_root).unwrap();

        let (reconciled, warnings) =
            reconcile_drifted_store_roots(&fx.global_db, &fx.profile_root).await;
        assert!(reconciled.is_empty(), "must not heal a nonexistent root");
        assert!(
            warnings.is_empty(),
            "gate is silent, not a warning: {warnings:?}"
        );
        assert_eq!(manifest_root(&fx.manifest_path), fx.stale_root);
    }
}
