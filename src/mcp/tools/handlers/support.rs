//! Shared helpers for MCP tool handlers.
//!
//! Keep this module free of tool dispatch logic. Handler modules use it for
//! argument normalization, scope filtering, and registered-project selection.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::{CodeProjectRecord, GlobalDb, ProjectRegistryContext};

/// Extracts the `node_id` parameter from tool arguments, accepting `id` as a
/// fallback alias. LLMs occasionally shorten `node_id` to `id`; this avoids a
/// confusing error when that happens.
pub(super) fn require_node_id(args: &Value) -> Result<&str> {
    args.get("node_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: node_id".to_string(),
        })
}

/// Returns the user-provided `path` argument, falling back to the scope
/// prefix when the argument is absent. This makes listing tools
/// automatically scoped to the subdirectory the server was launched from.
pub(super) fn effective_path<'a>(
    args: &'a Value,
    scope_prefix: Option<&'a str>,
) -> Option<&'a str> {
    args.get("path").and_then(|v| v.as_str()).or(scope_prefix)
}

/// Returns string elements from an optional JSON array argument.
pub(super) fn string_array_values(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Filters a Vec of items by file path prefix when a scope is active.
/// Returns the vec unchanged when `scope_prefix` is `None`.
pub(super) fn filter_by_scope<T, F>(
    items: Vec<T>,
    scope_prefix: Option<&str>,
    get_path: F,
) -> Vec<T>
where
    F: Fn(&T) -> &str,
{
    match scope_prefix {
        Some(prefix) => {
            let with_slash = if prefix.ends_with('/') {
                prefix.to_string()
            } else {
                format!("{prefix}/")
            };
            items
                .into_iter()
                .filter(|item| {
                    let p = get_path(item);
                    p.starts_with(&with_slash) || p == prefix
                })
                .collect()
        }
        None => items,
    }
}

/// Deduplicates an iterator of file path strings into a `Vec<String>`.
pub(super) fn unique_file_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for p in paths {
        if seen.insert(p) {
            result.push(p.to_string());
        }
    }
    result
}

pub(super) fn safe_profile_relpath(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(TraceDecayError::Config {
            message: format!("registry artifact path is not a safe profile-relative path: {value}"),
        });
    }
    Ok(path)
}

fn global_db_profile_root() -> Result<PathBuf> {
    crate::storage::default_profile_root()
}

pub(super) fn profile_root_for_global_db(
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<PathBuf> {
    if let Some(global_db) = global_db {
        return global_db
            .db_path()
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| TraceDecayError::Config {
                message: "could not resolve tracedecay profile root".to_string(),
            });
    }
    if !allow_default_registry_fallback {
        return Err(TraceDecayError::Config {
            message: "client project registry is unavailable for selector resolution".to_string(),
        });
    }
    global_db_profile_root()
}

pub(super) fn project_selector_present(args: &Value, top_level_path_keys: &[&str]) -> bool {
    args.get("project_selector").is_some()
        || args.get("project_id").is_some()
        || top_level_path_keys
            .iter()
            .any(|key| args.get(*key).is_some())
}

pub(super) async fn project_registry_context(
    args: &Value,
    top_level_path_keys: &[&str],
    global_db: Option<&GlobalDb>,
    allow_default_registry_fallback: bool,
) -> Result<Option<ProjectRegistryContext>> {
    let selector_present = project_selector_present(args, top_level_path_keys);
    let selector = args
        .get("project_selector")
        .map(|value| {
            value.as_object().ok_or_else(|| TraceDecayError::Config {
                message: "project_selector must be an object".to_string(),
            })
        })
        .transpose()?;
    let project_id = selector
        .and_then(|selector| selector.get("project_id"))
        .or_else(|| args.get("project_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let project_path = selector
        .and_then(|selector| {
            selector
                .get("path")
                .or_else(|| selector.get("project_path"))
        })
        .or_else(|| top_level_path_keys.iter().find_map(|key| args.get(*key)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if project_id.is_none() && project_path.is_none() {
        if selector_present {
            return Err(TraceDecayError::Config {
                message: "project selector must include project_id or project_path".to_string(),
            });
        }
        return Ok(None);
    }

    let owned_db;
    let db = match global_db {
        Some(db) => db,
        None if allow_default_registry_fallback => {
            owned_db = GlobalDb::open()
                .await
                .ok_or_else(|| TraceDecayError::Config {
                    message:
                        "could not open tracedecay project registry; run tracedecay init first"
                            .to_string(),
                })?;
            &owned_db
        }
        None => {
            return Err(TraceDecayError::Config {
                message: "client project registry is unavailable for selector resolution"
                    .to_string(),
            });
        }
    };
    let context = resolve_project_registry_context(db, project_id, project_path).await;

    context
        .ok_or_else(|| unresolved_project_selector_error(project_id, project_path))
        .map(Some)
}

async fn resolve_project_registry_context(
    db: &GlobalDb,
    project_id: Option<&str>,
    project_path: Option<&str>,
) -> Option<ProjectRegistryContext> {
    if let Some(project_id) = project_id {
        return db.project_registry_context_by_id(project_id).await;
    }
    let project_path = project_path?;
    let selector_path = Path::new(project_path);
    if let Some(context) = db.project_registry_context_by_alias(selector_path).await {
        return Some(context);
    }
    if GlobalDb::is_explicit_project_path_selector(project_path) {
        let git_common_dir = crate::worktree::git_common_dir(selector_path);
        if let Some(context) = db
            .project_registry_context_by_identity(selector_path, git_common_dir.as_deref())
            .await
        {
            return Some(context);
        }
    }
    let basename = bare_project_name(project_path)?;
    unique_project_basename_context(db, basename).await
}

async fn unique_project_basename_context(
    db: &GlobalDb,
    basename: &str,
) -> Option<ProjectRegistryContext> {
    let mut matching_ids = Vec::new();
    for project in db.search_code_projects(basename, usize::MAX).await {
        if !project_basename_matches(&project, basename)
            || matching_ids.contains(&project.project_id)
        {
            continue;
        }
        matching_ids.push(project.project_id);
        if matching_ids.len() > 1 {
            return None;
        }
    }
    let project_id = matching_ids.into_iter().next()?;
    db.project_registry_context_by_id(&project_id).await
}

fn bare_project_name(value: &str) -> Option<&str> {
    let mut components = Path::new(value).components();
    let first = components.next()?;
    if components.next().is_some() {
        return None;
    }
    match first {
        Component::Normal(name) => name.to_str().filter(|name| !name.is_empty()),
        _ => None,
    }
}

fn project_basename_matches(project: &CodeProjectRecord, basename: &str) -> bool {
    [
        project.display_root.as_str(),
        project.canonical_root.as_str(),
    ]
    .into_iter()
    .filter_map(|root| Path::new(root).file_name())
    .any(|name| name == basename)
}

fn unresolved_project_selector_error(
    project_id: Option<&str>,
    project_path: Option<&str>,
) -> TraceDecayError {
    let selector = project_id
        .map(|value| format!("project_id={value}"))
        .or_else(|| project_path.map(|value| format!("project_path={value}")))
        .unwrap_or_else(|| "empty selector".to_string());
    TraceDecayError::Config {
        message: format!(
            "registered project not found for selector ({selector}); run tracedecay_project_search to find the registered project_id or full project_path"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use libsql::{params, Connection};
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use crate::global_db::GlobalDb;

    use super::{
        require_node_id, resolve_project_registry_context, string_array_values,
        unique_project_basename_context,
    };

    static CWD_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    struct CurrentDirGuard(PathBuf);

    impl CurrentDirGuard {
        fn capture() -> Result<Self, std::io::Error> {
            std::env::current_dir().map(Self)
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    struct TestProjectSeed {
        project_id: String,
        project_root: PathBuf,
    }

    async fn seed_code_project_registry_rows(
        db: &GlobalDb,
        projects: &[TestProjectSeed],
    ) -> std::result::Result<(), libsql::Error> {
        let conn = db.dashboard_connection();
        conn.execute("BEGIN IMMEDIATE", ()).await?;
        if let Err(err) = seed_code_project_registry_rows_in_tx(&conn, projects).await {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }
        if let Err(err) = conn.execute("COMMIT", ()).await {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }
        Ok(())
    }

    async fn seed_code_project_registry_rows_in_tx(
        conn: &Connection,
        projects: &[TestProjectSeed],
    ) -> std::result::Result<(), libsql::Error> {
        let now = crate::tracedecay::current_timestamp();
        for project in projects {
            let canonical_root = GlobalDb::canonical_project_key(&project.project_root);
            let display_root = project.project_root.to_string_lossy().to_string();
            conn.execute(
                "INSERT INTO code_projects
                 (project_id, canonical_root, display_root, git_common_dir, git_remote_url,
                  default_branch, created_at, last_seen_at)
                 VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5, ?5)
                 ON CONFLICT(project_id) DO UPDATE SET
                    canonical_root = excluded.canonical_root,
                    display_root = excluded.display_root,
                    git_common_dir = excluded.git_common_dir,
                    git_remote_url = excluded.git_remote_url,
                    default_branch = excluded.default_branch,
                    last_seen_at = excluded.last_seen_at",
                params![
                    project.project_id.as_str(),
                    canonical_root.as_str(),
                    display_root.as_str(),
                    "main",
                    now,
                ],
            )
            .await?;
            conn.execute(
                "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(alias_path) DO UPDATE SET
                    project_id = excluded.project_id,
                    last_seen_at = excluded.last_seen_at",
                params![canonical_root.as_str(), project.project_id.as_str(), now],
            )
            .await?;
        }
        Ok(())
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        #[cfg(not(windows))]
        {
            paths.push(PathBuf::from("/usr/bin"));
            paths.push(PathBuf::from("/bin"));
        }
        let mut command = Command::new("git");
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
        let output = command
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_require_node_id_canonical() {
        let args = json!({"node_id": "fn:abc123"});
        assert!(matches!(require_node_id(&args), Ok("fn:abc123")));
    }

    #[test]
    fn test_require_node_id_alias() {
        let args = json!({"id": "trait:def456"});
        assert!(matches!(require_node_id(&args), Ok("trait:def456")));
    }

    #[test]
    fn test_require_node_id_prefers_canonical() {
        let args = json!({"node_id": "fn:canonical", "id": "fn:alias"});
        assert!(matches!(require_node_id(&args), Ok("fn:canonical")));
    }

    #[test]
    fn test_require_node_id_missing() {
        let args = json!({"query": "something"});
        assert!(require_node_id(&args).is_err());
    }

    #[test]
    fn test_string_array_values_keeps_only_string_items() {
        let args = json!({
            "values": ["alpha", 7, null, "beta"],
            "not_array": "alpha"
        });

        assert_eq!(
            string_array_values(&args, "values"),
            vec!["alpha".to_string(), "beta".to_string()]
        );
        assert!(string_array_values(&args, "missing").is_empty());
        assert!(string_array_values(&args, "not_array").is_empty());
    }

    #[tokio::test]
    async fn unique_project_basename_context_scans_past_first_search_page(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let db = GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .ok_or_else(|| std::io::Error::other("failed to open test global db"))?;

        let mut projects = Vec::with_capacity(102);
        projects.push(TestProjectSeed {
            project_id: "z_exact_old".to_string(),
            project_root: dir.path().join("first").join("target"),
        });
        for index in 0..100 {
            projects.push(TestProjectSeed {
                project_id: format!("n_noise_{index:03}"),
                project_root: dir
                    .path()
                    .join("noise")
                    .join(format!("target-noise-{index:03}")),
            });
        }
        projects.push(TestProjectSeed {
            project_id: "a_exact_new".to_string(),
            project_root: dir.path().join("second").join("target"),
        });
        seed_code_project_registry_rows(&db, &projects).await?;

        assert!(
            unique_project_basename_context(&db, "target")
                .await
                .is_none(),
            "duplicate exact basenames must fail closed even when one match falls outside the first search page"
        );
        Ok(())
    }

    #[tokio::test]
    async fn bare_project_path_selector_prefers_unique_basename_over_cwd_git_identity(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let _guard = CWD_TEST_LOCK.lock().await;
        let _cwd_guard = CurrentDirGuard::capture()?;
        let dir = TempDir::new()?;
        let repo = dir.path().join("active-repo");
        let active_root = repo.join("active");
        let incidental_target_dir = repo.join("target");
        let target_root = dir.path().join("registered").join("target");
        std::fs::create_dir_all(&active_root)?;
        std::fs::create_dir_all(&incidental_target_dir)?;
        std::fs::create_dir_all(&target_root)?;
        run_git(&repo, &["init", "--quiet"]);

        let db = GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .ok_or_else(|| std::io::Error::other("failed to open test global db"))?;
        db.upsert_code_project(
            "proj_active",
            &active_root,
            Some(&repo.join(".git")),
            None,
            Some("main"),
        )
        .await
        .ok_or_else(|| std::io::Error::other("failed to seed active project"))?;
        db.upsert_code_project("proj_target", &target_root, None, None, Some("main"))
            .await
            .ok_or_else(|| std::io::Error::other("failed to seed target project"))?;

        std::env::set_current_dir(&repo)?;
        let context = resolve_project_registry_context(&db, None, Some("target"))
            .await
            .ok_or_else(|| std::io::Error::other("failed to resolve bare target selector"))?;

        assert_eq!(
            context.project.project_id, "proj_target",
            "bare project_path selectors should keep basename semantics even when cwd has a git-tracked directory with the same name"
        );
        Ok(())
    }
}
