//! Where a project's TypeScript configurations are, and what the compiler
//! recorded reading for one of them.
//!
//! A monorepo rarely has a root `tsconfig.json`: each package carries its
//! own. Every `tsconfig.json` outside `node_modules` and hidden directories
//! is therefore a project tsc can be pointed at with `-p`. What a project
//! inherits, which files it holds and which projects it references are the
//! compiler's answers ([`super::typescript`]), so nothing here reads a
//! configuration.
//!
//! The compiler is the project's own `node_modules/.bin/tsc`, resolved the way
//! Node resolves a binary: from the configuration's directory up to the
//! workspace root.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use walkdir::WalkDir;

use super::typescript::TYPESCRIPT_INSTALL_COMMAND;

const TSCONFIG: &str = "tsconfig.json";
/// Lockfile to the command that installs that workspace's dependencies.
const LOCKFILE_INSTALL_COMMANDS: [(&str, &str); 6] = [
    ("pnpm-lock.yaml", "pnpm install"),
    ("yarn.lock", "yarn install"),
    ("bun.lock", "bun install"),
    ("bun.lockb", "bun install"),
    ("package-lock.json", "npm install"),
    ("npm-shrinkwrap.json", "npm install"),
];

/// One TypeScript project: a tsconfig tsc can be pointed at with `-p`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeScriptProject {
    pub tsconfig: PathBuf,
    /// The nearest `node_modules/.bin/tsc` from the tsconfig's directory up to
    /// the project root; `None` before the workspace's dependencies are
    /// installed.
    pub compiler: Option<PathBuf>,
}

/// Every `tsconfig.json` under `project_root` outside `node_modules` and
/// hidden directories, with its compiler. The configurations they reference
/// are the compiler's answer, not this walk's.
pub fn typescript_projects(project_root: &Path) -> Vec<TypeScriptProject> {
    WalkDir::new(project_root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !is_skipped_dir(&entry.file_name().to_string_lossy())
        })
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == TSCONFIG)
        .map(|entry| typescript_project(project_root, entry.into_path()))
        .collect()
}

/// The project of a configuration under `project_root`, such as one a
/// compiler-resolved reference names, with its compiler.
#[must_use]
pub fn typescript_project(project_root: &Path, tsconfig: PathBuf) -> TypeScriptProject {
    let tsconfig = normalize(&tsconfig);
    let binary = if cfg!(windows) { "tsc.cmd" } else { "tsc" };
    let compiler = tsconfig.parent().and_then(|dir| {
        ancestors_within(project_root, dir)
            .map(|dir| dir.join("node_modules").join(".bin").join(binary))
            .find(|candidate| candidate.is_file())
    });
    TypeScriptProject { tsconfig, compiler }
}

/// The command that installs the workspace dependencies for a package at
/// `package_dir`, from the nearest lockfile up to `project_root`. Without a
/// lockfile there is no workspace install to name, so the command adds the
/// compiler itself.
pub fn typescript_install_command(project_root: &Path, package_dir: &Path) -> &'static str {
    ancestors_within(project_root, package_dir)
        .find_map(|dir| {
            LOCKFILE_INSTALL_COMMANDS
                .iter()
                .find(|(lockfile, _)| dir.join(lockfile).is_file())
                .map(|(_, command)| *command)
        })
        .unwrap_or(TYPESCRIPT_INSTALL_COMMAND)
}

/// Whether `name` is a package manifest or lockfile: the files that decide
/// how imports resolve from `node_modules` and which module format a
/// package's sources have.
#[must_use]
pub fn is_typescript_package_manifest(name: &str) -> bool {
    name == "package.json"
        || LOCKFILE_INSTALL_COMMANDS
            .iter()
            .any(|(lockfile, _)| *lockfile == name)
}

/// The program one check read, as its `--tsBuildInfoFile` records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeScriptProgram {
    /// Every file the program read, absolute.
    pub files: Vec<PathBuf>,
    /// The files the configuration itself named, absolute; the rest were
    /// reached through imports, type references and default libraries.
    pub roots: Vec<PathBuf>,
}

/// The program a `--tsBuildInfoFile` records. `None` when the file is missing
/// or not a build-info tsc writes, so a caller cannot mistake an unreadable
/// record for a project that reads nothing.
///
/// tsc records each file relative to the build-info, and its roots as
/// one-based file ids or inclusive id ranges. typescript@7 leaves an empty
/// program's file list out; typescript@5 writes the names it compares with,
/// lower-cased where the filesystem ignores case.
#[must_use]
pub fn typescript_build_info_program(build_info: &Path) -> Option<TypeScriptProgram> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BuildInfo {
        version: String,
        #[serde(default)]
        file_names: Vec<String>,
        #[serde(default)]
        root: Vec<RootFiles>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RootFiles {
        One(usize),
        Range(usize, usize),
    }

    let text = std::fs::read_to_string(build_info).ok()?;
    let info: BuildInfo = serde_json::from_str(&text).ok()?;
    if info.version.is_empty() {
        return None;
    }
    let dir = build_info.parent()?;
    let files = info
        .file_names
        .iter()
        .map(|name| normalize(&dir.join(name)))
        .collect::<Vec<_>>();
    let mut roots = Vec::new();
    for entry in info.root {
        let (first, last) = match entry {
            RootFiles::One(id) => (id, id),
            RootFiles::Range(first, last) => (first, last),
        };
        if first == 0 || first > last {
            return None;
        }
        roots.extend_from_slice(files.get(first - 1..last)?);
    }
    Some(TypeScriptProgram { files, roots })
}

fn is_skipped_dir(name: &str) -> bool {
    name == "node_modules" || name.starts_with('.')
}

fn ancestors_within<'a>(project_root: &'a Path, from: &'a Path) -> impl Iterator<Item = &'a Path> {
    from.ancestors()
        .take_while(move |dir| dir.starts_with(project_root))
}

/// Folds `.` and `..` lexically, as tsc names the paths it resolves.
pub(super) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::fs;

    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// The rspack shape: per-package configs, one beside a solution config,
    /// none in installed packages or hidden directories.
    fn monorepo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();
        write(root_path, "tsconfig.base.json", "{}");
        write(root_path, "packages/app/tsconfig.json", "{}");
        write(root_path, "packages/lib/tsconfig.json", "{}");
        write(
            root_path,
            "packages/lib/node_modules/dep/tsconfig.json",
            "{}",
        );
        write(root_path, ".cache/tsconfig.json", "{}");
        write(root_path, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        root
    }

    #[test]
    fn every_package_tsconfig_is_a_project_with_its_nearest_compiler() {
        let root = monorepo();
        let binary = if cfg!(windows) { "tsc.cmd" } else { "tsc" };
        assert_eq!(
            typescript_projects(root.path()),
            ["packages/app/tsconfig.json", "packages/lib/tsconfig.json"].map(|tsconfig| {
                TypeScriptProject {
                    tsconfig: root.path().join(tsconfig),
                    compiler: None,
                }
            }),
            "a base config is not a tsconfig.json, and installed or hidden ones are no project"
        );

        write(root.path(), &format!("node_modules/.bin/{binary}"), "");
        write(
            root.path(),
            &format!("packages/app/node_modules/.bin/{binary}"),
            "",
        );
        let compilers = typescript_projects(root.path())
            .into_iter()
            .map(|project| project.compiler.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            compilers,
            [
                root.path()
                    .join("packages/app/node_modules/.bin")
                    .join(binary),
                root.path().join("node_modules/.bin").join(binary),
            ]
        );
        assert_eq!(
            typescript_project(
                root.path(),
                root.path().join("packages/lib/../lib/tsconfig.build.json")
            ),
            TypeScriptProject {
                tsconfig: root.path().join("packages/lib/tsconfig.build.json"),
                compiler: Some(root.path().join("node_modules/.bin").join(binary)),
            },
            "a referenced configuration resolves its compiler the same way"
        );
    }

    #[test]
    fn the_install_command_follows_the_workspace_lockfile() {
        let root = monorepo();
        let package = root.path().join("packages/app");
        assert_eq!(
            typescript_install_command(root.path(), &package),
            "pnpm install"
        );
        fs::remove_file(root.path().join("pnpm-lock.yaml")).unwrap();
        write(root.path(), "yarn.lock", "");
        assert_eq!(
            typescript_install_command(root.path(), &package),
            "yarn install"
        );
        fs::remove_file(root.path().join("yarn.lock")).unwrap();
        assert_eq!(
            typescript_install_command(root.path(), &package),
            TYPESCRIPT_INSTALL_COMMAND
        );
    }

    /// tsc records program files relative to the build-info file and roots as
    /// one-based ids or inclusive id ranges; a record that is not tsc's shape
    /// is unknown, never an empty program.
    #[test]
    fn build_info_program_names_files_and_roots_against_the_build_info_directory() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "store/app.tsbuildinfo",
            r#"{"fileNames":["lib.es5.d.ts","../project/src/a.ts","../project/src/b.ts","../project/src/util.ts","../project/src/c.ts"],"root":[[2,3],5],"version":"5.9.3"}"#,
        );
        assert_eq!(
            typescript_build_info_program(&root.path().join("store/app.tsbuildinfo")),
            Some(TypeScriptProgram {
                files: [
                    "store/lib.es5.d.ts",
                    "project/src/a.ts",
                    "project/src/b.ts",
                    "project/src/util.ts",
                    "project/src/c.ts"
                ]
                .map(|file| root.path().join(file))
                .to_vec(),
                roots: ["project/src/a.ts", "project/src/b.ts", "project/src/c.ts"]
                    .map(|file| root.path().join(file))
                    .to_vec(),
            })
        );

        // typescript@7 leaves an empty program's lists out.
        write(
            root.path(),
            "store/solution.tsbuildinfo",
            r#"{"version":"7.0.2","fileInfos":[],"options":{}}"#,
        );
        assert_eq!(
            typescript_build_info_program(&root.path().join("store/solution.tsbuildinfo")),
            Some(TypeScriptProgram {
                files: Vec::new(),
                roots: Vec::new()
            })
        );

        for (name, record) in [
            ("unversioned", r#"{"fileNames":["a.ts"],"root":[1]}"#),
            ("blank", r#"{"version":"","fileNames":[]}"#),
            (
                "unnamed-root",
                r#"{"version":"5.9.3","fileNames":["a.ts"],"root":[2]}"#,
            ),
            (
                "zero-root",
                r#"{"version":"5.9.3","fileNames":["a.ts"],"root":[0]}"#,
            ),
            (
                "reversed-root",
                r#"{"version":"5.9.3","fileNames":["a.ts","b.ts"],"root":[[2,1]]}"#,
            ),
            ("not-json", "{ invalid"),
        ] {
            let path = root.path().join(format!("store/{name}.tsbuildinfo"));
            fs::write(&path, record).unwrap();
            assert_eq!(typescript_build_info_program(&path), None, "{name}");
        }
        assert_eq!(
            typescript_build_info_program(&root.path().join("store/missing.tsbuildinfo")),
            None
        );
    }
}
