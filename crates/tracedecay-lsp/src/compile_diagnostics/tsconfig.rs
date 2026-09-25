//! Which TypeScript project owns a file, and what checks it.
//!
//! A monorepo rarely has a root `tsconfig.json`: each package carries its own,
//! usually extending a shared `tsconfig.base.json`. Ownership therefore follows
//! tsserver: the nearest `tsconfig.json` from the file's directory up to the
//! project root owns the file when its effective `files`/`include`/`exclude`
//! (inherited through `extends`) cover it; otherwise its project references,
//! which is the only way a `tsconfig.*.json` owns anything, are consulted
//! before the search continues upward.
//!
//! The compiler is the project's own `node_modules/.bin/tsc`, resolved the way
//! Node resolves a binary: from the owning package up to the workspace root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use globset::GlobBuilder;
use serde::Deserialize;
use walkdir::WalkDir;

use super::typescript::TYPESCRIPT_INSTALL_COMMAND;

const TSCONFIG: &str = "tsconfig.json";
const TS_EXTENSIONS: [&str; 4] = ["ts", "tsx", "mts", "cts"];
const JS_EXTENSIONS: [&str; 4] = ["js", "jsx", "mjs", "cjs"];
/// tsc's `exclude` when a config names none.
const DEFAULT_EXCLUDES: [&str; 3] = ["node_modules", "bower_components", "jspm_packages"];
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

/// A `tsconfig.json` location the owner search checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchedTsconfig {
    /// Relative to the project root.
    pub path: PathBuf,
    /// The config exists, but neither it nor its references include the file.
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeScriptFileOwner {
    Owned(TypeScriptProject),
    /// No tsconfig owns the file; every location the search checked, nearest
    /// first.
    Unowned {
        searched: Vec<SearchedTsconfig>,
    },
}

/// Resolves the project that owns `file` (relative to `project_root`, or
/// absolute beneath it).
pub fn typescript_file_owner(project_root: &Path, file: &Path) -> TypeScriptFileOwner {
    let file = normalize(&project_root.join(file));
    let mut searched = Vec::new();
    let mut dir = file.parent().filter(|_| file.starts_with(project_root));
    while let Some(current) = dir
        && current.starts_with(project_root)
    {
        let candidate = current.join(TSCONFIG);
        let present = candidate.is_file();
        if present && let Some(owner) = owning_config(&candidate, &file, &mut BTreeSet::new()) {
            return TypeScriptFileOwner::Owned(project(project_root, owner));
        }
        searched.push(SearchedTsconfig {
            path: candidate
                .strip_prefix(project_root)
                .unwrap_or(&candidate)
                .to_path_buf(),
            present,
        });
        dir = current.parent();
    }
    TypeScriptFileOwner::Unowned { searched }
}

/// Every project under `project_root` that owns inputs: each `tsconfig.json`
/// outside `node_modules` and hidden directories, plus the configs they
/// reference. A solution-style config (`"files": []` with references) owns
/// nothing and is represented by its references.
pub fn typescript_projects(project_root: &Path) -> Vec<TypeScriptProject> {
    let mut configs = BTreeMap::new();
    let roots = WalkDir::new(project_root)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !is_skipped_dir(&entry.file_name().to_string_lossy())
        })
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && entry.file_name() == TSCONFIG)
        .map(walkdir::DirEntry::into_path)
        .collect::<Vec<_>>();
    for root in roots {
        collect_config(normalize(&root), &mut configs);
    }
    configs
        .into_iter()
        .filter(|(_, config)| !config.owns_nothing())
        .map(|(path, _)| project(project_root, path))
        .collect()
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

fn is_skipped_dir(name: &str) -> bool {
    name == "node_modules" || name.starts_with('.')
}

fn project(project_root: &Path, tsconfig: PathBuf) -> TypeScriptProject {
    let binary = if cfg!(windows) { "tsc.cmd" } else { "tsc" };
    let compiler = tsconfig.parent().and_then(|dir| {
        ancestors_within(project_root, dir)
            .map(|dir| dir.join("node_modules").join(".bin").join(binary))
            .find(|candidate| candidate.is_file())
    });
    TypeScriptProject { tsconfig, compiler }
}

fn ancestors_within<'a>(project_root: &'a Path, from: &'a Path) -> impl Iterator<Item = &'a Path> {
    from.ancestors()
        .take_while(move |dir| dir.starts_with(project_root))
}

fn collect_config(path: PathBuf, configs: &mut BTreeMap<PathBuf, Tsconfig>) {
    if configs.contains_key(&path) {
        return;
    }
    let Some(config) = Tsconfig::load(&path) else {
        return;
    };
    let references = config.references.clone();
    configs.insert(path, config);
    for reference in references {
        collect_config(reference, configs);
    }
}

/// The config among `path` and its transitive references that includes `file`.
fn owning_config(path: &Path, file: &Path, visited: &mut BTreeSet<PathBuf>) -> Option<PathBuf> {
    if !visited.insert(path.to_path_buf()) {
        return None;
    }
    let config = Tsconfig::load(path)?;
    if config.owns(file) {
        return Some(path.to_path_buf());
    }
    config
        .references
        .iter()
        .find_map(|reference| owning_config(reference, file, visited))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTsconfig {
    extends: Option<RawExtends>,
    files: Option<Vec<String>>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    #[serde(default)]
    references: Vec<RawReference>,
    #[serde(default)]
    compiler_options: RawCompilerOptions,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawExtends {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct RawReference {
    path: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCompilerOptions {
    allow_js: Option<bool>,
}

impl RawTsconfig {
    fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default()).ok()
    }
}

/// The inputs tsc resolves for a config after `extends`; each field is the
/// nearest definition in the chain, resolved against the config defining it.
#[derive(Default)]
struct Inputs {
    files: Option<Vec<PathBuf>>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    allow_js: Option<bool>,
}

impl Inputs {
    fn resolve(path: &Path, raw: RawTsconfig, visited: &mut BTreeSet<PathBuf>) -> Self {
        visited.insert(path.to_path_buf());
        let dir = path.parent().unwrap_or(path);
        let bases = match raw.extends {
            None => Vec::new(),
            Some(RawExtends::One(base)) => vec![base],
            Some(RawExtends::Many(bases)) => bases,
        };
        // A package base (`@tsconfig/node20`) is unreadable before install;
        // such bases never carry inputs, so ownership does not depend on them.
        let mut inputs = Self::default();
        for base in bases {
            if let Some(base) = resolve_extends(dir, &base)
                && !visited.contains(&base)
                && let Some(raw) = RawTsconfig::read(&base)
            {
                inputs.overlay(Self::resolve(&base, raw, visited));
            }
        }
        inputs.overlay(Self {
            files: raw.files.map(|files| {
                files
                    .iter()
                    .map(|file| normalize(&dir.join(file)))
                    .collect()
            }),
            include: raw.include.map(|include| {
                include
                    .iter()
                    .map(|pattern| include_pattern(dir, pattern))
                    .collect()
            }),
            exclude: raw.exclude.map(|exclude| {
                exclude
                    .iter()
                    .map(|pattern| rooted_pattern(dir, pattern))
                    .collect()
            }),
            allow_js: raw.compiler_options.allow_js,
        });
        inputs
    }

    fn overlay(&mut self, nearer: Self) {
        self.files = nearer.files.or(self.files.take());
        self.include = nearer.include.or(self.include.take());
        self.exclude = nearer.exclude.or(self.exclude.take());
        self.allow_js = nearer.allow_js.or(self.allow_js);
    }
}

struct Tsconfig {
    dir: PathBuf,
    inputs: Inputs,
    references: Vec<PathBuf>,
}

impl Tsconfig {
    fn load(path: &Path) -> Option<Self> {
        let mut raw = RawTsconfig::read(path)?;
        let dir = path.parent()?.to_path_buf();
        let references = std::mem::take(&mut raw.references)
            .into_iter()
            .map(|reference| {
                let target = normalize(&dir.join(reference.path));
                if target.extension().is_some_and(|ext| ext == "json") {
                    target
                } else {
                    target.join(TSCONFIG)
                }
            })
            .collect();
        let inputs = Inputs::resolve(path, raw, &mut BTreeSet::new());
        Some(Self {
            dir,
            inputs,
            references,
        })
    }

    fn owns_nothing(&self) -> bool {
        self.inputs.include.is_none() && self.inputs.files.as_ref().is_some_and(Vec::is_empty)
    }

    fn owns(&self, file: &Path) -> bool {
        let Some(extension) = file.extension().and_then(|ext| ext.to_str()) else {
            return false;
        };
        let supported = TS_EXTENSIONS.contains(&extension)
            || (self.inputs.allow_js == Some(true) && JS_EXTENSIONS.contains(&extension));
        if !supported {
            return false;
        }
        if self
            .inputs
            .files
            .as_ref()
            .is_some_and(|files| files.iter().any(|listed| listed == file))
        {
            return true;
        }
        let include = match (&self.inputs.include, &self.inputs.files) {
            (Some(include), _) => include.clone(),
            (None, Some(_)) => return false,
            (None, None) => vec![rooted_pattern(&self.dir, "**/*")],
        };
        let exclude = self.inputs.exclude.clone().unwrap_or_else(|| {
            DEFAULT_EXCLUDES
                .iter()
                .map(|dir| rooted_pattern(&self.dir, dir))
                .collect()
        });
        let file = slash(file);
        include.iter().any(|pattern| glob_matches(pattern, &file))
            && !exclude.iter().any(|pattern| {
                glob_matches(pattern, &file) || glob_matches(&format!("{pattern}/**"), &file)
            })
    }
}

/// tsc reads an `include` entry whose last segment has no wildcard and no
/// extension as a directory of sources.
fn include_pattern(dir: &Path, pattern: &str) -> String {
    let rooted = rooted_pattern(dir, pattern);
    let last = rooted.rsplit('/').next().unwrap_or_default();
    if last == "**" {
        format!("{rooted}/*")
    } else if !last.contains(['*', '?', '.']) {
        format!("{rooted}/**/*")
    } else {
        rooted
    }
}

/// Anchors a config-relative glob at `dir`, folding leading `./` and `../`
/// into the literal (escaped) prefix.
fn rooted_pattern(dir: &Path, pattern: &str) -> String {
    let mut base = normalize(dir);
    let mut rest = pattern.trim_end_matches('/');
    loop {
        if let Some(tail) = rest.strip_prefix("./") {
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("../") {
            base.pop();
            rest = tail;
        } else if rest == ".." {
            base.pop();
            rest = "";
        } else if rest == "." {
            rest = "";
        } else {
            break;
        }
    }
    let base = globset::escape(&slash(&base));
    if rest.is_empty() {
        base
    } else {
        format!("{}/{rest}", base.trim_end_matches('/'))
    }
}

fn resolve_extends(dir: &Path, spec: &str) -> Option<PathBuf> {
    let with_json = |path: PathBuf| {
        if path.is_file() {
            Some(path)
        } else {
            let mut json = path.into_os_string();
            json.push(".json");
            let json = PathBuf::from(json);
            json.is_file().then_some(json)
        }
    };
    if spec.starts_with('.') || Path::new(spec).is_absolute() {
        return with_json(normalize(&dir.join(spec)));
    }
    dir.ancestors().find_map(|ancestor| {
        let package = ancestor.join("node_modules").join(spec);
        with_json(package.clone()).or_else(|| {
            let nested = package.join(TSCONFIG);
            nested.is_file().then_some(nested)
        })
    })
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .is_ok_and(|glob| glob.compile_matcher().is_match(path))
}

fn slash(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

fn normalize(path: &Path) -> PathBuf {
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

    fn owner(root: &Path, file: &str) -> TypeScriptFileOwner {
        typescript_file_owner(root, Path::new(file))
    }

    /// The rspack shape: per-package configs extending a root base that is
    /// not itself a `tsconfig.json`, and no root `tsconfig.json`.
    fn monorepo() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path();
        write(
            root_path,
            "tsconfig.base.json",
            "{\n  // shared options\n  \"compilerOptions\": { \"strict\": true, },\n  \"include\": [\"./src\"],\n}\n",
        );
        write(
            root_path,
            "packages/app/tsconfig.json",
            "{ \"extends\": \"../../tsconfig.base.json\", \"include\": [\"src\"], \"exclude\": [\"src/generated\"] }",
        );
        write(
            root_path,
            "packages/lib/tsconfig.json",
            "{ \"extends\": \"../../tsconfig.base\" }",
        );
        write(root_path, "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        root
    }

    #[test]
    fn the_nearest_package_tsconfig_owns_its_sources() {
        let root = monorepo();
        let expected = root.path().join("packages/app/tsconfig.json");
        assert_eq!(
            owner(root.path(), "packages/app/src/nested/index.ts"),
            TypeScriptFileOwner::Owned(TypeScriptProject {
                tsconfig: expected,
                compiler: None,
            })
        );
        assert!(matches!(
            owner(root.path(), "packages/app/src/generated/out.ts"),
            TypeScriptFileOwner::Unowned { .. }
        ));
    }

    /// `include` defined only in the base resolves against the base's
    /// directory, exactly as tsc does, so it does not cover the package.
    #[test]
    fn extends_inherits_inputs_relative_to_the_defining_config() {
        let root = monorepo();
        let TypeScriptFileOwner::Unowned { searched } = owner(root.path(), "packages/lib/src/a.ts")
        else {
            panic!("the base's ./src is the root src, not the package's");
        };
        assert_eq!(
            searched,
            vec![
                SearchedTsconfig {
                    path: PathBuf::from("packages/lib/src/tsconfig.json"),
                    present: false
                },
                SearchedTsconfig {
                    path: PathBuf::from("packages/lib/tsconfig.json"),
                    present: true
                },
                SearchedTsconfig {
                    path: PathBuf::from("packages/tsconfig.json"),
                    present: false
                },
                SearchedTsconfig {
                    path: PathBuf::from("tsconfig.json"),
                    present: false
                },
            ]
        );
    }

    #[test]
    fn project_references_own_files_the_solution_config_does_not() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "tsconfig.json",
            "{ \"files\": [], \"references\": [{ \"path\": \"./tsconfig.lib.json\" }, { \"path\": \"./tools\" }] }",
        );
        write(
            root.path(),
            "tsconfig.lib.json",
            "{ \"include\": [\"src/**/*.ts\"] }",
        );
        write(
            root.path(),
            "tools/tsconfig.json",
            "{ \"compilerOptions\": { \"allowJs\": true } }",
        );
        let TypeScriptFileOwner::Owned(lib) = owner(root.path(), "src/deep/a.ts") else {
            panic!("the referenced tsconfig.lib.json owns src");
        };
        assert_eq!(lib.tsconfig, root.path().join("tsconfig.lib.json"));
        let TypeScriptFileOwner::Owned(tools) = owner(root.path(), "tools/run.js") else {
            panic!("allowJs admits the referenced tools project's scripts");
        };
        assert_eq!(tools.tsconfig, root.path().join("tools/tsconfig.json"));
        assert!(matches!(
            owner(root.path(), "src/main.rs"),
            TypeScriptFileOwner::Unowned { .. }
        ));

        let projects = typescript_projects(root.path());
        let configs = projects
            .iter()
            .map(|project| project.tsconfig.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            configs,
            vec![
                root.path().join("tools/tsconfig.json"),
                root.path().join("tsconfig.lib.json"),
            ],
            "the solution config owns nothing and is represented by its references"
        );
    }

    #[test]
    fn the_compiler_resolves_from_the_package_then_the_workspace_root() {
        let root = monorepo();
        let binary = if cfg!(windows) { "tsc.cmd" } else { "tsc" };
        write(root.path(), &format!("node_modules/.bin/{binary}"), "");
        let projects = typescript_projects(root.path());
        assert_eq!(projects.len(), 2, "{projects:?}");
        assert!(
            projects.iter().all(|project| project.compiler
                == Some(root.path().join("node_modules/.bin").join(binary)))
        );

        write(
            root.path(),
            &format!("packages/app/node_modules/.bin/{binary}"),
            "",
        );
        let TypeScriptFileOwner::Owned(app) = owner(root.path(), "packages/app/src/index.ts")
        else {
            panic!("app owns its sources");
        };
        assert_eq!(
            app.compiler,
            Some(
                root.path()
                    .join("packages/app/node_modules/.bin")
                    .join(binary)
            )
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

    #[test]
    fn a_path_outside_the_project_searches_nothing() {
        let root = monorepo();
        assert_eq!(
            owner(root.path(), "../elsewhere/a.ts"),
            TypeScriptFileOwner::Unowned {
                searched: Vec::new()
            }
        );
    }
}
