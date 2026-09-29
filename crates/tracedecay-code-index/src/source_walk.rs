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

use std::path::Path;
use std::sync::Arc;

use ignore::overrides::{Override, OverrideBuilder};
use ignore::{Walk, WalkBuilder};
use tracedecay_domain::{IndexPathPolicyV1, forward_slash_path};

#[derive(Debug)]
pub struct SourceWalkError {
    pub glob: String,
    pub message: String,
}

#[hotpath::measure(label = "code_index.capture.source_walk")]
pub fn source_walk(
    project_root: &Path,
    path_glob: Option<&str>,
    path_policy: &IndexPathPolicyV1,
) -> Result<Walk, SourceWalkError> {
    let overrides = build_overrides(project_root, path_glob)?;
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
