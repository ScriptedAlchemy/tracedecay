//! Per-connection project routing: initialize-roots resolution and the
//! connection-scoped route state threaded through request dispatch.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::global_db::RegisteredGlobalDb;
use crate::mcp::project_route::HookProjectRouteCache;

/// Per-connection routing and identity context, constructed once per client
/// connection (or per initialize-replay dispatch) and threaded through
/// [`McpServer::handle_request_for_connection`]. Bundling these values keeps
/// persisted application request correlation and cancellation scoped to the
/// exact client connection.
pub(crate) struct ConnectionRouteState {
    implicit_project_path: Option<PathBuf>,
    /// Connection-scoped prefix (`{mcp_instance_id}-c{seq}`) that widens
    /// client-chosen, connection-local envelope ids into store-unique
    /// application request identities.
    memory_request_scope: String,
    /// Hook workspace routing cache, refreshed per request.
    pub(crate) route_cache: HookProjectRouteCache,
}

impl ConnectionRouteState {
    pub(crate) fn new(memory_request_scope: String, route_cache: HookProjectRouteCache) -> Self {
        Self {
            implicit_project_path: None,
            memory_request_scope,
            route_cache,
        }
    }

    pub(crate) async fn observe_initialize(
        &mut self,
        params: Option<&Value>,
        registry_db: Option<&RegisteredGlobalDb>,
    ) {
        self.implicit_project_path =
            resolve_initialize_roots_project_path(params, registry_db).await;
    }

    pub(crate) fn implicit_project_path(&self) -> Option<&Path> {
        self.implicit_project_path.as_deref()
    }

    pub(crate) fn memory_request_scope(&self) -> &str {
        &self.memory_request_scope
    }
}

pub(crate) async fn resolve_initialize_roots_project_path(
    params: Option<&Value>,
    registry_db: Option<&RegisteredGlobalDb>,
) -> Option<PathBuf> {
    let roots = initialize_root_paths(params);
    if roots.is_empty() {
        return None;
    }
    let registry_db = registry_db?;
    for root in roots {
        match resolve_initialize_root_project_path(&root, registry_db).await {
            Ok(Some(project_path)) => return Some(project_path),
            Ok(None) => {}
            Err(_) => return None,
        }
    }
    None
}

async fn resolve_initialize_root_project_path(
    root: &Path,
    registry_db: &RegisteredGlobalDb,
) -> Result<Option<PathBuf>, InitializeRootResolutionError> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut candidates = Vec::with_capacity(2);

    for candidate in root.ancestors() {
        match registry_db
            .project_registry_context_by_alias(candidate)
            .await
        {
            Ok(Some(context)) => {
                candidates.push((candidate.to_path_buf(), context.project.project_id.clone()));
                break;
            }
            Ok(None) => {}
            Err(_) => return Err(InitializeRootResolutionError::AuthorityUnavailable),
        }
    }

    if let Some(git_root) = crate::worktree::git_worktree_root(&root) {
        let git_common_dir = crate::worktree::git_common_dir(&git_root);
        match registry_db
            .project_registry_context_by_identity(&git_root, git_common_dir.as_deref())
            .await
        {
            Ok(Some(context)) => candidates.push((git_root, context.project.project_id)),
            Ok(None) => {}
            Err(_) => return Err(InitializeRootResolutionError::AuthorityUnavailable),
        }
    }

    select_initialize_project_path(&candidates)
}

pub(crate) fn initialize_root_paths(params: Option<&Value>) -> Vec<PathBuf> {
    params
        .and_then(|p| p.get("roots"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|root| {
            let uri = root.get("uri").and_then(Value::as_str)?;
            crate::serve::local_path_from_mcp_root_uri(uri)
        })
        .collect()
}

fn select_initialize_project_path(
    candidates: &[(PathBuf, String)],
) -> Result<Option<PathBuf>, InitializeRootResolutionError> {
    let Some((preferred_path, preferred_project_id)) =
        candidates.iter().max_by(|(left_path, _), (right_path, _)| {
            left_path
                .components()
                .count()
                .cmp(&right_path.components().count())
                .then_with(|| right_path.cmp(left_path))
        })
    else {
        return Ok(None);
    };
    let preferred_depth = preferred_path.components().count();
    if candidates.iter().any(|(path, project_id)| {
        path.components().count() == preferred_depth && project_id != preferred_project_id
    }) {
        return Err(InitializeRootResolutionError::AmbiguousIdentity);
    }
    Ok(Some(preferred_path.clone()))
}

#[derive(Debug, PartialEq, Eq)]
enum InitializeRootResolutionError {
    AuthorityUnavailable,
    AmbiguousIdentity,
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::json;
    use tempfile::TempDir;

    use super::{resolve_initialize_roots_project_path, select_initialize_project_path};
    use crate::host_admission::HostAdmissionTestRuntimeV1;
    use tracedecay_usecases::host_admission::HostAdmissionScope;

    fn run_git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git must be available for routing tests");
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            root.display()
        );
    }

    fn initialize_params(root: &Path) -> serde_json::Value {
        json!({
            "roots": [{
                "uri": format!("file://{}", root.display()),
                "name": "workspace"
            }]
        })
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn renamed_registered_project_symlink_resolves_same_project() {
        let profile = TempDir::new().expect("profile temp dir");
        let projects = TempDir::new().expect("projects temp dir");
        let old_root = projects.path().join("old");
        let new_root = projects.path().join("new");
        fs::create_dir_all(&old_root).expect("create original project root");
        run_git(&old_root, &["init", "--quiet"]);

        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .expect("open registered profile runtime");
        let registry = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let git_common_dir =
            crate::worktree::git_common_dir(&old_root).expect("resolve git common dir");
        registry
            .upsert_code_project(
                "proj_renamed",
                &old_root,
                Some(&git_common_dir),
                None,
                Some("main"),
            )
            .await
            .expect("register original project root");
        crate::storage::write_repository_identity_marker(&old_root, "proj_renamed")
            .expect("write repository identity");

        fs::rename(&old_root, &new_root).expect("rename project root");
        symlink(&new_root, &old_root).expect("link old root to renamed root");

        let params = initialize_params(&new_root);
        let resolved = resolve_initialize_roots_project_path(Some(&params), Some(registry)).await;

        assert_eq!(
            resolved,
            Some(new_root.canonicalize().expect("canonical renamed root"))
        );
    }

    #[tokio::test]
    async fn linked_worktree_resolves_registered_primary_checkout() {
        let profile = TempDir::new().expect("profile temp dir");
        let projects = TempDir::new().expect("projects temp dir");
        let primary_root = projects.path().join("primary");
        let linked_root = projects.path().join("linked");
        fs::create_dir_all(&primary_root).expect("create primary checkout");
        run_git(&primary_root, &["init", "--quiet"]);
        fs::write(primary_root.join("README.md"), "fixture").expect("write initial file");
        run_git(&primary_root, &["add", "README.md"]);
        run_git(
            &primary_root,
            &[
                "-c",
                "user.name=TraceDecay Test",
                "-c",
                "user.email=test@tracedecay.invalid",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        );
        run_git(
            &primary_root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked_root.to_str().expect("UTF-8 linked root"),
                "HEAD",
            ],
        );

        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .expect("open registered profile runtime");
        let registry = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        let git_common_dir =
            crate::worktree::git_common_dir(&primary_root).expect("resolve git common dir");
        registry
            .upsert_code_project(
                "proj_primary",
                &primary_root,
                Some(&git_common_dir),
                None,
                Some("main"),
            )
            .await
            .expect("register primary checkout");
        crate::storage::write_repository_identity_marker(&primary_root, "proj_primary")
            .expect("write repository identity");

        let params = initialize_params(&linked_root);
        let resolved = resolve_initialize_roots_project_path(Some(&params), Some(registry)).await;

        assert_eq!(
            resolved,
            Some(linked_root.canonicalize().expect("canonical linked root"))
        );
    }

    #[tokio::test]
    async fn nested_registered_project_wins_over_registered_ancestor() {
        let profile = TempDir::new().expect("profile temp dir");
        let projects = TempDir::new().expect("projects temp dir");
        let ancestor_root = projects.path().join("ancestor");
        let nested_root = ancestor_root.join("nested");
        let workspace_root = nested_root.join("workspace");
        fs::create_dir_all(&workspace_root).expect("create nested workspace");

        let runtime = HostAdmissionTestRuntimeV1::profile(profile.path())
            .await
            .expect("open registered profile runtime");
        let registry = runtime
            .registered_database(HostAdmissionScope::Profile)
            .expect("registered profile database");
        registry
            .upsert_code_project("proj_ancestor", &ancestor_root, None, None, Some("main"))
            .await
            .expect("register ancestor project");
        registry
            .upsert_code_project("proj_nested", &nested_root, None, None, Some("main"))
            .await
            .expect("register nested project");

        let params = initialize_params(&workspace_root);
        let resolved = resolve_initialize_roots_project_path(Some(&params), Some(registry)).await;

        assert_eq!(
            resolved,
            Some(nested_root.canonicalize().expect("canonical nested root"))
        );
    }

    #[test]
    fn equal_depth_candidates_keep_lexicographic_tie_break() {
        let candidates = vec![
            (PathBuf::from("/repo/zeta"), "proj_same".to_string()),
            (PathBuf::from("/repo/alpha"), "proj_same".to_string()),
        ];

        assert_eq!(
            select_initialize_project_path(&candidates),
            Ok(Some(PathBuf::from("/repo/alpha")))
        );
    }

    #[test]
    fn equal_depth_conflicting_identities_fail_closed() {
        let candidates = vec![
            (PathBuf::from("/repo/zeta"), "proj_zeta".to_string()),
            (PathBuf::from("/repo/alpha"), "proj_alpha".to_string()),
        ];

        assert!(select_initialize_project_path(&candidates).is_err());
    }
}
