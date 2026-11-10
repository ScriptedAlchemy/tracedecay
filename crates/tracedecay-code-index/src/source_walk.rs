//! The one working-tree walk every source-reading surface shares.
//!
//! Grep, ast-grep search, and the module-mount audit must all agree on what
//! "a file in this project" means: the same `.gitignore` rules, the same
//! `index.exclude.v1` / `index.include.v1` path policy the code index captures
//! under, the same refusal to follow links. A second walker
//! built next to this one would drift, and a scan that disagrees with the one
//! the indexer used reports findings the rest of the product cannot see. The
//! walk is therefore public rather than crate-private, the audit in the root
//! crate reuses this policy instead of restating it.

pub mod classification;
pub mod submodules;
mod tracked_links;
pub use tracked_links::resolve_parent_source;

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ignore::WalkBuilder;
use ignore::overrides::{Override, OverrideBuilder};
use tracedecay_domain::{IndexPathPolicyV1, forward_slash_path};

pub type SourceWalk = std::iter::Flatten<std::vec::IntoIter<submodules::CheckoutWalk>>;

#[derive(Debug)]
pub struct SourceWalkError {
    pub glob: String,
    pub message: String,
}

/// Conservative directory reachability for a positive override. The override
/// matcher remains authoritative for files, including directory inheritance.
#[derive(Clone)]
struct PathGlobScope {
    literal_prefix: PathBuf,
    may_match_descendants: bool,
}

impl PathGlobScope {
    fn from_path_glob(path_glob: &str) -> Option<Self> {
        if path_glob != path_glob.trim() {
            return None;
        }
        let path_glob = path_glob.trim();
        if path_glob.is_empty() || path_glob.starts_with('!') || path_glob.contains('\\') {
            return None;
        }
        let segments = path_glob
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if segments
            .iter()
            .any(|segment| matches!(*segment, "." | ".."))
        {
            return None;
        }
        let matches_basename_at_any_depth = !path_glob.contains('/');
        let wildcard_start = segments
            .iter()
            .position(|segment| {
                segment.contains('*')
                    || segment.contains('?')
                    || segment.contains('[')
                    || segment.contains('{')
            })
            .unwrap_or(segments.len());
        let literal_prefix = if matches_basename_at_any_depth {
            PathBuf::new()
        } else {
            segments[..wildcard_start]
                .iter()
                .fold(PathBuf::new(), |mut prefix, segment| {
                    prefix.push(segment);
                    prefix
                })
        };
        let wildcard_suffix = &segments[wildcard_start..];
        let may_match_descendants = matches_basename_at_any_depth
            || wildcard_suffix
                .iter()
                .enumerate()
                .any(|(index, segment)| index > 0 || *segment == "**");
        Some(Self {
            literal_prefix,
            may_match_descendants,
        })
    }

    fn allows(&self, project_root: &Path, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(project_root) else {
            return false;
        };
        if self.literal_prefix.as_os_str().is_empty() {
            return self.may_match_descendants;
        }
        self.literal_prefix.starts_with(relative)
            || relative == self.literal_prefix
            || (self.may_match_descendants && relative.starts_with(&self.literal_prefix))
    }

    fn may_contain_match(&self, project_root: &Path, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(project_root) else {
            return false;
        };
        // A literal override can also name a directory, whose descendants
        // inherit its whitelist. Never prune below that literal prefix.
        self.literal_prefix.starts_with(relative) || relative.starts_with(&self.literal_prefix)
    }
}

const MAX_REPOSITORY_NAMES: usize = 8;
const MAX_REPOSITORY_PATH_CHARS: usize = 160;

fn nested_repository_marker(directory: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(directory.join(".git")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Default-deny all gitlinks in the parent's Git index even when a child
/// checkout's .git marker has been removed. A gitlink is a repository boundary,
/// not an ordinary directory the live source walker can authenticate.
/// This is deny-only; it never enables submodule admission.
pub fn parent_gitlink_paths(project_root: &Path) -> Result<BTreeSet<String>, SourceWalkError> {
    parent_gitlink_paths_scoped(project_root, None)
}

/// A positive source glob with a literal prefix cannot inspect paths outside
/// that prefix. Keep ancestor gitlinks and any gitlinks below a named directory.
pub(super) fn parent_gitlink_paths_scoped(
    project_root: &Path,
    scope: Option<&Path>,
) -> Result<BTreeSet<String>, SourceWalkError> {
    let scope = scope.filter(|scope| {
        !scope.as_os_str().is_empty()
            && scope
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
    });
    let repository = match tracedecay_runtime_core::git_open::open(project_root) {
        Ok(repository) => repository,
        Err(error) => {
            // Non-Git projects still support source walks. An initialized
            // checkout whose Git authority is unreadable must fail closed.
            return match nested_repository_marker(project_root) {
                Ok(false) => Ok(BTreeSet::new()),
                Ok(true) => Err(SourceWalkError {
                    glob: String::new(),
                    message: format!("cannot inspect parent Git boundaries: {error}"),
                }),
                Err(marker_error) => Err(SourceWalkError {
                    glob: String::new(),
                    message: format!("cannot inspect parent Git marker: {marker_error}"),
                }),
            };
        }
    };
    if repository
        .workdir()
        .and_then(|workdir| workdir.canonicalize().ok())
        != project_root.canonicalize().ok()
    {
        return Err(SourceWalkError {
            glob: String::new(),
            message: "source walk root is not the opened Git worktree".to_owned(),
        });
    }
    let index = repository
        .index_or_empty()
        .map_err(|error| SourceWalkError {
            glob: String::new(),
            message: format!("cannot inspect parent Git index: {error}"),
        })?;
    let mut paths = BTreeSet::new();
    {
        let mut record = |path: &[u8]| -> Result<(), SourceWalkError> {
            let relative = std::str::from_utf8(path).map_err(|_| SourceWalkError {
                glob: String::new(),
                message: "non-UTF-8 parent gitlink cannot be safely scoped".to_owned(),
            })?;
            paths.insert(relative.to_owned());
            Ok(())
        };
        let entries = index.entries();
        if let Some(scope) = scope {
            // Git index entries are sorted by byte-exact path. Probe every
            // ancestor, including unmerged stages, then only the prefix range.
            for ancestor in scope.ancestors().filter(|path| !path.as_os_str().is_empty()) {
                let name = forward_slash_path(ancestor);
                let bytes = name.as_bytes();
                let start = entries.partition_point(|entry| {
                    let path: &[u8] = entry.path(&index).as_ref();
                    path < bytes
                });
                for entry in entries[start..].iter().take_while(|entry| {
                    let path: &[u8] = entry.path(&index).as_ref();
                    path == bytes
                }) {
                    if entry.mode.is_submodule() {
                        record(bytes)?;
                    }
                }
            }
            let prefix = format!("{}/", forward_slash_path(scope));
            let start = entries.partition_point(|entry| {
                let path: &[u8] = entry.path(&index).as_ref();
                path < prefix.as_bytes()
            });
            for entry in entries[start..].iter().take_while(|entry| {
                let path: &[u8] = entry.path(&index).as_ref();
                path.starts_with(prefix.as_bytes())
            }) {
                if entry.mode.is_submodule() {
                    let path: &[u8] = entry.path(&index).as_ref();
                    record(path)?;
                }
            }
        } else {
            for entry in entries {
                if entry.mode.is_submodule() {
                    let path: &[u8] = entry.path(&index).as_ref();
                    record(path)?;
                }
            }
        }
    }
    // A staged gitlink removal does not erase the checkout boundary from
    // HEAD. Without this second authority, a child whose .git marker was
    // removed becomes ordinary live source until the parent commits.
    let head = repository.head().map_err(|error| SourceWalkError {
        glob: String::new(),
        message: format!("cannot inspect parent Git HEAD: {error}"),
    })?;
    if head.id().is_some() {
        let commit = repository.head_commit().map_err(|error| SourceWalkError {
            glob: String::new(),
            message: format!("cannot resolve parent Git HEAD: {error}"),
        })?;
        let tree = commit.tree().map_err(|error| SourceWalkError {
            glob: String::new(),
            message: format!("cannot read parent Git HEAD tree: {error}"),
        })?;
        let mut record = |path: &[u8]| -> Result<(), SourceWalkError> {
            let relative = std::str::from_utf8(path).map_err(|_| SourceWalkError {
                glob: String::new(),
                message: "non-UTF-8 HEAD gitlink cannot be safely scoped".to_owned(),
            })?;
            paths.insert(relative.to_owned());
            Ok(())
        };
        if let Some(scope) = scope {
            // A staged gitlink removal survives in HEAD even without a child
            // marker. Follow only the scope's ancestors and committed subtree.
            let ancestors = scope
                .ancestors()
                .filter(|path| !path.as_os_str().is_empty())
                .collect::<Vec<_>>();
            for ancestor in ancestors.into_iter().rev() {
                let Some(entry) = tree
                    .lookup_entry_by_path(ancestor)
                    .map_err(|error| SourceWalkError {
                        glob: String::new(),
                        message: format!("cannot inspect scoped parent HEAD tree: {error}"),
                    })?
                else {
                    break;
                };
                if entry.mode().is_commit() {
                    record(forward_slash_path(ancestor).as_bytes())?;
                    break;
                }
                if !entry.mode().is_tree() {
                    break;
                }
                if ancestor == scope {
                    let subtree = repository
                        .find_tree(entry.object_id())
                        .map_err(|error| SourceWalkError {
                            glob: String::new(),
                            message: format!("cannot read scoped parent HEAD tree: {error}"),
                        })?;
                    let entries = subtree
                        .traverse()
                        .breadthfirst
                        .files()
                        .map_err(|error| SourceWalkError {
                            glob: String::new(),
                            message: format!("cannot walk scoped parent HEAD tree: {error}"),
                        })?;
                    let prefix = forward_slash_path(scope);
                    for entry in entries {
                        if entry.mode.is_commit() {
                            let mut path = prefix.as_bytes().to_vec();
                            path.push(b'/');
                            path.extend_from_slice(entry.filepath.as_ref());
                            record(&path)?;
                        }
                    }
                }
            }
        } else {
            let entries = tree
                .traverse()
                .breadthfirst
                .files()
                .map_err(|error| SourceWalkError {
                    glob: String::new(),
                    message: format!("cannot walk parent Git HEAD tree: {error}"),
                })?;
            for entry in entries {
                if entry.mode.is_commit() {
                    record(entry.filepath.as_ref())?;
                }
            }
        }
    } else if head.referent_name().is_none() {
        return Err(SourceWalkError {
            glob: String::new(),
            message: "parent Git HEAD has neither a commit nor an unborn branch".to_owned(),
        });
    }
    Ok(paths)
}

/// Reject a source beneath another checkout or a symlinked directory.
/// The root's own Git metadata is exempt. This boundary is shared by
/// interactive source walks and the indexed worktree candidate roster,
/// so neither may admit bytes the other refuses.
pub fn has_nested_repository_ancestor(
    project_root: &Path,
    relative_path: &Path,
) -> std::io::Result<bool> {
    if relative_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "source path must be repository-relative",
        ));
    }

    let mut parent = relative_path.parent();
    while let Some(relative_dir) = parent {
        if relative_dir.as_os_str().is_empty() {
            break;
        }
        let directory = project_root.join(relative_dir);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.is_dir() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if nested_repository_marker(&directory)? {
            return Ok(true);
        }
        parent = relative_dir.parent();
    }
    Ok(false)
}

/// Distinguish leaf links before selecting their source authority.
/// Missing paths remain possible tombstones and are not symlink exclusions.
pub fn is_source_symlink(project_root: &Path, relative_path: &Path) -> std::io::Result<bool> {
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "source path must be repository-relative",
        ));
    }
    match std::fs::symlink_metadata(project_root.join(relative_path)) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Diagnostics emitted by the same source walker that enforces the nested
/// repository boundary. Only directories within the walk's policy are named.
#[derive(Clone, Default)]
pub(crate) struct SourceWalkDiagnostics {
    skipped_nested_repositories: Arc<Mutex<NestedRepositorySummary>>,
}

#[derive(Default)]
struct NestedRepositorySummary {
    total: usize,
    names: BTreeSet<String>,
}

impl SourceWalkDiagnostics {
    fn record_skipped_nested_repository(&self, relative: String) {
        let mut summary = self
            .skipped_nested_repositories
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // A pruned directory is visited once. Keep only the smallest names,
        // independent of filesystem iteration order, rather than collecting
        // every path and truncating only after the walk has finished.
        summary.total += 1;
        summary.names.insert(relative);
        if summary.names.len() > MAX_REPOSITORY_NAMES {
            let _ = summary.names.pop_last();
        }
    }

    pub(crate) fn skipped_nested_repositories_note(&self) -> Option<String> {
        let summary = self
            .skipped_nested_repositories
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if summary.total == 0 {
            return None;
        }
        let names = summary
            .names
            .iter()
            .map(|path| {
                // Bound by characters before escaping, so truncation cannot
                // split UTF-8 or an escape. Quoting prevents a filename from
                // injecting new lines or delimiters into an agent's report.
                let mut chars = path.chars();
                let prefix = chars
                    .by_ref()
                    .take(MAX_REPOSITORY_PATH_CHARS)
                    .collect::<String>();
                let suffix = if chars.next().is_some() { "…" } else { "" };
                format!("{prefix:?}{suffix}")
            })
            .collect::<Vec<_>>();
        let remaining = summary.total - summary.names.len();
        let suffix = if remaining == 0 {
            String::new()
        } else {
            format!(" (and {remaining} more)")
        };
        Some(format!(
            "Skipped nested repositories outside parent index: {}{suffix}",
            names.join(", ")
        ))
    }
}

#[tracing::instrument(name = "code_index.capture.source_walk", level = "trace", skip_all)]
pub fn source_walk(
    project_root: &Path,
    path_glob: Option<&str>,
    path_policy: &IndexPathPolicyV1,
) -> Result<SourceWalk, SourceWalkError> {
    source_walk_inner(project_root, path_glob, path_policy, None)
}

/// Expose excluded nested repository roots without changing source admission.
/// The unmounted-file audit uses this to avoid falsely implying completeness.
/// It uses the same traversal rather than an independent repository walker.
pub(crate) fn source_walk_with_diagnostics(
    project_root: &Path,
    path_glob: Option<&str>,
    path_policy: &IndexPathPolicyV1,
) -> Result<(SourceWalk, SourceWalkDiagnostics), SourceWalkError> {
    let diagnostics = SourceWalkDiagnostics::default();
    let walk = source_walk_inner(
        project_root,
        path_glob,
        path_policy,
        Some(diagnostics.clone()),
    )?;
    Ok((walk, diagnostics))
}

fn source_walk_inner(
    project_root: &Path,
    path_glob: Option<&str>,
    path_policy: &IndexPathPolicyV1,
    diagnostics: Option<SourceWalkDiagnostics>,
) -> Result<SourceWalk, SourceWalkError> {
    let overrides = build_overrides(project_root, path_glob)?;
    let has_positive_override = overrides
        .as_ref()
        .is_some_and(|overrides| overrides.num_whitelists() > 0);
    let generated_dir_overrides = overrides.clone();
    let path_glob_scope = path_glob.and_then(PathGlobScope::from_path_glob);
    let filter_root = project_root.to_path_buf();
    let path_policy = path_policy.clone();
    let scoped_policy = {
        let root = filter_root.clone();
        let scope = path_glob_scope.clone();
        move |path: &Path, is_dir: bool| {
            has_positive_override
                && (generated_dir_overrides
                    .as_ref()
                    .is_some_and(|overrides| overrides.matched(path, is_dir).is_whitelist())
                    || (is_dir
                        && scope
                            .as_ref()
                            .is_some_and(|scope| scope.allows(&root, path))))
        }
    };
    let literal_scope = path_glob_scope.as_ref().and_then(|scope| {
        (!scope.literal_prefix.as_os_str().is_empty()).then_some(scope.literal_prefix.as_path())
    });
    let boundaries = submodules::CheckoutBoundaries::open_scoped(
        project_root,
        path_policy.includes_submodules(),
        literal_scope,
    )?;
    let mut parent_gitlinks = boundaries.gitlinks;
    let mut authenticated = BTreeSet::new();
    let mut child_files = BTreeSet::new();
    let mut roots = Vec::new();
    for (path, child) in boundaries.children {
        let absolute = project_root.join(&path);
        let policy = if scoped_policy(&absolute, true) {
            path_policy.without_generated_dir_defaults()
        } else {
            &path_policy
        };
        if policy.excludes_directory(&path)
            || path_glob_scope
                .as_ref()
                .is_some_and(|scope| !scope.may_contain_match(project_root, &absolute))
        {
            continue;
        }
        let inventory = child.sources()?;
        child_files.extend(
            inventory
                .files
                .into_iter()
                .map(|file| format!("{path}/{file}")),
        );
        // Only direct registrations belong to this parent.
        for nested in inventory.gitlinks {
            parent_gitlinks.insert(format!("{path}/{nested}"));
        }
        authenticated.insert(path);
        roots.push((absolute, child));
    }

    let child_dirs = child_files
        .iter()
        .flat_map(|path| Path::new(path).ancestors().skip(1))
        .map(Path::to_owned)
        .collect::<BTreeSet<_>>();

    let accept = Arc::new(move |entry: &ignore::DirEntry, in_child: bool| {
        let segment = entry.file_name().to_string_lossy();
        if segment == ".git" || segment == ".tracedecay" {
            return false;
        }
        let Ok(relative) = entry.path().strip_prefix(&filter_root) else {
            return false;
        };
        let relative = forward_slash_path(relative);
        let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
        if is_dir
            && path_glob_scope
                .as_ref()
                .is_some_and(|scope| !scope.may_contain_match(&filter_root, entry.path()))
        {
            return false;
        }
        // An entry the glob names, or a directory it must pass through,
        // is judged without the generated-directory defaults: the scope
        // is the operator asking for that noise. Every other exclusion
        // still applies; only `index.include.v1` lifts those.
        let path_policy = if scoped_policy(entry.path(), is_dir) {
            path_policy.without_generated_dir_defaults()
        } else {
            &path_policy
        };
        if !is_dir {
            return !path_policy.excludes(&relative)
                && (!in_child || child_files.contains(&relative));
        }
        // Excluded directories are outside this audit, not missing source
        // caused by the repository boundary. Do not probe or name them.
        if path_policy.excludes_directory(&relative) {
            return false;
        }
        // Only an independently bound direct gitlink may cross a checkout boundary.
        if authenticated.contains(&relative) {
            return in_child;
        }
        if parent_gitlinks.contains(&relative) {
            if let Some(diagnostics) = &diagnostics {
                diagnostics.record_skipped_nested_repository(relative);
            }
            return false;
        }
        match nested_repository_marker(entry.path()) {
            Ok(true) => {
                if let Some(diagnostics) = &diagnostics {
                    diagnostics.record_skipped_nested_repository(relative);
                }
                false
            }
            Ok(false) => !in_child || child_dirs.contains(Path::new(&relative)),
            // The walk cannot prove that an unreadable Git marker is
            // absent. Never admit files across that boundary.
            Err(_) => false,
        }
    });
    let mut builders = vec![(WalkBuilder::new(project_root), None)];
    builders.extend(
        roots
            .into_iter()
            .map(|(root, child)| (WalkBuilder::new(root), Some(child))),
    );
    let walks = builders
        .into_iter()
        .map(|(mut builder, child)| {
            let in_child = child.is_some();
            // The child Git roster already applied that checkout's ignores, including
            // tracked exceptions. Running parent ignore rules over it would hide those files.
            if in_child {
                builder.standard_filters(false);
            } else {
                builder.add_custom_ignore_filename(".gitignore");
            }
            let accept = Arc::clone(&accept);
            builder
                .follow_links(false)
                .hidden(false)
                .filter_entry(move |entry| accept(entry, in_child));
            if let Some(overrides) = &overrides {
                builder.overrides(overrides.clone());
            }
            submodules::CheckoutWalk::new(builder.build(), child)
        })
        .collect::<Vec<_>>();
    Ok(walks.into_iter().flatten())
}

/// Project-relative path rendered with forward slashes.
///
/// Grep and ast-grep share this so each walk entry normalizes once into an
/// [`Arc<str>`] that hits can clone cheaply instead of re-allocating the path
/// string per match (including on zero-hit files when callers skip this until
/// the first hit).
#[must_use]
pub fn forward_slash_relative(relative: &Path) -> Arc<str> {
    tracedecay_domain::forward_slash_path(relative).into()
}

fn build_overrides(
    project_root: &Path,
    path_glob: Option<&str>,
) -> Result<Option<Override>, SourceWalkError> {
    match path_glob {
        Some(raw) if !raw.trim().is_empty() => {
            let mut builder = OverrideBuilder::new(project_root);
            builder.add(raw).map_err(|error| SourceWalkError {
                glob: raw.to_owned(),
                message: error.to_string(),
            })?;
            builder.build().map(Some).map_err(|error| SourceWalkError {
                glob: raw.to_owned(),
                message: error.to_string(),
            })
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod nested_repository_tests;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;
    use tracedecay_domain::IndexPathPolicyV1;

    use super::{
        has_nested_repository_ancestor, is_source_symlink, source_walk,
        source_walk_with_diagnostics,
    };

    #[test]
    fn nested_repository_boundary_excludes_parent_markers_and_path_escape() {
        let root = TempDir::new().unwrap();
        fs::create_dir_all(root.path().join("parent/nested/.git")).unwrap();
        fs::write(
            root.path().join("parent/nested/.git/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        assert!(
            !has_nested_repository_ancestor(root.path(), std::path::Path::new("parent/source.rs"))
                .unwrap()
        );
        assert!(
            has_nested_repository_ancestor(
                root.path(),
                std::path::Path::new("parent/nested/source.rs")
            )
            .unwrap()
        );
        assert!(
            has_nested_repository_ancestor(root.path(), std::path::Path::new("../escape.rs"))
                .is_err()
        );
        assert!(
            has_nested_repository_ancestor(root.path(), std::path::Path::new("/absolute.rs"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn nested_repository_boundary_excludes_symlinked_ancestors() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        symlink(external.path(), root.path().join("foreign")).unwrap();
        assert!(
            has_nested_repository_ancestor(root.path(), std::path::Path::new("foreign/source.rs"))
                .unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn leaf_link_detection_preserves_missing_paths() {
        use std::os::unix::fs::symlink;

        let root = TempDir::new().unwrap();
        fs::write(root.path().join("real.rs"), "pub fn real() {}\n").unwrap();
        symlink("real.rs", root.path().join("alias.rs")).unwrap();
        assert!(is_source_symlink(root.path(), std::path::Path::new("alias.rs")).unwrap());
        assert!(!is_source_symlink(root.path(), std::path::Path::new("real.rs")).unwrap());
        assert!(!is_source_symlink(root.path(), std::path::Path::new("missing.rs")).unwrap());
        assert!(is_source_symlink(root.path(), std::path::Path::new("../escape.rs")).is_err());
    }

    fn walked_files(root: &std::path::Path, path_policy: &IndexPathPolicyV1) -> Vec<PathBuf> {
        let mut files = source_walk(root, None, path_policy)
            .expect("source walk")
            .map(|entry| entry.expect("source entry"))
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("project-relative path")
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    #[test]
    fn a_literal_file_scope_prunes_unrelated_subtrees() {
        let root = TempDir::new().unwrap();
        for path in [
            "crates/core/src/compiler.rs",
            "crates/other/deep/noise.rs",
            "packages/deep/noise.rs",
        ] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "pub struct Compiler;\n").unwrap();
        }
        let entries = source_walk(
            root.path(),
            Some("crates/core/src/compiler.rs"),
            &IndexPathPolicyV1::new(vec![], vec![]).unwrap(),
        )
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .strip_prefix(root.path())
                .unwrap()
                .to_path_buf()
        })
        .collect::<Vec<_>>();
        assert!(entries.contains(&PathBuf::from("crates/core/src/compiler.rs")));
        assert!(
            entries
                .iter()
                .all(|path| !path.starts_with("packages") && !path.starts_with("crates/other"))
        );
    }

    #[test]
    fn path_pruning_preserves_directory_inheritance_and_basename_globs() {
        let root = TempDir::new().unwrap();
        for path in ["src/deep/keep.rs", "other/deep/also.rs"] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "source\n").unwrap();
        }
        let files = |glob| {
            let mut paths = source_walk(
                root.path(),
                Some(glob),
                &IndexPathPolicyV1::new(vec![], vec![]).unwrap(),
            )
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(root.path())
                    .unwrap()
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
            paths.sort();
            paths
        };
        assert_eq!(files("/src/**"), vec![PathBuf::from("src/deep/keep.rs")]);
        assert_eq!(
            files("*.rs"),
            vec![
                PathBuf::from("other/deep/also.rs"),
                PathBuf::from("src/deep/keep.rs")
            ]
        );
        assert_eq!(files("{src,other}/**/*.rs"), files("*.rs"));
        assert_eq!(files("!src/**"), vec![PathBuf::from("other/deep/also.rs")]);
    }

    #[test]
    fn escaped_literal_prefixes_keep_the_override_matcher_authoritative() {
        let root = TempDir::new().unwrap();
        fs::create_dir_all(root.path().join("src/a[1]")).unwrap();
        fs::write(root.path().join("src/a[1]/keep.rs"), "source\n").unwrap();
        let files = source_walk(
            root.path(),
            Some(r"src/a\[1\]/keep.rs"),
            &IndexPathPolicyV1::new(vec![], vec![]).unwrap(),
        )
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root.path())
                .unwrap()
                .to_path_buf()
        })
        .collect::<Vec<_>>();
        assert_eq!(files, vec![PathBuf::from("src/a[1]/keep.rs")]);
    }

    #[test]
    fn nested_repository_is_not_part_of_the_parent_source_tree() {
        let root = TempDir::new().expect("project root");
        fixture_git(root.path(), &["init", "-q"]);
        fs::create_dir_all(root.path().join("src")).expect("parent source directory");
        fs::write(root.path().join("src/lib.rs"), "pub fn parent() {}\n").expect("parent source");
        fs::create_dir_all(root.path().join("plain")).expect("plain directory");
        fs::write(root.path().join("plain/keep.rs"), "pub fn keep() {}\n").expect("plain source");

        let nested = root.path().join("nested-worktree");
        fs::create_dir_all(nested.join("src")).expect("nested source directory");
        fs::write(nested.join(".git"), "gitdir: /tmp/foreign-worktree\n")
            .expect("linked-worktree marker");
        fs::write(nested.join("src/foreign.rs"), "pub fn foreign() {}\n").expect("nested source");
        let nested_clone = root.path().join("nested-clone");
        fs::create_dir_all(nested_clone.join(".git")).expect("nested git directory");
        fs::write(nested_clone.join("foreign.rs"), "pub fn cloned() {}\n")
            .expect("nested clone source");

        let files = walked_files(
            root.path(),
            &IndexPathPolicyV1::new(Vec::new(), Vec::new()).expect("empty policy"),
        );

        assert!(files.contains(&PathBuf::from("src/lib.rs")));
        assert!(files.contains(&PathBuf::from("plain/keep.rs")));
        assert!(
            !files.contains(&PathBuf::from("nested-worktree/src/foreign.rs")),
            "a linked worktree nested under the project must not be indexed as parent source"
        );
        assert!(
            !files.contains(&PathBuf::from("nested-clone/foreign.rs")),
            "a nested clone must not be indexed as parent source"
        );
    }

    fn fixture_git(root: &std::path::Path, args: &[&str]) -> std::process::Output {
        let program = tracedecay_runtime_core::git::try_git_program().expect("Git program");
        let mut command = std::process::Command::new(program);
        command.current_dir(root);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join(".fixture-gitconfig"))
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("run fixture Git");
        assert!(
            output.status.success(),
            "Git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[test]
    fn a_literal_scope_does_not_read_unrelated_committed_trees() {
        let root = TempDir::new().expect("parent checkout");
        fixture_git(root.path(), &["init", "-q"]);
        for (name, contents) in [
            ("src/keep.rs", "pub fn kept() {}\n"),
            ("unrelated/deep/noise.rs", "pub fn noise() {}\n"),
        ] {
            let path = root.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        fixture_git(root.path(), &["add", "."]);
        fixture_git(root.path(), &["commit", "-qm", "two independent trees"]);
        let output = fixture_git(root.path(), &["rev-parse", "HEAD:unrelated"]);
        let oid = String::from_utf8(output.stdout).unwrap();
        let oid = oid.trim();
        assert!(oid.len() > 2 && oid.bytes().all(|byte| byte.is_ascii_hexdigit()));
        let object = root
            .path()
            .join(".git/objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        assert!(object.is_file(), "fixture must have a loose unrelated tree");
        fs::remove_file(&object).unwrap();

        let policy = IndexPathPolicyV1::new(vec![], vec![]).unwrap();
        let files = source_walk(root.path(), Some("src/keep.rs"), &policy)
            .expect("literal scope must not read unrelated HEAD tree")
            .map(|entry| entry.expect("scoped source entry"))
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(|entry| {
                entry
                    .into_path()
                    .strip_prefix(root.path())
                    .unwrap()
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        assert_eq!(files, vec![PathBuf::from("src/keep.rs")]);
        assert!(
            source_walk(root.path(), None, &policy).is_err(),
            "an unscoped walk must still refuse unreadable HEAD tree authority"
        );
    }

    #[test]
    fn includes_cannot_admit_registered_submodule_files_as_parent_source() {
        let fixture = TempDir::new().expect("repository fixture");
        let root = fixture.path().join("parent");
        let child = fixture.path().join("child");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(child.join("src")).unwrap();
        fixture_git(&root, &["init", "-q"]);
        fixture_git(&child, &["init", "-q"]);
        fs::write(root.join("src/parent.rs"), "pub fn parent() {}\n").unwrap();
        fs::write(child.join("src/feature.rs"), "pub fn feature() {}\n").unwrap();
        fixture_git(&child, &["add", "."]);
        fixture_git(&child, &["commit", "-qm", "child source"]);
        fixture_git(
            &root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                "../child",
                "app",
            ],
        );
        let index = fixture_git(&root, &["ls-files", "--stage", "-z", "--", "app"]);
        assert!(index.stdout.starts_with(b"160000 "));
        assert!(index.stdout.ends_with(b" 0\tapp\0"));
        assert!(root.join("app/src/feature.rs").is_file());

        let (walk, diagnostics) = source_walk_with_diagnostics(
            &root,
            None,
            &IndexPathPolicyV1::new(vec![], vec![]).unwrap(),
        )
        .expect("source walk with nested repository diagnostics");
        for entry in walk {
            entry.expect("source entry");
        }
        assert_eq!(
            diagnostics.skipped_nested_repositories_note().as_deref(),
            Some("Skipped nested repositories outside parent index: \"app\"")
        );

        for rule in ["app/**", "app/src/feature.rs", "app/src/fe*.rs"] {
            let policy = IndexPathPolicyV1::new(vec![], vec![rule.into()]).unwrap();
            let files = walked_files(&root, &policy);
            assert!(files.contains(&PathBuf::from("src/parent.rs")), "{files:?}");
            assert!(
                files.iter().all(|path| !path.starts_with("app")),
                "{rule}: a parent gitlink is not child source authority: {files:?}"
            );
            let scoped = source_walk(&root, Some(rule), &policy)
                .expect("explicit submodule scope")
                .map(|entry| entry.expect("source entry"))
                .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
                .collect::<Vec<_>>();
            assert!(
                scoped.is_empty(),
                "{rule}: an explicit scope cannot cross repositories"
            );
        }

        let enabled = IndexPathPolicyV1::new(vec![], vec![])
            .unwrap()
            .with_submodules(true);
        fixture_git(
            &child,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                root.join("other-worktree").to_str().unwrap(),
                "HEAD",
            ],
        );
        fixture_git(
            &root,
            &["clone", "-q", child.to_str().unwrap(), "other-clone"],
        );
        let files = walked_files(&root, &enabled);
        assert!(files.contains(&PathBuf::from("app/src/feature.rs")));
        assert!(files.iter().all(|path|!path.starts_with("other-worktree") && !path.starts_with("other-clone")));
        fixture_git(
            &child,
            &[
                "worktree",
                "remove",
                root.join("other-worktree").to_str().unwrap(),
            ],
        );
        fs::remove_dir_all(root.join("other-clone")).unwrap();
        fs::remove_file(root.join(".gitmodules")).unwrap();
        assert!(!walked_files(&root, &enabled).contains(&PathBuf::from("app/src/feature.rs")));
        fixture_git(&root, &["checkout", "--", ".gitmodules"]);
        // A missing child .git marker must not turn the parent's stage-0
        // gitlink into ordinary live source under an explicit include.
        fs::remove_file(root.join("app/.git")).expect("remove child gitdir marker");
        assert!(root.join("app/src/feature.rs").is_file());
        let policy = IndexPathPolicyV1::new(vec![], vec!["app/**".into()]).unwrap();
        assert!(!walked_files(&root, &enabled).contains(&PathBuf::from("app/src/feature.rs")));
        let (walk, diagnostics) =
            source_walk_with_diagnostics(&root, None, &policy).expect("source walk");
        let files = walk
            .map(|entry| entry.expect("source entry"))
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(|entry| entry.into_path())
            .collect::<Vec<_>>();
        assert!(
            files.iter().all(|path| !path.starts_with(root.join("app"))),
            "a registered gitlink stays excluded without a child marker: {files:?}"
        );
        assert_eq!(
            diagnostics.skipped_nested_repositories_note().as_deref(),
            Some("Skipped nested repositories outside parent index: \"app\"")
        );
        let explicitly_scoped = source_walk(&root, Some("app/**"), &policy)
            .expect("explicit gitlink scope")
            .map(|entry| entry.expect("source entry"))
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .count();
        assert_eq!(explicitly_scoped, 0);

        // Staging removal of the gitlink also removes it from the parent
        // index, but HEAD still owns that repository boundary. Neither an
        // explicit include nor a path glob may adopt its live child bytes.
        fixture_git(&root, &["add", ".gitmodules", "src/parent.rs"]);
        fixture_git(&root, &["commit", "-qm", "parent with submodule"]);
        fixture_git(&root, &["rm", "--cached", "-f", "app"]);
        assert!(
            fixture_git(&root, &["ls-files", "--stage", "--", "app"])
                .stdout
                .is_empty()
        );
        assert!(
            fixture_git(&root, &["ls-tree", "HEAD", "--", "app"])
                .stdout
                .starts_with(b"160000 commit ")
        );
        assert!(root.join("app/src/feature.rs").is_file());

        let files = walked_files(&root, &policy);
        assert!(
            files.iter().all(|path| !path.starts_with("app")),
            "HEAD gitlink is still an excluded source boundary: {files:?}"
        );
        let scoped = source_walk(&root, Some("app/**"), &policy)
            .expect("scope cannot cross HEAD gitlink")
            .map(|entry| entry.expect("source entry"))
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .count();
        assert_eq!(scoped, 0);
    }

    #[test]
    fn submodule_metadata_cannot_break_an_unrelated_source_include() {
        let root = TempDir::new().expect("project root");
        fixture_git(root.path(), &["init", "-q"]);
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::create_dir_all(root.path().join("app/.git")).unwrap();
        fs::write(root.path().join("src/keep.rs"), "pub fn keep() {}\n").unwrap();
        fs::write(root.path().join("app/foreign.rs"), "pub fn foreign() {}\n").unwrap();
        fs::write(
            root.path().join(".gitmodules"),
            "[invalid submodule metadata\n",
        )
        .unwrap();
        let policy = IndexPathPolicyV1::new(vec![], vec!["src/**".into()]).unwrap();
        let files = walked_files(root.path(), &policy);
        assert!(files.contains(&PathBuf::from("src/keep.rs")));
        assert!(
            files.iter().all(|path| !path.starts_with("app")),
            "{files:?}"
        );
    }

    #[test]
    fn explicit_glob_cannot_read_excluded_paths_without_policy_include() {
        let root = TempDir::new().expect("project root");
        fs::create_dir(root.path().join("secrets")).expect("secret directory");
        fs::write(root.path().join("secrets/token.rs"), "secret_token\n").expect("secret source");
        let walk = |policy: &IndexPathPolicyV1| {
            source_walk(root.path(), Some("secrets/**"), policy)
                .expect("source walk")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
                .map(|entry| {
                    entry
                        .path()
                        .strip_prefix(root.path())
                        .unwrap()
                        .to_path_buf()
                })
                .collect::<Vec<_>>()
        };
        let excluded = IndexPathPolicyV1::new(vec!["secrets/**".into()], vec![]).unwrap();
        assert!(
            walk(&excluded).is_empty(),
            "path_glob cannot bypass a configured exclusion"
        );
        let included =
            IndexPathPolicyV1::new(vec!["secrets/**".into()], vec!["secrets/token.rs".into()])
                .unwrap();
        assert_eq!(walk(&included), vec![PathBuf::from("secrets/token.rs")]);
    }

    #[test]
    fn explicit_glob_reaches_directories_only_the_generated_defaults_exclude() {
        let root = TempDir::new().expect("project root");
        for path in ["dist/bundle.js", "dist/secrets/token.js", "src/app.js"] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().expect("parent")).expect("fixture directory");
            fs::write(path, "generated\n").expect("fixture file");
        }
        let walk = |glob: &str, policy: &IndexPathPolicyV1| {
            let mut files = source_walk(root.path(), Some(glob), policy)
                .expect("source walk")
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
                .map(|entry| {
                    entry
                        .path()
                        .strip_prefix(root.path())
                        .unwrap()
                        .to_path_buf()
                })
                .collect::<Vec<_>>();
            files.sort();
            files
        };
        let defaults =
            IndexPathPolicyV1::new(vec!["dist/**".into(), "**/dist/**".into()], vec![]).unwrap();
        assert!(
            walked_files(root.path(), &defaults)
                .iter()
                .all(|f| f.starts_with("src"))
        );
        assert_eq!(
            walk("dist/**/*.js", &defaults),
            vec![
                PathBuf::from("dist/bundle.js"),
                PathBuf::from("dist/secrets/token.js")
            ],
            "a scope naming a generated directory reaches it"
        );
        assert_eq!(
            walk("*.js", &defaults),
            vec![
                PathBuf::from("dist/bundle.js"),
                PathBuf::from("dist/secrets/token.js"),
                PathBuf::from("src/app.js")
            ],
            "a slashless glob reaches generated descendants"
        );

        let with_operator_rule = IndexPathPolicyV1::new(
            vec![
                "dist/**".into(),
                "**/dist/**".into(),
                "**/secrets/**".into(),
            ],
            vec![],
        )
        .unwrap();
        assert_eq!(
            walk("dist/**/*.js", &with_operator_rule),
            vec![PathBuf::from("dist/bundle.js")],
            "the same scope cannot lift an operator's exclusion beneath the generated directory"
        );
    }

    #[test]
    fn the_walk_honors_the_index_path_policy() {
        let root = TempDir::new().expect("project root");
        for (path, contents) in [
            ("src/lib.rs", "pub fn kept() {}\n"),
            ("generated-fixtures/gen.rs", "pub fn generated_only() {}\n"),
            ("vendor/drop/lib.rs", "pub fn dropped() {}\n"),
            ("vendor/kept/lib.rs", "pub fn vendored_kept() {}\n"),
            ("assets/app.min.js", "export const minified = 1;\n"),
        ] {
            let path = root.path().join(path);
            fs::create_dir_all(path.parent().expect("parent")).expect("fixture directory");
            fs::write(path, contents).expect("fixture file");
        }
        let policy = IndexPathPolicyV1::new(
            vec![
                "generated-fixtures/**".to_owned(),
                "vendor/**".to_owned(),
                "**/*.min.*".to_owned(),
            ],
            vec!["vendor/kept/**".to_owned()],
        )
        .expect("policy");
        assert_eq!(
            walked_files(root.path(), &policy),
            vec![
                PathBuf::from("src/lib.rs"),
                PathBuf::from("vendor/kept/lib.rs")
            ]
        );
        assert_eq!(
            walked_files(
                root.path(),
                &IndexPathPolicyV1::new(Vec::new(), Vec::new()).expect("empty policy")
            ),
            vec![
                PathBuf::from("assets/app.min.js"),
                PathBuf::from("generated-fixtures/gen.rs"),
                PathBuf::from("src/lib.rs"),
                PathBuf::from("vendor/drop/lib.rs"),
                PathBuf::from("vendor/kept/lib.rs"),
            ]
        );
    }
}
