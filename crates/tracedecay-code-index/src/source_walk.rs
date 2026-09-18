//! The one working-tree walk every source-reading surface shares.
//!
//! Grep, ast-grep search, and the module-mount audit must all agree on what
//! "a file in this project" means: the same `.gitignore` rules, the same
//! generated-directory skips, the same refusal to follow links. A second walker
//! built next to this one would drift, and a scan that disagrees with the one
//! the indexer used reports findings the rest of the product cannot see. The
//! walk is therefore public rather than crate-private, the audit in the root
//! crate reuses this policy instead of restating it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::overrides::{Override, OverrideBuilder};
use ignore::{Walk, WalkBuilder};

#[derive(Debug)]
pub struct SourceWalkError {
    pub glob: String,
    pub message: String,
}

struct GeneratedDirScope {
    literal_prefix: PathBuf,
    may_match_descendants: bool,
}

impl GeneratedDirScope {
    fn from_path_glob(path_glob: &str) -> Option<Self> {
        let path_glob = path_glob.trim();
        if path_glob.is_empty() || path_glob.starts_with('!') {
            return None;
        }
        let segments = path_glob
            .trim_start_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
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
}

#[hotpath::measure(label = "code_index.capture.source_walk")]
pub fn source_walk(project_root: &Path, path_glob: Option<&str>) -> Result<Walk, SourceWalkError> {
    let overrides = build_overrides(project_root, path_glob)?;
    let has_positive_override = overrides
        .as_ref()
        .is_some_and(|overrides| overrides.num_whitelists() > 0);
    let generated_dir_overrides = overrides.clone();
    let generated_dir_scope = path_glob.and_then(GeneratedDirScope::from_path_glob);
    let filter_root = project_root.to_path_buf();

    let mut builder = WalkBuilder::new(project_root);
    builder
        .follow_links(false)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".gitignore")
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let segment = entry.file_name().to_string_lossy();
            if segment == ".git" || segment == ".tracedecay" {
                return false;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return true;
            }
            // A directory with its own Git authority is another project, not
            // source owned by this one. This covers linked worktrees (`.git`
            // file), nested clones/submodules (`.git` directory), and keeps a
            // primary checkout containing agent worktrees from indexing many
            // copies of itself. The project root is deliberately exempt at
            // depth zero above.
            if std::fs::symlink_metadata(entry.path().join(".git")).is_ok() {
                return false;
            }
            let explicitly_requested = has_positive_override
                && (generated_dir_overrides
                    .as_ref()
                    .is_some_and(|overrides| overrides.matched(entry.path(), true).is_whitelist())
                    || generated_dir_scope
                        .as_ref()
                        .is_some_and(|scope| scope.allows(&filter_root, entry.path())));
            explicitly_requested || !tracedecay_domain::is_generated_dir_segment(&segment)
        });
    if let Some(overrides) = overrides {
        builder.overrides(overrides);
    }
    Ok(builder.build())
}

/// Project-relative path rendered with forward slashes.
///
/// Grep and ast-grep share this so each walk entry normalizes once into an
/// [`Arc<str>`] that hits can clone cheaply instead of re-allocating the path
/// string per match (including on zero-hit files when callers skip this until
/// the first hit).
#[must_use]
pub fn forward_slash_relative(relative: &Path) -> Arc<str> {
    relative.to_string_lossy().replace('\\', "/").into()
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
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::source_walk;

    #[test]
    fn nested_repository_is_not_part_of_the_parent_source_tree() {
        let root = TempDir::new().expect("project root");
        fs::create_dir(root.path().join(".git")).expect("parent git directory");
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

        let files = source_walk(root.path(), None)
            .expect("source walk")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(root.path())
                    .expect("project-relative path")
                    .to_path_buf()
            })
            .collect::<Vec<_>>();

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
}
