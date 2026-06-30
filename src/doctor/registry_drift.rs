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
