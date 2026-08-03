// This file is `include!`d into build.rs as well as compiled as a module, so it
// carries no inner doc comments and no `use` statements: both would collide
// with the build script's own file-level items.

/// Commit identity of a source tree at build time.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildIdentity {
    /// Short `HEAD` commit, or `None` when the tree is not a git worktree.
    pub sha: Option<String>,
    /// Whether tracked modifications or untracked files were present.
    pub dirty: bool,
}

/// Resolves the git identity of the crate rooted at `root`.
///
/// A registry install unpacks the crate into a directory that can itself sit
/// inside an unrelated repository, where `git rev-parse HEAD` would happily
/// report that repository's commit. A commit is therefore only trusted when git
/// reports `root` as the worktree top level; every other case — no git binary,
/// no checkout, a nested unpack — yields an empty identity and leaves the
/// version at bare `CARGO_PKG_VERSION`.
pub fn resolve(root: &std::path::Path) -> BuildIdentity {
    if !is_own_worktree(root) {
        return BuildIdentity::default();
    }
    let Some(sha) = git_stdout(root, &["rev-parse", "--short=12", "HEAD"]) else {
        return BuildIdentity::default();
    };
    BuildIdentity {
        sha: Some(sha),
        dirty: git_stdout(root, &["status", "--porcelain"]).is_some(),
    }
}

/// Paths whose change makes [`resolve`] answer differently: `HEAD` and its
/// reflog move with commits, checkouts, and resets; the index tracks staged and
/// stat-refreshed worktree state; and every existing tracked or untracked input
/// observes ordinary worktree edits. Without these the baked commit silently
/// describes whichever tree happened to trigger the previous build-script run.
///
/// Absent paths are dropped, because Cargo treats a missing `rerun-if-changed`
/// path as perpetually changed and would rerun the script on every build.
///
/// A newly created top-level untracked file (or one outside the source roots
/// below) cannot trigger an otherwise fresh build. Watching the worktree root
/// would catch it, but Cargo recursively scans directory watches and would
/// include target, generated, vendor, and dependency output. Those roots must
/// not become build-script inputs.
pub fn watch_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    if !is_own_worktree(root) {
        return Vec::new();
    }

    let mut paths = std::collections::BTreeSet::new();
    for name in ["HEAD", "logs/HEAD", "index"] {
        if let Some(raw) = git_stdout(root, &["rev-parse", "--git-path", name]) {
            let relative = std::path::Path::new(&raw);
            let path = if relative.is_absolute() {
                relative.to_path_buf()
            } else {
                root.join(relative)
            };
            if path.exists() {
                paths.insert(path);
            }
        }
    }
    paths.extend(worktree_input_paths(root));
    paths.extend(source_watch_paths(root));
    paths.into_iter().collect()
}

/// Safe source trees that may gain an untracked file without pulling generated
/// or dependency output into Cargo's recursive directory watcher. `crates`,
/// `dashboard`, and `plugin` are deliberately narrowed below rather than
/// watched as whole trees.
const SOURCE_WATCH_ROOTS: &[&str] = &[
    "src",
    "tests",
    "benches",
    "examples",
    "build-support",
    "scripts",
    "eval",
    "evals",
    "dashboard/src",
    "dashboard/codegen/schemas",
    "plugin/agents",
    "plugin/skills",
];

/// Existing Git inputs are watched individually so vendor and generated files
/// never turn into recursive directory watches. Git's standard excludes keep
/// ignored output such as `target`, `node_modules`, and `dashboard/app-dist`
/// out of this list.
fn worktree_input_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Some(output) = git_output(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    ) else {
        return Vec::new();
    };
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(git_path)
        .filter_map(|relative| {
            if relative.is_absolute()
                || relative.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                return None;
            }
            let path = root.join(relative);
            path.is_file().then_some(path)
        })
        .collect()
}

fn git_path(raw: &[u8]) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        Some(std::path::PathBuf::from(
            <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(raw.to_vec()),
        ))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(raw).ok().map(std::path::PathBuf::from)
    }
}

fn source_watch_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = SOURCE_WATCH_ROOTS
        .iter()
        .map(|relative| root.join(relative))
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    let crates = root.join("crates");
    let Ok(entries) = std::fs::read_dir(crates) else {
        return paths;
    };
    for entry in entries.flatten() {
        let crate_root = entry.path();
        if !crate_root.is_dir() {
            continue;
        }
        for source in ["src", "tests", "benches", "examples", "test-support"] {
            let path = crate_root.join(source);
            if path.is_dir() {
                paths.push(path);
            }
        }
    }
    paths
}

/// Whether git reports `root` itself — not some ancestor — as a worktree top
/// level. This is the guard that keeps an unrelated enclosing repository out of
/// both the baked commit and the rebuild triggers.
fn is_own_worktree(root: &std::path::Path) -> bool {
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return false;
    };
    let Some(toplevel) = git_stdout(root, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    std::fs::canonicalize(toplevel).ok() == Some(canonical_root)
}

/// Trimmed stdout of a successful `git` run in `root`, or `None` when git is
/// missing, the command failed, or it printed nothing.
///
/// `--no-optional-locks` keeps every probe strictly read-only. Plain
/// `git status` refreshes the index stat cache and rewrites `.git/index` — a
/// path [`watch_paths`] hands Cargo as a rebuild trigger — so probing the tree
/// would arm the very trigger it reads. The flag suppresses only that
/// incidental write; the reported status is unchanged.
fn git_stdout(root: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = git_output(root, args)?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn git_output(root: &std::path::Path, args: &[&str]) -> Option<std::process::Output> {
    let output = std::process::Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::{BuildIdentity, resolve, watch_paths};

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .expect("git should run");
        assert!(status.status.success(), "git {args:?} failed");
    }

    fn committed_repo(root: &std::path::Path) {
        git(root, &["init", "--quiet"]);
        std::fs::write(root.join("tracked.txt"), "one").expect("write tracked file");
        git(root, &["add", "tracked.txt"]);
        git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "init",
            ],
        );
    }

    struct CargoFixture {
        _directory: tempfile::TempDir,
        root: std::path::PathBuf,
        target: std::path::PathBuf,
    }

    impl CargoFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("fixture directory");
            let root = directory.path().join("repository");
            let target = directory.path().join("target");
            std::fs::create_dir_all(root.join("src")).expect("fixture source directory");
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"identity-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            )
            .expect("fixture manifest");
            std::fs::write(
                root.join("src/main.rs"),
                "fn main() { println!(\"{}\", env!(\"FIXTURE_IDENTITY\")); }\n",
            )
            .expect("fixture main source");
            std::fs::write(root.join("README.md"), "clean\n").expect("fixture readme");

            let shared_identity = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/version/build_identity.rs");
            std::fs::write(
                root.join("build.rs"),
                format!(
                    "use std::{{fs, path::{{Path, PathBuf}}}};\n\
                     include!({shared_identity:?});\n\
                     fn main() {{\n\
                         let manifest_dir = std::env::var(\"CARGO_MANIFEST_DIR\").expect(\"manifest directory\");\n\
                         let root = Path::new(&manifest_dir);\n\
                         let identity = resolve(root);\n\
                         for path in watch_paths(root) {{\n\
                             println!(\"cargo::rerun-if-changed={{}}\", path.display());\n\
                         }}\n\
                         let counter = PathBuf::from(std::env::var(\"OUT_DIR\").expect(\"output directory\")).join(\"build-script-runs\");\n\
                         let runs = fs::read_to_string(&counter).ok().and_then(|value| value.parse::<u32>().ok()).unwrap_or(0) + 1;\n\
                         fs::write(counter, runs.to_string()).expect(\"write run count\");\n\
                         println!(\"cargo::rustc-env=FIXTURE_IDENTITY={{}}\", if identity.dirty {{ \"dirty\" }} else {{ \"clean\" }});\n\
                     }}\n"
                ),
            )
            .expect("fixture build script");

            Self {
                _directory: directory,
                root,
                target,
            }
        }

        fn root(&self) -> &std::path::Path {
            &self.root
        }

        fn commit_sources(&self) {
            self.cargo(&["generate-lockfile", "--offline"]);
            git(&self.root, &["init", "--quiet"]);
            git(&self.root, &["add", "--all"]);
            git(
                &self.root,
                &[
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
            );
        }

        fn build(&self) {
            self.cargo(&["build", "--offline", "--quiet"]);
        }

        fn reported_identity(&self) -> String {
            let output = std::process::Command::new(
                self.target
                    .join("debug")
                    .join(format!("identity-fixture{}", std::env::consts::EXE_SUFFIX)),
            )
            .output()
            .expect("fixture binary should run");
            assert!(
                output.status.success(),
                "fixture binary should exit successfully"
            );
            String::from_utf8(output.stdout)
                .expect("fixture binary should print UTF-8")
                .trim()
                .to_string()
        }

        fn build_script_runs(&self) -> u32 {
            let build_directory = self.target.join("debug/build");
            let counters = std::fs::read_dir(build_directory)
                .expect("fixture build output directory")
                .flatten()
                .map(|entry| entry.path().join("out/build-script-runs"))
                .filter(|path| path.is_file())
                .collect::<Vec<_>>();
            assert_eq!(
                counters.len(),
                1,
                "fixture should have one build-script counter"
            );
            std::fs::read_to_string(&counters[0])
                .expect("fixture build-script counter")
                .trim()
                .parse()
                .expect("fixture build-script counter should be an integer")
        }

        fn cargo(&self, args: &[&str]) {
            let status = std::process::Command::new(
                std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
            )
            .args(args)
            .current_dir(&self.root)
            .env("CARGO_TARGET_DIR", &self.target)
            .status()
            .expect("fixture cargo should run");
            assert!(status.success(), "fixture cargo {args:?} failed");
        }
    }

    #[test]
    fn cargo_rebuilds_the_identity_after_a_tracked_non_source_edit() {
        let fixture = CargoFixture::new();
        fixture.commit_sources();
        fixture.build();
        assert_eq!(fixture.reported_identity(), "clean");
        assert_eq!(fixture.build_script_runs(), 1);

        std::fs::write(fixture.root().join("README.md"), "modified\n")
            .expect("modify tracked non-source file");
        fixture.build();

        assert_eq!(fixture.reported_identity(), "dirty");
        assert_eq!(fixture.build_script_runs(), 2);
    }

    #[test]
    fn cargo_rebuilds_the_identity_after_an_existing_untracked_file_changes() {
        let fixture = CargoFixture::new();
        fixture.commit_sources();
        std::fs::write(fixture.root().join("loose.txt"), "one\n").expect("create untracked file");
        fixture.build();
        assert_eq!(fixture.reported_identity(), "dirty");
        assert_eq!(fixture.build_script_runs(), 1);

        std::fs::write(fixture.root().join("loose.txt"), "two\n").expect("modify untracked file");
        fixture.build();

        assert_eq!(fixture.build_script_runs(), 2);
    }

    #[test]
    fn a_noop_build_does_not_rerun_the_identity_script_from_its_output() {
        let fixture = CargoFixture::new();
        fixture.commit_sources();
        fixture.build();
        assert_eq!(fixture.build_script_runs(), 1);

        fixture.build();

        assert_eq!(fixture.build_script_runs(), 1);
    }

    #[test]
    fn a_tree_without_git_has_no_commit_identity() {
        let dir = tempfile::tempdir().expect("temp dir");

        assert_eq!(resolve(dir.path()), BuildIdentity::default());
        assert!(watch_paths(dir.path()).is_empty());
    }

    #[test]
    fn a_committed_worktree_reports_its_head_and_is_clean() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());

        let identity = resolve(dir.path());

        let sha = identity.sha.expect("a committed worktree has a HEAD");
        assert_eq!(
            sha.len(),
            12,
            "expected a 12-character short sha, got {sha}"
        );
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!identity.dirty, "a freshly committed tree is not dirty");
        assert!(!watch_paths(dir.path()).is_empty());
    }

    #[test]
    fn an_uncommitted_change_marks_the_worktree_dirty() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());

        std::fs::write(dir.path().join("tracked.txt"), "two").expect("modify tracked file");
        assert!(
            resolve(dir.path()).dirty,
            "a modified tracked file is dirty"
        );

        git(dir.path(), &["checkout", "--", "tracked.txt"]);
        std::fs::write(dir.path().join("stray.txt"), "stray").expect("write untracked file");
        assert!(resolve(dir.path()).dirty, "an untracked file is dirty");
    }

    /// `watch_paths` hands `.git/index` to Cargo as a rebuild trigger, and any
    /// build-script rerun recompiles the whole root crate. Probing identity
    /// must therefore leave the index alone: a plain `git status` refreshes its
    /// stat cache and rewrites it, which would arm that trigger on every build
    /// that follows an edit.
    #[test]
    fn probing_identity_does_not_rewrite_the_index_it_watches() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());
        let index = dir.path().join(".git/index");
        // A stale stat cache is what tempts git into rewriting the index.
        std::fs::write(dir.path().join("tracked.txt"), "modified").expect("modify tracked file");
        let before = std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .expect("index mtime");

        assert!(resolve(dir.path()).dirty, "the fixture must read as dirty");

        let after = std::fs::metadata(&index)
            .and_then(|meta| meta.modified())
            .expect("index mtime");
        assert_eq!(
            before, after,
            "resolve() rewrote .git/index, which watch_paths registers as a rebuild trigger"
        );
    }

    /// A crate unpacked below an unrelated repository must not inherit that
    /// repository's commit — the exact shape of a `cargo install` from the
    /// registry inside a developer's checkout.
    #[test]
    fn a_subdirectory_of_another_repository_has_no_commit_identity() {
        let dir = tempfile::tempdir().expect("temp dir");
        committed_repo(dir.path());
        let unpacked = dir.path().join("registry/tracedecay-0.0.0");
        std::fs::create_dir_all(&unpacked).expect("create unpacked crate dir");

        assert_eq!(resolve(&unpacked), BuildIdentity::default());
        assert!(
            watch_paths(&unpacked).is_empty(),
            "an unrelated repository must not drive this crate's rebuilds"
        );
    }
}
