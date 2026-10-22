//! The TypeScript/JavaScript half of the unmounted-file audit.
//!
//! "Unmounted" is a weaker claim here than in cargo, and the report says so
//! rather than borrowing Rust's certainty. Rust has one authority, a file the
//! module tree does not reach is a file the compiler never parses. A TS project
//! has two: the bundler follows imports from an entry point, while `tsc`
//! type-checks everything a `tsconfig` `include` glob matches whether or not
//! anything imports it. Treating `include` as an entry set would make the audit
//! vacuous (`include: ["src"]` mounts the entire tree by definition), so the
//! question asked is the one that actually finds dead weight:
//!
//!   from every entry point this project *declares*, is there a static
//!   `import` / `require` / `export … from` path to this file?
//!
//! A "no" means nothing links the file into a program. It does not mean the
//! type-checker ignores it, and the verdict line and blind-spot list carry that
//! distinction into the response instead of leaving a reader to assume.
//!
//! Entry points are read from the places a project states them: `package.json`
//! (`main`, `module`, `browser`, `types`, `bin`, `exports`, and the file paths
//! named in `scripts`), the string literals in root-level `*.config.*` files
//! (this is how `rsbuild.config.ts`'s `source.entry` and `vitest.config.ts`'s
//! `setupFiles` are found without executing them), `tsconfig` `files`, and the
//! conventional roots a runner discovers on its own, tests, stories, ambient
//! `.d.ts`, `src/index.*`, and the Next.js `app/`+`pages/` route files.
//!
//! Package discovery walks to every `package.json`, exactly as the cargo half
//! walks to every `Cargo.toml`. That covers npm, pnpm, and yarn workspace globs
//! without parsing any of them: a member is a package because it has a
//! manifest, not because a glob mentioned it, and a package the globs forgot is
//! audited anyway.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_code_extraction::{LanguageExtractor, TypeScriptExtractor};
use tracedecay_domain::blank_json_comments;
use tracedecay_domain::errors::Result;
use tree_sitter::Node;

use super::{
    EcosystemAudit, EcosystemStatus, ProjectFiles, UnmountedFile, normalized, relative_display,
};

/// Extensions this audit treats as TypeScript/JavaScript source.
const SOURCE_EXTENSIONS: [&str; 8] = ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Extension order tried when resolving an extensionless specifier, matching
/// the order bundlers and `tsc` use.
const RESOLUTION_EXTENSIONS: [&str; 9] =
    ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "d.ts"];

const TYPESCRIPT_VERDICT: &str = "no static import, require, or export-from path reaches this file from any declared entry \
     point, nothing links it into a program (`tsc` may still type-check it via a tsconfig \
     `include`)";

const TYPESCRIPT_BLIND_SPOTS: [&str; 6] = [
    "a dynamic `import(expr)` or `require(expr)` whose specifier is not a literal is not \
     followed, so a file reached only that way reads as unmounted",
    "aliases declared outside tsconfig `paths`, webpack/vite/rspack `resolve.alias`, jest \
     `moduleNameMapper`, are not resolved",
    "glob imports (`import.meta.glob`, `require.context`) and plugin-generated virtual modules \
     are not expanded",
    "a file reached only from HTML, CSS, a JSON manifest, or a runtime string is not seen",
    "single-file component formats (`.vue`, `.svelte`, `.astro`) are not parsed for imports, so \
     files imported only from one of them read as unmounted",
    "an entry point named by a config file is found by reading that file's string literals, not \
     by executing it: an entry computed at config-evaluation time is missed",
];

/// One npm package and everything the audit needs to judge its files.
struct NodePackage {
    name: String,
    dir: PathBuf,
    /// Project-relative manifest path.
    manifest: String,
    /// Files a runner, bundler, or type-checker starts from.
    entries: Vec<PathBuf>,
    /// `compilerOptions.paths` rules, already anchored to a base directory.
    aliases: Vec<AliasRule>,
    /// `compilerOptions.baseUrl` directories, for bare specifiers.
    base_urls: Vec<PathBuf>,
    coverage: AuditCoverage,
}

#[derive(Default)]
struct AuditCoverage {
    omissions: BTreeSet<String>,
}

impl AuditCoverage {
    fn omit(&mut self, root: &Path, path: &Path, reason: impl std::fmt::Display) {
        self.omissions
            .insert(format!("{}: {reason}", relative_display(root, path)));
    }

    fn read(
        &mut self,
        root: &Path,
        path: &Path,
        reader: &impl Fn(&Path) -> std::io::Result<String>,
    ) -> Option<String> {
        match reader(path) {
            Ok(source) => Some(source),
            Err(error) => {
                self.omit(root, path, error);
                None
            }
        }
    }

    fn json(
        &mut self,
        root: &Path,
        path: &Path,
        reader: &impl Fn(&Path) -> std::io::Result<String>,
        comments: bool,
    ) -> Option<Value> {
        let source = self.read(root, path, reader)?;
        let source = if comments {
            blank_json_comments(&source)
        } else {
            source
        };
        match serde_json::from_str::<Value>(&source) {
            Ok(value) if value.is_object() => Some(value),
            Ok(_) => {
                self.omit(root, path, "expected a JSON object");
                None
            }
            Err(error) => {
                self.omit(root, path, error);
                None
            }
        }
    }
}

/// One `compilerOptions.paths` entry: `"@app/*": ["src/*"]`.
struct AliasRule {
    /// The text before `*`, or the whole pattern when it has no `*`.
    prefix: String,
    /// The text after `*`, empty when the pattern has no `*`.
    suffix: String,
    wildcard: bool,
    /// Target templates, already joined onto their base directory.
    targets: Vec<PathBuf>,
}

impl AliasRule {
    /// The paths `specifier` expands to under this rule, if it matches.
    fn expand(&self, specifier: &str) -> Vec<PathBuf> {
        if !self.wildcard {
            if specifier != self.prefix {
                return Vec::new();
            }
            return self.targets.clone();
        }
        let Some(rest) = specifier.strip_prefix(&self.prefix) else {
            return Vec::new();
        };
        let Some(matched) = rest.strip_suffix(&self.suffix) else {
            return Vec::new();
        };
        self.targets
            .iter()
            .map(|target| PathBuf::from(target.to_string_lossy().replace('*', matched)))
            .collect()
    }
}

/// Walks every discovered npm package's import graph and reports the source
/// files no entry point reaches.
pub(super) fn audit(files: &ProjectFiles) -> Result<EcosystemAudit> {
    Ok(audit_with_reader(files, &|path| {
        std::fs::read_to_string(path)
    }))
}

fn audit_with_reader(
    files: &ProjectFiles,
    reader: &impl Fn(&Path) -> std::io::Result<String>,
) -> EcosystemAudit {
    let manifest_paths = files.named("package.json");
    let source_files = files.with_extensions(&SOURCE_EXTENSIONS);
    if manifest_paths.is_empty() && source_files.is_empty() {
        return EcosystemAudit::not_present("typescript", TYPESCRIPT_VERDICT);
    }

    let owned_files = source_files.iter().copied().collect::<HashSet<&Path>>();
    let package_dirs = manifest_paths
        .iter()
        .filter_map(|manifest| manifest.parent())
        .map(normalized)
        .collect::<Vec<_>>();
    let package_indices = package_dirs
        .iter()
        .enumerate()
        .map(|(index, dir)| (dir.as_path(), index))
        .collect::<HashMap<_, _>>();
    let owners = source_files
        .iter()
        .map(|file| {
            file.ancestors()
                .find_map(|dir| package_indices.get(dir).copied())
        })
        .collect::<Vec<_>>();
    let mut package_files = vec![Vec::new(); package_dirs.len()];
    for (&file, owner) in source_files.iter().zip(&owners) {
        if let Some(index) = owner {
            package_files[*index].push(file);
        }
    }
    let mut packages = package_dirs
        .iter()
        .zip(&package_files)
        .map(|(dir, sources)| node_package(files, dir, &owned_files, sources, reader))
        .collect::<Vec<_>>();

    let entry_point_count = packages.iter().map(|package| package.entries.len()).sum();
    let mounted = packages
        .iter_mut()
        .flat_map(|package| walk_imports(files.root(), package, &owned_files, reader))
        .collect::<HashSet<_>>();
    let omissions = packages
        .iter()
        .flat_map(|package| {
            package
                .coverage
                .omissions
                .iter()
                .map(|reason| format!("{}: {reason}", package.manifest))
        })
        .collect::<Vec<_>>();

    let mut scanned_file_count = 0usize;
    let mut unclaimed_file_count = 0usize;
    let mut unmounted = Vec::new();
    for (absolute, owner) in source_files.iter().zip(owners) {
        let Some(index) = owner else {
            unclaimed_file_count += 1;
            continue;
        };
        let package = &packages[index];
        scanned_file_count += 1;
        if mounted.contains(&normalized(absolute)) || !omissions.is_empty() {
            continue;
        }
        unmounted.push(UnmountedFile {
            file: files.relative(absolute),
            package: package.name.clone(),
            manifest: package.manifest.clone(),
            // An unimported file has no "nearest mounted parent" to repair
            // against, and no single import line would be the right fix, the
            // file is either dead or reached through a blind spot. Inventing a
            // suggestion here would invent a caller.
            nearest_mounted_parent: None,
            suggested_declaration: None,
        });
    }

    unmounted.sort_by(|left, right| left.file.cmp(&right.file));
    EcosystemAudit {
        ecosystem: "typescript",
        status: if omissions.is_empty() {
            EcosystemStatus::Audited
        } else {
            EcosystemStatus::Partial
        },
        package_count: packages.len(),
        entry_point_count,
        scanned_file_count,
        mounted_file_count: mounted.len(),
        unclaimed_file_count,
        verdict: TYPESCRIPT_VERDICT,
        blind_spots: TYPESCRIPT_BLIND_SPOTS.to_vec(),
        note: (!omissions.is_empty()).then(|| {
            format!(
                "TypeScript negative reachability findings withheld because missing inputs may reference any package: {}",
                omissions.join("; ")
            )
        }),
        excluded_globs: Vec::new(),
        unmounted,
    }
}

/// Reads one `package.json` (and the tsconfigs and config files beside it) into
/// the entry points and alias rules the walk needs.
#[tracing::instrument(
    name = "code_index.unmounted_files.ts.package",
    level = "trace",
    skip_all
)]
fn node_package(
    files: &ProjectFiles,
    dir: &Path,
    owned: &HashSet<&Path>,
    sources: &[&Path],
    reader: &impl Fn(&Path) -> std::io::Result<String>,
) -> NodePackage {
    let project_root = files.root();
    let mut coverage = AuditCoverage::default();
    let manifest_path = dir.join("package.json");
    let manifest = coverage.json(project_root, &manifest_path, reader, false);
    let name = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("name"))
        .and_then(Value::as_str)
        .map_or_else(
            || {
                dir.file_name().map_or_else(
                    || "<unnamed>".to_owned(),
                    |name| name.to_string_lossy().into_owned(),
                )
            },
            str::to_owned,
        );

    let mut specifiers: Vec<String> = Vec::new();
    if let Some(manifest) = manifest.as_ref() {
        for key in ["main", "module", "browser", "types", "typings", "source"] {
            if let Some(value) = manifest.get(key).and_then(Value::as_str) {
                specifiers.push(value.to_owned());
            }
        }
        collect_string_leaves(manifest.get("bin"), &mut specifiers);
        collect_string_leaves(manifest.get("exports"), &mut specifiers);
        // `scripts` names the files a repository actually runs, `tsx
        // codegen/src/cli.ts generate` is an entry point in every sense that
        // matters, and it is nowhere else in the manifest.
        if let Some(scripts) = manifest.get("scripts").and_then(Value::as_object) {
            for command in scripts.values().filter_map(Value::as_str) {
                specifiers.extend(command.split_whitespace().map(str::to_owned));
            }
        }
    }

    let mut entries: BTreeSet<PathBuf> = BTreeSet::new();
    for specifier in &specifiers {
        let mut resolved = resolve_relative(dir, specifier)
            .into_iter()
            .filter(|candidate| owned.contains(candidate.as_path()))
            .collect::<Vec<_>>();
        // A published manifest points at build output (`./dist/client.js`)
        // that no source walk can see. The source behind it is the same path
        // rooted at `src/`, and without that mapping every entry of every
        // published library reads as unresolved and its whole tree as dead.
        if resolved.is_empty() {
            resolved = source_behind_build_output(dir, specifier)
                .into_iter()
                .filter(|candidate| owned.contains(candidate.as_path()))
                .collect();
        }
        entries.extend(resolved);
    }

    let (aliases, base_urls, tsconfig_files) =
        tsconfig_declarations(files, dir, reader, &mut coverage);
    for file in tsconfig_files {
        for resolved in resolve_relative(dir, &file) {
            if owned.contains(resolved.as_path()) {
                entries.insert(resolved);
            }
        }
    }

    // Config files are loaded by tooling, and the paths they name are entry
    // points that appear nowhere else. Reading their string literals finds
    // those without executing anyone's config.
    for candidate in sources
        .iter()
        .filter(|file| file.parent() == Some(dir) && is_config_file(file))
    {
        entries.insert((*candidate).to_path_buf());
        let Some(source) = coverage.read(project_root, candidate, reader) else {
            continue;
        };
        let Some(literals) = config_string_literals(candidate, &source) else {
            coverage.omit(
                project_root,
                candidate,
                "TypeScript config could not be parsed without syntax errors",
            );
            continue;
        };
        for literal in literals {
            for resolved in resolve_relative(dir, &literal) {
                if owned.contains(resolved.as_path()) {
                    entries.insert(resolved);
                }
            }
        }
    }

    // Roots a runner discovers by convention rather than by declaration,
    // only this package's own, never a nested package's.
    for candidate in sources
        .iter()
        .filter(|file| is_conventional_entry(dir, file))
    {
        entries.insert((*candidate).to_path_buf());
    }

    NodePackage {
        name,
        dir: dir.to_path_buf(),
        manifest: relative_display(project_root, &manifest_path),
        entries: entries.into_iter().collect(),
        aliases,
        base_urls,
        coverage,
    }
}

/// Every string value anywhere inside a JSON value, how `exports` and `bin`
/// name files without a fixed shape.
fn collect_string_leaves(value: Option<&Value>, out: &mut Vec<String>) {
    match value {
        Some(Value::String(text)) => out.push(text.clone()),
        Some(Value::Array(items)) => {
            for item in items {
                collect_string_leaves(Some(item), out);
            }
        }
        Some(Value::Object(fields)) => {
            for field in fields.values() {
                collect_string_leaves(Some(field), out);
            }
        }
        _ => {}
    }
}

/// A file tooling loads on sight: `vite.config.ts`, `postcss.config.mjs`,
/// `eslint.config.js`, and friends.
fn is_config_file(file: &Path) -> bool {
    let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.contains(".config.") || name.contains(".setup.")
}

/// Roots a test runner, a framework, or the type system starts from without
/// anyone declaring them.
fn is_conventional_entry(package_dir: &Path, file: &Path) -> bool {
    let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // Ambient declarations are never imported and are always in the program.
    if name.ends_with(".d.ts") {
        return true;
    }
    // Test, story, and benchmark files are discovered by their runner.
    for marker in [".test.", ".spec.", ".stories.", ".bench.", ".story."] {
        if name.contains(marker) {
            return true;
        }
    }
    let Ok(relative) = file.strip_prefix(package_dir) else {
        return false;
    };
    let segments = relative
        .iter()
        .map(|segment| segment.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| matches!(segment.as_str(), "__tests__" | "stories" | "__stories__"))
    {
        return true;
    }
    let stem = name.split('.').next().unwrap_or_default();
    match segments.as_slice() {
        // `index.*` / `main.*` at the package root or directly under `src/`.
        [only] => matches!(only.split('.').next(), Some("index" | "main")),
        [first, _] if first == "src" => matches!(stem, "index" | "main"),
        _ => {
            // Next.js routes: every `page`/`layout`/`route`/… file under
            // `app/` or `pages/` (at the package root or under `src/`) is an
            // entry the framework mounts itself. The directory test is narrow
            // on purpose, treating any `src/**/page.tsx` as an entry would
            // silently mount a plain component in a router-less app.
            let router_root = match segments.as_slice() {
                [first, ..] if first == "app" || first == "pages" => true,
                [first, second, ..] if first == "src" => second == "app" || second == "pages",
                _ => false,
            };
            router_root
                && matches!(
                    stem,
                    "page"
                        | "layout"
                        | "route"
                        | "loading"
                        | "error"
                        | "template"
                        | "default"
                        | "middleware"
                        | "instrumentation"
                )
        }
    }
}

/// `compilerOptions.paths` rules, `baseUrl` directories, and `files` entries
/// from every `tsconfig*.json` beside the manifest.
fn tsconfig_declarations(
    inventory: &ProjectFiles,
    dir: &Path,
    reader: &impl Fn(&Path) -> std::io::Result<String>,
    coverage: &mut AuditCoverage,
) -> (Vec<AliasRule>, Vec<PathBuf>, Vec<String>) {
    let mut aliases = Vec::new();
    let mut base_urls = Vec::new();
    let mut files = Vec::new();
    let configs = inventory.files.iter().filter(|path| {
        path.parent() == Some(dir)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tsconfig") && name.ends_with(".json"))
    });
    for config in configs {
        let Some(parsed) = coverage.json(inventory.root(), config, reader, true) else {
            continue;
        };
        let options = parsed.get("compilerOptions");
        let base = options
            .and_then(|options| options.get("baseUrl"))
            .and_then(Value::as_str)
            .map_or_else(|| dir.to_path_buf(), |base| normalized(&dir.join(base)));
        if options.and_then(|options| options.get("baseUrl")).is_some() {
            base_urls.push(base.clone());
        }
        if let Some(paths) = options
            .and_then(|options| options.get("paths"))
            .and_then(Value::as_object)
        {
            for (pattern, targets) in paths {
                let mut target_paths = Vec::new();
                collect_string_leaves(Some(targets), &mut target_paths);
                let targets = target_paths
                    .iter()
                    .map(|target| normalized(&base.join(target)))
                    .collect::<Vec<_>>();
                match pattern.split_once('*') {
                    Some((prefix, suffix)) => aliases.push(AliasRule {
                        prefix: prefix.to_owned(),
                        suffix: suffix.to_owned(),
                        wildcard: true,
                        targets,
                    }),
                    None => aliases.push(AliasRule {
                        prefix: pattern.clone(),
                        suffix: String::new(),
                        wildcard: false,
                        targets,
                    }),
                }
            }
        }
        if let Some(declared) = parsed.get("files").and_then(Value::as_array) {
            files.extend(declared.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    (aliases, base_urls, files)
}

/// Every static string literal in a config file's syntax tree.
fn config_string_literals(file: &Path, source: &str) -> Option<Vec<String>> {
    let tree = parse_typescript(file, source)?;
    if tree.root_node().has_error() {
        return None;
    }
    let mut out = Vec::new();
    collect_config_string_literals(source, tree.root_node(), &mut out);
    Some(out)
}

fn collect_config_string_literals(source: &str, node: Node<'_>, out: &mut Vec<String>) {
    match node.kind() {
        "string" => {
            if let Some(literal) = node.utf8_text(source.as_bytes()).ok().and_then(unquote) {
                out.push(literal);
            }
            return;
        }
        "template_string" => {
            let mut cursor = node.walk();
            let is_static = node
                .named_children(&mut cursor)
                .all(|child| child.kind() != "template_substitution");
            if is_static
                && let Some(literal) = node.utf8_text(source.as_bytes()).ok().and_then(unquote)
            {
                out.push(literal);
            }
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_config_string_literals(source, child, out);
    }
}

/// Breadth-first traversal of one package's import graph from its entry points.
#[tracing::instrument(
    name = "code_index.unmounted_files.ts.import_walk",
    level = "trace",
    skip_all
)]
fn walk_imports(
    root: &Path,
    package: &mut NodePackage,
    owned: &HashSet<&Path>,
    reader: &impl Fn(&Path) -> std::io::Result<String>,
) -> HashSet<PathBuf> {
    let mut mounted = HashSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    for entry in &package.entries {
        let entry = normalized(entry);
        if mounted.insert(entry.clone()) {
            queue.push_back(entry);
        }
    }
    while let Some(file) = queue.pop_front() {
        let Some(source) = package.coverage.read(root, &file, reader) else {
            continue;
        };
        for specifier in module_specifiers(root, &file, &source, &mut package.coverage) {
            for target in resolve_specifier(package, &file, &specifier) {
                if owned.contains(target.as_path()) && mounted.insert(target.clone()) {
                    queue.push_back(target);
                }
            }
        }
    }
    mounted
}

/// Every module specifier one file names.
///
/// `import` statements, `export … from`, and `require(…)` come from the same
/// extractor the code graph is built from, so the audit and the graph cannot
/// disagree about those forms. Dynamic `import(…)` is still read off the
/// tree-sitter grammar because the extractor does not emit that form as
/// import evidence today.
fn module_specifiers(
    root: &Path,
    file: &Path,
    source: &str,
    coverage: &mut AuditCoverage,
) -> Vec<String> {
    let logical_path = file.to_string_lossy().into_owned();
    let mut specifiers = TypeScriptExtractor
        .extract_artifact(&logical_path, source)
        .imports
        .into_iter()
        .map(|evidence| evidence.module_specifier)
        .collect::<Vec<_>>();

    match parse_typescript(file, source) {
        Some(tree) => {
            if tree.root_node().has_error() {
                coverage.omit(root, file, "source contains syntax errors");
            }
            collect_reexports_and_calls(source, tree.root_node(), &mut specifiers);
        }
        None => coverage.omit(root, file, "source parser unavailable"),
    }
    specifiers.sort();
    specifiers.dedup();
    specifiers
}

fn parse_typescript(file: &Path, source: &str) -> Option<tree_sitter::Tree> {
    let key = match file.extension().and_then(|ext| ext.to_str()) {
        Some("tsx") => "tsx",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        _ => "typescript",
    };
    let language = tracedecay_code_extraction::ts_provider::try_language(key).ok()?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    parser.parse(source, None)
}

fn collect_reexports_and_calls(source: &str, node: Node<'_>, out: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "export_statement" | "import_require_clause" => {
                if let Some(text) = child
                    .child_by_field_name("source")
                    .and_then(|source_node| source_node.utf8_text(source.as_bytes()).ok())
                    && let Some(literal) = unquote(text)
                {
                    out.push(literal);
                }
            }
            "call_expression" => {
                let callee = child
                    .child_by_field_name("function")
                    .and_then(|callee| callee.utf8_text(source.as_bytes()).ok())
                    .unwrap_or_default();
                if matches!(callee, "require" | "import")
                    && let Some(argument) = child
                        .child_by_field_name("arguments")
                        .and_then(|arguments| arguments.named_child(0))
                    && let Some(literal) =
                        argument.utf8_text(source.as_bytes()).ok().and_then(unquote)
                {
                    out.push(literal);
                }
            }
            _ => {}
        }
        collect_reexports_and_calls(source, child, out);
    }
}

fn unquote(text: &str) -> Option<String> {
    let text = text.trim();
    for quote in ['"', '\'', '`'] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return Some(inner.to_owned());
        }
    }
    None
}

/// The files a specifier may name, from the importing file's position.
fn resolve_specifier(package: &NodePackage, from: &Path, specifier: &str) -> Vec<PathBuf> {
    // `./x?raw`, `./x?url`, bundler query suffixes name the same file.
    let specifier = specifier.split(['?', '#']).next().unwrap_or(specifier);
    if specifier.is_empty() {
        return Vec::new();
    }
    // `.`, `..`, `./x`, `../x`, every form that names a sibling rather than a
    // package. A bare `.` is a directory import and resolves to its `index`.
    if matches!(specifier, "." | "..")
        || specifier.starts_with("./")
        || specifier.starts_with("../")
    {
        let base = from.parent().unwrap_or(&package.dir);
        return resolve_relative(base, specifier);
    }
    let mut resolved = Vec::new();
    for alias in &package.aliases {
        for candidate in alias.expand(specifier) {
            resolved.extend(resolve_existing(&candidate));
        }
    }
    if resolved.is_empty() {
        for base in &package.base_urls {
            resolved.extend(resolve_existing(&base.join(specifier)));
        }
    }
    resolved
}

/// The source file behind a build-output specifier: `./dist/client.js` is
/// written by `./src/client.ts`.
///
/// Only ever tried after the literal path failed to resolve to a walked file,
/// so it cannot override a real one.
fn source_behind_build_output(package_dir: &Path, specifier: &str) -> Vec<PathBuf> {
    const OUTPUT_DIRS: [&str; 7] = ["dist", "lib", "build", "out", "es", "esm", "cjs"];
    let trimmed = specifier.trim_start_matches("./");
    let Some((head, rest)) = trimmed.split_once('/') else {
        return Vec::new();
    };
    if !OUTPUT_DIRS.contains(&head) {
        return Vec::new();
    }
    resolve_existing(&package_dir.join("src").join(rest))
}

/// Resolves one relative specifier against `base`.
fn resolve_relative(base: &Path, specifier: &str) -> Vec<PathBuf> {
    if specifier.trim_start_matches("./").is_empty() {
        // `./` and `` both name the directory itself, whose `index` is the
        // module being imported.
        return resolve_existing(base);
    }
    resolve_existing(&base.join(specifier))
}

/// Node/TypeScript file resolution for one candidate path: the file itself, the
/// TypeScript source behind a `.js` specifier, an added extension, or the
/// directory's `index`.
fn resolve_existing(candidate: &Path) -> Vec<PathBuf> {
    let candidate = normalized(candidate);
    let mut out = Vec::new();
    if candidate.is_file() && has_source_extension(&candidate) {
        out.push(candidate.clone());
    }
    // `import './x.js'` in an ESM TypeScript project names `x.ts` on disk.
    if names_javascript(&candidate) {
        for extension in ["ts", "tsx", "mts", "cts"] {
            let rewritten = candidate.with_extension(extension);
            if rewritten.is_file() {
                out.push(rewritten);
            }
        }
    }
    for extension in RESOLUTION_EXTENSIONS {
        let with_extension = PathBuf::from(format!("{}.{extension}", candidate.to_string_lossy()));
        if with_extension.is_file() {
            out.push(with_extension);
        }
    }
    for extension in RESOLUTION_EXTENSIONS {
        let index = candidate.join(format!("index.{extension}"));
        if index.is_file() {
            out.push(index);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn has_source_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SOURCE_EXTENSIONS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
}

/// Whether the specifier names a JavaScript file, whose TypeScript source may
/// be what is actually on disk.
fn names_javascript(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "js" | "jsx" | "mjs" | "cjs"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::super::tests::{project, write};
    use super::{AliasRule, EcosystemAudit, audit, audit_with_reader};

    fn audit_typescript(root: &Path) -> EcosystemAudit {
        audit(&project(root)).expect("TypeScript audit")
    }

    fn unmounted_paths(audit: &EcosystemAudit) -> Vec<&str> {
        audit
            .unmounted
            .iter()
            .map(|entry| entry.file.as_str())
            .collect()
    }

    #[test]
    fn malformed_manifest_withholds_findings_across_package_boundaries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "broken/package.json", "{");
        write(root, "broken/src/index.ts", "import './used';");
        write(root, "broken/src/used.ts", "export const used = 1;");
        write(root, "broken/src/unknown.ts", "export const unknown = 1;");
        write(root, "healthy/package.json", r#"{"name":"healthy"}"#);
        write(root, "healthy/src/index.ts", "export const entry = 1;");
        write(root, "healthy/src/orphan.ts", "export const orphan = 1;");

        let audit = audit_typescript(root);
        assert_eq!(audit.status.as_str(), "partial");
        assert_eq!(unmounted_paths(&audit), Vec::<&str>::new());
        assert_eq!(audit.mounted_file_count, 3);
        let note = audit.note.expect("missing-input explanation");
        assert!(note.contains("broken/package.json"), "{note}");
        assert!(note.contains("EOF"), "{note}");
        assert!(!note.contains(&root.display().to_string()), "{note}");
    }

    #[test]
    fn unreadable_inputs_withhold_findings_and_preserve_proven_mounts() {
        for unreadable in [
            "package.json",
            "tsconfig.json",
            "src/index.ts",
            "src/intermediate.ts",
            "rsbuild.config.ts",
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path();
            write(root, "broken/package.json", r#"{"name":"broken"}"#);
            write(root, "broken/tsconfig.json", "{}");
            write(root, "broken/rsbuild.config.ts", "export default {};");
            write(root, "broken/src/index.ts", "import './intermediate';");
            write(root, "broken/src/intermediate.ts", "import './descendant';");
            write(root, "broken/src/descendant.ts", "export const used = 1;");
            write(root, "broken/src/orphan.ts", "export const orphan = 1;");
            write(root, "healthy/package.json", r#"{"name":"healthy"}"#);
            write(root, "healthy/src/index.ts", "export const entry = 1;");
            write(root, "healthy/src/orphan.ts", "export const orphan = 1;");
            let failed_path = root.join("broken").join(unreadable);
            let audit = audit_with_reader(&project(root), &|path| {
                if path == failed_path {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "fixture read denied",
                    ))
                } else {
                    std::fs::read_to_string(path)
                }
            });
            assert_eq!(audit.status.as_str(), "partial", "{unreadable}");
            assert_eq!(unmounted_paths(&audit), Vec::<&str>::new(), "{unreadable}");
            let expected_mounted = match unreadable {
                "src/index.ts" => 3,
                "src/intermediate.ts" => 4,
                _ => 5,
            };
            assert_eq!(audit.mounted_file_count, expected_mounted, "{unreadable}");
            let note = audit.note.expect("read omission");
            assert!(note.contains(&format!("broken/{unreadable}")), "{note}");
            assert!(note.contains("fixture read denied"), "{note}");
            assert!(!note.contains(&root.display().to_string()), "{note}");
        }
    }

    #[test]
    fn unreadable_entry_declarations_withhold_unknown_cross_package_targets() {
        for (declaration, contents) in [
            (
                "package.json",
                r#"{"name":"app","main":"../shared/src/used.ts"}"#,
            ),
            ("tsconfig.json", r#"{"files":["../shared/src/used.ts"]}"#),
            (
                "rsbuild.config.ts",
                "export default { entry: '../shared/src/used.ts' };",
            ),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path();
            write(root, "app/package.json", r#"{"name":"app"}"#);
            write(root, &format!("app/{declaration}"), contents);
            write(root, "shared/package.json", r#"{"name":"shared"}"#);
            write(root, "shared/src/used.ts", "export const used = 1;");
            write(root, "shared/src/orphan.ts", "export const orphan = 1;");

            let complete = audit_typescript(root);
            assert_eq!(complete.status.as_str(), "audited", "{declaration}");
            assert_eq!(
                unmounted_paths(&complete),
                vec!["shared/src/orphan.ts"],
                "{declaration}"
            );

            let failed_path = root.join("app").join(declaration);
            let partial = audit_with_reader(&project(root), &|path| {
                if path == failed_path {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "fixture read denied",
                    ))
                } else {
                    std::fs::read_to_string(path)
                }
            });
            assert_eq!(partial.status.as_str(), "partial", "{declaration}");
            assert_eq!(
                unmounted_paths(&partial),
                Vec::<&str>::new(),
                "{declaration}"
            );
            let note = partial.note.expect("read omission");
            assert!(note.contains(&format!("app/{declaration}")), "{note}");
        }
    }

    #[test]
    fn unreadable_importer_withholds_unknown_cross_package_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "app/package.json", r#"{"name":"app"}"#);
        write(root, "app/src/index.ts", "import '../../shared/src/used';");
        write(root, "shared/package.json", r#"{"name":"shared"}"#);
        write(root, "shared/src/used.ts", "export const used = 1;");
        write(root, "shared/src/orphan.ts", "export const orphan = 1;");

        let complete = audit_typescript(root);
        assert_eq!(complete.status.as_str(), "audited");
        assert_eq!(unmounted_paths(&complete), vec!["shared/src/orphan.ts"]);
        assert_eq!(complete.mounted_file_count, 2);

        let failed_path = root.join("app/src/index.ts");
        let partial = audit_with_reader(&project(root), &|path| {
            if path == failed_path {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fixture read denied",
                ))
            } else {
                std::fs::read_to_string(path)
            }
        });
        assert_eq!(partial.status.as_str(), "partial");
        assert!(
            partial.unmounted.is_empty(),
            "unknown outgoing imports cannot prove these files unmounted: {:?}",
            unmounted_paths(&partial)
        );
        assert_eq!(partial.mounted_file_count, 1);
        let note = partial.note.expect("read omission");
        assert!(note.contains("app/src/index.ts"), "{note}");
        assert!(note.contains("fixture read denied"), "{note}");
    }

    #[test]
    fn unreadable_cross_package_source_withholds_unknown_descendants() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "app/package.json", r#"{"name":"app"}"#);
        write(
            root,
            "app/src/index.ts",
            "import '../../shared/src/intermediate';",
        );
        write(root, "shared/package.json", r#"{"name":"shared"}"#);
        write(root, "shared/src/intermediate.ts", "import './descendant';");
        write(root, "shared/src/descendant.ts", "export const used = 1;");
        write(root, "healthy/package.json", r#"{"name":"healthy"}"#);
        write(root, "healthy/src/index.ts", "export const entry = 1;");
        write(root, "healthy/src/orphan.ts", "export const orphan = 1;");
        let failed_path = root.join("shared/src/intermediate.ts");
        let audit = audit_with_reader(&project(root), &|path| {
            if path == failed_path {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "fixture read denied",
                ))
            } else {
                std::fs::read_to_string(path)
            }
        });
        assert_eq!(audit.status.as_str(), "partial");
        assert_eq!(unmounted_paths(&audit), Vec::<&str>::new());
        assert_eq!(audit.mounted_file_count, 3);
    }

    #[test]
    fn malformed_tsconfig_and_source_withhold_negative_findings() {
        for (path, contents) in [
            ("tsconfig.json", "{"),
            ("src/index.ts", "import './used'; export const broken = ;"),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path();
            write(root, "package.json", r#"{"name":"broken"}"#);
            write(root, "src/index.ts", "import './used';");
            write(root, "src/used.ts", "export const used = 1;");
            write(root, "src/orphan.ts", "export const orphan = 1;");
            write(root, path, contents);
            let audit = audit_typescript(root);
            assert_eq!(audit.status.as_str(), "partial", "{path}");
            assert!(audit.unmounted.is_empty(), "{path}");
            assert_eq!(audit.mounted_file_count, 2, "{path}");
            let note = audit.note.expect("parse omission");
            assert!(note.contains(path), "{note}");
        }
    }

    #[test]
    fn missing_optional_tsconfig_is_a_complete_audit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "package.json", r#"{"name":"complete"}"#);
        write(root, "src/index.ts", "export const entry = 1;");
        write(root, "src/orphan.ts", "export const orphan = 1;");
        let audit = audit_typescript(root);
        assert_eq!(audit.status.as_str(), "audited");
        assert_eq!(unmounted_paths(&audit), vec!["src/orphan.ts"]);
        assert!(audit.note.is_none());
    }

    /// The headline case: one module imported from the declared entry, one that
    /// nothing imports.
    #[test]
    fn a_declared_main_mounts_its_import_graph_and_nothing_else() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            "{\"name\":\"app\",\"main\":\"./src/index.ts\"}",
        );
        write(
            root,
            "src/index.ts",
            "import { used } from './used';\nexport { used };\n",
        );
        write(root, "src/used.ts", "export const used = 1;\n");
        write(root, "src/orphan.ts", "export const orphan = 1;\n");

        let audit = audit_typescript(root);
        assert_eq!(unmounted_paths(&audit), vec!["src/orphan.ts"]);
        assert_eq!(audit.unmounted[0].package, "app");
        assert_eq!(audit.unmounted[0].manifest, "package.json");
        // No repair line is invented for a file nothing imports.
        assert!(audit.unmounted[0].suggested_declaration.is_none());
    }

    /// `export … from`, `require(…)`, and a literal dynamic `import(…)` are all
    /// real edges; missing any of them would report a live file as dead.
    #[test]
    fn reexports_requires_and_literal_dynamic_imports_all_mount() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            "{\"name\":\"app\",\"exports\":{\".\":{\"import\":\"./src/index.ts\"}}}",
        );
        write(
            root,
            "src/index.ts",
            concat!(
                "export * from './reexported';\n",
                "const legacy = require('./required');\n",
                "export const lazy = () => import('./dynamic');\n",
                "export const computed = (name: string) => import(name);\n",
                "export { legacy };\n",
            ),
        );
        write(root, "src/reexported.ts", "export const a = 1;\n");
        write(root, "src/required.ts", "export const b = 1;\n");
        write(root, "src/dynamic.ts", "export const c = 1;\n");
        write(root, "src/computed_only.ts", "export const d = 1;\n");

        assert_eq!(
            unmounted_paths(&audit_typescript(root)),
            vec!["src/computed_only.ts"]
        );
    }

    /// A build config names its entry in a string literal. Reading the literal
    /// finds it; refusing to would report the whole application as dead.
    #[test]
    fn a_config_file_string_literal_declares_an_entry_point() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "package.json", "{\"name\":\"web\",\"private\":true}");
        write(
            root,
            "rsbuild.config.ts",
            "export default { source: { entry: { index: './src/app/main.tsx' } } };\n",
        );
        write(
            root,
            "src/app/main.tsx",
            "import './boot';\nexport const app = 1;\n",
        );
        write(root, "src/app/boot.ts", "export const boot = 1;\n");
        write(root, "src/app/stale.ts", "export const stale = 1;\n");

        assert_eq!(
            unmounted_paths(&audit_typescript(root)),
            vec!["src/app/stale.ts"]
        );
    }

    #[test]
    fn malformed_config_does_not_produce_authoritative_findings() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "package.json", "{\"name\":\"web\",\"private\":true}");
        write(
            root,
            "rsbuild.config.ts",
            "export default { source: { entry: './src/index.ts' };\n",
        );
        write(root, "src/index.ts", "export const app = 1;\n");

        let audit = audit_typescript(root);
        assert_eq!(audit.status.as_str(), "partial");
        assert!(audit.unmounted.is_empty());
        let note = audit.note.expect("config omission");
        assert!(note.contains("rsbuild.config.ts"), "{note}");
        assert!(
            note.contains("could not be parsed without syntax errors"),
            "{note}"
        );
    }

    /// A `package.json` script names the file it runs, and that file is an
    /// entry point in every sense that matters.
    #[test]
    fn a_script_command_declares_the_file_it_runs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            "{\"name\":\"tools\",\"scripts\":{\"gen\":\"tsx codegen/src/cli.ts generate\"}}",
        );
        write(
            root,
            "codegen/src/cli.ts",
            "import './emit';\nexport const cli = 1;\n",
        );
        write(root, "codegen/src/emit.ts", "export const emit = 1;\n");
        write(root, "codegen/src/unused.ts", "export const unused = 1;\n");

        assert_eq!(
            unmounted_paths(&audit_typescript(root)),
            vec!["codegen/src/unused.ts"]
        );
    }

    /// Test, story, and ambient-declaration files are discovered by their
    /// runner or by the type system, not by an import.
    #[test]
    fn conventional_runner_roots_are_entry_points() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "package.json", "{\"name\":\"app\"}");
        write(root, "src/index.ts", "export const app = 1;\n");
        write(
            root,
            "src/feature.test.ts",
            "import { helper } from './test-helper';\nexport { helper };\n",
        );
        write(root, "src/test-helper.ts", "export const helper = 1;\n");
        write(root, "src/env.d.ts", "declare const x: number;\n");
        write(
            root,
            "src/__tests__/legacy.ts",
            "export const legacy = 1;\n",
        );
        write(root, "stories/peek.ts", "export const peek = 1;\n");
        write(root, "src/nobody.ts", "export const nobody = 1;\n");

        assert_eq!(
            unmounted_paths(&audit_typescript(root)),
            vec!["src/nobody.ts"]
        );
    }

    /// A tsconfig `paths` alias is the one alias mechanism the audit resolves;
    /// a file reached only through it must not be reported.
    #[test]
    fn tsconfig_path_aliases_resolve() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            "{\"name\":\"app\",\"main\":\"src/index.ts\"}",
        );
        write(
            root,
            "tsconfig.json",
            concat!(
                "{\n  // trailing comment support matters: tsconfig is JSONC\n",
                "  \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@app/*\": [\"src/*\"] } }\n}\n",
            ),
        );
        write(
            root,
            "src/index.ts",
            "import { aliased } from '@app/aliased';\nexport { aliased };\n",
        );
        write(root, "src/aliased.ts", "export const aliased = 1;\n");
        write(root, "src/unaliased.ts", "export const unaliased = 1;\n");

        assert_eq!(
            unmounted_paths(&audit_typescript(root)),
            vec!["src/unaliased.ts"]
        );
    }

    /// An ESM TypeScript project imports `./x.js` and means `x.ts`.
    #[test]
    fn a_javascript_specifier_resolves_to_its_typescript_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            "{\"name\":\"app\",\"main\":\"src/index.ts\"}",
        );
        write(
            root,
            "src/index.ts",
            "import { compiled } from './compiled.js';\nexport { compiled };\n",
        );
        write(root, "src/compiled.ts", "export const compiled = 1;\n");
        write(
            root,
            "src/unreferenced.ts",
            "export const unreferenced = 1;\n",
        );

        assert_eq!(
            unmounted_paths(&audit_typescript(root)),
            vec!["src/unreferenced.ts"]
        );
    }

    /// Each workspace package is its own reachability walk, discovered by its
    /// manifest rather than by whichever glob syntax the monorepo happens to
    /// use. A nested package's files are never blamed on the root.
    #[test]
    fn workspace_members_are_audited_under_their_own_manifests() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            "{\"name\":\"monorepo\",\"private\":true,\"workspaces\":[\"packages/*\"]}",
        );
        write(root, "pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n");
        write(
            root,
            "packages/ui/package.json",
            "{\"name\":\"@repo/ui\",\"main\":\"src/index.ts\"}",
        );
        write(
            root,
            "packages/ui/src/index.ts",
            "export * from './button';\n",
        );
        write(root, "packages/ui/src/button.ts", "export const b = 1;\n");
        write(root, "packages/ui/src/dead.ts", "export const d = 1;\n");
        write(
            root,
            "packages/api/package.json",
            "{\"name\":\"@repo/api\",\"main\":\"src/index.ts\"}",
        );
        write(root, "packages/api/src/index.ts", "export const api = 1;\n");

        let audit = audit_typescript(root);
        assert_eq!(unmounted_paths(&audit), vec!["packages/ui/src/dead.ts"]);
        assert_eq!(audit.unmounted[0].package, "@repo/ui");
        assert_eq!(audit.package_count, 3);
    }

    /// A published library points its manifest at build output the walk never
    /// sees. Without mapping that back to `src/`, its whole tree would read as
    /// dead the moment `dist/` is untracked.
    #[test]
    fn a_manifest_pointing_at_build_output_resolves_to_the_source_behind_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(
            root,
            "package.json",
            concat!(
                "{\"name\":\"@scope/sdk\",\"main\":\"./dist/index.js\",",
                "\"exports\":{\"./client\":{\"import\":\"./dist/client.js\"}}}",
            ),
        );
        write(root, "src/index.ts", "export const sdk = 1;\n");
        write(
            root,
            "src/client.ts",
            "import './wire';\nexport const c = 1;\n",
        );
        write(root, "src/wire.ts", "export const wire = 1;\n");
        write(root, "src/abandoned.ts", "export const gone = 1;\n");

        assert_eq!(
            unmounted_paths(&audit_typescript(root)),
            vec!["src/abandoned.ts"]
        );
    }

    /// A project with no manifest and no source files is absent, not clean.
    #[test]
    fn a_project_without_javascript_reports_the_ecosystem_absent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "README.md", "nothing here\n");

        assert_eq!(audit_typescript(root).status.as_str(), "not_present");
    }

    /// Source files with no manifest above them are counted, not reported:
    /// nothing declares what would ever load them.
    #[test]
    fn source_without_a_manifest_is_unclaimed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        write(root, "scratch/thing.ts", "export const thing = 1;\n");

        let audit = audit_typescript(root);
        assert!(audit.unmounted.is_empty());
        assert_eq!(audit.unclaimed_file_count, 1);
    }

    #[test]
    fn alias_rules_expand_only_matching_specifiers() {
        let rule = AliasRule {
            prefix: "@app/".to_owned(),
            suffix: String::new(),
            wildcard: true,
            targets: vec![PathBuf::from("/base/src/*")],
        };
        assert_eq!(
            rule.expand("@app/thing"),
            vec![PathBuf::from("/base/src/thing")]
        );
        assert!(rule.expand("other/thing").is_empty());
    }
}
