//! TypeScript/JavaScript module resolution over the sealed file set.
//!
//! Cross-file edges are recomputed from per-file artifacts whenever a
//! generation is sealed or restored, so this resolver reads nothing but those
//! artifacts: the indexed source paths, the `package.json` and `tsconfig`
//! pairs the JSON extractor exposes as `Const` symbols, and each file's
//! parser-attested import bindings. The rules are the ones `tsc` and bundlers
//! apply: relative specifiers with extension probing (`./foo.helpers` names
//! `foo.helpers.ts`), `.js` specifiers that name `.ts` sources, directory
//! `index` files, tsconfig `paths`/`baseUrl` aliases (with `extends`),
//! workspace packages by their manifest `name` (`exports`, `main`, `module`,
//! `types`, `source`, then the conventional `index`/`src/index`), and
//! `export … from` chains through barrels.
//!
//! A specifier that matches no alias and no workspace package is an external
//! dependency and binds nothing. A specifier that names a project module but
//! reaches no indexed file, or reaches one that does not define the imported
//! name, is a typed resolution gap: the seal binds no edge and the graph
//! discloses the affected call sites as unresolved so `callers` and
//! `file_dependents` report partial coverage instead of a complete empty list.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_extraction::ImportNamespaceV1;
use tracedecay_domain::{RelationEdgeKindV1, blank_json_comments};

use super::FileGenerationArtifactsV1;
use crate::chunks::{CodeIndexImportEvidenceV1, relation_target_kind_is_compatible};
use crate::lineage::LineageSymbolRecordV1;

/// Extension order tried for an extensionless specifier, matching `tsc` and
/// bundler resolution order.
const RESOLUTION_EXTENSIONS: [&str; 9] =
    ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "d.ts"];

/// Build-output directories a published manifest may name whose source lives
/// under `src/` with the same relative path.
const OUTPUT_DIRS: [&str; 7] = ["dist", "lib", "build", "out", "es", "esm", "cjs"];

/// Deepest `export … from` chain followed before a barrel cycle or an
/// unusually deep forwarding tree is treated as unresolved.
const MAX_REEXPORT_DEPTH: usize = 16;

/// Languages whose files import through TypeScript/JavaScript module syntax.
pub(super) fn is_typescript_family(language: &str) -> bool {
    matches!(
        language,
        "typescript" | "tsx" | "javascript" | "astro" | "svelte"
    )
}

/// One resolved import binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ImportBindingOutcomeV1<'a> {
    /// The exact defining symbol.
    Bound(usize, &'a LineageSymbolRecordV1),
    /// The specifier names a dependency outside the indexed project.
    External,
    /// The specifier names project code the seal cannot reach or that does
    /// not define the imported name; the call site is a disclosed gap.
    Unresolved,
}

/// One `compilerOptions.paths` rule anchored to its base directory.
struct AliasRuleV1 {
    prefix: String,
    suffix: String,
    wildcard: bool,
    /// Project-relative target templates (`*` kept for wildcard rules).
    targets: Vec<String>,
}

impl AliasRuleV1 {
    fn expand(&self, specifier: &str) -> Vec<String> {
        if !self.wildcard {
            return if specifier == self.prefix {
                self.targets.clone()
            } else {
                Vec::new()
            };
        }
        let Some(matched) = specifier
            .strip_prefix(self.prefix.as_str())
            .and_then(|rest| rest.strip_suffix(self.suffix.as_str()))
        else {
            return Vec::new();
        };
        self.targets
            .iter()
            .map(|target| target.replace('*', matched))
            .collect()
    }
}

#[derive(Default)]
struct TsConfigV1 {
    /// Directory of the config that declares `extends`, resolved lazily.
    extends: Vec<String>,
    aliases: Vec<AliasRuleV1>,
    base_url: Option<String>,
}

struct NodePackageV1 {
    dir: String,
    /// Project-relative entry specifiers for the bare package name, in the
    /// order a resolver consults them.
    entries: Vec<String>,
    /// `exports` subpaths (`./utils`, `./*`) to their target templates.
    subpath_exports: Vec<(String, Vec<String>)>,
}

/// Module facts for one sealed file set.
pub(super) struct TypeScriptModuleIndexV1 {
    /// Indexed TypeScript-family source paths.
    sources: HashMap<String, usize>,
    packages: Vec<NodePackageV1>,
    /// Manifest `name` to its package, absent when two manifests share one.
    by_package_name: HashMap<String, Option<usize>>,
    /// tsconfig directory to the merged rules of every `tsconfig*.json` and
    /// `jsconfig.json` beside it.
    tsconfigs: BTreeMap<String, TsConfigV1>,
}

impl TypeScriptModuleIndexV1 {
    pub(super) fn new<T>(files: &[T]) -> Self
    where
        T: AsRef<FileGenerationArtifactsV1>,
    {
        let mut sources = HashMap::new();
        let mut packages = Vec::new();
        let mut by_package_name: HashMap<String, Option<usize>> = HashMap::new();
        let mut tsconfigs: BTreeMap<String, TsConfigV1> = BTreeMap::new();
        for (index, file) in files.iter().enumerate() {
            let file = file.as_ref();
            let path = file.authority.logical_path.as_str();
            let language = file.extraction.language.as_str();
            if is_typescript_family(language) {
                sources.insert(path.to_owned(), index);
                continue;
            }
            if language != "json" {
                continue;
            }
            let (dir, name) = split_parent(path);
            if name == "package.json" {
                let (package_name, package) = node_package(dir, &file.artifacts.symbols);
                if let Some(package_name) = package_name {
                    by_package_name
                        .entry(package_name)
                        .and_modify(|slot| *slot = None)
                        .or_insert(Some(packages.len()));
                }
                packages.push(package);
            } else if (name.starts_with("tsconfig") && name.ends_with(".json"))
                || name == "jsconfig.json"
            {
                let config = tsconfig(dir, &file.artifacts.symbols);
                let merged = tsconfigs.entry(dir.to_owned()).or_default();
                merged.extends.extend(config.extends);
                merged.aliases.extend(config.aliases);
                if merged.base_url.is_none() {
                    merged.base_url = config.base_url;
                }
            }
        }
        Self {
            sources,
            packages,
            by_package_name,
            tsconfigs,
        }
    }

    pub(super) fn has_sources(&self) -> bool {
        !self.sources.is_empty()
    }

    /// The file `specifier` names from `from_path`, or why it names none.
    pub(super) fn resolve_specifier(&self, from_path: &str, specifier: &str) -> ModuleTargetV1 {
        let specifier = specifier.split(['?', '#']).next().unwrap_or(specifier);
        if specifier.is_empty() {
            return ModuleTargetV1::Unresolved;
        }
        let (from_dir, _) = split_parent(from_path);
        if matches!(specifier, "." | "..")
            || specifier.starts_with("./")
            || specifier.starts_with("../")
        {
            return self
                .probe(&join_normalized(from_dir, specifier))
                .map_or(ModuleTargetV1::Unresolved, ModuleTargetV1::File);
        }
        let mut names_project_code = false;
        if let Some(config_dir) = self.nearest_tsconfig_dir(from_dir) {
            let mut visited = HashSet::new();
            if let Some(found) =
                self.resolve_alias(config_dir, specifier, &mut visited, &mut names_project_code)
            {
                return ModuleTargetV1::File(found);
            }
        }
        if let Some((package, subpath)) = self.workspace_package(specifier) {
            names_project_code = true;
            if let Some(found) = self.resolve_package(package, subpath) {
                return ModuleTargetV1::File(found);
            }
        }
        if let Some(config_dir) = self.nearest_tsconfig_dir(from_dir) {
            let mut visited = HashSet::new();
            if let Some(found) = self.resolve_base_url(config_dir, specifier, &mut visited) {
                return ModuleTargetV1::File(found);
            }
        }
        if names_project_code {
            ModuleTargetV1::Unresolved
        } else {
            ModuleTargetV1::External
        }
    }

    /// The defining symbol behind `imported_name` in the module `binding`
    /// names, following `export … from` chains.
    pub(super) fn resolve_import_binding<'a, T>(
        &self,
        files: &'a [T],
        by_simple_name: &HashMap<&str, Vec<(usize, &'a LineageSymbolRecordV1)>>,
        binding: &CodeIndexImportEvidenceV1,
        kind: RelationEdgeKindV1,
    ) -> ImportBindingOutcomeV1<'a>
    where
        T: AsRef<FileGenerationArtifactsV1>,
    {
        let Some(imported_name) = binding.imported_name.as_deref() else {
            return ImportBindingOutcomeV1::Unresolved;
        };
        let target = match self.resolve_specifier(&binding.logical_path, &binding.module_specifier)
        {
            ModuleTargetV1::File(index) => index,
            ModuleTargetV1::External => return ImportBindingOutcomeV1::External,
            ModuleTargetV1::Unresolved => return ImportBindingOutcomeV1::Unresolved,
        };
        let mut found = Vec::new();
        let mut visited = HashSet::new();
        self.collect_exported_symbols(
            files,
            by_simple_name,
            target,
            imported_name,
            kind,
            &mut visited,
            0,
            &mut found,
        );
        found.sort_by_key(|(index, symbol)| (*index, symbol.occurrence.clone()));
        found.dedup_by(|left, right| left.1.occurrence == right.1.occurrence);
        match found.as_slice() {
            [(index, symbol)] => ImportBindingOutcomeV1::Bound(*index, symbol),
            _ => ImportBindingOutcomeV1::Unresolved,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_exported_symbols<'a, T>(
        &self,
        files: &'a [T],
        by_simple_name: &HashMap<&str, Vec<(usize, &'a LineageSymbolRecordV1)>>,
        file_index: usize,
        name: &str,
        kind: RelationEdgeKindV1,
        visited: &mut HashSet<(usize, String)>,
        depth: usize,
        found: &mut Vec<(usize, &'a LineageSymbolRecordV1)>,
    ) where
        T: AsRef<FileGenerationArtifactsV1>,
    {
        if depth > MAX_REEXPORT_DEPTH || !visited.insert((file_index, name.to_owned())) {
            return;
        }
        let defined = by_simple_name
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(index, symbol)| {
                *index == file_index && relation_target_kind_is_compatible(kind, &symbol.kind)
            })
            .copied()
            .collect::<Vec<_>>();
        if !defined.is_empty() {
            found.extend(defined);
            return;
        }
        let file = files[file_index].as_ref();
        for binding in file
            .artifacts
            .imports
            .iter()
            .filter(|binding| binding.is_public)
        {
            let forwarded = if binding.is_glob {
                Some(name)
            } else if binding.local_name.as_deref() == Some(name) {
                binding
                    .imported_name
                    .as_deref()
                    .filter(|imported| *imported != "*")
            } else {
                None
            };
            let Some(forwarded) = forwarded else {
                continue;
            };
            if let ModuleTargetV1::File(next) =
                self.resolve_specifier(&binding.logical_path, &binding.module_specifier)
            {
                self.collect_exported_symbols(
                    files,
                    by_simple_name,
                    next,
                    forwarded,
                    kind,
                    visited,
                    depth + 1,
                    found,
                );
            }
        }
    }

    /// Node/TypeScript file probing over the indexed set: the path itself, the
    /// TypeScript source behind a `.js` specifier, an added extension, then the
    /// directory `index`.
    fn probe(&self, candidate: &str) -> Option<usize> {
        if let Some(index) = self.sources.get(candidate) {
            return Some(*index);
        }
        if let Some((stem, extension)) = candidate.rsplit_once('.')
            && matches!(extension, "js" | "jsx" | "mjs" | "cjs")
            && !stem.ends_with('/')
        {
            for extension in ["ts", "tsx", "mts", "cts"] {
                if let Some(index) = self.sources.get(&format!("{stem}.{extension}")) {
                    return Some(*index);
                }
            }
        }
        for extension in RESOLUTION_EXTENSIONS {
            if let Some(index) = self.sources.get(&format!("{candidate}.{extension}")) {
                return Some(*index);
            }
        }
        let directory = candidate.trim_end_matches('/');
        for extension in RESOLUTION_EXTENSIONS {
            let index_file = if directory.is_empty() {
                format!("index.{extension}")
            } else {
                format!("{directory}/index.{extension}")
            };
            if let Some(index) = self.sources.get(&index_file) {
                return Some(*index);
            }
        }
        None
    }

    fn nearest_tsconfig_dir<'d>(&self, from_dir: &'d str) -> Option<&'d str> {
        let mut dir = from_dir;
        loop {
            if self.tsconfigs.contains_key(dir) {
                return Some(dir);
            }
            if dir.is_empty() {
                return None;
            }
            dir = split_parent(dir).0;
        }
    }

    fn resolve_alias(
        &self,
        config_dir: &str,
        specifier: &str,
        visited: &mut HashSet<String>,
        names_project_code: &mut bool,
    ) -> Option<usize> {
        if !visited.insert(config_dir.to_owned()) {
            return None;
        }
        let config = self.tsconfigs.get(config_dir)?;
        for rule in &config.aliases {
            for target in rule.expand(specifier) {
                *names_project_code = true;
                if let Some(found) = self.probe(&target) {
                    return Some(found);
                }
            }
        }
        for parent in &config.extends {
            if let Some(found) = self.resolve_alias(parent, specifier, visited, names_project_code)
            {
                return Some(found);
            }
        }
        None
    }

    fn resolve_base_url(
        &self,
        config_dir: &str,
        specifier: &str,
        visited: &mut HashSet<String>,
    ) -> Option<usize> {
        if !visited.insert(config_dir.to_owned()) {
            return None;
        }
        let config = self.tsconfigs.get(config_dir)?;
        if let Some(base) = &config.base_url
            && let Some(found) = self.probe(&join_normalized(base, specifier))
        {
            return Some(found);
        }
        for parent in &config.extends {
            if let Some(found) = self.resolve_base_url(parent, specifier, visited) {
                return Some(found);
            }
        }
        None
    }

    /// The workspace package whose `name` is the specifier or its `/`-bounded
    /// prefix, with the remaining subpath.
    fn workspace_package<'s>(&self, specifier: &'s str) -> Option<(&NodePackageV1, &'s str)> {
        let mut candidate = specifier;
        loop {
            if let Some(Some(index)) = self.by_package_name.get(candidate) {
                let subpath = specifier[candidate.len()..].trim_start_matches('/');
                return Some((&self.packages[*index], subpath));
            }
            let (head, _) = candidate.rsplit_once('/')?;
            candidate = head;
        }
    }

    fn resolve_package(&self, package: &NodePackageV1, subpath: &str) -> Option<usize> {
        if subpath.is_empty() {
            for entry in &package.entries {
                if let Some(found) = self.probe_package_path(&package.dir, entry) {
                    return Some(found);
                }
            }
            return self
                .probe(&package.dir)
                .or_else(|| self.probe(&join_normalized(&package.dir, "src")));
        }
        let export_key = format!("./{subpath}");
        for (pattern, targets) in &package.subpath_exports {
            let matched = match pattern.split_once('*') {
                Some((prefix, suffix)) => export_key
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix(suffix)),
                None => (pattern == &export_key).then_some(""),
            };
            let Some(matched) = matched else {
                continue;
            };
            for target in targets {
                let target = target.replace('*', matched);
                if let Some(found) = self.probe_package_path(&package.dir, &target) {
                    return Some(found);
                }
            }
        }
        self.probe(&join_normalized(&package.dir, subpath))
            .or_else(|| {
                self.probe(&join_normalized(
                    &join_normalized(&package.dir, "src"),
                    subpath,
                ))
            })
    }

    /// A manifest entry relative to its package, or the `src/` source behind
    /// a build-output path (`./dist/index.js` is written by `./src/index.ts`).
    fn probe_package_path(&self, package_dir: &str, entry: &str) -> Option<usize> {
        if let Some(found) = self.probe(&join_normalized(package_dir, entry)) {
            return Some(found);
        }
        let trimmed = entry.trim_start_matches("./");
        let (head, rest) = trimmed.split_once('/')?;
        OUTPUT_DIRS
            .contains(&head)
            .then(|| self.probe(&join_normalized(&join_normalized(package_dir, "src"), rest)))
            .flatten()
    }
}

/// Where a specifier lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModuleTargetV1 {
    File(usize),
    External,
    Unresolved,
}

/// `(parent directory, file name)` of a project-relative path; the root's
/// parent is `""`.
fn split_parent(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// `base/relative` with `.` and `..` segments folded; a path that would climb
/// above the project root stays clamped at the root, so it can never name a
/// file outside the indexed set.
fn join_normalized(base: &str, relative: &str) -> String {
    let mut segments: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            part => segments.push(part),
        }
    }
    segments.join("/")
}

/// The JSON value behind one top-level manifest pair the JSON extractor
/// exposed as a `Const` symbol.
fn pair_value(symbols: &[Arc<LineageSymbolRecordV1>], key: &str) -> Option<Value> {
    let symbol = symbols
        .iter()
        .find(|symbol| symbol.kind == "const" && symbol.simple_name == key)?;
    let signature = symbol.signature.as_deref()?;
    let object: Value =
        serde_json::from_str(&format!("{{{}}}", blank_json_comments(signature))).ok()?;
    object.get(key).cloned()
}

fn string_leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| string_leaves(item, out)),
        Value::Object(fields) => fields.values().for_each(|field| string_leaves(field, out)),
        _ => {}
    }
}

fn node_package(
    dir: &str,
    symbols: &[Arc<LineageSymbolRecordV1>],
) -> (Option<String>, NodePackageV1) {
    let name = pair_value(symbols, "name")
        .as_ref()
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut entries = Vec::new();
    let mut subpath_exports = Vec::new();
    match pair_value(symbols, "exports") {
        Some(Value::Object(fields)) if fields.keys().any(|key| key.starts_with('.')) => {
            for (subpath, target) in &fields {
                let mut targets = Vec::new();
                string_leaves(target, &mut targets);
                if subpath == "." {
                    entries.extend(targets);
                } else if subpath.starts_with("./") {
                    subpath_exports.push((subpath.clone(), targets));
                }
            }
        }
        Some(other) => string_leaves(&other, &mut entries),
        None => {}
    }
    for key in ["main", "module", "types", "typings", "source", "browser"] {
        if let Some(Value::String(entry)) = pair_value(symbols, key) {
            entries.push(entry);
        }
    }
    (
        name,
        NodePackageV1 {
            dir: dir.to_owned(),
            entries,
            subpath_exports,
        },
    )
}

fn tsconfig(dir: &str, symbols: &[Arc<LineageSymbolRecordV1>]) -> TsConfigV1 {
    let mut config = TsConfigV1::default();
    let mut extends = Vec::new();
    if let Some(value) = pair_value(symbols, "extends") {
        string_leaves(&value, &mut extends);
    }
    // A base config named by package (`@tsconfig/node20/tsconfig.json`) lives
    // in `node_modules` and is never indexed; only relative bases resolve.
    config.extends = extends
        .iter()
        .filter(|base| base.starts_with('.'))
        .map(|base| split_parent(&join_normalized(dir, base)).0.to_owned())
        .collect();
    let options = pair_value(symbols, "compilerOptions");
    let base_url = options
        .as_ref()
        .and_then(|options| options.get("baseUrl"))
        .and_then(Value::as_str)
        .map(|base| join_normalized(dir, base));
    let alias_base = base_url.clone().unwrap_or_else(|| dir.to_owned());
    config.base_url = base_url;
    if let Some(Value::Object(paths)) = options.as_ref().and_then(|options| options.get("paths")) {
        for (pattern, targets) in paths {
            let mut leaves = Vec::new();
            string_leaves(targets, &mut leaves);
            let targets = leaves
                .iter()
                .map(|target| join_normalized(&alias_base, target))
                .collect();
            let (prefix, suffix, wildcard) = match pattern.split_once('*') {
                Some((prefix, suffix)) => (prefix.to_owned(), suffix.to_owned(), true),
                None => (pattern.clone(), String::new(), false),
            };
            config.aliases.push(AliasRuleV1 {
                prefix,
                suffix,
                wildcard,
                targets,
            });
        }
    }
    config
}

/// The one local (non-forwarding) import binding of `local_name` usable for
/// `relation`, or `None` when the file binds it zero or several times.
pub(super) fn unique_local_import<'a>(
    file: &'a FileGenerationArtifactsV1,
    local_name: &str,
    relation: RelationEdgeKindV1,
) -> Option<&'a CodeIndexImportEvidenceV1> {
    let mut matches = file.artifacts.imports.iter().filter(|binding| {
        !binding.is_public
            && binding.local_name.as_deref() == Some(local_name)
            && match relation {
                // A type-only import cannot be called; a value import may
                // still be extended or used as a type.
                RelationEdgeKindV1::Calls => binding.namespace == ImportNamespaceV1::Value,
                _ => binding.namespace != ImportNamespaceV1::SideEffect,
            }
    });
    let binding = matches.next()?;
    matches.next().is_none().then_some(binding)
}

#[cfg(test)]
mod tests {
    use super::{AliasRuleV1, join_normalized, split_parent};

    #[test]
    fn join_normalized_folds_dots_and_clamps_at_the_root() {
        assert_eq!(
            join_normalized("packages/app/src", "../foo.helpers"),
            "packages/app/foo.helpers"
        );
        assert_eq!(join_normalized("src", "./lib"), "src/lib");
        assert_eq!(join_normalized("src", "../../escape"), "escape");
        assert_eq!(join_normalized("", "./index"), "index");
    }

    #[test]
    fn split_parent_treats_root_files_as_children_of_the_empty_directory() {
        assert_eq!(split_parent("package.json"), ("", "package.json"));
        assert_eq!(split_parent("a/b/c.ts"), ("a/b", "c.ts"));
    }

    #[test]
    fn wildcard_alias_substitutes_the_matched_segment() {
        let rule = AliasRuleV1 {
            prefix: "@app/".to_owned(),
            suffix: String::new(),
            wildcard: true,
            targets: vec!["apps/web/src/*".to_owned()],
        };
        assert_eq!(
            rule.expand("@app/lib/x"),
            vec!["apps/web/src/lib/x".to_owned()]
        );
        assert!(rule.expand("@other/x").is_empty());
    }
}
