//! The cargo half of the unmounted-file audit.
//!
//! For each cargo package: walk the module tree from that package's target
//! roots, following `mod name;`, `#[path = "…"]`, `#[cfg_attr(…, path = "…")]`
//! and `include!("…")`, then diff the reachable set against the `.rs` files
//! under the package's own source directories.
//!
//! Package discovery is the working tree, not `workspace.members`. A member
//! list is one *declaration* of where crates live and it is routinely
//! incomplete: path dependencies outside the workspace, fixture crates with
//! their own `[workspace]`, and vendored sub-projects all compile as real
//! packages while appearing in no member glob. Walking to every `Cargo.toml`
//! finds those, and — this is the part that removes false verdicts — makes
//! every manifest directory a *claim boundary*, so an outer package can never
//! be blamed for a file that belongs to an inner one.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};

use ignore::overrides::OverrideBuilder;
use tree_sitter::{Node, Parser};

use super::*;

/// Directories a cargo package may own source under.
///
/// Deliberately not "everything below the manifest": a crate directory also
/// holds `build.rs`, generated corpora, and support trees that no target ever
/// compiles, and reporting those as orphans would make the answer useless.
/// Anything outside these four directories is out of scope by construction and
/// counted as unclaimed rather than unmounted.
const CARGO_SOURCE_DIRS: [&str; 4] = ["src", "tests", "benches", "examples"];

const RUST_VERDICT: &str = "no `mod` declaration reaches this file from any cargo target root — \
                            the compiler never parses it";

/// Reachability this walk genuinely cannot see, stated rather than guessed at.
const RUST_BLIND_SPOTS: [&str; 4] = [
    "`include!` with a computed path (`concat!(env!(\"OUT_DIR\"), …)`) is not resolved; a \
     build-script module included that way is reported as unmounted only if it also lives in the \
     working tree, which generated code does not",
    "a `mod` produced by macro expansion is invisible — the scan reads declarations, not expanded \
     token trees",
    "`#[cfg(…)]` and `#[cfg_attr(…)]` predicates are not evaluated: every declared path counts as \
     mounted, so a module gated to a platform you never build still reads as reachable",
    "a manifest with no target root at all (no lib, bin, test, bench, example or build script) \
     claims its subtree out of scope rather than reporting every file under it",
];

/// One `mod` declaration read out of a source file, with the directory its
/// candidate files resolve against.
struct ModuleDeclaration {
    name: String,
    /// An unconditional `#[path = "…"]`, which replaces convention entirely.
    explicit_path: Option<String>,
    /// Paths from `#[cfg_attr(predicate, path = "…")]`. Predicates are not
    /// evaluated, so each one is an *additional* file that may be the module —
    /// convention still applies alongside them.
    conditional_paths: Vec<String>,
    /// Where `mod name;` resolves: the *module* directory, one level deeper for
    /// each inline `mod x { ... }` the declaration sits inside.
    module_directory: PathBuf,
    /// Where `#[path = "…"]` resolves, which is a different directory and the
    /// single subtlety most worth getting right here.
    ///
    /// A path attribute on a module that is *not* inside an inline module block
    /// is relative to the directory holding the source file — so in
    /// `src/profile_backup.rs`, `#[path = "profile_backup/error.rs"]` means
    /// `src/profile_backup/error.rs`, not `src/profile_backup/profile_backup/`.
    /// Resolving it against the module directory instead reports every such
    /// module as an orphan; this repository alone has thirty of them.
    attribute_directory: PathBuf,
}

impl ModuleDeclaration {
    /// The directory an inline module's own children resolve against.
    fn nested_directory(&self) -> PathBuf {
        match self.explicit_path.as_deref() {
            Some(path) => self.attribute_directory.join(path),
            None => self.module_directory.join(&self.name),
        }
    }

    /// Files that may all be this module at once (every declared path), and the
    /// conventional pair of which at most one may be.
    fn candidates(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let declared = self
            .explicit_path
            .iter()
            .chain(self.conditional_paths.iter())
            .map(|path| self.attribute_directory.join(path))
            .collect::<Vec<_>>();
        if self.explicit_path.is_some() {
            return (declared, Vec::new());
        }
        (
            declared,
            vec![
                self.module_directory.join(format!("{}.rs", self.name)),
                self.module_directory.join(&self.name).join("mod.rs"),
            ],
        )
    }
}

/// One cargo package and everything the audit needs to judge its files.
struct CratePackage {
    name: String,
    /// Project-relative manifest path, so a finding names the crate a reader
    /// can open rather than an absolute path from this machine.
    manifest: String,
    /// Every target entry point, paired with the directory its own `mod`
    /// declarations resolve against. Roots resolve against their parent
    /// directory — `src/lib.rs` declares `src/foo.rs`, not `src/lib/foo.rs`.
    roots: Vec<(PathBuf, PathBuf)>,
    /// Absolute source directories owned by this package.
    source_dirs: Vec<PathBuf>,
}

/// One manifest directory: a claim boundary, with a package when the manifest
/// declares targets that exist.
struct ManifestEntry {
    dir: PathBuf,
    package: Option<CratePackage>,
}

/// Walks every discovered cargo package's module tree and reports the files it
/// never reaches.
pub(super) fn audit(files: &ProjectFiles) -> Result<EcosystemAudit> {
    let project_root = files.root();
    let manifest_paths = files.named("Cargo.toml");
    let rust_files = files.with_extensions(&["rs"]);
    if manifest_paths.is_empty() && rust_files.is_empty() {
        return Ok(EcosystemAudit::not_present("rust", RUST_VERDICT));
    }

    let (excluded_globs, workspace_excluded_dirs) = workspace_declarations(project_root)?;
    let manifests = manifest_paths
        .iter()
        .filter_map(|manifest| manifest.parent())
        .map(normalized)
        .filter(|dir| {
            !workspace_excluded_dirs
                .iter()
                .any(|excluded| dir.starts_with(excluded))
        })
        .map(|dir| {
            let package = cargo_package(project_root, &dir);
            ManifestEntry { dir, package }
        })
        .collect::<Vec<_>>();

    // One mounted set across all packages: a file is mounted if *any* target in
    // the tree reaches it, and asking per package would report a file
    // twice-owned by a shared directory as an orphan of the package that does
    // not declare it.
    let mut mounted: HashSet<PathBuf> = HashSet::new();
    let mut visited: HashSet<(PathBuf, PathBuf)> = HashSet::new();
    let mut directories = DirIndex::default();
    let mut entry_point_count = 0usize;
    for package in manifests.iter().filter_map(|entry| entry.package.as_ref()) {
        entry_point_count += package.roots.len();
        walk_mounted_files(&package.roots, &mut mounted, &mut visited, &mut directories);
    }

    let excluded = build_excluded_matcher(project_root, &excluded_globs);
    let mut scanned_file_count = 0usize;
    let mut unclaimed_file_count = 0usize;
    let mut unmounted = Vec::new();

    for absolute in rust_files {
        let relative = files.relative(absolute);
        if excluded
            .as_ref()
            .is_some_and(|matcher| matcher.matched(&relative, false).is_whitelist())
        {
            continue;
        }
        // The deepest manifest directory above this file owns it, whether or
        // not that manifest could be audited. An outer package must never be
        // blamed for a file that lives inside an inner package.
        let Some(entry) = manifests
            .iter()
            .filter(|entry| absolute.starts_with(&entry.dir))
            .max_by_key(|entry| entry.dir.as_os_str().len())
        else {
            unclaimed_file_count += 1;
            continue;
        };
        let Some(package) = entry.package.as_ref().filter(|package| {
            package
                .source_dirs
                .iter()
                .any(|dir| absolute.starts_with(dir))
        }) else {
            unclaimed_file_count += 1;
            continue;
        };
        scanned_file_count += 1;
        if mounted.contains(&normalized(absolute)) {
            continue;
        }
        let (nearest_mounted_parent, suggested_declaration) =
            repair_for_unmounted_file(project_root, absolute, &mounted);
        unmounted.push(UnmountedFile {
            file: relative,
            package: package.name.clone(),
            manifest: package.manifest.clone(),
            nearest_mounted_parent,
            suggested_declaration: Some(suggested_declaration),
        });
    }

    unmounted.sort_by(|left, right| left.file.cmp(&right.file));
    let package_count = manifests
        .iter()
        .filter(|entry| entry.package.is_some())
        .count();
    Ok(EcosystemAudit {
        ecosystem: "rust",
        status: EcosystemStatus::Audited,
        package_count,
        entry_point_count,
        scanned_file_count,
        mounted_file_count: mounted.len(),
        unclaimed_file_count,
        verdict: RUST_VERDICT,
        blind_spots: RUST_BLIND_SPOTS.to_vec(),
        note: None,
        excluded_globs,
        unmounted,
    })
}

/// The two things the root manifest declares about scope: globs the workspace
/// already called "not a source-repo target", and directories it excluded.
///
/// `[workspace.metadata.cargo-shear] ignored-paths` is the existing, reviewed
/// answer to "which `.rs` files under a source directory are deliberately not
/// linked" — fixture corpora, distribution acceptance sources built only by a
/// script. Re-deriving that judgement here would create a second list to keep
/// in sync and would report the same false positives the workspace already
/// wrote down.
fn workspace_declarations(project_root: &Path) -> Result<(Vec<String>, Vec<PathBuf>)> {
    let Ok(text) = std::fs::read_to_string(project_root.join("Cargo.toml")) else {
        // Not a cargo project at the root. Packages may still exist deeper in
        // the tree, so this is a missing declaration, not a missing audit.
        return Ok((Vec::new(), Vec::new()));
    };
    let Ok(manifest) = toml::from_str::<toml::Value>(&text) else {
        return Err(TraceDecayError::Config {
            message: "the workspace Cargo.toml is not valid TOML".to_owned(),
        });
    };

    let excluded_globs = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("metadata"))
        .and_then(|metadata| metadata.get("cargo-shear"))
        .and_then(|shear| shear.get("ignored-paths"))
        .and_then(toml::Value::as_array)
        .map(|globs| {
            globs
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // `exclude` names a directory, and cargo excludes everything under it —
    // comparing for equality would keep auditing every member below an
    // excluded tree.
    let excluded_dirs = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("exclude"))
        .and_then(toml::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(toml::Value::as_str)
                .map(|entry| normalized(&project_root.join(entry)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok((excluded_globs, excluded_dirs))
}

fn build_excluded_matcher(
    project_root: &Path,
    globs: &[String],
) -> Option<ignore::overrides::Override> {
    if globs.is_empty() {
        return None;
    }
    let mut builder = OverrideBuilder::new(project_root);
    for glob in globs {
        // A malformed entry in someone else's manifest must narrow the
        // exclusion list, never fail the audit.
        let _ = builder.add(glob);
    }
    builder.build().ok()
}

/// Reads one package manifest into the target roots and source directories the
/// audit walks. Returns `None` for a manifest with no `[package]` (a virtual
/// workspace root owns no targets) and for one whose declared targets do not
/// exist on disk — both are claim boundaries with nothing to audit.
fn cargo_package(project_root: &Path, dir: &Path) -> Option<CratePackage> {
    let manifest_path = dir.join("Cargo.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path).ok()?;
    let manifest = toml::from_str::<toml::Value>(&manifest_text).ok()?;
    let package = manifest.get("package")?;
    let name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or("<unnamed>")
        .to_owned();

    let mut roots: Vec<PathBuf> = Vec::new();

    // Library: an explicit `[lib] path`, otherwise the conventional file.
    roots.push(
        manifest
            .get("lib")
            .and_then(|lib| lib.get("path"))
            .and_then(toml::Value::as_str)
            .map_or_else(|| dir.join("src/lib.rs"), |path| dir.join(path)),
    );

    // Build script: declared or conventional. It is a compilation unit of its
    // own, and a `mod` it declares mounts a file just as any other root does.
    match package.get("build") {
        Some(toml::Value::String(build)) => roots.push(dir.join(build)),
        Some(toml::Value::Boolean(false)) => {}
        _ => roots.push(dir.join("build.rs")),
    }

    for (table, directory, auto_key) in [
        ("bin", "src/bin", "autobins"),
        ("test", "tests", "autotests"),
        ("bench", "benches", "autobenches"),
        ("example", "examples", "autoexamples"),
    ] {
        if table == "bin" {
            roots.push(dir.join("src/main.rs"));
        }
        // Explicit target entries always count, even when auto-discovery is
        // switched off — that switch disables convention, not declaration. A
        // declared `path` may point outside the package directory, which is how
        // a thin manifest re-targets a binary owned by another crate.
        if let Some(entries) = manifest.get(table).and_then(toml::Value::as_array) {
            for entry in entries {
                if let Some(path) = entry.get("path").and_then(toml::Value::as_str) {
                    roots.push(dir.join(path));
                    continue;
                }
                if let Some(target_name) = entry.get("name").and_then(toml::Value::as_str) {
                    roots.push(dir.join(directory).join(format!("{target_name}.rs")));
                    roots.push(dir.join(directory).join(target_name).join("main.rs"));
                }
            }
        }
        let auto_discovers = package
            .get(auto_key)
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        if auto_discovers {
            roots.extend(auto_discovered_roots(&dir.join(directory)));
        }
    }

    let mut deduped: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        let root = normalized(&root);
        if !root.is_file() || !seen.insert(root.clone()) {
            continue;
        }
        // A crate root's own `mod` declarations resolve against its parent
        // directory, never against a directory named after the root file:
        // `src/lib.rs` declares `src/foo.rs`. Only non-root module files push a
        // directory level.
        let parent = root.parent().unwrap_or(project_root).to_path_buf();
        deduped.push((root, parent));
    }
    if deduped.is_empty() {
        return None;
    }

    let source_dirs = CARGO_SOURCE_DIRS
        .iter()
        .map(|name| dir.join(name))
        .filter(|candidate| candidate.is_dir())
        .map(|candidate| normalized(&candidate))
        .collect::<Vec<_>>();

    Some(CratePackage {
        name,
        manifest: relative_display(project_root, &manifest_path),
        roots: deduped,
        source_dirs,
    })
}

/// Cargo's convention for a target directory: every `<dir>/*.rs`, plus every
/// `<dir>/<name>/main.rs`.
///
/// The second form is the one that hides orphans. `tests/mcp_suite/main.rs` is
/// a root; every other file under `tests/mcp_suite/` is reachable only through
/// that root's own `mod` declarations, so a file added to the directory without
/// a matching `mod` line compiles nowhere and is seen by no one.
fn auto_discovered_roots(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let main = path.join("main.rs");
            if main.is_file() {
                roots.push(main);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            roots.push(path);
        }
    }
    roots.sort();
    roots
}

/// Directory listings, cached, so a resolved candidate carries the file's real
/// on-disk spelling.
///
/// On a case-insensitive filesystem `mod Config;` opens `config.rs`, and a set
/// keyed on the declaration's spelling would never match the walker's. The
/// filesystem still decides whether the file exists — the index only answers
/// "under what name?", so nothing here invents a match a case-sensitive
/// filesystem would refuse.
#[derive(Default)]
struct DirIndex {
    entries: HashMap<PathBuf, HashMap<String, Vec<OsString>>>,
}

impl DirIndex {
    fn resolve(&mut self, candidate: &Path) -> Option<PathBuf> {
        if !candidate.is_file() {
            return None;
        }
        let candidate = normalized(candidate);
        let (Some(parent), Some(name)) = (candidate.parent(), candidate.file_name()) else {
            return Some(candidate.clone());
        };
        let listing = self
            .entries
            .entry(parent.to_path_buf())
            .or_insert_with(|| read_dir_index(parent));
        match on_disk_spelling(listing, name) {
            Some(actual) if actual != name => Some(parent.join(actual)),
            _ => Some(candidate.clone()),
        }
    }
}

fn read_dir_index(directory: &Path) -> HashMap<String, Vec<OsString>> {
    let mut index: HashMap<String, Vec<OsString>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return index;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let lowered = name.to_string_lossy().to_ascii_lowercase();
        index.entry(lowered).or_default().push(name);
    }
    index
}

/// The on-disk name for `wanted`: itself when the directory really holds that
/// spelling, the single case-variant when it holds exactly one, and `None` when
/// the directory holds several (a case-sensitive filesystem with both `Foo.rs`
/// and `foo.rs` — guessing there would mount the wrong file).
fn on_disk_spelling(listing: &HashMap<String, Vec<OsString>>, wanted: &OsStr) -> Option<OsString> {
    let lowered = wanted.to_string_lossy().to_ascii_lowercase();
    let names = listing.get(&lowered)?;
    if names.iter().any(|name| name == wanted) {
        return Some(wanted.to_owned());
    }
    match names.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

/// Breadth-first traversal of the module tree from a package's target roots.
///
/// Every file it reaches is mounted; the closed set it produces is the only
/// thing that makes "unmounted" a fact rather than a heuristic. `visited` is
/// keyed by file *and* the directory its own declarations resolve against: the
/// same file reached as a crate root and as a module resolves its children
/// differently, so both visits must happen.
fn walk_mounted_files(
    roots: &[(PathBuf, PathBuf)],
    mounted: &mut HashSet<PathBuf>,
    visited: &mut HashSet<(PathBuf, PathBuf)>,
    directories: &mut DirIndex,
) {
    let mut queue: VecDeque<(PathBuf, PathBuf)> = VecDeque::new();
    for (root, directory) in roots {
        mounted.insert(root.clone());
        if visited.insert((root.clone(), directory.clone())) {
            queue.push_back((root.clone(), directory.clone()));
        }
    }
    while let Some((file, directory)) = queue.pop_front() {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Some(tree) = parse_rust(&source) else {
            continue;
        };
        // `mod name;` resolves against the module directory; `#[path]` at the
        // top level of a file resolves against the directory the file is in.
        // For a crate root or a `mod.rs` those are the same directory, which is
        // exactly why the distinction is easy to miss and expensive to get
        // wrong.
        let file_directory = file.parent().unwrap_or(&directory).to_path_buf();
        let mut declarations = Vec::new();
        collect_module_declarations(
            &source,
            tree.root_node(),
            &directory,
            &file_directory,
            true,
            &mut declarations,
        );
        for declaration in declarations {
            let (inclusive, exclusive) = declaration.candidates();
            // A file reached through `#[path]` has its own children resolve
            // against the directory that holds it, as if it were `mod.rs`.
            // Conventional `name.rs` still nests under `name/`. Mixing the two
            // is how `#[path = "automation/tests.rs"] mod tests;` then
            // `mod helper;` inside that file mounts `automation/helper.rs`
            // rather than looking in a phantom `automation/tests/` directory.
            let mut reached = Vec::new();
            for candidate in inclusive {
                if let Some(actual) = directories.resolve(&candidate) {
                    reached.push((actual, true));
                }
            }
            for candidate in exclusive {
                if let Some(actual) = directories.resolve(&candidate) {
                    reached.push((actual, false));
                    break;
                }
            }
            for (actual, via_explicit_path) in reached {
                mounted.insert(actual.clone());
                let child_directory = if via_explicit_path {
                    actual.parent().unwrap_or(&directory).to_path_buf()
                } else {
                    module_child_directory(&actual)
                };
                if visited.insert((actual.clone(), child_directory.clone())) {
                    queue.push_back((actual, child_directory));
                }
            }
        }
        // `include!` splices a file's tokens in at the invocation site; the
        // path is relative to the *including file*, not to the module
        // directory. The spliced file compiles, so it is mounted, and any `mod`
        // it declares resolves against the including module's directory.
        let mut includes = Vec::new();
        collect_include_paths(&source, tree.root_node(), &file_directory, &mut includes);
        for include in includes {
            let Some(actual) = directories.resolve(&include) else {
                continue;
            };
            mounted.insert(actual.clone());
            if visited.insert((actual.clone(), directory.clone())) {
                queue.push_back((actual, directory.clone()));
            }
        }
    }
}

fn parse_rust(source: &str) -> Option<tree_sitter::Tree> {
    let language = tracedecay_code_extraction::ts_provider::try_language("rust").ok()?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

/// The directory a non-root module file's own declarations resolve against:
/// `a/b/mod.rs` declares into `a/b/`, `a/b.rs` declares into `a/b/`.
fn module_child_directory(file: &Path) -> PathBuf {
    let parent = file.parent().map(Path::to_path_buf).unwrap_or_default();
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("mod") => parent,
        Some(stem) => parent.join(stem),
        None => parent,
    }
}

/// Every external `mod` declaration in one source file, with inline modules
/// descended into so their children resolve one directory deeper.
///
/// Parsed rather than matched: `// mod child;` in a comment, `"mod child;"` in
/// a string literal, and `mod child { ... }` inline all look identical to a
/// text scan and none of them mounts `child.rs`. A false "mounted" verdict here
/// silently hides exactly the orphan this tool exists to find.
///
/// `module_scope` is false once the walk descends into anything that is not a
/// module body. Rust refuses a non-inline `mod` inside a block unless it
/// carries `#[path]`, so outside module scope only path-carrying declarations
/// are believed — a bare `mod x;` down there does not compile and must not be
/// allowed to mount `x.rs` and hide it.
///
/// `attribute_directory` starts as the directory holding the source file and
/// then tracks the module directory once the walk is inside an inline module
/// block, which is exactly what the language reference specifies for `#[path]`.
fn collect_module_declarations(
    source: &str,
    node: Node<'_>,
    directory: &Path,
    attribute_directory: &Path,
    module_scope: bool,
    out: &mut Vec<ModuleDeclaration>,
) {
    let mut cursor = node.walk();
    // tree-sitter-rust emits attributes as preceding siblings, so a pending
    // `#[path]` has to be carried forward to the item it decorates.
    let mut pending_explicit: Option<String> = None;
    let mut pending_conditional: Vec<String> = Vec::new();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "attribute_item" => match path_attribute(source, child) {
                Some(PathAttribute::Unconditional(path)) => pending_explicit = Some(path),
                Some(PathAttribute::Conditional(paths)) => pending_conditional.extend(paths),
                None => {}
            },
            "mod_item" => {
                let explicit_path = pending_explicit.take();
                let conditional_paths = std::mem::take(&mut pending_conditional);
                let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                    .map(str::to_owned)
                else {
                    continue;
                };
                let declaration = ModuleDeclaration {
                    name,
                    explicit_path,
                    conditional_paths,
                    module_directory: directory.to_path_buf(),
                    attribute_directory: attribute_directory.to_path_buf(),
                };
                match child.child_by_field_name("body") {
                    // An inline module declares no file of its own, but its
                    // children resolve under a directory named for it — and
                    // inside that block a `#[path]` resolves against the same
                    // directory, not against the file's own.
                    Some(body) => {
                        let nested = declaration.nested_directory();
                        collect_module_declarations(source, body, &nested, &nested, true, out);
                    }
                    None => {
                        if module_scope || declaration.explicit_path.is_some() {
                            out.push(declaration);
                        }
                    }
                }
            }
            _ => {
                pending_explicit = None;
                pending_conditional.clear();
                // A `#[path]`-carrying `mod` may sit inside a function body, an
                // `impl`, or a `cfg`-gated block. Descending finds those; the
                // `module_scope` flag keeps the bare form from being believed.
                collect_module_declarations(
                    source,
                    child,
                    directory,
                    attribute_directory,
                    false,
                    out,
                );
            }
        }
    }
}

/// Every `include!("literal.rs")` in one file, resolved against `directory`.
///
/// A computed path (`concat!`, `env!("OUT_DIR")`) is deliberately not guessed
/// at: those name generated files that do not exist in the working tree, so
/// resolving them would be inventing a mount for a file the audit never sees.
fn collect_include_paths(source: &str, node: Node<'_>, directory: &Path, out: &mut Vec<PathBuf>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "macro_invocation"
            && child
                .child_by_field_name("macro")
                .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                == Some("include")
            && let Some(literal) = sole_string_argument(source, child)
        {
            out.push(directory.join(literal));
        }
        collect_include_paths(source, child, directory, out);
    }
}

/// The single string literal a macro invocation was handed, if that is all it
/// was handed.
fn sole_string_argument(source: &str, invocation: Node<'_>) -> Option<String> {
    let mut cursor = invocation.walk();
    let tokens = invocation.named_children(&mut cursor).collect::<Vec<_>>();
    let arguments = tokens.last()?;
    if arguments.kind() != "token_tree" {
        return None;
    }
    let mut argument_cursor = arguments.walk();
    let named = arguments
        .named_children(&mut argument_cursor)
        .collect::<Vec<_>>();
    let [literal] = named.as_slice() else {
        return None;
    };
    if literal.kind() != "string_literal" && literal.kind() != "raw_string_literal" {
        return None;
    }
    string_literal_value(literal.utf8_text(source.as_bytes()).ok()?)
}

/// The contents of a Rust string literal, plain or raw.
fn string_literal_value(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('r') {
        let hashes = rest.len() - rest.trim_start_matches('#').len();
        let closing = "\"".to_owned() + &"#".repeat(hashes);
        return rest
            .trim_start_matches('#')
            .strip_prefix('"')?
            .strip_suffix(&closing)
            .map(str::to_owned);
    }
    text.strip_prefix('"')?.strip_suffix('"').map(str::to_owned)
}

/// What a `#[…]` attribute says about where a module's file lives.
enum PathAttribute {
    /// `#[path = "…"]` — replaces convention outright.
    Unconditional(String),
    /// `#[cfg_attr(predicate, path = "…")]` — one more file the module may be.
    Conditional(Vec<String>),
}

fn path_attribute(source: &str, attribute: Node<'_>) -> Option<PathAttribute> {
    let text = attribute.utf8_text(source.as_bytes()).ok()?;
    let inner = text
        .trim()
        .strip_prefix("#[")?
        .strip_suffix(']')?
        .trim_start();
    if let Some(rest) = inner.strip_prefix("path")
        && let Some(value) = rest.trim_start().strip_prefix('=')
    {
        let value = value.trim().strip_prefix('"')?;
        let end = value.find('"')?;
        return Some(PathAttribute::Unconditional(value[..end].to_owned()));
    }
    if !inner.starts_with("cfg_attr") {
        return None;
    }
    let paths = scan_path_assignments(inner);
    (!paths.is_empty()).then_some(PathAttribute::Conditional(paths))
}

/// Every `path = "…"` assignment in attribute text, skipping string contents so
/// `#[cfg_attr(feature = "path", path = "real.rs")]` yields only `real.rs`.
fn scan_path_assignments(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() && bytes[index] != b'"' {
                index += if bytes[index] == b'\\' { 2 } else { 1 };
            }
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"path")
            && index
                .checked_sub(1)
                .is_none_or(|before| !is_ident_byte(bytes[before]))
        {
            let mut cursor = index + "path".len();
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'=') {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if bytes.get(cursor) == Some(&b'"')
                    && let Some(end) = text[cursor + 1..].find('"')
                {
                    out.push(text[cursor + 1..cursor + 1 + end].to_owned());
                    index = cursor + 2 + end;
                    continue;
                }
            }
        }
        index += 1;
    }
    out
}

/// The nearest mounted ancestor that could declare `file`, and the `mod` line
/// that would do it.
///
/// Naming the ancestor is what turns the report into an action: `src/daemon.rs`
/// is missing `mod foo;` is a fix, while "`src/daemon/foo.rs` is unreachable"
/// is a puzzle. Climbing rather than reporting only the immediate parent
/// matters because a whole detached subtree has one repair at its top, not one
/// per file.
fn repair_for_unmounted_file(
    project_root: &Path,
    file: &Path,
    mounted: &HashSet<PathBuf>,
) -> (Option<String>, String) {
    let declaration =
        module_stem(file).map_or_else(|| "mod <module>;".to_owned(), |stem| format!("mod {stem};"));
    let mut current = normalized(file);
    // Bounded by the path depth; the loop always either returns or shortens
    // `current`, and stops at the project root.
    while let Some(parent_module_dir) = parent_module_directory(&current) {
        if !parent_module_dir.starts_with(project_root) {
            break;
        }
        for candidate in parent_module_files(&parent_module_dir) {
            if mounted.contains(&candidate) {
                return (
                    Some(relative_display(project_root, &candidate)),
                    declaration,
                );
            }
        }
        // Nothing at this level is mounted: the branch is detached higher up,
        // so keep climbing from the module file that would have owned it.
        current = parent_module_dir.join("mod.rs");
    }
    (None, declaration)
}

/// The directory owned by the module that would declare `file`.
fn parent_module_directory(file: &Path) -> Option<PathBuf> {
    let parent = file.parent()?;
    match file.file_stem().and_then(|stem| stem.to_str()) {
        // `a/b/mod.rs` is module `b`; its declaring parent owns `a/`.
        Some("mod") => parent.parent().map(Path::to_path_buf),
        _ => Some(parent.to_path_buf()),
    }
}

/// The files that could be the module owning `directory`.
fn parent_module_files(directory: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![
        directory.join("mod.rs"),
        directory.join("lib.rs"),
        directory.join("main.rs"),
    ];
    if let (Some(parent), Some(name)) = (
        directory.parent(),
        directory.file_name().and_then(|name| name.to_str()),
    ) {
        candidates.push(parent.join(format!("{name}.rs")));
    }
    candidates
}

/// The module name a file would be declared under.
fn module_stem(file: &Path) -> Option<String> {
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("mod") => file
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map(str::to_owned),
        Some(stem) => Some(stem.to_owned()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{project, write};
    use super::*;

    fn audit_rust(root: &Path) -> EcosystemAudit {
        audit(&project(root)).expect("audit")
    }

    fn unmounted_paths(audit: &EcosystemAudit) -> Vec<&str> {
        audit
            .unmounted
            .iter()
            .map(|entry| entry.file.as_str())
            .collect()
    }

    /// The one-crate manifest most fixtures need.
    fn package_manifest(root: &Path, name: &str) {
        write(
            root,
            "Cargo.toml",
            &format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\n"),
        );
    }

    /// The headline case, in miniature: one file declared from the crate root
    /// and one file that nobody declares.
    #[test]
    fn one_mounted_and_one_orphan_reports_exactly_the_orphan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "pub mod mounted;\n");
        write(root, "src/mounted.rs", "pub fn mounted() {}\n");
        write(root, "src/orphan.rs", "pub fn orphan() {}\n");

        let audit = audit_rust(root);
        assert_eq!(unmounted_paths(&audit), vec!["src/orphan.rs"]);
        let finding = &audit.unmounted[0];
        assert_eq!(finding.package, "fixture");
        assert_eq!(finding.manifest, "Cargo.toml");
        assert_eq!(
            finding.nearest_mounted_parent.as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            finding.suggested_declaration.as_deref(),
            Some("mod orphan;")
        );
    }

    /// `mod.rs` and `name.rs` are both legal spellings of the same module and
    /// both mount their directory's children.
    #[test]
    fn mod_rs_and_name_rs_module_files_both_mount_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "mod dir_form;\nmod file_form;\n");
        write(root, "src/dir_form/mod.rs", "mod leaf;\n");
        write(root, "src/dir_form/leaf.rs", "");
        write(root, "src/file_form.rs", "mod leaf;\n");
        write(root, "src/file_form/leaf.rs", "");

        assert!(unmounted_paths(&audit_rust(root)).is_empty());
    }

    /// `#[path]` relocates a module's file, including out of its own directory.
    #[test]
    fn path_attribute_overrides_conventional_resolution() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            "#[path = \"relocated/elsewhere.rs\"]\nmod renamed;\nmod plain;\n",
        );
        write(root, "src/relocated/elsewhere.rs", "");
        write(root, "src/plain.rs", "");
        write(root, "src/relocated/unreferenced.rs", "");

        assert_eq!(
            unmounted_paths(&audit_rust(root)),
            vec!["src/relocated/unreferenced.rs"]
        );
    }

    /// `#[path]` is relative to the directory holding the *source file*, while
    /// `mod name;` is relative to the *module* directory. In a `mod.rs` or a
    /// crate root those are the same directory and the difference is invisible;
    /// in `src/thing.rs` they are not, and the difference is thirty false
    /// findings in this repository alone.
    #[test]
    fn path_attributes_resolve_against_the_source_file_directory_not_the_module_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "pub mod backup;\n");
        write(
            root,
            "src/backup.rs",
            concat!(
                "#[path = \"backup/error.rs\"]\nmod error;\n",
                "mod conventional;\n",
                "pub mod inline {\n    #[path = \"nested.rs\"]\n    mod nested;\n}\n",
            ),
        );
        write(root, "src/backup/error.rs", "");
        write(root, "src/backup/conventional.rs", "");
        // The inline block's `#[path]` resolves under the inline module's own
        // directory, not under the file's directory.
        write(root, "src/backup/inline/nested.rs", "");
        write(root, "src/nested.rs", "");

        assert_eq!(unmounted_paths(&audit_rust(root)), vec!["src/nested.rs"]);
    }

    /// A module loaded with `#[path]` resolves its own `mod` children against
    /// the directory holding that file, not against a directory named for the
    /// file stem. This is the `automation.rs` / `#[path = "automation/tests.rs"]`
    /// shape: `mod helper;` inside the path file mounts `automation/helper.rs`.
    #[test]
    fn path_loaded_module_children_resolve_beside_the_path_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            "#[path = \"automation/tests.rs\"]\nmod tests;\n",
        );
        write(root, "src/automation/tests.rs", "mod helper;\n");
        write(root, "src/automation/helper.rs", "");
        write(root, "src/automation/forgotten.rs", "");
        // The wrong resolution (`automation/tests/`) would look here.
        write(root, "src/automation/tests/wrong.rs", "");

        assert_eq!(
            unmounted_paths(&audit_rust(root)),
            vec![
                "src/automation/forgotten.rs",
                "src/automation/tests/wrong.rs"
            ]
        );
    }

    /// The crate-root shape in this repository's `src/lib.rs`:
    /// `#[path = "sessions/claude_observation_benchmark.rs"]`, then that file
    /// itself uses `#[path = "claude_observation_benchmark/artifact.rs"]`.
    /// Both hops must resolve against the file that holds the attribute, or
    /// the whole sessions tree reads as unmounted.
    #[test]
    fn crate_root_path_to_a_file_that_itself_uses_path_attributes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            concat!(
                "#[path = \"sessions/claude_observation_benchmark.rs\"]\n",
                "mod claude_observation_benchmark;\n",
                "#[path = \"sessions/ingest_tests.rs\"]\n",
                "mod session_ingest_tests;\n",
            ),
        );
        write(
            root,
            "src/sessions/claude_observation_benchmark.rs",
            concat!(
                "#[path = \"claude_observation_benchmark/artifact.rs\"]\n",
                "mod artifact;\n",
                "#[path = \"claude_observation_benchmark/tests.rs\"]\n",
                "mod tests;\n",
            ),
        );
        write(
            root,
            "src/sessions/claude_observation_benchmark/artifact.rs",
            "",
        );
        write(
            root,
            "src/sessions/claude_observation_benchmark/tests.rs",
            "",
        );
        write(root, "src/sessions/ingest_tests.rs", "");
        write(root, "src/sessions/forgotten.rs", "");

        assert_eq!(
            unmounted_paths(&audit_rust(root)),
            vec!["src/sessions/forgotten.rs"]
        );
    }

    /// A cfg-gated module is declared; the audit does not evaluate predicates
    /// and must not report its file as an orphan.
    #[test]
    fn cfg_gated_modules_count_as_mounted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            "#[cfg(test)]\nmod gated;\n#[cfg(feature = \"never\")]\npub mod featured;\n",
        );
        write(root, "src/gated.rs", "");
        write(root, "src/featured.rs", "");

        assert!(unmounted_paths(&audit_rust(root)).is_empty());
    }

    /// `#[cfg_attr(…, path = "…")]` names a file the module may be on some
    /// target. Predicates are not evaluated, so every named file is mounted and
    /// the conventional name stays live alongside them.
    #[test]
    fn cfg_attr_path_mounts_every_conditional_file_and_keeps_convention() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            concat!(
                "#[cfg_attr(unix, path = \"platform_unix.rs\")]\n",
                "#[cfg_attr(windows, path = \"platform_windows.rs\")]\n",
                "mod platform;\n",
                "#[cfg_attr(feature = \"path\", path = \"aliased.rs\")]\n",
                "mod fallback;\n",
            ),
        );
        write(root, "src/platform_unix.rs", "");
        write(root, "src/platform_windows.rs", "");
        write(root, "src/aliased.rs", "");
        write(root, "src/fallback.rs", "");
        write(root, "src/nobody.rs", "");

        assert_eq!(unmounted_paths(&audit_rust(root)), vec!["src/nobody.rs"]);
    }

    /// `include!` splices a file in at the invocation site, so the compiler
    /// does parse it. A computed include names generated code that is not in
    /// the working tree and must not be guessed at.
    #[test]
    fn include_of_a_literal_path_mounts_that_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            concat!(
                "mod table;\n",
                "include!(concat!(env!(\"OUT_DIR\"), \"/generated.rs\"));\n",
            ),
        );
        write(root, "src/table.rs", "include!(\"table/rows.rs\");\n");
        write(root, "src/table/rows.rs", "");
        write(root, "src/table/unused.rs", "");

        assert_eq!(
            unmounted_paths(&audit_rust(root)),
            vec!["src/table/unused.rs"]
        );
    }

    /// A declaration inside an inline module resolves one directory deeper, and
    /// a comment or string that merely spells `mod` mounts nothing.
    #[test]
    fn inline_modules_nest_and_text_lookalikes_do_not_mount() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            concat!(
                "pub mod outer {\n    pub mod inner;\n}\n",
                "// mod commented;\n",
                "const NOTE: &str = \"mod stringly;\";\n",
            ),
        );
        write(root, "src/outer/inner.rs", "");
        write(root, "src/commented.rs", "");
        write(root, "src/stringly.rs", "");

        assert_eq!(
            unmounted_paths(&audit_rust(root)),
            vec!["src/commented.rs", "src/stringly.rs"]
        );
    }

    /// A `mod` line that only ever exists inside a macro *definition* is not a
    /// declaration — nothing expands it here, and believing it would hide the
    /// orphan. A `#[path]` module inside a function body is the one block-scoped
    /// form Rust accepts, and it does mount its file.
    #[test]
    fn macro_bodies_do_not_mount_and_block_scoped_path_modules_do() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            concat!(
                "macro_rules! declare {\n    () => {\n        mod macro_only;\n    };\n}\n",
                "fn scoped() {\n    #[path = \"block_scoped.rs\"]\n    mod inner;\n}\n",
            ),
        );
        write(root, "src/macro_only.rs", "");
        write(root, "src/block_scoped.rs", "");

        assert_eq!(
            unmounted_paths(&audit_rust(root)),
            vec!["src/macro_only.rs"]
        );
    }

    /// The integration-test shape the daemon orphans hid in: every `tests/*.rs`
    /// is its own root, `tests/<name>/main.rs` is a root, and a sibling under
    /// that directory is mounted only by the root's own `mod` line.
    #[test]
    fn integration_test_roots_are_per_file_and_per_suite_main() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "");
        write(root, "tests/standalone.rs", "");
        write(root, "tests/suite/main.rs", "mod declared;\n");
        write(root, "tests/suite/declared.rs", "");
        write(root, "tests/suite/forgotten.rs", "");

        let audit = audit_rust(root);
        assert_eq!(unmounted_paths(&audit), vec!["tests/suite/forgotten.rs"]);
        assert_eq!(
            audit.unmounted[0].nearest_mounted_parent.as_deref(),
            Some("tests/suite/main.rs")
        );
    }

    /// A suite root reached first as a *module* of another target still gets
    /// walked as a root, with its own directory semantics. Skipping the second
    /// visit would leave everything the suite declares unmounted.
    #[test]
    fn a_file_that_is_both_a_module_and_a_target_root_is_walked_as_both() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(
            root,
            "src/lib.rs",
            "#[path = \"../tests/suite/main.rs\"]\nmod shared;\n",
        );
        write(root, "tests/suite/main.rs", "mod helper;\n");
        write(root, "tests/suite/helper.rs", "");

        assert!(unmounted_paths(&audit_rust(root)).is_empty());
    }

    /// Workspace members are audited under their own manifests, and a file the
    /// workspace already declared as not-a-target is excluded.
    #[test]
    fn workspace_members_are_audited_and_declared_exclusions_are_honoured() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            concat!(
                "[workspace]\nmembers = [\"crates/*\"]\n\n",
                "[workspace.metadata.cargo-shear]\n",
                "ignored-paths = [\"crates/member/tests/fixtures/**/*.rs\"]\n",
            ),
        );
        write(
            root,
            "crates/member/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.0.0\"\n",
        );
        write(root, "crates/member/src/lib.rs", "");
        write(root, "crates/member/src/detached.rs", "");
        write(root, "crates/member/tests/fixtures/corpus/sample.rs", "");

        let audit = audit_rust(root);
        assert_eq!(
            unmounted_paths(&audit),
            vec!["crates/member/src/detached.rs"]
        );
        assert_eq!(audit.package_count, 1);
        assert_eq!(audit.unmounted[0].package, "member");
        assert_eq!(audit.unmounted[0].manifest, "crates/member/Cargo.toml");
    }

    /// `workspace.exclude` names a directory, and everything under it is out —
    /// an equality check would keep auditing every crate below the excluded
    /// tree.
    #[test]
    fn workspace_exclude_removes_the_whole_subtree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"third_party\"]\n",
        );
        write(
            root,
            "crates/member/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.0.0\"\n",
        );
        write(root, "crates/member/src/lib.rs", "");
        write(
            root,
            "third_party/nested/Cargo.toml",
            "[package]\nname = \"nested\"\nversion = \"0.0.0\"\n",
        );
        write(root, "third_party/nested/src/lib.rs", "");
        write(root, "third_party/nested/src/orphan.rs", "");

        let audit = audit_rust(root);
        assert!(unmounted_paths(&audit).is_empty());
        assert_eq!(audit.package_count, 1);
    }

    /// A crate with its own `[workspace]` nested inside another repository is a
    /// real package. Its declared target may even live outside its directory,
    /// which is how a thin manifest re-targets a binary owned elsewhere.
    #[test]
    fn a_nested_independent_workspace_is_discovered_and_its_out_of_tree_target_mounts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "outer");
        write(root, "src/lib.rs", "");
        write(
            root,
            "sdks/codegen/Cargo.toml",
            concat!(
                "[workspace]\n\n[package]\nname = \"codegen\"\nversion = \"0.0.0\"\n\n",
                "[[bin]]\nname = \"generate\"\npath = \"../../tools/generate.rs\"\n",
            ),
        );
        write(root, "tools/generate.rs", "fn main() {}\n");
        write(
            root,
            "evals/fixture/Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(root, "evals/fixture/src/lib.rs", "pub mod used;\n");
        write(root, "evals/fixture/src/used.rs", "");
        write(root, "evals/fixture/src/loose.rs", "");

        let audit = audit_rust(root);
        assert_eq!(unmounted_paths(&audit), vec!["evals/fixture/src/loose.rs"]);
        assert_eq!(audit.unmounted[0].package, "fixture");
    }

    /// A manifest directory is a claim boundary even when the manifest declares
    /// no target at all. The outer crate must not be blamed for a fixture crate
    /// that lives under its `tests/` directory.
    #[test]
    fn a_manifest_with_no_targets_claims_its_subtree_out_of_scope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "outer");
        write(root, "src/lib.rs", "");
        write(
            root,
            "tests/fixtures/overlay/Cargo.toml",
            "[package]\nname = \"overlay-fixture\"\nversion = \"0.1.0\"\n",
        );
        write(root, "tests/fixtures/overlay/src/auth/login.rs", "");

        let audit = audit_rust(root);
        assert!(unmounted_paths(&audit).is_empty());
        assert_eq!(audit.unclaimed_file_count, 1);
    }

    /// A whole detached subtree reports the highest mounted ancestor rather
    /// than an equally-detached immediate parent.
    #[test]
    fn detached_subtree_climbs_to_the_nearest_mounted_ancestor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "");
        write(root, "src/branch/leaf.rs", "");

        let audit = audit_rust(root);
        assert_eq!(unmounted_paths(&audit), vec!["src/branch/leaf.rs"]);
        assert_eq!(
            audit.unmounted[0].nearest_mounted_parent.as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            audit.unmounted[0].suggested_declaration.as_deref(),
            Some("mod leaf;")
        );
    }

    /// A crate with both a library and a binary has two roots, and the binary's
    /// own module tree is walked from `src/main.rs`.
    #[test]
    fn a_crate_with_both_lib_and_main_walks_both_roots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "pub mod shared;\n");
        write(root, "src/shared.rs", "");
        write(root, "src/main.rs", "mod cli;\nfn main() {}\n");
        write(root, "src/cli.rs", "");
        write(root, "src/bin/extra/main.rs", "mod helper;\nfn main() {}\n");
        write(root, "src/bin/extra/helper.rs", "");

        assert!(unmounted_paths(&audit_rust(root)).is_empty());
    }

    /// A project with no cargo manifest and no Rust files is answered as
    /// absent, not as a clean bill of health.
    #[test]
    fn a_project_without_cargo_reports_the_ecosystem_absent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "README.md", "no rust here\n");

        let audit = audit_rust(root);
        assert_eq!(audit.status.as_str(), "not_present");
        assert!(audit.unmounted.is_empty());
    }

    /// Rust files with no manifest anywhere are counted as unclaimed rather
    /// than reported: nothing declares what would compile them.
    #[test]
    fn rust_files_without_any_manifest_are_unclaimed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "src/whatever.rs", "");

        let audit = audit_rust(root);
        assert_eq!(audit.package_count, 0);
        assert!(audit.unmounted.is_empty());
        assert_eq!(audit.unclaimed_file_count, 1);
    }

    /// The walker does not follow links, so neither does the audit. A module
    /// reached *through* a symlinked directory is mounted under the path the
    /// declaration names, and the files behind the link are never walked — so
    /// nothing on either side of the link is reported as an orphan of the
    /// other.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_source_directory_produces_no_findings_on_either_side() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "mod linked;\n");
        write(root, "external/linked/mod.rs", "mod leaf;\n");
        write(root, "external/linked/leaf.rs", "");
        std::os::unix::fs::symlink(root.join("external/linked"), root.join("src/linked"))
            .expect("symlink");

        let audit = audit_rust(root);
        assert!(
            unmounted_paths(&audit).is_empty(),
            "symlinked tree reported: {:?}",
            unmounted_paths(&audit)
        );
    }

    /// A file whose name is not valid UTF-8 must be answered, not panicked on.
    /// Nothing can declare it, so it is a finding — with a repair line that
    /// admits it cannot spell the module name.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_file_name_is_reported_without_panicking() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        package_manifest(root, "fixture");
        write(root, "src/lib.rs", "");
        std::fs::create_dir_all(root.join("src")).expect("src");
        let invalid = OsStr::from_bytes(b"inv\xFFalid.rs");
        std::fs::write(root.join("src").join(invalid), b"").expect("write");

        let audit = audit_rust(root);
        assert_eq!(audit.unmounted.len(), 1);
        assert_eq!(
            audit.unmounted[0].suggested_declaration.as_deref(),
            Some("mod <module>;")
        );
    }

    #[test]
    fn path_assignments_skip_string_contents() {
        assert_eq!(
            scan_path_assignments("cfg_attr(feature = \"path\", path = \"real.rs\")"),
            vec!["real.rs".to_owned()]
        );
        assert!(scan_path_assignments("cfg_attr(unix, deprecated)").is_empty());
        assert!(scan_path_assignments("cfg_attr(unix, subpath = \"x.rs\")").is_empty());
    }

    /// On a case-insensitive filesystem the declaration's spelling and the
    /// walker's differ; the mounted set has to hold the on-disk one or the file
    /// is reported as an orphan of itself.
    #[test]
    fn on_disk_spelling_resolves_case_variants_but_never_guesses_between_two() {
        let mut listing: HashMap<String, Vec<OsString>> = HashMap::new();
        listing.insert("config.rs".to_owned(), vec![OsString::from("Config.rs")]);
        listing.insert(
            "dual.rs".to_owned(),
            vec![OsString::from("Dual.rs"), OsString::from("dual.rs")],
        );
        assert_eq!(
            on_disk_spelling(&listing, OsStr::new("config.rs")),
            Some(OsString::from("Config.rs"))
        );
        assert_eq!(
            on_disk_spelling(&listing, OsStr::new("dual.rs")),
            Some(OsString::from("dual.rs"))
        );
        assert_eq!(on_disk_spelling(&listing, OsStr::new("DUAL.rs")), None);
        assert_eq!(on_disk_spelling(&listing, OsStr::new("absent.rs")), None);
    }

    #[test]
    fn string_literals_unwrap_plain_and_raw_forms() {
        assert_eq!(
            string_literal_value("\"a/b.rs\""),
            Some("a/b.rs".to_owned())
        );
        assert_eq!(string_literal_value("r\"a.rs\""), Some("a.rs".to_owned()));
        assert_eq!(string_literal_value("r#\"a.rs\"#"), Some("a.rs".to_owned()));
    }
}
