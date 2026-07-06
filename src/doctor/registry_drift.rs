use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegistryDriftFinding {
    pub(super) project_id: String,
    pub(super) store_id: String,
    pub(super) field: &'static str,
    pub(super) registry_value: String,
    pub(super) manifest_value: String,
    pub(super) manifest_path: PathBuf,
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
                });
            }

            let registry_project_root = comparable_path(Path::new(&project.canonical_root));
            let manifest_project_root = comparable_path(&manifest.project_root);
            if registry_project_root != manifest_project_root {
                findings.push(RegistryDriftFinding {
                    project_id: project.project_id.clone(),
                    store_id: store.store_id.clone(),
                    field: "project_root",
                    registry_value: registry_project_root,
                    manifest_value: manifest_project_root,
                    manifest_path,
                });
            }
        }
    }
    findings
}

/// One reconciled store: the stale roots that were rewritten to the registry
/// canonical path during the heal pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledStoreRoot {
    pub store_id: String,
    /// The `store_manifest.json` whose `project_root` was rewritten.
    pub manifest_path: PathBuf,
    /// The sibling `config.json` whose `root_dir` was rewritten, when it was
    /// also stale.
    pub config_path: Option<PathBuf>,
}

/// Reconciles stale `store_manifest.json` `project_root` (and the sibling
/// `config.json` `root_dir`, when stale) to the registry canonical path.
///
/// This is the durable half of the project-rename self-heal: the `GlobalDb`
/// registry already self-heals `canonical_root`/`display_root` to the current
/// path on every open, but the on-disk store artifacts keep the old path. This
/// runs ONLY in the on-demand doctor heal pass — never from resolution or
/// registration — so it stays off the hot path.
///
/// Safety: a drift is healed only when the registry canonical root
/// (`finding.registry_value`, already `comparable_path(canonical_root)`) still
/// resolves to a path that exists on disk. That is precisely the "registry
/// already binds the current path" precondition — after a rename the stale
/// manifest path no longer exists while the registry canonical path does, so
/// the existence check both proves the registry is authoritative and prevents
/// inventing a path. Every I/O failure becomes a warning; nothing aborts.
///
/// Idempotent: once a manifest's `project_root` equals the canonical value the
/// drift finding disappears, so a second pass finds nothing to do.
pub(super) async fn reconcile_drifted_store_roots(
    global_db: &crate::global_db::GlobalDb,
    profile_root: &Path,
) -> (Vec<ReconciledStoreRoot>, Vec<String>) {
    let mut reconciled = Vec::new();
    let mut warnings = Vec::new();

    for finding in registry_drift_findings(global_db, profile_root).await {
        if finding.field != "project_root" {
            continue;
        }
        // SAFETY GATE: only heal when the registry canonical root exists on
        // disk (the registry already binds the current path). Never invent a
        // path; leave drift whose canonical root is missing as a report-only
        // finding.
        let canonical = Path::new(&finding.registry_value);
        if !canonical.exists() {
            continue;
        }

        match reconcile_one_store_root(&finding, canonical) {
            Ok(Some(entry)) => reconciled.push(entry),
            Ok(None) => {}
            Err(message) => warnings.push(message),
        }
    }

    (reconciled, warnings)
}

/// Rewrites the manifest `project_root` (and sibling config `root_dir` if
/// stale) for one drift finding. Returns `Ok(None)` when nothing needed
/// rewriting (idempotent no-op).
fn reconcile_one_store_root(
    finding: &RegistryDriftFinding,
    canonical: &Path,
) -> std::result::Result<Option<ReconciledStoreRoot>, String> {
    let mut manifest =
        crate::storage::read_store_manifest(&finding.manifest_path).map_err(|e| {
            format!(
                "could not read store manifest '{}': {e}",
                finding.manifest_path.display()
            )
        })?;

    let mut manifest_rewritten = false;
    if comparable_path(&manifest.project_root) != finding.registry_value {
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

    // config.json is a sibling of store_manifest.json in the data_root. Heal
    // its root_dir opportunistically when it is also stale — registry_drift
    // does not emit a separate finding for it.
    let config_path = finding
        .manifest_path
        .parent()
        .map(|parent| parent.join(crate::config::CONFIG_FILENAME));
    let mut config_rewritten = None;
    if let Some(config_path) = config_path {
        if config_path.is_file() {
            match reconcile_config_root_dir(&config_path, canonical, &finding.registry_value) {
                Ok(true) => config_rewritten = Some(config_path),
                Ok(false) => {}
                Err(message) => return Err(message),
            }
        }
    }

    if !manifest_rewritten && config_rewritten.is_none() {
        return Ok(None);
    }

    Ok(Some(ReconciledStoreRoot {
        store_id: finding.store_id.clone(),
        manifest_path: finding.manifest_path.clone(),
        config_path: config_rewritten,
    }))
}

/// Rewrites `config.json`'s `root_dir` to the canonical path when stale.
/// Returns `Ok(true)` when it wrote, `Ok(false)` when already current.
fn reconcile_config_root_dir(
    config_path: &Path,
    canonical: &Path,
    canonical_spelling: &str,
) -> std::result::Result<bool, String> {
    // `root_dir` is a free-form String; normalize both sides through
    // comparable_path so an already-canonical (but differently spelled) value
    // is treated as current and not needlessly rewritten.
    let mut config = crate::config::load_config_from_path(canonical, config_path)
        .map_err(|e| format!("could not read config '{}': {e}", config_path.display()))?;
    if comparable_path(Path::new(&config.root_dir)) == canonical_spelling {
        return Ok(false);
    }
    config.root_dir = canonical.to_string_lossy().to_string();
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

fn comparable_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::global_db::{GlobalDb, StoreInstanceUpsert};
    use crate::storage::{StorageMode, StoreKind, StoreManifest, STORE_MANIFEST_SCHEMA_VERSION};

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

    /// Builds a profile with one profile-sharded store whose on-disk manifest
    /// (and config) still point at a stale `stale_root`, while the registry
    /// canonical root is `current_root` (a real directory that exists).
    async fn build_fixture() -> Fixture {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let current_root = tmp.path().join("current-name");
        let stale_root = tmp.path().join("old-name");
        std::fs::create_dir_all(&current_root).unwrap();
        // stale_root is intentionally NOT created — after a rename the old
        // path no longer exists on disk.

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
        // Registry already self-healed canonical_root to the current path.
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

    // (a) Detection alone (the report-only path) never mutates the on-disk
    //     files — the "dry-run" analog, since the heal pass has no separate
    //     apply flag and running reconcile IS the apply.
    #[tokio::test]
    async fn detection_does_not_mutate() {
        let fx = build_fixture().await;

        let findings = registry_drift_findings(&fx.global_db, &fx.profile_root).await;
        let root_drift: Vec<_> = findings
            .iter()
            .filter(|f| f.field == "project_root")
            .collect();
        assert_eq!(root_drift.len(), 1, "should detect project_root drift");

        // No write happened: the manifest and config still hold the stale root.
        assert_eq!(manifest_root(&fx.manifest_path), fx.stale_root);
        assert_eq!(
            config_root_dir(&fx.config_path),
            fx.stale_root.to_string_lossy()
        );
    }

    // (c) Applying the reconcile rewrites the manifest and config to the
    //     registry canonical (current) path; (d) a second run is a no-op.
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

        // Manifest + config now hold the canonical current path.
        assert_eq!(manifest_root(&fx.manifest_path), canonical);
        assert_eq!(
            config_root_dir(&fx.config_path),
            canonical.to_string_lossy()
        );

        // Every other manifest field survived the surgical rewrite.
        let healed = crate::storage::read_store_manifest(&fx.manifest_path).unwrap();
        assert_eq!(healed.project_id.as_deref(), Some(PROJECT_ID));
        assert_eq!(
            healed.graph_db_relpath,
            PathBuf::from(crate::config::DB_FILENAME)
        );

        // The drift finding is gone now that the manifest matches the registry.
        let post = registry_drift_findings(&fx.global_db, &fx.profile_root).await;
        assert!(
            post.iter().all(|f| f.field != "project_root"),
            "project_root drift should be resolved: {post:?}"
        );

        // Second run: nothing left to heal.
        let (again, warnings) =
            reconcile_drifted_store_roots(&fx.global_db, &fx.profile_root).await;
        assert!(again.is_empty(), "second run must be a no-op: {again:?}");
        assert!(
            warnings.is_empty(),
            "no warnings on no-op run: {warnings:?}"
        );
    }

    // (b)/safety: when the registry canonical root does NOT exist on disk the
    //     drift is left untouched — never invent a path.
    #[tokio::test]
    async fn missing_canonical_root_is_not_healed() {
        let fx = build_fixture().await;
        // Remove the current root so the registry canonical path no longer
        // exists; the safety gate must refuse to heal.
        std::fs::remove_dir_all(&fx.current_root).unwrap();

        let (reconciled, warnings) =
            reconcile_drifted_store_roots(&fx.global_db, &fx.profile_root).await;
        assert!(reconciled.is_empty(), "must not heal a nonexistent root");
        assert!(
            warnings.is_empty(),
            "gate is silent, not a warning: {warnings:?}"
        );
        // Manifest untouched.
        assert_eq!(manifest_root(&fx.manifest_path), fx.stale_root);
    }
}
