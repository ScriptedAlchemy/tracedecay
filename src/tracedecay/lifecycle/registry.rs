//! Global-registry registration: publishing a profile-sharded store's
//! project, store, and branch scope rows so cross-project lookups and the
//! registry-driven identity resolution in [`super::identity`] can find it.

use std::path::{Path, PathBuf};

use crate::branch_meta;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{
    GraphScopeUpsert, RegisteredGlobalDb, StoreArtifactUpsert, StoreInstanceUpsert,
};
use crate::storage::{self, StoreLayout};
use crate::tracedecay::current_timestamp;

use super::TraceDecay;

impl TraceDecay {
    pub(crate) async fn register_project_store_in_global_registry(&self) -> Result<()> {
        static REGISTRY_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

        if self.store_layout.storage_mode != storage::StorageMode::ProfileSharded {
            return Ok(());
        }

        let project_id = self
            .store_layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| {
                registry_registration_error("profile-sharded store has no project identity")
            })?;
        let profile_root = profile_root_for_layout(&self.store_layout)
            .ok_or_else(|| registry_registration_error("store is outside a profile root"))?;
        let store_relpath = profile_relative(&profile_root, &self.store_layout.data_root)
            .ok_or_else(|| registry_registration_error("store root is outside its profile"))?;

        let _registry_write = REGISTRY_WRITE_LOCK.lock().await;

        let global_db = self.profile_database.as_ref();

        let meta = branch_meta::load_branch_meta(&self.store_layout.data_root);
        let default_branch = meta.as_ref().map(|meta| meta.default_branch.as_str());
        // Registering without the git common dir leaves the row unreachable
        // by repository identity, so the next first touch from a sibling
        // checkout mints a fresh store. Detached worktrees are no exception:
        // they belong to the same repository as every other checkout.
        let git_common_dir = crate::worktree::git_common_dir(&self.project_root);
        let git_remote_url = git_remote_url(&self.project_root);

        // A shared project id can be reached from any linked worktree (see
        // the git-common-dir alias registered below), so registering
        // straight from `self.project_root` would let whichever worktree
        // happens to touch the project last pin its canonical_root /
        // display_root to a transient worktree path. Redirect registration
        // to the primary checkout when one is detected and still exists.
        let primary_root = crate::project_registry::primary_checkout_root(
            &self.project_root,
            git_common_dir.as_deref(),
        );
        let previous_canonical_root = if primary_root.is_some() {
            global_db
                .get_code_project(project_id)
                .await
                .map(|record| record.canonical_root)
        } else {
            None
        };
        let registration_root = primary_root.as_deref().unwrap_or(&self.project_root);

        let project = global_db
            .upsert_code_project(
                project_id,
                registration_root,
                git_common_dir.as_deref(),
                git_remote_url.as_deref(),
                default_branch,
            )
            .await
            .ok_or_else(|| registry_registration_error("upsert code project failed"))?;

        storage::write_repository_identity_marker(&self.project_root, &project.project_id)?;

        if let Some(primary_root) = primary_root.as_deref() {
            // The registry now points canonical_root/display_root at the
            // primary checkout; keep this worktree itself resolvable for
            // future lookups by registering its own path as an alias.
            global_db
                .upsert_project_alias(&self.project_root, &project.project_id)
                .await
                .ok_or_else(|| registry_registration_error("upsert worktree alias failed"))?;

            let repaired_stale_worktree_root = previous_canonical_root.is_some_and(|previous| {
                previous != RegisteredGlobalDb::canonical_project_key(primary_root)
            });
            if repaired_stale_worktree_root {
                eprintln!(
                    "warning: repaired tracedecay project '{project_id}' canonical_root — \
                     it was pinned to a linked worktree ({}); restored to the primary checkout ({})",
                    self.project_root.display(),
                    primary_root.display()
                );
            }
        }

        let store_id = profile_store_id(&project.project_id);
        let manifest_relpath = self
            .store_layout
            .manifest_path
            .as_ref()
            .and_then(|path| profile_relative(&profile_root, path));
        let now = current_timestamp();
        let store = global_db
            .upsert_store_instance(StoreInstanceUpsert {
                store_id,
                project_id: project.project_id,
                store_kind: "code_project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath,
                manifest_relpath,
                last_verified_at: Some(now),
                last_write_at: Some(now),
            })
            .await
            .ok_or_else(|| registry_registration_error("upsert store instance failed"))?;

        if let Some(meta) = meta {
            for (branch_name, entry) in meta.branches {
                let db_path = self.store_layout.data_root.join(&entry.db_file);
                let db_relpath = profile_relative(&profile_root, &db_path).ok_or_else(|| {
                    registry_registration_error("branch database is outside its profile")
                })?;
                global_db
                    .upsert_graph_scope(GraphScopeUpsert {
                        graph_scope_id: profile_graph_scope_id(&store.store_id, &branch_name),
                        project_id: store.project_id.clone(),
                        store_id: store.store_id.clone(),
                        branch_name: branch_name.clone(),
                        db_relpath,
                        parent_scope_id: entry
                            .parent
                            .as_deref()
                            .map(|parent| profile_graph_scope_id(&store.store_id, parent)),
                        last_synced_at: entry.last_synced_at.parse::<i64>().ok(),
                        writable: true,
                    })
                    .await
                    .ok_or_else(|| registry_registration_error("upsert graph scope failed"))?;
            }
        }

        let mut artifacts = Vec::new();
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "graph_db",
            &profile_root,
            &self.store_layout.graph_db_path,
            None,
            now,
        );
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "sessions_db",
            &profile_root,
            &self.store_layout.sessions_db_path,
            None,
            now,
        );
        push_existing_store_artifact(
            &mut artifacts,
            &store.store_id,
            "branch_meta",
            &profile_root,
            &self.store_layout.branch_meta_path,
            None,
            now,
        );
        if let Some(manifest_path) = &self.store_layout.manifest_path {
            push_existing_store_artifact(
                &mut artifacts,
                &store.store_id,
                "store_manifest",
                &profile_root,
                manifest_path,
                Some(storage::STORE_MANIFEST_SCHEMA_VERSION.to_string()),
                now,
            );
        }
        for artifact in artifacts {
            global_db
                .upsert_store_artifact(artifact)
                .await
                .ok_or_else(|| registry_registration_error("upsert store artifact failed"))?;
        }
        Ok(())
    }
}

fn profile_relative(profile_root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(profile_root)
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

fn profile_root_for_layout(layout: &StoreLayout) -> Option<PathBuf> {
    layout.data_root.parent()?.parent().map(Path::to_path_buf)
}

fn profile_store_id(project_id: &str) -> String {
    format!("store:{project_id}:profile_sharded")
}

fn registry_registration_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        operation: "register project store".to_string(),
        message: message.into(),
    }
}

pub(crate) fn git_remote_url(project_root: &Path) -> Option<String> {
    // gix reads the same config `git config --get` would (repo-local +
    // global) without a subprocess spawn.
    if let Ok(repo) = gix::discover(project_root) {
        let url = repo
            .config_snapshot()
            .string("remote.origin.url")?
            .to_string();
        let url = url.trim();
        return (!url.is_empty()).then(|| url.to_string());
    }
    if !crate::worktree::git_may_resolve_repo(project_root) {
        return None;
    }
    crate::git::git_capture(project_root, &["config", "--get", "remote.origin.url"])
}

fn profile_graph_scope_id(store_id: &str, branch_name: &str) -> String {
    format!("{store_id}:branch:{branch_name}")
}

fn push_existing_store_artifact(
    artifacts: &mut Vec<StoreArtifactUpsert>,
    store_id: &str,
    artifact_kind: &str,
    profile_root: &Path,
    path: &Path,
    schema_version: Option<String>,
    updated_at: i64,
) {
    let Some(relpath) = profile_relative(profile_root, path) else {
        return;
    };
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    artifacts.push(StoreArtifactUpsert {
        store_id: store_id.to_string(),
        artifact_kind: artifact_kind.to_string(),
        relpath,
        size_bytes: i64::try_from(metadata.len()).ok(),
        schema_version,
        updated_at: Some(updated_at),
    });
}
