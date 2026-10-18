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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::overrides::{Override, OverrideBuilder};
use ignore::{Walk, WalkBuilder};
use tracedecay_domain::{IndexPathPolicyV1, forward_slash_path};

#[derive(Debug)]
pub struct SourceWalkError {
    pub glob: String,
    pub message: String,
}

/// Conservative directory reachability for a positive override. The override
/// matcher remains authoritative for files, including directory inheritance.
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

#[tracing::instrument(name = "code_index.capture.source_walk", level = "trace", skip_all)]
pub fn source_walk(
    project_root: &Path,
    path_glob: Option<&str>,
    path_policy: &IndexPathPolicyV1,
) -> Result<Walk, SourceWalkError> {
    let overrides = build_overrides(project_root, path_glob)?;
    let has_positive_override = overrides
        .as_ref()
        .is_some_and(|overrides| overrides.num_whitelists() > 0);
    let generated_dir_overrides = overrides.clone();
    let path_glob_scope = path_glob.and_then(PathGlobScope::from_path_glob);
    let filter_root = project_root.to_path_buf();
    let path_policy = path_policy.clone();

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
            let explicitly_requested = has_positive_override
                && (generated_dir_overrides.as_ref().is_some_and(|overrides| {
                    overrides.matched(entry.path(), is_dir).is_whitelist()
                }) || (is_dir
                    && path_glob_scope
                        .as_ref()
                        .is_some_and(|scope| scope.allows(&filter_root, entry.path()))));
            let path_policy = if explicitly_requested {
                path_policy.without_generated_dir_defaults()
            } else {
                &path_policy
            };
            if !is_dir {
                return !path_policy.excludes(&relative);
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
            !path_policy.excludes_directory(&relative)
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
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;
    use tracedecay_domain::IndexPathPolicyV1;

    use super::source_walk;

    fn walked_files(root: &std::path::Path, path_policy: &IndexPathPolicyV1) -> Vec<PathBuf> {
        let mut files = source_walk(root, None, path_policy)
            .expect("source walk")
            .filter_map(Result::ok)
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
