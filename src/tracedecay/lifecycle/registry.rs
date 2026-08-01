//! Global-registry registration: publishing a profile-sharded store's
//! project, store, and branch scope rows so cross-project lookups and the
//! registry-driven identity resolution in [`super::identity`] can find it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex as StdMutex};
use std::time::SystemTime;

use crate::branch_meta;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::{
    GraphScopeUpsert, RegisteredGlobalDb, StoreArtifactUpsert, StoreInstanceUpsert,
};
use crate::storage::{self, StoreLayout};
use crate::tracedecay::current_timestamp;

use super::TraceDecay;

/// Cheap fingerprint of everything that would change what
/// [`TraceDecay::register_project_store_in_global_registry`] writes.
///
/// Every field here is load-bearing for the duplicate-store bug the
/// registration body guards against (see the comments on `git_common_dir`
/// and `primary_root` below): dropping `git_common_dir` would make a sibling
/// checkout's next first touch mint a fresh store, and dropping
/// `canonical_root` would let a linked worktree's registration pin the
/// project's canonical/display root to a transient path. `tracked_branches`
/// and the artifact mtimes catch every other observable change (branch
/// tracking, store file replacement) that this function is responsible for
/// publishing.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RegistrationDigest {
    project_id: String,
    canonical_root: PathBuf,
    git_common_dir: Option<PathBuf>,
    tracked_branches: BTreeSet<String>,
    artifact_mtimes: Vec<Option<SystemTime>>,
}

/// Process-global cache of the last digest successfully registered for each
/// project id, so a redundant `register_project_store_in_global_registry`
/// call (every writable open re-runs this) can skip straight to `Ok(())`
/// instead of redoing branch-meta/git lookups and every upsert.
static LAST_REGISTERED_DIGEST: LazyLock<StdMutex<HashMap<String, RegistrationDigest>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// Pure equality check split out of the caching logic above so it can be
/// unit tested against a synthetic cache without a real global database.
fn registration_digest_matches(
    cache: &HashMap<String, RegistrationDigest>,
    project_id: &str,
    digest: &RegistrationDigest,
) -> bool {
    cache.get(project_id) == Some(digest)
}

fn artifact_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

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

        let global_db = self.profile_database.as_ref();

        let meta = branch_meta::load_branch_meta(&self.store_layout.data_root);
        let default_branch = meta.as_ref().map(|meta| meta.default_branch.as_str());
        // Registering without the git common dir leaves the row unreachable
        // by repository identity, so the next first touch from a sibling
        // checkout mints a fresh store. Detached worktrees are no exception:
        // they belong to the same repository as every other checkout.
        let git_common_dir = crate::worktree::git_common_dir(&self.project_root);

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
        let registration_root = primary_root.as_deref().unwrap_or(&self.project_root);

        let tracked_branches: BTreeSet<String> = meta
            .as_ref()
            .map(|meta| meta.branches.keys().cloned().collect())
            .unwrap_or_default();
        let artifact_mtimes = vec![
            artifact_mtime(&self.store_layout.graph_db_path),
            artifact_mtime(&self.store_layout.sessions_db_path),
            artifact_mtime(&self.store_layout.branch_meta_path),
            self.store_layout
                .manifest_path
                .as_deref()
                .and_then(artifact_mtime),
        ];
        let digest = RegistrationDigest {
            project_id: project_id.to_string(),
            canonical_root: registration_root.to_path_buf(),
            git_common_dir: git_common_dir.clone(),
            tracked_branches,
            artifact_mtimes,
        };

        {
            let cache = LAST_REGISTERED_DIGEST
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if registration_digest_matches(&cache, project_id, &digest) {
                return Ok(());
            }
        }

        let _registry_write = REGISTRY_WRITE_LOCK.lock().await;
        // Re-check under the write lock: a concurrent writable open may have
        // just registered the same digest while we were computing ours.
        {
            let cache = LAST_REGISTERED_DIGEST
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if registration_digest_matches(&cache, project_id, &digest) {
                return Ok(());
            }
        }

        let git_remote_url = git_remote_url(&self.project_root);

        let previous_canonical_root = if primary_root.is_some() {
            global_db
                .get_code_project(project_id)
                .await
                .map(|record| record.canonical_root)
        } else {
            None
        };

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

        LAST_REGISTERED_DIGEST
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(project_id.to_string(), digest);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(canonical_root: &str, branches: &[&str]) -> RegistrationDigest {
        RegistrationDigest {
            project_id: "proj-1".to_string(),
            canonical_root: PathBuf::from(canonical_root),
            git_common_dir: Some(PathBuf::from("/repo/.git")),
            tracked_branches: branches.iter().map(|b| b.to_string()).collect(),
            artifact_mtimes: vec![None, None, None, None],
        }
    }

    /// Simulates the real call path: check-then-maybe-register-then-record,
    /// counting how many times a "register" (the expensive upsert body)
    /// would actually run.
    fn simulate_call(
        cache: &mut HashMap<String, RegistrationDigest>,
        project_id: &str,
        digest: &RegistrationDigest,
        register_calls: &mut u32,
    ) {
        if registration_digest_matches(cache, project_id, digest) {
            return;
        }
        *register_calls += 1;
        cache.insert(project_id.to_string(), digest.clone());
    }

    #[test]
    fn identical_inputs_skip_the_second_registration() {
        let mut cache = HashMap::new();
        let mut register_calls = 0;
        let d = digest("/repo", &["main"]);

        simulate_call(&mut cache, "proj-1", &d, &mut register_calls);
        simulate_call(&mut cache, "proj-1", &d, &mut register_calls);

        assert_eq!(
            register_calls, 1,
            "second call with an identical digest must not re-register"
        );
    }

    #[test]
    fn changed_branch_set_does_not_skip() {
        let mut cache = HashMap::new();
        let mut register_calls = 0;
        let first = digest("/repo", &["main"]);
        let second = digest("/repo", &["main", "feature/x"]);

        simulate_call(&mut cache, "proj-1", &first, &mut register_calls);
        simulate_call(&mut cache, "proj-1", &second, &mut register_calls);

        assert_eq!(
            register_calls, 2,
            "a changed tracked-branch set must force re-registration"
        );
    }

    #[test]
    fn changed_canonical_root_does_not_skip() {
        let mut cache = HashMap::new();
        let mut register_calls = 0;
        let first = digest("/repo", &["main"]);
        let second = digest("/other/primary-checkout", &["main"]);

        simulate_call(&mut cache, "proj-1", &first, &mut register_calls);
        simulate_call(&mut cache, "proj-1", &second, &mut register_calls);

        assert_eq!(
            register_calls, 2,
            "a changed canonical_root (primary-checkout redirect) must force re-registration"
        );
    }

    #[test]
    fn changed_git_common_dir_does_not_skip() {
        let mut cache = HashMap::new();
        let mut register_calls = 0;
        let mut first = digest("/repo", &["main"]);
        first.git_common_dir = Some(PathBuf::from("/repo/.git"));
        let mut second = first.clone();
        second.git_common_dir = None;

        simulate_call(&mut cache, "proj-1", &first, &mut register_calls);
        simulate_call(&mut cache, "proj-1", &second, &mut register_calls);

        assert_eq!(
            register_calls, 2,
            "a changed git_common_dir must force re-registration"
        );
    }

    #[test]
    fn changed_artifact_mtime_does_not_skip() {
        let mut cache = HashMap::new();
        let mut register_calls = 0;
        let mut first = digest("/repo", &["main"]);
        first.artifact_mtimes = vec![Some(SystemTime::UNIX_EPOCH), None, None, None];
        let mut second = first.clone();
        second.artifact_mtimes[0] = Some(SystemTime::now());

        simulate_call(&mut cache, "proj-1", &first, &mut register_calls);
        simulate_call(&mut cache, "proj-1", &second, &mut register_calls);

        assert_eq!(
            register_calls, 2,
            "a changed artifact mtime must force re-registration"
        );
    }

    #[test]
    fn different_project_ids_are_tracked_independently() {
        let mut cache = HashMap::new();
        let mut register_calls = 0;
        let a = digest("/repo-a", &["main"]);
        let mut b = digest("/repo-b", &["main"]);
        b.project_id = "proj-2".to_string();

        simulate_call(&mut cache, "proj-1", &a, &mut register_calls);
        simulate_call(&mut cache, "proj-2", &b, &mut register_calls);
        simulate_call(&mut cache, "proj-1", &a, &mut register_calls);
        simulate_call(&mut cache, "proj-2", &b, &mut register_calls);

        assert_eq!(register_calls, 2);
    }
}
