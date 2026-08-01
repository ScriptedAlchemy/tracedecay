//! Resolution of a legacy store's durable target project identity.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::copy::{MIGRATION_QUERY_PAGE_ROWS, ensure_materialized_row_room, table_columns};
use crate::root_seam::global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};
use tracedecay_runtime_core::path_safety::{
    canonicalize_existing_prefix, canonicalize_path_or_existing_parent,
};

pub struct ResolvedTargetProject {
    pub root: PathBuf,
    pub registry_project_id: Option<String>,
    pub user_scope: bool,
}

/// Compares two paths after canonicalizing the deepest existing ancestor and
/// reattaching any missing tail. This preserves OS aliases such as macOS
/// `/var` -> `/private/var` even after the final project directory was moved
/// or a symlink alias was removed.
pub fn same_path(left: &Path, right: &Path) -> bool {
    canonicalize_path_or_existing_parent(left) == canonicalize_path_or_existing_parent(right)
}

fn real_project_root(
    candidate: &Path,
    user_home: &Path,
    hermes_homes: &[PathBuf],
) -> Option<PathBuf> {
    if !candidate.is_absolute() || !candidate.is_dir() {
        return None;
    }
    let canonical = candidate.canonicalize().ok()?;
    let canonical_user_home = user_home
        .canonicalize()
        .unwrap_or_else(|_| user_home.to_path_buf());
    let is_hermes_home = hermes_homes.iter().any(|hermes_home| {
        let canonical_hermes_home = hermes_home
            .canonicalize()
            .unwrap_or_else(|_| hermes_home.clone());
        canonical == canonical_hermes_home
    });
    if canonical == canonical_user_home || is_hermes_home {
        return None;
    }
    if let Some(git_root) = tracedecay_runtime_core::worktree::git_worktree_root(&canonical) {
        return Some(git_root);
    }
    tracedecay_runtime_core::config::has_project_database(&canonical).then_some(canonical)
}

fn target_key(target: &ResolvedTargetProject) -> String {
    target.registry_project_id.clone().unwrap_or_else(|| {
        format!(
            "path:{}",
            RegisteredGlobalDb::canonical_project_key(&target.root)
        )
    })
}

fn project_identity_collision(
    key: &str,
    existing: &ResolvedTargetProject,
    target: &ResolvedTargetProject,
) -> String {
    format!(
        "registered project identity '{key}' maps to both '{}' and '{}'; refusing a collision",
        existing.root.display(),
        target.root.display()
    )
}

fn is_projectless_candidate(candidate: &Path, user_home: &Path, hermes_homes: &[PathBuf]) -> bool {
    if candidate.as_os_str().is_empty() || candidate == Path::new("user") {
        return true;
    }
    if same_path(candidate, user_home) {
        return true;
    }
    hermes_homes
        .iter()
        .any(|hermes_home| same_path(candidate, hermes_home))
}

fn collect_metadata_project_candidates(
    raw: &str,
    candidates: &mut BTreeSet<PathBuf>,
) -> Result<(), ()> {
    let metadata = serde_json::from_str::<serde_json::Value>(raw).map_err(|_| ())?;
    let metadata = metadata.as_object().ok_or(())?;
    for key in [
        "hermes_session_cwd",
        "hermes_session_worktree",
        "cwd",
        "worktree",
        "project_root",
    ] {
        if let Some(value) = metadata.get(key) {
            let path = value.as_str().ok_or(())?;
            candidates.insert(PathBuf::from(path));
        }
    }
    Ok(())
}

async fn resolve_project_candidate(
    candidate: &Path,
    user_home: &Path,
    hermes_homes: &[PathBuf],
    registry: Option<&RegisteredGlobalDb>,
) -> Result<Option<ResolvedTargetProject>, String> {
    if !candidate.is_absolute() {
        return Ok(None);
    }

    let canonical_candidate = canonicalize_existing_prefix(candidate);
    let context = if let Some(registry) = registry {
        let direct = registry
            .project_registry_context_by_alias(candidate)
            .await
            .map_err(|error| error.to_string())?;
        match (direct, canonical_candidate.as_deref()) {
            (Some(context), _) => Some(context),
            (None, Some(canonical)) if canonical != candidate => registry
                .project_registry_context_by_alias(canonical)
                .await
                .map_err(|error| error.to_string())?,
            _ => None,
        }
    } else {
        None
    };
    if let Some(context) = context {
        let mut registered_paths = vec![
            PathBuf::from(&context.project.display_root),
            PathBuf::from(&context.project.canonical_root),
        ];
        registered_paths.extend(
            context
                .aliases
                .iter()
                .map(|alias| PathBuf::from(&alias.alias_path)),
        );
        for registered_path in registered_paths {
            if let Some(root) = real_project_root(&registered_path, user_home, hermes_homes) {
                return Ok(Some(ResolvedTargetProject {
                    root,
                    registry_project_id: Some(context.project.project_id),
                    user_scope: false,
                }));
            }
        }
        return Err(format!(
            "registered project alias '{}' maps to '{}', but no durable current project root exists",
            candidate.display(),
            context.project.project_id
        ));
    }

    Ok(
        real_project_root(candidate, user_home, hermes_homes).map(|root| ResolvedTargetProject {
            root,
            registry_project_id: None,
            user_scope: false,
        }),
    )
}

pub async fn resolve_target_project<Q>(
    source: Option<&Q>,
    registry: Option<&RegisteredGlobalDb>,
    config_path: &Path,
    user_home: &Path,
    hermes_homes: &[PathBuf],
) -> Result<ResolvedTargetProject, String>
where
    Q: QueryExecutor + ?Sized,
{
    if let Some(pin) =
        crate::root_seam::agents::hermes::read_config_pinned_project_root(config_path)
    {
        return resolve_project_candidate(Path::new(&pin), user_home, hermes_homes, registry)
            .await?
            .ok_or_else(|| format!("legacy project pin '{pin}' is not a resolvable code project"));
    }

    let source = source
        .ok_or_else(|| "legacy memory store has no project pin or session metadata".to_string())?;
    let columns = table_columns(source, "sessions").await?;
    if columns.is_empty() {
        return Err("source has no sessions table and no legacy project pin".to_string());
    }
    let path_expr = if columns.iter().any(|column| column == "project_path") {
        "project_path"
    } else {
        "NULL"
    };
    let key_expr = if columns.iter().any(|column| column == "project_key") {
        "project_key"
    } else {
        "NULL"
    };
    let metadata_expr = if columns.iter().any(|column| column == "metadata_json") {
        "metadata_json"
    } else {
        "NULL"
    };
    let mut targets: BTreeMap<String, ResolvedTargetProject> = BTreeMap::new();
    let mut has_projectless_evidence = false;
    let mut has_unresolved_project_evidence = false;
    let mut last_rowid = i64::MIN;
    let mut first_page = true;
    loop {
        let sql = format!(
            "SELECT rowid, {path_expr}, {key_expr}, {metadata_expr} FROM sessions
             WHERE rowid > ?1 OR (?3 = 1 AND rowid = ?1)
             ORDER BY rowid LIMIT ?2"
        );
        let mut rows = source
            .query(
                &sql,
                params![last_rowid, MIGRATION_QUERY_PAGE_ROWS, i64::from(first_page)],
            )
            .await
            .map_err(|error| format!("could not read source project metadata: {error}"))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| format!("could not read source project metadata row: {error}"))?
        {
            let rowid = row
                .get::<i64>(0)
                .map_err(|error| format!("invalid source session rowid: {error}"))?;
            if rowid < last_rowid || (rowid == last_rowid && (!first_page || page_rows > 0)) {
                return Err("source sessions returned an unstable row order".to_string());
            }
            last_rowid = rowid;
            page_rows += 1;
            let mut candidates = BTreeSet::new();
            for candidate in [row.get::<Option<String>>(1), row.get::<Option<String>>(2)]
                .into_iter()
                .flatten()
                .flatten()
            {
                candidates.insert(PathBuf::from(candidate));
            }
            let malformed_metadata = match row.get::<Option<String>>(3) {
                Ok(Some(metadata)) => {
                    collect_metadata_project_candidates(&metadata, &mut candidates).is_err()
                }
                Ok(None) => false,
                Err(_) => true,
            };
            let mut row_targets: BTreeMap<String, ResolvedTargetProject> = BTreeMap::new();
            let mut row_has_unresolved_project_evidence = malformed_metadata;
            for candidate in candidates {
                if is_projectless_candidate(&candidate, user_home, hermes_homes) {
                    continue;
                }
                let resolved =
                    resolve_project_candidate(&candidate, user_home, hermes_homes, registry)
                        .await?;
                let Some(target) = resolved else {
                    row_has_unresolved_project_evidence = true;
                    continue;
                };
                let key = target_key(&target);
                if let Some(existing) = row_targets.get(&key)
                    && !same_path(&existing.root, &target.root)
                {
                    return Err(project_identity_collision(&key, existing, &target));
                }
                row_targets.insert(key, target);
            }
            if row_targets.len() > 1 {
                return Err(format!(
                    "one source session maps to {} projects; refusing an ambiguous migration",
                    row_targets.len()
                ));
            }
            if row_has_unresolved_project_evidence {
                has_unresolved_project_evidence = true;
            }
            if let Some((key, target)) = row_targets.into_iter().next() {
                if let Some(existing) = targets.get(&key)
                    && !same_path(&existing.root, &target.root)
                {
                    return Err(project_identity_collision(&key, existing, &target));
                }
                if !targets.contains_key(&key) {
                    ensure_materialized_row_room(targets.len(), "resolved project map")?;
                }
                targets.insert(key, target);
            } else if !row_has_unresolved_project_evidence {
                has_projectless_evidence = true;
            }
        }
        if page_rows < MIGRATION_QUERY_PAGE_ROWS {
            break;
        }
        first_page = false;
    }
    match targets.len() {
        1 if !has_projectless_evidence && !has_unresolved_project_evidence => targets
            .into_values()
            .next()
            .ok_or_else(|| "resolved project target disappeared".to_string()),
        0 if !has_unresolved_project_evidence => Ok(ResolvedTargetProject {
            root: PathBuf::from("user"),
            registry_project_id: None,
            user_scope: true,
        }),
        0 => Err("no durable real project path exists in source session metadata".to_string()),
        1 => Err(
            "source session metadata mixes projectless or unresolved evidence with a project; refusing an ambiguous migration"
                .to_string(),
        ),
        count => Err(format!(
            "source session metadata maps to {count} projects; refusing an ambiguous migration"
        )),
    }
}
