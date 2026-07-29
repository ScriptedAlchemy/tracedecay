#![allow(clippy::collapsible_if)] // test scaffolding
//! Query kernel source guards.
//!
//! Validates that `crates/tracedecay-query/src` stays pure: only allowlisted dependency roots,
//! macros, attributes, and derives; conventional module layout reachable from
//! `crates/tracedecay-query/src/lib.rs`; and no generated or out-of-repository sources.

use crate::manifest::{
    cargo_source_layout, filesystem_rust_sources, git_tracked_paths,
    inspect_physical_manifest_paths, physical_manifest_layout,
};
use crate::module_scanner::{
    SourceReference, Token, matching_delimiter, normalize_identifier, normalize_relative,
    scan_references, token_is_ident, tokenize,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN_LIBSQL_CRATE: &str = "libsql";

const QUERY_ALLOWED_ROOTS: &[&str] = &[
    "alloc",
    "core",
    "hex",
    "hmac",
    "serde",
    "serde_json",
    "sha2",
    "static_assertions",
    "std",
    "thiserror",
    "tracedecay_code_index",
    "tracedecay_domain",
    "tracedecay_policy",
    "tracedecay_store",
    "zeroize",
];
/// Pure compile-time macros re-exported from allowlisted crates. Unlike the
/// built-in macros in [`QUERY_ALLOWED_MACROS`], these must be imported before
/// use, so the import-shadowing guard deliberately does not flag them.
const QUERY_ALLOWED_IMPORTED_MACROS: &[&str] = &["assert_not_impl_any"];
const QUERY_ALLOWED_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "format",
    "include_str",
    "matches",
    "panic",
    "unreachable",
    "vec",
    "write",
    "writeln",
];
const QUERY_ALLOWED_PRELUDE_PATH_ROOTS: &[&str] = &[
    "Box", "Option", "Result", "String", "ToString", "Vec", "bool", "char", "f32", "f64", "i8",
    "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16", "u32", "u64", "u128", "usize",
];
const QUERY_ALLOWED_DERIVES: &[&str] = &[
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Eq",
    "Hash",
    "Ord",
    "PartialEq",
    "PartialOrd",
];
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UseBinding {
    pub(crate) path: Vec<String>,
    alias: Option<String>,
    scope: Vec<String>,
    direct_module_scope: bool,
    glob: bool,
}

fn query_source_violations(source: &str) -> BTreeSet<String> {
    query_source_violations_at_depth(source, usize::MAX)
}

fn query_source_violations_at_depth(source: &str, module_depth: usize) -> BTreeSet<String> {
    let tokens = tokenize(source);
    let uses = scan_use_bindings(&tokens);
    let mut local_roots = module_level_query_names(&tokens, &[]);
    let mut violations = BTreeSet::new();

    for binding in &uses {
        if !binding.direct_module_scope {
            violations.insert(
                "block-local use is forbidden; query roots must be module-level and provable"
                    .to_string(),
            );
        }
        let before = violations.len();
        validate_query_path(&binding.path, module_depth, &local_roots, &mut violations);
        if let Some(alias) = &binding.alias {
            if binding.path == ["crate"] {
                violations.insert("crate root alias is forbidden in query source".to_string());
            } else {
                violations.insert(format!(
                    "query import alias {alias} is forbidden; roots must remain lexically explicit"
                ));
            }
        }
        if let Some(name) = binding.path.last()
            && QUERY_ALLOWED_MACROS.contains(&name.as_str())
        {
            violations.insert(format!(
                "query import {name} shadows an allowlisted built-in macro"
            ));
        }
        if let Some(binding_name) = binding.path.last().filter(|name| {
            name.as_str() != "self" && binding.alias.is_none() && before == violations.len()
        }) {
            local_roots.insert(binding_name.clone());
        }
    }

    for binding in scan_extern_crate_bindings(&tokens) {
        validate_query_path(&binding.path, module_depth, &local_roots, &mut violations);
        if let Some(alias) = binding.alias {
            violations.insert(format!(
                "extern crate alias {alias} is forbidden; dependency roots must remain explicit"
            ));
        } else if let Some(name) = binding.path.last() {
            local_roots.insert(name.clone());
        }
    }

    for (_, path) in scan_qualified_paths(&tokens) {
        validate_query_path(&path, module_depth, &local_roots, &mut violations);
    }
    validate_query_macros(&tokens, &mut violations);
    validate_query_attributes(&tokens, &uses, &mut violations);

    violations
}

fn query_source_violations_with_graph(
    source: &str,
    path: &Path,
    graph: &QueryModuleGraph,
) -> BTreeSet<String> {
    let tokens = tokenize(source);
    let uses = scan_use_bindings(&tokens);
    let (scopes, _) = token_module_scopes_and_depths(&tokens);
    let Some(base_module) = graph.source_modules.get(path) else {
        return [format!(
            "{} has no resolved query module identity",
            path.display()
        )]
        .into_iter()
        .collect();
    };
    let mut imported = BTreeMap::<Vec<String>, BTreeSet<String>>::new();
    let mut violations = BTreeSet::new();

    for binding in &uses {
        let mut module = base_module.clone();
        module.extend(binding.scope.clone());
        if !binding.direct_module_scope {
            violations.insert(format!(
                "{} contains a block-local use; query roots must be module-level",
                display_query_module(&module)
            ));
            continue;
        }
        let valid =
            validate_graph_query_path(&binding.path, &module, graph, &imported, &mut violations);
        if let Some(alias) = &binding.alias {
            violations.insert(format!(
                "query import alias {alias} is forbidden; roots must remain lexically explicit"
            ));
        }
        if let Some(name) = binding.path.last()
            && QUERY_ALLOWED_MACROS.contains(&name.as_str())
        {
            violations.insert(format!(
                "query import {name} shadows an allowlisted built-in macro"
            ));
        }
        if !valid || binding.alias.is_some() {
            continue;
        }
        if binding.glob {
            if let Some(target) =
                resolved_local_module_target(&binding.path, &module, graph, &imported)
            {
                let mut names = graph_module_symbols(graph, &target);
                if let Some(imported_names) = imported.get(&target) {
                    names.extend(imported_names.iter().cloned());
                }
                imported.entry(module).or_default().extend(names);
            } else {
                violations.insert(format!(
                    "glob import {} does not resolve to a scanned query module",
                    binding.path.join("::")
                ));
            }
        } else if let Some(name) = binding.path.last() {
            imported.entry(module).or_default().insert(name.clone());
        }
    }

    for (index, qualified) in scan_qualified_paths(&tokens) {
        let mut module = base_module.clone();
        module.extend(scopes[index].clone());
        validate_graph_query_path(&qualified, &module, graph, &imported, &mut violations);
    }
    validate_query_macros(&tokens, &mut violations);
    validate_query_attributes(&tokens, &uses, &mut violations);
    violations
}

fn validate_graph_query_path(
    path: &[String],
    current_module: &[String],
    graph: &QueryModuleGraph,
    imported: &BTreeMap<Vec<String>, BTreeSet<String>>,
    violations: &mut BTreeSet<String>,
) -> bool {
    let Some(root) = path.first() else {
        return true;
    };
    let normalized = normalize_identifier(root);
    if QUERY_ALLOWED_ROOTS.contains(&normalized.as_str())
        || QUERY_ALLOWED_PRELUDE_PATH_ROOTS.contains(&root.as_str())
        || root == "Self"
        || (root.len() == 1 && root.chars().all(char::is_uppercase))
    {
        return true;
    }
    let mut visible = graph_module_symbols(graph, current_module);
    if let Some(imported_names) = imported.get(current_module) {
        visible.extend(imported_names.iter().cloned());
    }
    if imported
        .get(current_module)
        .is_some_and(|names| names.contains(root))
    {
        return true;
    }
    if visible.contains(root)
        && root.chars().next().is_some_and(char::is_uppercase)
        && !is_local_module_root(root, current_module, graph)
    {
        return true;
    }
    if resolved_local_module_target(path, current_module, graph, imported).is_some() {
        return true;
    }
    if local_path_resolves_to_symbol(path, current_module, graph, imported) {
        return true;
    }

    violations.insert(format!(
        "query path root or local symbol is unresolved by the scanned module graph: {} from {}",
        path.join("::"),
        display_query_module(current_module)
    ));
    false
}

fn resolved_local_module_target(
    path: &[String],
    current_module: &[String],
    graph: &QueryModuleGraph,
    imported: &BTreeMap<Vec<String>, BTreeSet<String>>,
) -> Option<Vec<String>> {
    let (mut module, rest) = local_path_base(path, current_module, graph)?;
    for segment in rest {
        let mut child = module.clone();
        child.push(segment.clone());
        if graph.modules.contains(&child) {
            module = child;
            continue;
        }
        let mut symbols = graph_module_symbols(graph, &module);
        if let Some(imported_names) = imported.get(&module) {
            symbols.extend(imported_names.iter().cloned());
        }
        if symbols.contains(segment) {
            return None;
        }
        return None;
    }
    Some(module)
}

fn local_path_resolves_to_symbol(
    path: &[String],
    current_module: &[String],
    graph: &QueryModuleGraph,
    imported: &BTreeMap<Vec<String>, BTreeSet<String>>,
) -> bool {
    let Some((mut module, rest)) = local_path_base(path, current_module, graph) else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    for (index, segment) in rest.iter().enumerate() {
        let mut child = module.clone();
        child.push(segment.clone());
        if graph.modules.contains(&child) {
            module = child;
            continue;
        }
        let mut symbols = graph_module_symbols(graph, &module);
        if let Some(imported_names) = imported.get(&module) {
            symbols.extend(imported_names.iter().cloned());
        }
        return symbols.contains(segment) && index < rest.len();
    }
    true
}

fn local_path_base<'a>(
    path: &'a [String],
    current_module: &[String],
    graph: &QueryModuleGraph,
) -> Option<(Vec<String>, &'a [String])> {
    match path.first().map(String::as_str) {
        Some("crate") => Some((Vec::new(), &path[1..])),
        Some("self") => Some((current_module.to_vec(), &path[1..])),
        Some("super") => {
            let ascents = path
                .iter()
                .take_while(|segment| *segment == "super")
                .count();
            if ascents > current_module.len() {
                None
            } else {
                Some((
                    current_module[..current_module.len() - ascents].to_vec(),
                    &path[ascents..],
                ))
            }
        }
        Some(root) if is_local_module_root(root, current_module, graph) => {
            Some((current_module.to_vec(), path))
        }
        _ => None,
    }
}

fn is_local_module_root(root: &str, current_module: &[String], graph: &QueryModuleGraph) -> bool {
    let mut child = current_module.to_vec();
    child.push(root.to_string());
    graph.modules.contains(&child)
}

fn graph_module_symbols(graph: &QueryModuleGraph, module: &[String]) -> BTreeSet<String> {
    let mut symbols = graph.symbols.get(module).cloned().unwrap_or_default();
    for candidate in &graph.modules {
        if candidate.len() == module.len() + 1
            && candidate.starts_with(module)
            && let Some(name) = candidate.last()
        {
            symbols.insert(name.clone());
        }
    }
    symbols
}

fn display_query_module(module: &[String]) -> String {
    if module.is_empty() {
        "crate".to_string()
    } else {
        format!("crate::{}", module.join("::"))
    }
}

pub(crate) fn scan_use_bindings(tokens: &[Token]) -> Vec<UseBinding> {
    let (scopes, depths) = token_module_scopes_and_depths(tokens);
    let mut bindings = Vec::new();
    let mut index = 0usize;

    while index < tokens.len() {
        if !token_is_ident(tokens.get(index), "use") {
            index += 1;
            continue;
        }

        let mut cursor = index + 1;
        scan_use_tree(
            tokens,
            &mut cursor,
            &[],
            &scopes[index],
            depths[index] == scopes[index].len(),
            &mut bindings,
        );
        while cursor < tokens.len() && tokens.get(cursor) != Some(&Token::Punct(';')) {
            cursor += 1;
        }
        index = cursor.saturating_add(1);
    }

    bindings
}

fn scan_use_tree(
    tokens: &[Token],
    index: &mut usize,
    prefix: &[String],
    scope: &[String],
    direct_module_scope: bool,
    bindings: &mut Vec<UseBinding>,
) {
    while is_path_separator(tokens, *index) {
        *index += 2;
    }

    if tokens.get(*index) == Some(&Token::Punct('{')) {
        *index += 1;
        while *index < tokens.len() && tokens.get(*index) != Some(&Token::Punct('}')) {
            if tokens.get(*index) == Some(&Token::Punct(',')) {
                *index += 1;
            } else {
                scan_use_tree(tokens, index, prefix, scope, direct_module_scope, bindings);
            }
        }
        if tokens.get(*index) == Some(&Token::Punct('}')) {
            *index += 1;
        }
        return;
    }

    if tokens.get(*index) == Some(&Token::Punct('*')) {
        bindings.push(UseBinding {
            path: prefix.to_vec(),
            alias: None,
            scope: scope.to_vec(),
            direct_module_scope,
            glob: true,
        });
        *index += 1;
        return;
    }

    let Some(Token::Ident(segment)) = tokens.get(*index) else {
        return;
    };
    let mut path = prefix.to_vec();
    path.push(segment.clone());
    *index += 1;

    if is_path_separator(tokens, *index) {
        *index += 2;
        scan_use_tree(tokens, index, &path, scope, direct_module_scope, bindings);
        return;
    }

    if segment == "self" && !prefix.is_empty() {
        path = prefix.to_vec();
    }
    let mut alias = None;
    if token_is_ident(tokens.get(*index), "as") {
        *index += 1;
        if let Some(Token::Ident(name)) = tokens.get(*index) {
            alias = Some(name.clone());
            *index += 1;
        }
    }
    bindings.push(UseBinding {
        path,
        alias,
        scope: scope.to_vec(),
        direct_module_scope,
        glob: false,
    });
}

pub(crate) fn scan_qualified_paths(tokens: &[Token]) -> Vec<(usize, Vec<String>)> {
    let mut paths = Vec::new();
    let use_tokens = use_token_mask(tokens);
    let mut index = 0usize;

    while index < tokens.len() {
        if tokens.get(index) == Some(&Token::Punct('#')) {
            let bracket = if tokens.get(index + 1) == Some(&Token::Punct('!')) {
                index + 2
            } else {
                index + 1
            };
            if tokens.get(bracket) == Some(&Token::Punct('['))
                && let Some(end) = matching_delimiter(tokens, bracket, '[', ']')
            {
                index = end + 1;
                continue;
            }
        }
        if use_tokens[index] {
            index += 1;
            continue;
        }
        let Some(Token::Ident(segment)) = tokens.get(index) else {
            index += 1;
            continue;
        };
        if index >= 2
            && is_path_separator(tokens, index - 2)
            && matches!(tokens.get(index - 3), Some(Token::Ident(_)))
        {
            index += 1;
            continue;
        }
        let mut path = vec![segment.clone()];
        let mut cursor = index + 1;
        while is_path_separator(tokens, cursor) {
            let Some(Token::Ident(next)) = tokens.get(cursor + 2) else {
                break;
            };
            path.push(next.clone());
            cursor += 3;
        }
        if path.len() > 1 {
            paths.push((index, path));
        }
        index += 1;
    }

    paths
}

fn use_token_mask(tokens: &[Token]) -> Vec<bool> {
    let mut mask = vec![false; tokens.len()];
    let mut index = 0usize;
    while index < tokens.len() {
        if !token_is_ident(tokens.get(index), "use") {
            index += 1;
            continue;
        }
        while index < tokens.len() {
            mask[index] = true;
            if tokens.get(index) == Some(&Token::Punct(';')) {
                index += 1;
                break;
            }
            index += 1;
        }
    }
    mask
}

pub(crate) fn scan_extern_crate_bindings(tokens: &[Token]) -> Vec<UseBinding> {
    let mut bindings = Vec::new();

    for index in 0..tokens.len() {
        if token_is_ident(tokens.get(index), "extern")
            && token_is_ident(tokens.get(index + 1), "crate")
            && let Some(Token::Ident(name)) = tokens.get(index + 2)
        {
            let alias = if token_is_ident(tokens.get(index + 3), "as") {
                match tokens.get(index + 4) {
                    Some(Token::Ident(alias)) => Some(alias.clone()),
                    _ => None,
                }
            } else {
                None
            };
            bindings.push(UseBinding {
                path: vec![name.clone()],
                alias,
                scope: Vec::new(),
                direct_module_scope: true,
                glob: false,
            });
        }
    }

    bindings
}

fn token_module_scopes_and_depths(tokens: &[Token]) -> (Vec<Vec<String>>, Vec<usize>) {
    let mut scopes = Vec::with_capacity(tokens.len());
    let mut depths = Vec::with_capacity(tokens.len());
    let mut modules = Vec::<(usize, String)>::new();
    let mut brace_depth = 0usize;

    for index in 0..tokens.len() {
        scopes.push(modules.iter().map(|(_, name)| name.clone()).collect());
        depths.push(brace_depth);
        match tokens.get(index) {
            Some(Token::Punct('{')) => {
                brace_depth += 1;
                if index >= 2
                    && token_is_ident(tokens.get(index - 2), "mod")
                    && let Some(Token::Ident(name)) = tokens.get(index - 1)
                {
                    modules.push((brace_depth, name.clone()));
                }
            }
            Some(Token::Punct('}')) => {
                while modules
                    .last()
                    .is_some_and(|(depth, _)| *depth == brace_depth)
                {
                    modules.pop();
                }
                brace_depth = brace_depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    (scopes, depths)
}

fn module_level_query_names(tokens: &[Token], scope: &[String]) -> BTreeSet<String> {
    let (scopes, depths) = token_module_scopes_and_depths(tokens);
    (0..tokens.len().saturating_sub(1))
        .filter(|index| scopes[*index] == scope && depths[*index] == scope.len())
        .filter_map(|index| {
            let Some(Token::Ident(keyword)) = tokens.get(index) else {
                return None;
            };
            if !matches!(
                keyword.as_str(),
                "const" | "enum" | "fn" | "static" | "struct" | "trait" | "type" | "union"
            ) {
                return None;
            }
            match tokens.get(index + 1) {
                Some(Token::Ident(name)) => Some(name.clone()),
                _ => None,
            }
        })
        .collect()
}

/// Collects the leaf names re-exported by module-level `pub use` statements in
/// the given scope. A `pub use path::Name` (or `pub use path::{A, B as C}`)
/// adds `Name`/`A`/`C` to the module's public surface, so glob imports such as
/// `use super::*` in a child module can resolve them. Glob re-exports
/// (`pub use path::*`) are left to the existing local module-target resolution.
fn module_level_reexport_names(tokens: &[Token], scope: &[String]) -> BTreeSet<String> {
    let (scopes, depths) = token_module_scopes_and_depths(tokens);
    let mut names = BTreeSet::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if !token_is_ident(tokens.get(index), "use") {
            index += 1;
            continue;
        }
        if scopes[index] != *scope || depths[index] != scope.len() || !is_pub_use(tokens, index) {
            index += 1;
            continue;
        }
        let mut cursor = index + 1;
        let mut bindings = Vec::new();
        scan_use_tree(
            tokens,
            &mut cursor,
            &[],
            &scopes[index],
            true,
            &mut bindings,
        );
        for binding in bindings {
            if binding.glob {
                continue;
            }
            let leaf = binding.alias.or_else(|| binding.path.last().cloned());
            if let Some(leaf) = leaf
                && leaf != "self"
            {
                names.insert(leaf);
            }
        }
        while cursor < tokens.len() && tokens.get(cursor) != Some(&Token::Punct(';')) {
            cursor += 1;
        }
        index = cursor.saturating_add(1);
    }
    names
}

/// Reports whether the `use` at `use_index` carries a `pub` (or `pub(..)`)
/// visibility, i.e. it re-exports rather than privately importing.
fn is_pub_use(tokens: &[Token], use_index: usize) -> bool {
    if use_index == 0 {
        return false;
    }
    if token_is_ident(tokens.get(use_index - 1), "pub") {
        return true;
    }
    // pub(crate) / pub(super) / pub(in ..) use: the token before `use` is ')'.
    if tokens.get(use_index - 1) == Some(&Token::Punct(')')) {
        let mut cursor = use_index - 1;
        while cursor > 0 && tokens.get(cursor) != Some(&Token::Punct('(')) {
            cursor -= 1;
        }
        if cursor > 0 && token_is_ident(tokens.get(cursor - 1), "pub") {
            return true;
        }
    }
    false
}

fn validate_query_path(
    path: &[String],
    module_depth: usize,
    local_roots: &BTreeSet<String>,
    violations: &mut BTreeSet<String>,
) {
    let Some(root) = path.first() else {
        return;
    };
    let normalized_root = normalize_identifier(root);
    if normalized_root == "crate" {
        if !path
            .get(1)
            .map(|segment| normalize_identifier(segment))
            .is_some_and(|segment| matches!(segment.as_str(), "retrieval" | "temporal"))
        {
            violations.insert(format!(
                "crate path escapes query kernel: {}",
                path.join("::")
            ));
        }
        return;
    }
    if normalized_root == "super" {
        let ascents = path
            .iter()
            .take_while(|segment| normalize_identifier(segment) == "super")
            .count();
        if ascents > module_depth {
            violations.insert(format!(
                "super path escapes crates/tracedecay-query/src: {}",
                path.join("::")
            ));
        }
        return;
    }
    if normalized_root == "self"
        || root == "Self"
        || QUERY_ALLOWED_ROOTS.contains(&normalized_root.as_str())
        || QUERY_ALLOWED_PRELUDE_PATH_ROOTS.contains(&root.as_str())
        || (local_roots.contains(root) && root.chars().next().is_some_and(char::is_uppercase))
        || (root.len() == 1 && root.chars().all(char::is_uppercase))
    {
        return;
    }

    violations.insert(format!(
        "non-allowlisted query path root {normalized_root}: {}",
        path.join("::")
    ));
}

fn validate_query_macros(tokens: &[Token], violations: &mut BTreeSet<String>) {
    let local_macros = proven_local_macros(tokens, violations);
    for (index, token) in tokens.iter().enumerate() {
        let Token::Ident(name) = token else {
            continue;
        };
        if tokens.get(index + 1) != Some(&Token::Punct('!')) {
            continue;
        }
        if matches!(name.as_str(), "if" | "macro_rules") {
            continue;
        }
        if !matches!(tokens.get(index + 2), Some(Token::Punct('(' | '[' | '{'))) {
            continue;
        }
        if !QUERY_ALLOWED_MACROS.contains(&name.as_str())
            && !QUERY_ALLOWED_IMPORTED_MACROS.contains(&name.as_str())
            && !local_macros.contains(name)
        {
            violations.insert(format!(
                "non-allowlisted code-generating macro {name}!; query source permits only explicit pure macros"
            ));
        }
    }
}

fn proven_local_macros(tokens: &[Token], violations: &mut BTreeSet<String>) -> BTreeSet<String> {
    let mut proven = BTreeSet::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if !token_is_ident(tokens.get(index), "macro_rules")
            || tokens.get(index + 1) != Some(&Token::Punct('!'))
        {
            index += 1;
            continue;
        }
        let Some(Token::Ident(name)) = tokens.get(index + 2) else {
            violations.insert("macro_rules definition has no static name".to_string());
            index += 2;
            continue;
        };
        let Some(Token::Punct(open @ ('{' | '(' | '['))) = tokens.get(index + 3) else {
            violations.insert(format!("local macro {name} has no static body"));
            index += 3;
            continue;
        };
        let close = match open {
            '{' => '}',
            '(' => ')',
            '[' => ']',
            _ => unreachable!(),
        };
        let Some(end) = matching_delimiter(tokens, index + 3, *open, close) else {
            violations.insert(format!("local macro {name} has an unterminated body"));
            break;
        };
        let body = &tokens[index + 4..end];
        let mut safe = true;
        for cursor in 0..body.len() {
            if is_path_separator(body, cursor) {
                safe = false;
                violations.insert(format!(
                    "local macro {name} emits a qualified path; generated roots are not provable"
                ));
            }
            if body.get(cursor) == Some(&Token::Punct('#')) {
                safe = false;
                violations.insert(format!(
                    "local macro {name} emits an attribute; generated attributes are forbidden"
                ));
            }
            if body.get(cursor) == Some(&Token::Punct('!'))
                && matches!(body.get(cursor + 1), Some(Token::Punct('(' | '[' | '{')))
            {
                let static_builtin = cursor > 0
                    && matches!(
                        body.get(cursor - 1),
                        Some(Token::Ident(invoked))
                            if QUERY_ALLOWED_MACROS.contains(&invoked.as_str())
                    );
                if !static_builtin {
                    safe = false;
                    violations.insert(format!(
                        "local macro {name} contains dynamic macro dispatch"
                    ));
                }
            }
            if body.get(cursor) != Some(&Token::Punct('$')) {
                continue;
            }
            let Some(Token::Ident(metavariable)) = body.get(cursor + 1) else {
                continue;
            };
            let macro_dispatch = body.get(cursor + 2) == Some(&Token::Punct('!'));
            let path_dispatch = is_path_separator(body, cursor + 2);
            let attribute_dispatch = cursor >= 2
                && body.get(cursor - 2) == Some(&Token::Punct('#'))
                && body.get(cursor - 1) == Some(&Token::Punct('['));
            if macro_dispatch || path_dispatch || attribute_dispatch {
                safe = false;
                violations.insert(format!(
                    "local macro {name} uses metavariable ${metavariable} as {} dispatch",
                    if macro_dispatch {
                        "macro"
                    } else if path_dispatch {
                        "path"
                    } else {
                        "attribute"
                    }
                ));
            }
        }
        if QUERY_ALLOWED_MACROS.contains(&name.as_str()) {
            safe = false;
            violations.insert(format!(
                "local macro {name} shadows an allowlisted built-in macro"
            ));
        }
        if safe {
            proven.insert(name.clone());
        }
        index = end + 1;
    }
    proven
}

fn validate_query_attributes(
    tokens: &[Token],
    uses: &[UseBinding],
    violations: &mut BTreeSet<String>,
) {
    let mut index = 0usize;
    while index < tokens.len() {
        if tokens.get(index) != Some(&Token::Punct('#')) {
            index += 1;
            continue;
        }
        let bracket = if tokens.get(index + 1) == Some(&Token::Punct('!')) {
            index + 2
        } else {
            index + 1
        };
        if tokens.get(bracket) != Some(&Token::Punct('[')) {
            index += 1;
            continue;
        }
        let Some(end) = matching_delimiter(tokens, bracket, '[', ']') else {
            violations.insert("unterminated query attribute".to_string());
            break;
        };
        let body = &tokens[bracket + 1..end];
        let Some(Token::Ident(name)) = body.first() else {
            violations.insert("query attribute has no statically identifiable name".to_string());
            index = end + 1;
            continue;
        };
        let normalized = normalize_identifier(name);
        if normalized == "derive" {
            validate_query_derives(body, uses, violations);
        } else {
            let exact = match normalized.as_str() {
                "allow" => {
                    matches!(
                        body,
                        [
                            Token::Ident(allow),
                            Token::Punct('('),
                            Token::Ident(lint),
                            Token::Punct(')'),
                        ] if allow == "allow" && (lint == "deprecated" || lint == "dead_code")
                    ) || matches!(
                        body,
                        [
                            Token::Ident(allow),
                            Token::Punct('('),
                            Token::Ident(clippy),
                            Token::Punct(':'),
                            Token::Punct(':'),
                            Token::Ident(_),
                            Token::Punct(')'),
                        ] if allow == "allow" && clippy == "clippy"
                    )
                }
                "must_use" => body == [Token::Ident("must_use".to_string())],
                "cfg" => {
                    body == [
                        Token::Ident("cfg".to_string()),
                        Token::Punct('('),
                        Token::Ident("test".to_string()),
                        Token::Punct(')'),
                    ]
                }
                "test" | "from" => body.len() == 1,
                "serde" => {
                    body == [
                        Token::Ident("serde".to_string()),
                        Token::Punct('('),
                        Token::Ident("deny_unknown_fields".to_string()),
                        Token::Punct(')'),
                    ] || matches!(
                        body,
                        [
                            Token::Ident(_),
                            Token::Punct('('),
                            Token::Ident(argument),
                            Token::Punct(')')
                        ] if argument == "default"
                    ) || matches!(
                        body,
                        [
                            Token::Ident(_),
                            Token::Punct('('),
                            Token::Ident(key),
                            Token::Punct('='),
                            Token::StringLiteral(_),
                            Token::Punct(')')
                        ] if key == "rename" || key == "skip_serializing_if"
                    ) || matches!(
                        body,
                        [
                            Token::Ident(_),
                            Token::Punct('('),
                            Token::Ident(default),
                            Token::Punct(','),
                            Token::Ident(key),
                            Token::Punct('='),
                            Token::StringLiteral(_),
                            Token::Punct(')')
                        ] if default == "default"
                            && (key == "rename" || key == "skip_serializing_if")
                    )
                }
                "error" => match body {
                    [
                        Token::Ident(_),
                        Token::Punct('('),
                        Token::StringLiteral(_),
                        Token::Punct(')'),
                    ] => true,
                    [
                        Token::Ident(_),
                        Token::Punct('('),
                        Token::Ident(value),
                        Token::Punct(')'),
                    ] => value == "transparent",
                    _ => false,
                },
                "deprecated" => {
                    matches!(
                        body,
                        [
                            Token::Ident(_),
                            Token::Punct('('),
                            Token::Ident(key),
                            Token::Punct('='),
                            Token::StringLiteral(_),
                            Token::Punct(')')
                        ] if key == "note"
                    )
                }
                _ => false,
            };
            if !exact {
                let helper = body.iter().find_map(|token| match token {
                    Token::Ident(helper) if helper != name => Some(helper.as_str()),
                    _ => None,
                });
                violations.insert(format!(
                    "query attribute {name} is not an exact allowlisted form{}",
                    helper.map_or_else(String::new, |helper| format!(": {helper}"))
                ));
            }
        }
        index = end + 1;
    }
}

fn validate_query_derives(body: &[Token], uses: &[UseBinding], violations: &mut BTreeSet<String>) {
    if body.len() < 4
        || body.get(1) != Some(&Token::Punct('('))
        || body.last() != Some(&Token::Punct(')'))
    {
        violations.insert("derive attribute is not a static comma-separated list".to_string());
        return;
    }
    let mut expect_derive = true;
    for token in &body[2..body.len() - 1] {
        match token {
            Token::Ident(derive) if expect_derive => {
                let imported = match derive.as_str() {
                    "Serialize" | "Deserialize" => uses.iter().any(|binding| {
                        binding.alias.is_none()
                            && binding.path == ["serde".to_string(), derive.clone()]
                    }),
                    "Error" => uses.iter().any(|binding| {
                        binding.alias.is_none()
                            && binding.path == ["thiserror".to_string(), "Error".to_string()]
                    }),
                    _ => QUERY_ALLOWED_DERIVES.contains(&derive.as_str()),
                };
                if !imported {
                    violations.insert(format!(
                        "derive macro {derive} is not a proven built-in or exact pure import"
                    ));
                }
                expect_derive = false;
            }
            Token::Punct(',') if !expect_derive => expect_derive = true,
            _ => {
                violations.insert(
                    "derive attribute contains a path, alias, or dynamic token".to_string(),
                );
                return;
            }
        }
    }
    if expect_derive {
        violations.insert("derive attribute has a trailing comma or missing derive".to_string());
    }
}

fn is_path_separator(tokens: &[Token], index: usize) -> bool {
    tokens.get(index) == Some(&Token::Punct(':'))
        && tokens.get(index + 1) == Some(&Token::Punct(':'))
}

pub(crate) fn query_kernel_sources(repository: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let source_roots = [PathBuf::from("crates/tracedecay-query/src")]
        .into_iter()
        .collect();
    let mut sources = filesystem_rust_sources(repository, &source_roots)?;
    if let Ok(tracked) = git_tracked_paths(repository) {
        let physical = inspect_physical_manifest_paths(repository, &tracked)?;
        if let Some(outside) = physical
            .violations
            .iter()
            .find(|violation| violation.contains("symlink"))
        {
            return Err(outside.clone());
        }
        sources.extend(
            physical
                .symlinked_rust_sources
                .into_iter()
                .filter(|path| path.starts_with("crates/tracedecay-query/src")),
        );
    }
    Ok(sources)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct QueryScanContext {
    path: PathBuf,
    module_dir: PathBuf,
    module_path: Vec<String>,
}

#[derive(Debug, Default)]
struct QueryModuleGraph {
    modules: BTreeSet<Vec<String>>,
    symbols: BTreeMap<Vec<String>, BTreeSet<String>>,
    source_modules: BTreeMap<PathBuf, Vec<String>>,
    violations: BTreeSet<String>,
}

pub(crate) fn query_kernel_violations(
    repository: &Path,
    sources: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<String>, String> {
    let graph = build_query_module_graph(repository, sources)?;
    let mut violations = graph.violations.clone();

    for path in sources {
        let absolute = repository.join(path);
        let source = fs::read_to_string(&absolute)
            .map_err(|error| format!("cannot read {}: {error}", absolute.display()))?;
        violations.extend(
            query_source_violations_with_graph(&source, path, &graph)
                .into_iter()
                .map(|violation| format!("{}: {violation}", path.display())),
        );
    }

    Ok(violations)
}

fn build_query_module_graph(
    repository: &Path,
    sources: &BTreeSet<PathBuf>,
) -> Result<QueryModuleGraph, String> {
    let query_root = repository.join("crates/tracedecay-query/src");
    fs::canonicalize(&query_root)
        .map_err(|error| format!("cannot canonicalize {}: {error}", query_root.display()))?;
    let canonical_repository = fs::canonicalize(repository)
        .map_err(|error| format!("cannot canonicalize {}: {error}", repository.display()))?;
    let root = PathBuf::from("crates/tracedecay-query/src/lib.rs");
    let mut graph = QueryModuleGraph::default();
    if !sources.contains(&root) {
        graph.violations.insert(
            "crates/tracedecay-query/src/lib.rs is required as the single query module root"
                .to_string(),
        );
        return Ok(graph);
    }

    let mut reachable = BTreeSet::new();
    let mut scanned = BTreeSet::new();
    let mut pending = VecDeque::from([QueryScanContext {
        path: root,
        module_dir: PathBuf::from("crates/tracedecay-query/src"),
        module_path: Vec::new(),
    }]);

    while let Some(context) = pending.pop_front() {
        reachable.insert(context.path.clone());
        if !scanned.insert(context.clone()) {
            continue;
        }
        graph.modules.insert(context.module_path.clone());
        if let Some(previous) = graph
            .source_modules
            .insert(context.path.clone(), context.module_path.clone())
            && previous != context.module_path
        {
            graph.violations.insert(format!(
                "{} resolves as multiple query modules: {} and {}",
                context.path.display(),
                previous.join("::"),
                context.module_path.join("::")
            ));
        }
        let absolute = repository.join(&context.path);
        let canonical = fs::canonicalize(&absolute)
            .map_err(|error| format!("cannot canonicalize {}: {error}", absolute.display()))?;
        if !canonical.starts_with(&canonical_repository) {
            graph.violations.insert(format!(
                "{} resolves outside the repository to {}",
                context.path.display(),
                canonical.display()
            ));
            continue;
        }
        let source = fs::read_to_string(&absolute)
            .map_err(|error| format!("cannot read {}: {error}", absolute.display()))?;
        let tokens = tokenize(&source);
        let (scopes, depths) = token_module_scopes_and_depths(&tokens);
        for scope in scopes.iter().cloned().collect::<BTreeSet<_>>() {
            let mut full_scope = context.module_path.clone();
            full_scope.extend(scope.clone());
            graph.modules.insert(full_scope.clone());
            let mut names = module_level_query_names(&tokens, &scope);
            names.extend(module_level_reexport_names(&tokens, &scope));
            graph.symbols.entry(full_scope).or_default().extend(names);
        }
        for index in 0..tokens.len().saturating_sub(2) {
            if scopes[index].len() != depths[index]
                || !token_is_ident(tokens.get(index), "mod")
                || !matches!(tokens.get(index + 1), Some(Token::Ident(_)))
                || tokens.get(index + 2) != Some(&Token::Punct('{'))
            {
                continue;
            }
            let Some(Token::Ident(name)) = tokens.get(index + 1) else {
                continue;
            };
            let mut inline_path = context.module_path.clone();
            inline_path.extend(scopes[index].clone());
            inline_path.push(name.clone());
            graph.modules.insert(inline_path);
        }

        for reference in scan_references(&source) {
            match reference {
                SourceReference::Include { .. } => {}
                SourceReference::Module {
                    name: _,
                    path: Some(path),
                    ..
                } => {
                    graph.violations.insert(format!(
                        "{}: #[path = {path:?}] is forbidden; query modules must follow the crates/tracedecay-query/src file convention",
                        context.path.display()
                    ));
                }
                SourceReference::Module {
                    name,
                    path: None,
                    inline_modules,
                } => {
                    let mut module_dir = context.module_dir.clone();
                    module_dir.extend(inline_modules.iter());
                    let child_module_dir = normalize_relative(&module_dir.join(&name))?;
                    let mut child_module_path = context.module_path.clone();
                    child_module_path.extend(inline_modules);
                    child_module_path.push(name.clone());
                    let candidates = [
                        module_dir.join(format!("{name}.rs")),
                        module_dir.join(&name).join("mod.rs"),
                    ];
                    let existing: Vec<_> = candidates
                        .into_iter()
                        .filter(|candidate| {
                            fs::canonicalize(repository.join(candidate))
                                .is_ok_and(|canonical| canonical.is_file())
                        })
                        .collect();
                    match existing.as_slice() {
                        [] => {
                            graph.violations.insert(format!(
                                "{}: unresolved module {name}; expected exactly one conventional query source",
                                context.path.display()
                            ));
                        }
                        [target] => {
                            if !sources.contains(target) {
                                graph.violations.insert(format!(
                                    "{}: module {name} resolves to unenumerated source {}",
                                    context.path.display(),
                                    target.display()
                                ));
                            } else {
                                pending.push_back(QueryScanContext {
                                    path: target.clone(),
                                    module_dir: child_module_dir,
                                    module_path: child_module_path,
                                });
                            }
                        }
                        _ => {
                            graph.violations.insert(format!(
                                "{}: module {name} is ambiguous between {}",
                                context.path.display(),
                                existing
                                    .iter()
                                    .map(|path| path.display().to_string())
                                    .collect::<Vec<_>>()
                                    .join(" and ")
                            ));
                        }
                    }
                }
            }
        }
    }

    for unreachable in sources.difference(&reachable) {
        graph.violations.insert(format!(
            "{} is not reachable from crates/tracedecay-query/src/lib.rs through conventional mod declarations",
            unreachable.display()
        ));
    }
    Ok(graph)
}

#[test]
fn query_source_guard_rejects_import_path_and_macro_bypasses() {
    for (name, source, expected) in [
        (
            "root group crate alias",
            "use { crate as outer }; outer::db::Connection::open();",
            "crate root alias",
        ),
        (
            "outer crate alias",
            "use crate as outer; outer::storage::Connection::open();",
            "crate root alias",
        ),
        (
            "macro indirection",
            "macro_rules! hidden { () => { crate::storage::open() } } hidden!();",
            "macro",
        ),
        (
            "macro metavariable dispatch",
            "macro_rules! mismatch { ($format:ident) => { $format!(\"hidden\") } }",
            "metavariable",
        ),
        (
            "nested declaration laundering",
            "fn decoy() { struct sqlx; } fn exploit() { sqlx::Pool::connect(); }",
            "sqlx",
        ),
        (
            "serde helper string",
            "#[serde(serialize_with = \"sqlx::encode\")] struct Record;",
            "serialize_with",
        ),
        (
            "OUT_DIR include",
            "include!(concat!(env!(\"OUT_DIR\"), \"/query.rs\"));",
            "include",
        ),
        (
            "cfg_attr path",
            "#[cfg_attr(unix, path = \"../db.rs\")] mod backend;",
            "cfg_attr",
        ),
        (
            "multiline tree import",
            "use crate::{\n    daemon::DaemonClient,\n};",
            "daemon",
        ),
        (
            "absolute grouped import",
            "use {::serde::Serialize, ::sqlx as database};",
            "sqlx",
        ),
        (
            "fully qualified root path",
            "crate::automation::run_background_workflow();",
            "automation",
        ),
        (
            "qualified type alias",
            "type Connection = mongodb::Client;",
            "mongodb",
        ),
        (
            "bare forbidden type",
            "RuntimeDatabase::open();",
            "runtimedatabase",
        ),
        ("raw identifier path", "crate::r#daemon::serve();", "daemon"),
        ("macro path", "sqlx::query!(\"SELECT 1\");", "sqlx"),
        ("attribute macro", "#[mcp]\nfn exposed() {}", "mcp"),
        (
            "extern crate alias",
            "extern crate diesel as store;",
            "diesel",
        ),
        ("MCP root module", "crate::mcp::Server::start();", "mcp"),
        (
            "dashboard root module",
            "crate::dashboard::Dashboard::new();",
            "dashboard",
        ),
        (
            "model runtime root module",
            "crate::model_runtime::ModelRuntime::load();",
            "model_runtime",
        ),
        ("policy root module", "crate::policy::evaluate();", "policy"),
        ("UI root module", "crate::ui::render();", "ui"),
        (
            "unlisted database client",
            "use cassandra_cpp::Cluster;",
            "cassandra_cpp",
        ),
        (
            "transport root module",
            "crate::transport::send();",
            "transport",
        ),
    ] {
        let violations = query_source_violations(source);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "query source guard missed {name}: {violations:?}"
        );
    }

    for database_crate in [
        FORBIDDEN_LIBSQL_CRATE,
        "sqlx",
        "rusqlite",
        "diesel",
        "sea_orm",
        "postgres",
        "mongodb",
        "redis",
        "rocksdb",
        "cassandra_cpp",
    ] {
        let source = format!("use {database_crate}::Connection as QueryConnection;");
        let violations = query_source_violations(&source);
        assert!(
            !violations.is_empty(),
            "query source guard missed database crate {database_crate}: {violations:?}"
        );
    }
}

#[test]
fn query_source_guard_scopes_clippy_lint_to_allowlisted_attribute() {
    let accepted = query_source_violations(
        r#"
        #[allow(clippy::too_many_arguments)]
        fn construct() {}
        "#,
    );
    assert!(
        accepted.is_empty(),
        "allowlisted clippy lint attribute produced violations: {accepted:?}"
    );

    let violations = query_source_violations("clippy::undeclared_lint_path();");
    assert!(
        violations
            .iter()
            .any(|violation| violation.contains("clippy")),
        "query source guard accepted an unresolved clippy path: {violations:?}"
    );
}

#[test]
fn query_source_guard_allows_comments_strings_and_query_contracts() {
    let source = r##"
        // use crate::daemon::DaemonClient; sqlx::query!("SELECT 1");
        /* RuntimeDatabase::open(); crate::dashboard::Dashboard::new(); */
        const PROSE: &str = "mcp::Server and crate::automation::run are not references";
        const RAW_PROSE: &str = r#"rusqlite::Connection and crate::transport::send"#;

        use {::serde::Serialize, std::collections::BTreeSet};
        use tracedecay_domain::session::SessionId;
        use tracedecay_store::memory::StorePort;
        use crate::temporal::ports::TemporalReadPort;

        #[derive(Clone, Debug, Serialize)]
        struct Contract;

        fn accept(port: &dyn TemporalReadPort) -> usize {
            let _ = port;
            let _: serde_json::Value = serde_json::Value::Null;
            let values = vec![format!("{}", BTreeSet::<SessionId>::new().len())];
            if matches!(values.len(), 1) { 1 } else { 0 }
        }
    "##;

    assert!(
        query_source_violations(source).is_empty(),
        "comments, strings, and domain/store/query contracts must be allowed"
    );
}

#[test]
fn query_source_guard_allows_proven_local_macros_and_exact_serde_forms() {
    let source = r#"
        use serde::{Deserialize, Serialize};

        #[derive(Clone, Debug, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Record {
            value: String,
        }

        macro_rules! require {
            ($condition:expr) => {
                assert!($condition);
            };
        }

        fn validate(record: &Record) {
            require!(!record.value.is_empty());
        }
    "#;

    assert!(
        query_source_violations(source).is_empty(),
        "exact serde forms and locally proven macros must be allowed: {:?}",
        query_source_violations(source)
    );
}

#[test]
fn query_kernel_guard_rejects_generated_source_and_unresolved_modules() {
    let temporary = tempfile::tempdir().expect("create query source fixture");
    let repository = temporary.path();
    fs::create_dir_all(repository.join("crates/tracedecay-query/src")).unwrap();
    fs::write(
        repository.join("crates/tracedecay-query/src/lib.rs"),
        r#"
        #[path = "../outside.rs"]
        mod path_dependency;
        #[cfg_attr(unix, path = "../conditional.rs")]
        mod conditional;
        mod missing;
        include!(concat!(env!("OUT_DIR"), "/generated_query.rs"));
        "#,
    )
    .unwrap();

    let sources = query_kernel_sources(repository).expect("enumerate query kernel sources");
    let violations = query_kernel_violations(repository, &sources).expect("inspect query sources");
    for expected in ["#[path", "cfg_attr", "missing", "include", "concat", "env"] {
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "query source convention missed {expected}: {violations:?}"
        );
    }
}

#[test]
fn query_kernel_guard_accepts_only_conventional_reachable_modules() {
    let temporary = tempfile::tempdir().expect("create query module fixture");
    let repository = temporary.path();
    fs::create_dir_all(repository.join("crates/tracedecay-query/src/temporal")).unwrap();
    fs::write(
        repository.join("crates/tracedecay-query/src/lib.rs"),
        "pub mod temporal;\n",
    )
    .unwrap();
    fs::write(
        repository.join("crates/tracedecay-query/src/temporal/mod.rs"),
        "mod ports;\nuse self::ports::Port;\nstruct Kernel(Port);\n",
    )
    .unwrap();
    fs::write(
        repository.join("crates/tracedecay-query/src/temporal/ports.rs"),
        "pub struct Port;\n",
    )
    .unwrap();

    let sources = query_kernel_sources(repository).expect("enumerate query kernel sources");
    assert_eq!(sources.len(), 3);
    assert!(
        query_kernel_violations(repository, &sources)
            .expect("inspect conventional query modules")
            .is_empty()
    );
}

#[test]
fn temporal_kernel_sources_respect_dependency_boundary() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let physical =
        physical_manifest_layout(&repository).expect("inspect tracked physical Cargo manifests");
    assert!(
        physical.violations.is_empty(),
        "workspace manifests violate driver-neutral dependency boundaries:\n{}",
        physical
            .violations
            .iter()
            .map(|violation| format!("  - {violation}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let layout = cargo_source_layout(&repository).expect("inspect Cargo workspace membership");
    assert!(
        layout.boundary_violations.is_empty(),
        "workspace dependencies or targets violate query/runtime boundaries:\n{}",
        layout
            .boundary_violations
            .iter()
            .map(|violation| format!("  - {violation}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let sources = query_kernel_sources(&repository).expect("resolve temporal kernel sources");
    assert!(!sources.is_empty(), "temporal kernel sources must exist");
    let violations =
        query_kernel_violations(&repository, &sources).expect("inspect temporal kernel sources");
    assert!(
        violations.is_empty(),
        "query kernel source convention or positive dependency contract violations:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn pr8_temporal_kernel_has_one_scope_cursor_and_payload_path() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = query_kernel_sources(&repository).expect("resolve temporal kernel sources");
    let temporal_sources = sources
        .iter()
        .filter(|path| path.starts_with("crates/tracedecay-query/src/temporal"))
        .collect::<Vec<_>>();
    assert!(
        !temporal_sources.is_empty(),
        "PR8 temporal kernel sources must exist"
    );
    assert_eq!(
        temporal_sources
            .iter()
            .filter(|path| path.as_path()
                == Path::new("crates/tracedecay-query/src/temporal/cursor.rs"))
            .count(),
        1,
        "PR8 must retain exactly one canonical cursor module"
    );

    let forbidden_identifiers = [
        ("TaskId", "Plan 24 task authority"),
        ("all_registered", "project registry fan-out"),
        ("DaemonSessionRuntimeRegistryV1", "session registry fan-out"),
        ("current_dir", "CWD-derived scope"),
        ("set_current_dir", "CWD-derived scope"),
        ("OpenOptions", "writable read fallback"),
        ("open_writable", "writable read fallback"),
        ("compatibility_cursor", "second LCM cursor"),
        ("AuthorizedSessionExpandCursorBinding", "second LCM cursor"),
        ("encode_lcm_expand_cursor", "second LCM cursor"),
        ("decode_lcm_expand_cursor", "second LCM cursor"),
        ("get_session_message", "second payload lookup"),
        ("hydrate_authorized_anchor_bytes", "second payload lookup"),
        ("expand_payload", "second payload lookup"),
    ];
    let mut violations = BTreeSet::new();
    for path in temporal_sources {
        let source = fs::read_to_string(repository.join(path))
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let identifiers = tokenize(&source)
            .into_iter()
            .filter_map(|token| match token {
                Token::Ident(identifier) => Some(identifier),
                Token::StringLiteral(_) | Token::Punct(_) => None,
            })
            .collect::<BTreeSet<_>>();
        for (identifier, authority) in forbidden_identifiers {
            if identifiers.contains(identifier) {
                violations.insert(format!(
                    "{} references {authority} through {identifier}",
                    path.display()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "PR8 temporal kernel crossed its task/scope/read/cursor/hydration boundary:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}
