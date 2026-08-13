//! `tracedecay_unmounted_files` — source files on disk that no `mod`
//! declaration reaches from any cargo target root.
//!
//! Rust's compiler only ever sees a file that some other file declares. A
//! `.rs` file nobody declares is invisible to `cargo check`, `cargo clippy`,
//! and every test run — but it is fully visible to a code-graph indexer, which
//! walks the working tree rather than the module tree. That asymmetry is the
//! exact failure this report exists to name: seven files under `src/daemon/`
//! sat in this repository indexed as healthy-looking symbols, with signatures
//! and neighbours and apparent callers, while the compiler had never parsed a
//! line of them. Nothing in the graph could say so, because the graph is built
//! from the filesystem and the truth lives in the module tree.
//!
//! The audit answers the one question that closes that gap: for each workspace
//! crate, which `.rs` files under its own source directories are NOT reachable
//! from its cargo targets by following `mod` declarations?
//!
//! Three deliberate choices keep the answer truthful rather than merely
//! plausible:
//!
//!   - **`#[cfg(...)] mod x;` counts as mounted.** This scan does not evaluate
//!     cfg predicates and must not pretend to. A module gated behind a feature
//!     *is* declared; whether it compiles today is cargo's decision, not this
//!     report's, and treating a gated module as an orphan would flood the
//!     answer with files that are working exactly as intended.
//!   - **Integration tests are many roots, not one.** Each `tests/*.rs` and
//!     each `tests/<name>/main.rs` is its own crate root, and files under
//!     `tests/<name>/` are reachable only through that root's own `mod`
//!     declarations. This is precisely the shape in which the daemon orphans
//!     hid, so the walk models it rather than treating `tests/` as one bag.
//!   - **A file the walk cannot claim is reported, never silently dropped.**
//!     A false positive costs a reader one look; a false negative costs a
//!     release. The two known blind spots (`include!` of a `.rs` file, and a
//!     `mod` declared inside a function body) are stated in the tool
//!     description rather than papered over with a guess.

use std::collections::VecDeque;
use std::path::{Component, PathBuf};

use ignore::overrides::OverrideBuilder;
use tree_sitter::{Node, Parser};

use super::*;

/// Default and ceiling for reported orphans in one response.
///
/// Unlike the paged import scan there is no cursor: the whole module tree must
/// be walked to know that *any* file is unmounted, so a second page would
/// repeat the entire walk for a suffix of the same answer. The response states
/// the true total and how many rows it omitted instead of pretending the
/// returned list is the whole finding.
const UNMOUNTED_FILES_DEFAULT_LIMIT: usize = 200;
const UNMOUNTED_FILES_MAX_LIMIT: usize = 2_000;

/// Directories a cargo package may own source under.
///
/// Deliberately not "everything below the manifest": a crate directory also
/// holds `build.rs`, generated corpora, and support trees that no target ever
/// compiles, and reporting those as orphans would make the answer useless.
/// Anything outside these four directories is out of scope by construction and
/// counted as unclaimed rather than unmounted.
const CARGO_SOURCE_DIRS: [&str; 4] = ["src", "tests", "benches", "examples"];

/// One `mod` declaration read out of a source file, with the directory its
/// candidate files resolve against.
struct ModuleDeclaration {
    name: String,
    /// The `#[path = "..."]` override, when the declaration carries one.
    path_attribute: Option<String>,
    /// The directory this declaration's file candidates are relative to. For a
    /// declaration inside an inline `mod x { ... }` this is already one level
    /// deeper, which is what makes nested inline modules resolve correctly.
    directory: PathBuf,
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

/// One orphaned file and the smallest repair that would mount it.
struct UnmountedFile {
    file: String,
    crate_name: String,
    crate_manifest: String,
    /// The nearest ancestor module file that IS mounted and could declare this
    /// file. `None` means the whole branch is detached, which is a different
    /// and larger finding than one missing `mod` line.
    nearest_mounted_parent: Option<String>,
    suggested_declaration: String,
}

/// The whole audit result, before argument-level filtering and paging.
struct ModuleMountAudit {
    crate_count: usize,
    scanned_file_count: usize,
    mounted_file_count: usize,
    unclaimed_file_count: usize,
    excluded_globs: Vec<String>,
    unmounted: Vec<UnmountedFile>,
}

/// Handles `tracedecay_unmounted_files` tool calls.
pub(crate) async fn handle_unmounted_files(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_unmounted_files")?;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(UNMOUNTED_FILES_DEFAULT_LIMIT, |limit| {
            (limit as usize).clamp(1, UNMOUNTED_FILES_MAX_LIMIT)
        });
    let path_filter = effective_path(&args, scope_prefix).map(str::to_owned);

    let project_root = cg.project_root().to_path_buf();
    // The walk reads every candidate source file, so it runs on a blocking
    // worker rather than holding the async dispatch thread through thousands
    // of synchronous reads.
    let audit = tokio::task::spawn_blocking(move || audit_module_mounts(&project_root))
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("unmounted-file audit did not complete: {error}"),
        })??;

    let matching = audit
        .unmounted
        .iter()
        .filter(|entry| {
            crate::path_scope::path_matches_scope(&entry.file, path_filter.as_deref())
        })
        .collect::<Vec<_>>();
    let unmounted_file_count = matching.len();
    let returned = matching.iter().take(limit).collect::<Vec<_>>();
    let touched_files =
        unique_file_paths(returned.iter().map(|entry| entry.file.as_str()));

    let rows = returned
        .iter()
        .map(|entry| {
            json!({
                "file": entry.file,
                "crate": entry.crate_name,
                "crate_manifest": entry.crate_manifest,
                "nearest_mounted_parent": entry.nearest_mounted_parent,
                "suggested_declaration": entry.suggested_declaration,
            })
        })
        .collect::<Vec<_>>();

    let output = json!({
        "unmounted_file_count": unmounted_file_count,
        "returned_count": rows.len(),
        "omitted_count": unmounted_file_count.saturating_sub(rows.len()),
        "complete": rows.len() == unmounted_file_count,
        "crate_count": audit.crate_count,
        "scanned_file_count": audit.scanned_file_count,
        "mounted_file_count": audit.mounted_file_count,
        "unclaimed_file_count": audit.unclaimed_file_count,
        "excluded_path_globs": audit.excluded_globs,
        "limit": limit,
        "path": path_filter,
        "unmounted": rows,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        touched_files,
        || render::unmounted_files_md(&output),
    ))
}

/// Walks every workspace crate's module tree and reports the files it never
/// reaches.
fn audit_module_mounts(project_root: &Path) -> Result<ModuleMountAudit> {
    let (packages, excluded_globs) = discover_cargo_packages(project_root)?;

    // One mounted set across all packages: a file is mounted if *any* target in
    // the workspace reaches it, and asking per package would report a file
    // twice-owned by a shared directory as an orphan of the package that does
    // not declare it.
    let mut mounted: HashSet<PathBuf> = HashSet::new();
    for package in &packages {
        walk_mounted_files(&package.roots, &mut mounted);
    }

    let excluded = build_excluded_matcher(project_root, &excluded_globs);
    let mut scanned_file_count = 0usize;
    let mut unclaimed_file_count = 0usize;
    let mut unmounted = Vec::new();

    for absolute in project_rust_files(project_root)? {
        let Ok(relative) = absolute.strip_prefix(project_root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if excluded
            .as_ref()
            .is_some_and(|matcher| matcher.matched(&relative, false).is_whitelist())
        {
            continue;
        }
        // Longest-prefix ownership: a package nested under another package's
        // directory owns its own files, and the outer package must not claim
        // them.
        let Some(package) = packages
            .iter()
            .filter(|package| {
                package
                    .source_dirs
                    .iter()
                    .any(|dir| absolute.starts_with(dir))
            })
            .max_by_key(|package| {
                package
                    .source_dirs
                    .iter()
                    .filter(|dir| absolute.starts_with(dir))
                    .map(|dir| dir.as_os_str().len())
                    .max()
                    .unwrap_or(0)
            })
        else {
            unclaimed_file_count += 1;
            continue;
        };
        scanned_file_count += 1;
        if mounted.contains(&absolute) {
            continue;
        }
        let (nearest_mounted_parent, suggested_declaration) =
            repair_for_unmounted_file(project_root, &absolute, &mounted);
        unmounted.push(UnmountedFile {
            file: relative,
            crate_name: package.name.clone(),
            crate_manifest: package.manifest.clone(),
            nearest_mounted_parent,
            suggested_declaration,
        });
    }

    unmounted.sort_by(|left, right| left.file.cmp(&right.file));
    Ok(ModuleMountAudit {
        crate_count: packages.len(),
        scanned_file_count,
        mounted_file_count: mounted.len(),
        unclaimed_file_count,
        excluded_globs,
        unmounted,
    })
}

/// Every `.rs` file in the working tree under the same walk the indexer and
/// `tracedecay_grep` use (`.gitignore` honoured, generated directories and
/// `target/`, `vendor/`, `node_modules/` skipped).
///
/// Reusing that walker rather than restating its policy is the point: a scan
/// that disagreed with the indexer's file set would report findings nothing
/// else in the product can see.
fn project_rust_files(project_root: &Path) -> Result<Vec<PathBuf>> {
    let walk = tracedecay_code_index::source_walk::source_walk(project_root, None).map_err(
        |error| TraceDecayError::Config {
            message: format!("source walk rejected its own scope: {}", error.message),
        },
    )?;
    let mut files = Vec::new();
    for entry in walk {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
        {
            files.push(entry.into_path());
        }
    }
    files.sort();
    Ok(files)
}

/// The globs a workspace has already declared as "not a source-repo target".
///
/// `[workspace.metadata.cargo-shear] ignored-paths` is the existing, reviewed
/// answer to "which `.rs` files under a source directory are deliberately not
/// linked" — fixture corpora, distribution acceptance sources built only by a
/// script. Re-deriving that judgement here would create a second list to keep
/// in sync and would report the same false positives the workspace already
/// wrote down.
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

/// Reads the workspace manifest and returns every package the audit covers,
/// plus the exclusion globs the workspace declared.
fn discover_cargo_packages(project_root: &Path) -> Result<(Vec<CratePackage>, Vec<String>)> {
    let root_manifest_path = project_root.join("Cargo.toml");
    let Ok(root_manifest_text) = std::fs::read_to_string(&root_manifest_path) else {
        // Not a cargo project. An empty roster is the honest answer: the audit
        // reports zero crates rather than erroring at a caller who simply
        // pointed it at a Python repository.
        return Ok((Vec::new(), Vec::new()));
    };
    let Ok(root_manifest) = toml::from_str::<toml::Value>(&root_manifest_text) else {
        return Err(TraceDecayError::Config {
            message: "the workspace Cargo.toml is not valid TOML".to_owned(),
        });
    };

    let excluded_globs = root_manifest
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

    let mut package_dirs: Vec<PathBuf> = Vec::new();
    if root_manifest.get("package").is_some() {
        package_dirs.push(project_root.to_path_buf());
    }
    if let Some(workspace) = root_manifest.get("workspace") {
        let excluded_dirs = workspace
            .get("exclude")
            .and_then(toml::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(|entry| project_root.join(entry))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if let Some(members) = workspace.get("members").and_then(toml::Value::as_array) {
            for member in members.iter().filter_map(toml::Value::as_str) {
                for candidate in expand_member_pattern(project_root, member) {
                    if !excluded_dirs.contains(&candidate) {
                        package_dirs.push(candidate);
                    }
                }
            }
        }
    }
    package_dirs.sort();
    package_dirs.dedup();

    let packages = package_dirs
        .iter()
        .filter_map(|dir| cargo_package(project_root, dir))
        .collect::<Vec<_>>();
    Ok((packages, excluded_globs))
}

/// Expands one `workspace.members` entry into concrete directories.
///
/// Cargo allows `crates/*` here. The expansion is per-segment rather than
/// recursive because cargo's own member globs are not `**` patterns: a member
/// entry names a package directory, not a tree to search.
fn expand_member_pattern(project_root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut candidates = vec![project_root.to_path_buf()];
    for segment in pattern.split('/').filter(|segment| !segment.is_empty()) {
        if !segment.contains('*') && !segment.contains('?') {
            for candidate in &mut candidates {
                candidate.push(segment);
            }
            continue;
        }
        let mut expanded = Vec::new();
        for candidate in &candidates {
            let Ok(entries) = std::fs::read_dir(candidate) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if segment_glob_matches(segment, &name) {
                    expanded.push(entry.path());
                }
            }
        }
        expanded.sort();
        candidates = expanded;
    }
    candidates
        .into_iter()
        .filter(|candidate| candidate.join("Cargo.toml").is_file())
        .collect()
}

/// `*` / `?` matching for one path segment, which is the whole of cargo's
/// member-glob vocabulary that appears in practice.
fn segment_glob_matches(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[u8], name: &[u8]) -> bool {
        match pattern.first() {
            None => name.is_empty(),
            Some(b'*') => {
                (0..=name.len()).any(|split| matches(&pattern[1..], &name[split..]))
            }
            Some(b'?') => !name.is_empty() && matches(&pattern[1..], &name[1..]),
            Some(byte) => name.first() == Some(byte) && matches(&pattern[1..], &name[1..]),
        }
    }
    matches(pattern.as_bytes(), name.as_bytes())
}

/// Reads one package manifest into the target roots and source directories the
/// audit walks. Returns `None` for a manifest with no `[package]` — a pure
/// workspace root owns no targets of its own.
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
        // switched off — that switch disables convention, not declaration.
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

    let manifest = manifest_path
        .strip_prefix(project_root)
        .unwrap_or(&manifest_path)
        .to_string_lossy()
        .replace('\\', "/");

    Some(CratePackage {
        name,
        manifest,
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

/// Breadth-first traversal of the module tree from a package's target roots.
///
/// Every file it reaches is mounted; the closed set it produces is the only
/// thing that makes "unmounted" a fact rather than a heuristic.
fn walk_mounted_files(roots: &[(PathBuf, PathBuf)], mounted: &mut HashSet<PathBuf>) {
    let mut queue: VecDeque<(PathBuf, PathBuf)> = VecDeque::new();
    for (root, directory) in roots {
        if mounted.insert(root.clone()) {
            queue.push_back((root.clone(), directory.clone()));
        }
    }
    while let Some((file, directory)) = queue.pop_front() {
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        for declaration in module_declarations(&source, &directory) {
            for candidate in declaration_candidates(&declaration) {
                if !candidate.is_file() {
                    continue;
                }
                let candidate = normalized(&candidate);
                if mounted.insert(candidate.clone()) {
                    let child_directory = module_child_directory(&candidate);
                    queue.push_back((candidate, child_directory));
                }
                break;
            }
        }
    }
}

/// The two files a `mod name;` declaration may resolve to, or the single file
/// a `#[path = "..."]` override names.
fn declaration_candidates(declaration: &ModuleDeclaration) -> Vec<PathBuf> {
    match declaration.path_attribute.as_deref() {
        Some(path) => vec![declaration.directory.join(path)],
        None => vec![
            declaration.directory.join(format!("{}.rs", declaration.name)),
            declaration.directory.join(&declaration.name).join("mod.rs"),
        ],
    }
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
fn module_declarations(source: &str, directory: &Path) -> Vec<ModuleDeclaration> {
    let Ok(language) = tracedecay_code_extraction::ts_provider::try_language("rust") else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let mut declarations = Vec::new();
    collect_module_declarations(source, tree.root_node(), directory, &mut declarations);
    declarations
}

fn collect_module_declarations(
    source: &str,
    node: Node<'_>,
    directory: &Path,
    out: &mut Vec<ModuleDeclaration>,
) {
    let mut cursor = node.walk();
    // tree-sitter-rust emits attributes as preceding siblings, so the pending
    // `#[path]` has to be carried forward to the item it decorates.
    let mut pending_path: Option<String> = None;
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "attribute_item" => {
                if let Some(value) = path_attribute_value(source, child) {
                    pending_path = Some(value);
                }
            }
            "mod_item" => {
                let path_attribute = pending_path.take();
                let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                    .map(str::to_owned)
                else {
                    continue;
                };
                match child.child_by_field_name("body") {
                    // An inline module declares no file of its own, but its
                    // children resolve under a directory named for it.
                    Some(body) => {
                        let nested = directory
                            .join(path_attribute.as_deref().unwrap_or(name.as_str()));
                        collect_module_declarations(source, body, &nested, out);
                    }
                    None => out.push(ModuleDeclaration {
                        name,
                        path_attribute,
                        directory: directory.to_path_buf(),
                    }),
                }
            }
            _ => pending_path = None,
        }
    }
}

/// The literal of a `#[path = "..."]` attribute, if this attribute is one.
fn path_attribute_value(source: &str, attribute: Node<'_>) -> Option<String> {
    let text = attribute.utf8_text(source.as_bytes()).ok()?;
    let inner = text
        .trim()
        .strip_prefix("#[")?
        .strip_suffix(']')?
        .trim_start();
    let value = inner.strip_prefix("path")?.trim_start().strip_prefix('=')?;
    let value = value.trim();
    let value = value.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(value[..end].to_owned())
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
    let declaration = module_stem(file).map_or_else(
        || "mod <module>;".to_owned(),
        |stem| format!("mod {stem};"),
    );
    let mut current = file.to_path_buf();
    // Bounded by the path depth; the loop always either returns or shortens
    // `current`, and stops at the project root.
    while let Some(parent_module_dir) = parent_module_directory(&current) {
        if !parent_module_dir.starts_with(project_root) {
            break;
        }
        for candidate in parent_module_files(&parent_module_dir) {
            if mounted.contains(&candidate) {
                let relative = candidate
                    .strip_prefix(project_root)
                    .unwrap_or(&candidate)
                    .to_string_lossy()
                    .replace('\\', "/");
                return (Some(relative), declaration);
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

/// Lexical `.`/`..` normalization.
///
/// `#[path = "../shared.rs"]` is legal and produces a path that would never
/// compare equal to the walker's own form of the same file, so both sides are
/// normalized before they meet in the mounted set. Symlinks are deliberately
/// not resolved: the walker does not follow them either, and resolving here
/// would make the two sets disagree again.
fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `contents` to `root/relative`, creating parents.
    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        std::fs::write(path, contents).expect("write fixture file");
    }

    fn audit(root: &Path) -> ModuleMountAudit {
        audit_module_mounts(root).expect("audit")
    }

    fn unmounted_paths(audit: &ModuleMountAudit) -> Vec<&str> {
        audit
            .unmounted
            .iter()
            .map(|entry| entry.file.as_str())
            .collect()
    }

    /// The headline case, in miniature: one file declared from the crate root
    /// and one file that nobody declares.
    #[test]
    fn one_mounted_and_one_orphan_reports_exactly_the_orphan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(root, "src/lib.rs", "pub mod mounted;\n");
        write(root, "src/mounted.rs", "pub fn mounted() {}\n");
        write(root, "src/orphan.rs", "pub fn orphan() {}\n");

        let audit = audit(root);
        assert_eq!(unmounted_paths(&audit), vec!["src/orphan.rs"]);
        let finding = &audit.unmounted[0];
        assert_eq!(finding.crate_name, "fixture");
        assert_eq!(finding.crate_manifest, "Cargo.toml");
        assert_eq!(finding.nearest_mounted_parent.as_deref(), Some("src/lib.rs"));
        assert_eq!(finding.suggested_declaration, "mod orphan;");
    }

    /// `mod.rs` and `name.rs` are both legal spellings of the same module and
    /// both mount their directory's children.
    #[test]
    fn mod_rs_and_name_rs_module_files_both_mount_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(root, "src/lib.rs", "mod dir_form;\nmod file_form;\n");
        write(root, "src/dir_form/mod.rs", "mod leaf;\n");
        write(root, "src/dir_form/leaf.rs", "");
        write(root, "src/file_form.rs", "mod leaf;\n");
        write(root, "src/file_form/leaf.rs", "");

        assert!(unmounted_paths(&audit(root)).is_empty());
    }

    /// `#[path]` relocates a module's file, including out of its own directory.
    #[test]
    fn path_attribute_overrides_conventional_resolution() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(
            root,
            "src/lib.rs",
            "#[path = \"relocated/elsewhere.rs\"]\nmod renamed;\nmod plain;\n",
        );
        write(root, "src/relocated/elsewhere.rs", "");
        write(root, "src/plain.rs", "");
        write(root, "src/relocated/unreferenced.rs", "");

        assert_eq!(
            unmounted_paths(&audit(root)),
            vec!["src/relocated/unreferenced.rs"]
        );
    }

    /// A cfg-gated module is declared; the audit does not evaluate predicates
    /// and must not report its file as an orphan.
    #[test]
    fn cfg_gated_modules_count_as_mounted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(
            root,
            "src/lib.rs",
            "#[cfg(test)]\nmod gated;\n#[cfg(feature = \"never\")]\npub mod featured;\n",
        );
        write(root, "src/gated.rs", "");
        write(root, "src/featured.rs", "");

        assert!(unmounted_paths(&audit(root)).is_empty());
    }

    /// A declaration inside an inline module resolves one directory deeper, and
    /// a comment or string that merely spells `mod` mounts nothing.
    #[test]
    fn inline_modules_nest_and_text_lookalikes_do_not_mount() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
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
            unmounted_paths(&audit(root)),
            vec!["src/commented.rs", "src/stringly.rs"]
        );
    }

    /// The integration-test shape the daemon orphans hid in: every `tests/*.rs`
    /// is its own root, `tests/<name>/main.rs` is a root, and a sibling under
    /// that directory is mounted only by the root's own `mod` line.
    #[test]
    fn integration_test_roots_are_per_file_and_per_suite_main() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(root, "src/lib.rs", "");
        write(root, "tests/standalone.rs", "");
        write(root, "tests/suite/main.rs", "mod declared;\n");
        write(root, "tests/suite/declared.rs", "");
        write(root, "tests/suite/forgotten.rs", "");

        let audit = audit(root);
        assert_eq!(unmounted_paths(&audit), vec!["tests/suite/forgotten.rs"]);
        assert_eq!(
            audit.unmounted[0].nearest_mounted_parent.as_deref(),
            Some("tests/suite/main.rs")
        );
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

        let audit = audit(root);
        assert_eq!(unmounted_paths(&audit), vec!["crates/member/src/detached.rs"]);
        assert_eq!(audit.crate_count, 1);
        assert_eq!(audit.unmounted[0].crate_name, "member");
        assert_eq!(
            audit.unmounted[0].crate_manifest,
            "crates/member/Cargo.toml"
        );
    }

    /// A whole detached subtree reports the highest mounted ancestor rather
    /// than an equally-detached immediate parent.
    #[test]
    fn detached_subtree_climbs_to_the_nearest_mounted_ancestor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\n",
        );
        write(root, "src/lib.rs", "");
        write(root, "src/branch/leaf.rs", "");

        let audit = audit(root);
        assert_eq!(unmounted_paths(&audit), vec!["src/branch/leaf.rs"]);
        assert_eq!(
            audit.unmounted[0].nearest_mounted_parent.as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(audit.unmounted[0].suggested_declaration, "mod leaf;");
    }

    /// A project with no cargo manifest is answered, not rejected.
    #[test]
    fn a_project_without_cargo_reports_zero_crates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "src/whatever.rs", "");

        let audit = audit(root);
        assert_eq!(audit.crate_count, 0);
        assert!(audit.unmounted.is_empty());
        assert_eq!(audit.unclaimed_file_count, 1);
    }

    #[test]
    fn member_globs_match_one_segment_at_a_time() {
        assert!(segment_glob_matches("*", "anything"));
        assert!(segment_glob_matches("tracedecay-*", "tracedecay-api"));
        assert!(!segment_glob_matches("tracedecay-*", "other-api"));
        assert!(segment_glob_matches("crate?", "crates"));
        assert!(!segment_glob_matches("crate?", "crate"));
    }

    #[test]
    fn path_normalization_resolves_parent_traversal() {
        assert_eq!(
            normalized(Path::new("/a/b/../c/./d.rs")),
            PathBuf::from("/a/c/d.rs")
        );
    }
}
