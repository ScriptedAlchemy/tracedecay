use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const REPOSITORY_SOURCE_ROOTS: &[&str] = &["src", "tests", "examples", "benches"];
const QUERY_ALLOWED_ROOTS: &[&str] = &[
    "alloc",
    "core",
    "hex",
    "hmac",
    "serde",
    "serde_json",
    "sha2",
    "std",
    "thiserror",
    "tracedecay_domain",
    "tracedecay_store",
    "zeroize",
];
const QUERY_ALLOWED_PACKAGES: &[&str] = &[
    "hex",
    "hmac",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
    "tracedecay-domain",
    "tracedecay-store",
    "zeroize",
];
const QUERY_ALLOWED_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "format",
    "matches",
    "panic",
    "unreachable",
    "vec",
    "write",
    "writeln",
];
const QUERY_ALLOWED_PRELUDE_PATH_ROOTS: &[&str] = &[
    "Box", "Option", "Result", "String", "Vec", "bool", "char", "f32", "f64", "i8", "i16", "i32",
    "i64", "i128", "isize", "str", "u8", "u16", "u32", "u64", "u128", "usize",
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
const PR8_WORKSPACE_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "crates/tracedecay-domain/Cargo.toml",
    "crates/tracedecay-store/Cargo.toml",
];
const PR8_TARGET_SNAPSHOT: &[&str] = &[
    "tracedecay-domain|tracedecay_domain|lib|crates/tracedecay-domain/src/lib.rs",
    "tracedecay-domain|integration_catalog_contract|test|crates/tracedecay-domain/tests/integration_catalog_contract.rs",
    "tracedecay-domain|observation_contract|test|crates/tracedecay-domain/tests/observation_contract.rs",
    "tracedecay-domain|session_contract|test|crates/tracedecay-domain/tests/session_contract.rs",
    "tracedecay-store|tracedecay_store|lib|crates/tracedecay-store/src/lib.rs",
    "tracedecay-store|session_contract|test|crates/tracedecay-store/tests/session_contract.rs",
    "tracedecay|tracedecay|lib|src/lib.rs",
    "tracedecay|tracedecay|bin|src/main.rs",
    "tracedecay|bench_extract|example|examples/bench_extract.rs",
    "tracedecay|agent_suite|test|tests/agent_suite/main.rs",
    "tracedecay|architecture_boundaries|test|tests/architecture_boundaries.rs",
    "tracedecay|automation_runner_test|test|tests/automation_runner_test/main.rs",
    "tracedecay|core_cli_suite|test|tests/core_cli_suite/main.rs",
    "tracedecay|cross_host_handoff_test|test|tests/cross_host_handoff_test.rs",
    "tracedecay|daemon_fault_harness_test|test|tests/daemon_fault_harness_test.rs",
    "tracedecay|daemon_suite|test|tests/daemon_suite/main.rs",
    "tracedecay|dashboard_api_test|test|tests/dashboard_api_test/main.rs",
    "tracedecay|extraction_suite|test|tests/extraction_suite/main.rs",
    "tracedecay|graph_suite|test|tests/graph_suite/main.rs",
    "tracedecay|hermes_suite|test|tests/hermes_suite/main.rs",
    "tracedecay|hooks_lsp_suite|test|tests/hooks_lsp_suite/main.rs",
    "tracedecay|host_event_fixture_test|test|tests/host_event_fixture_test.rs",
    "tracedecay|lcm_gc_report_compat|test|tests/lcm_gc_report_compat.rs",
    "tracedecay|mcp_suite|test|tests/mcp_suite/main.rs",
    "tracedecay|memory_suite|test|tests/memory_suite/main.rs",
    "tracedecay|session_suite|test|tests/session_suite/main.rs",
    "tracedecay|storage_suite|test|tests/storage_suite/main.rs",
    "tracedecay|tool_client_transport|test|tests/tool_client_transport.rs",
    "tracedecay|transcript_ingest_suite|test|tests/transcript_ingest_suite/main.rs",
    "tracedecay|update_health_pass_test|test|tests/update_health_pass_test.rs",
    "tracedecay|v2_corpus_suite|test|tests/v2_corpus_suite.rs",
    "tracedecay|large_repos|bench|benches/large_repos.rs",
    "tracedecay|queries|bench|benches/queries.rs",
    "tracedecay|repos|bench|benches/repos.rs",
    "tracedecay|session_temporal|bench|benches/session_temporal.rs",
    "tracedecay|build-script-build|custom-build|build.rs",
];
const PR8_ROOT_PACKAGE_ALIASES: &[(&str, &str)] = &[
    (
        "tracedecay-medium-treesitters",
        "tokensave-medium-treesitters",
    ),
    (
        "tracedecay-large-treesitters",
        "tokensave-large-treesitters",
    ),
];

// This is a sample project indexed by context-evaluation tests. Its Rust files
// are deliberately source input, not modules or targets of the tracedecay crate.
const INTENTIONAL_STANDALONE_RUST_INPUTS: &[&str] = &[
    "tests/fixtures/context_eval_project/src/auth/login.rs",
    "tests/fixtures/context_eval_project/src/auth/mod.rs",
    "tests/fixtures/context_eval_project/src/auth/session.rs",
    "tests/fixtures/context_eval_project/src/cli.rs",
    "tests/fixtures/context_eval_project/src/main.rs",
    "tests/fixtures/context_eval_project/src/net/http_client.rs",
    "tests/fixtures/context_eval_project/src/net/mod.rs",
    "tests/fixtures/context_eval_project/src/net/retry.rs",
    "tests/fixtures/context_eval_project/src/storage/cache.rs",
    "tests/fixtures/context_eval_project/src/storage/config_store.rs",
    "tests/fixtures/context_eval_project/src/storage/mod.rs",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    StringLiteral(String),
    Punct(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceReference {
    Module {
        name: String,
        path: Option<String>,
        inline_modules: Vec<String>,
    },
    Include {
        path: String,
        parse_as_rust: bool,
        inline_modules: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScanContext {
    path: PathBuf,
    module_dir: PathBuf,
}

fn tokenize(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index += 2;
            let mut depth = 1usize;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            continue;
        }
        if let Some((value, next)) = raw_string_at(source, index) {
            tokens.push(Token::StringLiteral(value));
            index = next;
            continue;
        }
        if bytes[index] == b'"' {
            let (value, next) = quoted_string_at(source, index);
            tokens.push(Token::StringLiteral(value));
            index = next;
            continue;
        }
        if bytes[index] == b'\''
            && let Some(next) = char_literal_end(bytes, index)
        {
            index = next;
            continue;
        }
        if bytes[index..].starts_with(b"r#")
            && bytes
                .get(index + 2)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        {
            let start = index + 2;
            index = start + 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(Token::Ident(source[start..index].to_string()));
            continue;
        }
        if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(Token::Ident(source[start..index].to_string()));
            continue;
        }

        let character = source[index..].chars().next().expect("valid UTF-8");
        if character.is_ascii() {
            tokens.push(Token::Punct(character));
        }
        index += character.len_utf8();
    }

    tokens
}

fn raw_string_at(source: &str, start: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'r') {
        return None;
    }
    let mut quote = start + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }

    let hashes = quote - start - 1;
    let content_start = quote + 1;
    let mut cursor = content_start;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes) == Some(&bytes[start + 1..quote])
        {
            return Some((
                source[content_start..cursor].to_string(),
                cursor + 1 + hashes,
            ));
        }
        cursor += 1;
    }
    Some((source[content_start..].to_string(), bytes.len()))
}

fn quoted_string_at(source: &str, start: usize) -> (String, usize) {
    let bytes = source.as_bytes();
    let mut value = String::new();
    let mut index = start + 1;

    while index < bytes.len() {
        match bytes[index] {
            b'"' => return (value, index + 1),
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    break;
                }
                match bytes[index] {
                    b'\\' => value.push('\\'),
                    b'"' => value.push('"'),
                    b'n' => value.push('\n'),
                    b'r' => value.push('\r'),
                    b't' => value.push('\t'),
                    b'0' => value.push('\0'),
                    b'\n' => {
                        index += 1;
                        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                            index += 1;
                        }
                        continue;
                    }
                    other => value.push(char::from(other)),
                }
                index += 1;
            }
            _ => {
                let character = source[index..].chars().next().expect("valid UTF-8");
                value.push(character);
                index += character.len_utf8();
            }
        }
    }

    (value, bytes.len())
}

fn char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    if bytes.get(index) == Some(&b'\\') {
        index += 2;
    } else {
        let character = std::str::from_utf8(bytes.get(index..)?)
            .ok()?
            .chars()
            .next()?;
        index += character.len_utf8();
    }
    (bytes.get(index) == Some(&b'\'')).then_some(index + 1)
}

fn scan_references(source: &str) -> Vec<SourceReference> {
    let tokens = tokenize(source);
    let mut references = Vec::new();
    let mut inline_modules: Vec<(usize, String)> = Vec::new();
    let mut pending_path = None;
    let mut brace_depth = 0usize;
    let mut index = 0usize;

    while index < tokens.len() {
        if tokens.get(index) == Some(&Token::Punct('#'))
            && tokens.get(index + 1) == Some(&Token::Punct('['))
            && let Some(end) = matching_delimiter(&tokens, index + 1, '[', ']')
        {
            if let Some(path) = path_attribute(&tokens[index + 2..end]) {
                pending_path = Some(path);
            }
            index = end + 1;
            continue;
        }

        if token_is_ident(tokens.get(index), "mod")
            && let Some(Token::Ident(name)) = tokens.get(index + 1)
        {
            match tokens.get(index + 2) {
                Some(Token::Punct(';')) => {
                    references.push(SourceReference::Module {
                        name: name.clone(),
                        path: pending_path.take(),
                        inline_modules: inline_module_names(&inline_modules),
                    });
                    index += 3;
                    continue;
                }
                Some(Token::Punct('{')) => {
                    brace_depth += 1;
                    inline_modules.push((brace_depth, name.clone()));
                    pending_path = None;
                    index += 3;
                    continue;
                }
                _ => {}
            }
        }

        if (token_is_ident(tokens.get(index), "include")
            || token_is_ident(tokens.get(index), "include_str"))
            && tokens.get(index + 1) == Some(&Token::Punct('!'))
            && tokens.get(index + 2) == Some(&Token::Punct('('))
            && let Some(Token::StringLiteral(path)) = tokens.get(index + 3)
            && Path::new(path).extension() == Some(OsStr::new("rs"))
        {
            references.push(SourceReference::Include {
                path: path.clone(),
                parse_as_rust: token_is_ident(tokens.get(index), "include"),
                inline_modules: inline_module_names(&inline_modules),
            });
        }

        match tokens.get(index) {
            Some(Token::Punct('{')) => {
                brace_depth += 1;
                pending_path = None;
            }
            Some(Token::Punct('}')) => {
                while inline_modules
                    .last()
                    .is_some_and(|(depth, _)| *depth == brace_depth)
                {
                    inline_modules.pop();
                }
                brace_depth = brace_depth.saturating_sub(1);
                pending_path = None;
            }
            Some(Token::Punct(';')) => pending_path = None,
            _ => {}
        }
        index += 1;
    }

    references
}

fn matching_delimiter(tokens: &[Token], start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::Punct(character) if *character == open => depth += 1,
            Token::Punct(character) if *character == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn path_attribute(tokens: &[Token]) -> Option<String> {
    match tokens {
        [
            Token::Ident(name),
            Token::Punct('='),
            Token::StringLiteral(path),
        ] if name == "path" => Some(path.clone()),
        _ => None,
    }
}

fn token_is_ident(token: Option<&Token>, expected: &str) -> bool {
    matches!(token, Some(Token::Ident(value)) if value == expected)
}

fn inline_module_names(modules: &[(usize, String)]) -> Vec<String> {
    modules.iter().map(|(_, name)| name.clone()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UseBinding {
    path: Vec<String>,
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
        Some("crate") if path.get(1).is_some_and(|segment| segment == "query") => {
            Some((Vec::new(), &path[2..]))
        }
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
        if candidate.len() == module.len() + 1 && candidate.starts_with(module) {
            if let Some(name) = candidate.last() {
                symbols.insert(name.clone());
            }
        }
    }
    symbols
}

fn display_query_module(module: &[String]) -> String {
    if module.is_empty() {
        "crate::query".to_string()
    } else {
        format!("crate::query::{}", module.join("::"))
    }
}

fn scan_use_bindings(tokens: &[Token]) -> Vec<UseBinding> {
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

fn scan_qualified_paths(tokens: &[Token]) -> Vec<(usize, Vec<String>)> {
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

fn scan_extern_crate_bindings(tokens: &[Token]) -> Vec<UseBinding> {
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
        if path.get(1).map(|segment| normalize_identifier(segment)) != Some("query".to_string()) {
            violations.insert(format!(
                "crate path must remain under crate::query: {}",
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
            violations.insert(format!("super path escapes src/query: {}", path.join("::")));
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
        if name == "macro_rules" {
            continue;
        }
        if !matches!(tokens.get(index + 2), Some(Token::Punct('(' | '[' | '{'))) {
            continue;
        }
        if !QUERY_ALLOWED_MACROS.contains(&name.as_str()) && !local_macros.contains(name) {
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
                    body == [
                        Token::Ident("allow".to_string()),
                        Token::Punct('('),
                        Token::Ident("deprecated".to_string()),
                        Token::Punct(')'),
                    ] || body
                        == [
                            Token::Ident("allow".to_string()),
                            Token::Punct('('),
                            Token::Ident("clippy".to_string()),
                            Token::Punct(':'),
                            Token::Punct(':'),
                            Token::Ident("too_many_arguments".to_string()),
                            Token::Punct(')'),
                        ]
                }
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
                            Token::Ident(key),
                            Token::Punct('='),
                            Token::StringLiteral(_),
                            Token::Punct(')')
                        ] if key == "rename"
                    ) || matches!(
                        body,
                        [
                            Token::Ident(_),
                            Token::Punct('('),
                            Token::Ident(default),
                            Token::Punct(','),
                            Token::Ident(skip),
                            Token::Punct('='),
                            Token::StringLiteral(_),
                            Token::Punct(')')
                        ] if default == "default" && skip == "skip_serializing_if"
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

fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character == '-' {
                '_'
            } else {
                character.to_ascii_lowercase()
            }
        })
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect()
}

fn resolve_reachable_sources(
    repository: &Path,
    target_roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut reachable = BTreeSet::new();
    let mut scanned = BTreeSet::new();
    let mut pending = VecDeque::new();

    for root in target_roots {
        let root = normalize_relative(root)?;
        pending.push_back(ScanContext {
            module_dir: root.parent().map_or_else(PathBuf::new, Path::to_path_buf),
            path: root,
        });
    }

    while let Some(context) = pending.pop_front() {
        reachable.insert(context.path.clone());
        if !scanned.insert(context.clone()) {
            continue;
        }
        let absolute = repository.join(&context.path);
        let source = fs::read_to_string(&absolute)
            .map_err(|error| format!("cannot read {}: {error}", absolute.display()))?;

        for reference in scan_references(&source) {
            match reference {
                SourceReference::Module {
                    name,
                    path,
                    inline_modules,
                } => {
                    if let Some(path) = path {
                        let mut base = context
                            .path
                            .parent()
                            .map_or_else(PathBuf::new, Path::to_path_buf);
                        base.extend(inline_modules);
                        let target = normalize_relative(&base.join(path))?;
                        enqueue_if_file(repository, &mut pending, target, None)?;
                    } else {
                        let mut module_dir = context.module_dir.clone();
                        module_dir.extend(inline_modules);
                        let child_module_dir = normalize_relative(&module_dir.join(&name))?;
                        for target in [
                            module_dir.join(format!("{name}.rs")),
                            module_dir.join(&name).join("mod.rs"),
                        ] {
                            enqueue_if_file(
                                repository,
                                &mut pending,
                                normalize_relative(&target)?,
                                Some(child_module_dir.clone()),
                            )?;
                        }
                    }
                }
                SourceReference::Include {
                    path,
                    parse_as_rust,
                    inline_modules,
                } => {
                    let parent = context
                        .path
                        .parent()
                        .map_or_else(PathBuf::new, Path::to_path_buf);
                    let target = normalize_relative(&parent.join(path))?;
                    if repository.join(&target).is_file() {
                        reachable.insert(target.clone());
                        if parse_as_rust {
                            let mut module_dir = context.module_dir.clone();
                            module_dir.extend(inline_modules);
                            pending.push_back(ScanContext {
                                path: target,
                                module_dir: normalize_relative(&module_dir)?,
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(reachable)
}

fn enqueue_if_file(
    repository: &Path,
    pending: &mut VecDeque<ScanContext>,
    path: PathBuf,
    module_dir: Option<PathBuf>,
) -> Result<(), String> {
    if repository.join(&path).is_file() {
        pending.push_back(ScanContext {
            module_dir: module_dir.unwrap_or_else(|| module_dir_for_file(&path)),
            path,
        });
    }
    Ok(())
}

fn module_dir_for_file(path: &Path) -> PathBuf {
    let parent = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    if path.file_name() == Some(OsStr::new("mod.rs")) {
        parent
    } else {
        path.file_stem()
            .map_or(parent.clone(), |stem| parent.join(stem))
    }
}

fn normalize_relative(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "source reference escapes repository root: {}",
                        path.display()
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Cargo and module paths must be repository-relative: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(normalized)
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    #[serde(default)]
    name: String,
    id: String,
    manifest_path: PathBuf,
    #[serde(default)]
    dependencies: Vec<CargoDependency>,
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoDependency {
    name: String,
    rename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    #[serde(default)]
    name: String,
    src_path: PathBuf,
    #[serde(default)]
    kind: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct CargoSourceLayout {
    target_roots: BTreeSet<PathBuf>,
    tracked_roots: BTreeSet<PathBuf>,
    workspace_manifests: BTreeSet<PathBuf>,
    pr8_violations: BTreeSet<String>,
}

fn cargo_source_layout(repository: &Path) -> Result<CargoSourceLayout, String> {
    let output = Command::new("cargo")
        .current_dir(repository)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(|error| format!("cannot run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    parse_cargo_source_layout(repository, &output.stdout)
}

fn parse_cargo_source_layout(
    repository: &Path,
    metadata_json: &[u8],
) -> Result<CargoSourceLayout, String> {
    let CargoMetadata {
        packages,
        workspace_members,
    } = serde_json::from_slice(metadata_json)
        .map_err(|error| format!("cannot parse cargo metadata: {error}"))?;
    let package_ids: BTreeSet<_> = packages.iter().map(|package| package.id.clone()).collect();
    let missing_members: Vec<_> = workspace_members.difference(&package_ids).collect();
    if !missing_members.is_empty() {
        return Err(format!(
            "cargo metadata omitted workspace packages: {missing_members:?}"
        ));
    }

    let mut target_roots = BTreeSet::new();
    let mut tracked_roots: BTreeSet<PathBuf> =
        REPOSITORY_SOURCE_ROOTS.iter().map(PathBuf::from).collect();
    let mut workspace_manifests = BTreeSet::new();
    let mut pr8_violations = BTreeSet::new();
    let mut target_snapshot = BTreeSet::new();

    for package in packages {
        if !workspace_members.contains(&package.id) {
            continue;
        }
        let manifest_path = metadata_path_relative(
            repository,
            &package.manifest_path,
            "workspace package manifest",
        )?;
        workspace_manifests.insert(manifest_path.clone());
        if let Some(expected_name) = expected_pr8_package_name(&manifest_path)
            && package.name != expected_name
        {
            pr8_violations.insert(format!(
                "{} must declare PR8 package name {expected_name}, found {}",
                manifest_path.display(),
                package.name
            ));
        }
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("manifest has no parent: {}", manifest_path.display()))?;
        if !package_root.as_os_str().is_empty() {
            tracked_roots.insert(package_root.to_path_buf());
        }

        if manifest_path == Path::new("Cargo.toml") {
            validate_query_dependency_aliases(&package.dependencies, &mut pr8_violations);
        } else if PR8_WORKSPACE_MANIFESTS
            .iter()
            .any(|allowed| manifest_path == Path::new(allowed))
        {
            validate_contract_package_dependencies(
                &manifest_path,
                &package.dependencies,
                &mut pr8_violations,
            );
        }

        for target in package.targets {
            let target_path =
                metadata_path_relative(repository, &target.src_path, "Cargo target source")?;
            let canonical_target_path =
                match canonical_repository_relative(repository, &target.src_path) {
                    Ok(path) => path,
                    Err(error) => {
                        pr8_violations.insert(format!(
                            "{} target {} has invalid source path: {error}",
                            manifest_path.display(),
                            target.name
                        ));
                        target_path.clone()
                    }
                };
            validate_pr8_target(
                &manifest_path,
                &package.name,
                &target,
                &canonical_target_path,
                &mut pr8_violations,
            );
            target_snapshot.insert(format!(
                "{}|{}|{}|{}",
                package.name,
                target.name,
                target.kind.join(","),
                canonical_target_path.display()
            ));
            target_roots.insert(target_path);
        }
    }

    if target_roots.is_empty() {
        return Err("cargo metadata exposes no workspace Rust targets".to_string());
    }
    for target_root in &target_roots {
        if !tracked_roots
            .iter()
            .any(|source_root| target_root.starts_with(source_root))
        {
            tracked_roots.insert(target_root.clone());
        }
    }

    let expected_manifests: BTreeSet<_> =
        PR8_WORKSPACE_MANIFESTS.iter().map(PathBuf::from).collect();
    for missing in expected_manifests.difference(&workspace_manifests) {
        pr8_violations.insert(format!(
            "required PR8 workspace member is missing: {}",
            missing.display()
        ));
    }
    for extra in workspace_manifests.difference(&expected_manifests) {
        pr8_violations.insert(format!(
            "additional PR8 workspace member is forbidden: {}",
            extra.display()
        ));
    }
    let expected_targets: BTreeSet<_> = PR8_TARGET_SNAPSHOT
        .iter()
        .map(|target| target.to_string())
        .collect();
    for missing in expected_targets.difference(&target_snapshot) {
        pr8_violations.insert(format!("required PR8 Cargo target is missing: {missing}"));
    }
    for extra in target_snapshot.difference(&expected_targets) {
        pr8_violations.insert(format!("additional PR8 Cargo target is forbidden: {extra}"));
    }

    Ok(CargoSourceLayout {
        target_roots,
        tracked_roots,
        workspace_manifests,
        pr8_violations,
    })
}

fn expected_pr8_package_name(manifest_path: &Path) -> Option<&'static str> {
    match manifest_path.to_str() {
        Some("Cargo.toml") => Some("tracedecay"),
        Some("crates/tracedecay-domain/Cargo.toml") => Some("tracedecay-domain"),
        Some("crates/tracedecay-store/Cargo.toml") => Some("tracedecay-store"),
        _ => None,
    }
}

fn validate_query_dependency_aliases(
    dependencies: &[CargoDependency],
    violations: &mut BTreeSet<String>,
) {
    for dependency in dependencies {
        let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
        if let Some(rename) = &dependency.rename
            && !PR8_ROOT_PACKAGE_ALIASES
                .iter()
                .any(|(allowed_alias, package)| {
                    rename.as_str() == *allowed_alias && dependency.name.as_str() == *package
                })
        {
            violations.insert(format!(
                "root package dependency alias {rename} -> {} is not in the exact PR8 alias snapshot",
                dependency.name
            ));
        }
        let normalized_alias = normalize_identifier(alias);
        let Some(expected_package) = allowed_package_for_query_root(&normalized_alias) else {
            continue;
        };
        if normalize_identifier(&dependency.name) != normalize_identifier(expected_package) {
            violations.insert(format!(
                "dependency alias {alias} maps allowlisted query root {normalized_alias} to non-allowlisted package {}",
                dependency.name
            ));
        }
    }
}

fn validate_contract_package_dependencies(
    manifest_path: &Path,
    dependencies: &[CargoDependency],
    violations: &mut BTreeSet<String>,
) {
    for dependency in dependencies {
        let alias = dependency.rename.as_deref().unwrap_or(&dependency.name);
        let normalized_alias = normalize_identifier(alias);
        let package_allowed = QUERY_ALLOWED_PACKAGES
            .iter()
            .any(|allowed| normalize_identifier(allowed) == normalize_identifier(&dependency.name));
        let alias_matches_package =
            allowed_package_for_query_root(&normalized_alias).is_some_and(|expected| {
                normalize_identifier(expected) == normalize_identifier(&dependency.name)
            });
        if !package_allowed || !alias_matches_package {
            violations.insert(format!(
                "{} contract dependency {alias} -> {} is outside the pure query package allowlist",
                manifest_path.display(),
                dependency.name
            ));
        }
    }
}

fn allowed_package_for_query_root(root: &str) -> Option<&'static str> {
    QUERY_ALLOWED_PACKAGES
        .iter()
        .copied()
        .find(|package| normalize_identifier(package) == root)
}

fn validate_pr8_target(
    manifest_path: &Path,
    package_name: &str,
    target: &CargoTarget,
    target_path: &Path,
    violations: &mut BTreeSet<String>,
) {
    if target.kind.len() != 1 {
        violations.insert(format!(
            "{} package {package_name} target {} has non-exact kinds {:?}",
            manifest_path.display(),
            target.name,
            target.kind
        ));
    }
    if target_path.starts_with("src/query")
        || target_path
            .components()
            .any(|component| matches!(component, Component::Normal(name) if matches!(normalize_identifier(name.to_string_lossy().as_ref()).as_str(), "query" | "kernel")))
    {
        violations.insert(format!(
            "{} package {package_name} exposes query code as {:?} target {} at {}",
            manifest_path.display(),
            target.kind,
            target.name,
            target_path.display()
        ));
    }
    if matches!(
        normalize_identifier(&target.name).as_str(),
        "query" | "query_kernel" | "temporal_query" | "temporal_kernel"
    ) {
        violations.insert(format!(
            "{} package {package_name} exposes reserved query/kernel target name {} ({:?})",
            manifest_path.display(),
            target.name,
            target.kind
        ));
    }
}

fn canonical_repository_relative(repository: &Path, path: &Path) -> Result<PathBuf, String> {
    let canonical_repository = fs::canonicalize(repository)
        .map_err(|error| format!("cannot canonicalize {}: {error}", repository.display()))?;
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot canonicalize {}: {error}", path.display()))?;
    let relative = canonical.strip_prefix(&canonical_repository).map_err(|_| {
        format!(
            "{} resolves outside repository to {}",
            path.display(),
            canonical.display()
        )
    })?;
    normalize_relative(relative)
}

fn metadata_path_relative(
    repository: &Path,
    path: &Path,
    description: &str,
) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{description} path is not absolute: {}",
            path.display()
        ));
    }
    let relative = path.strip_prefix(repository).map_err(|_| {
        format!(
            "{description} path is outside repository: {}",
            path.display()
        )
    })?;
    normalize_relative(relative)
}

fn git_tracked_rust_sources(
    repository: &Path,
    source_roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let tracked = git_tracked_paths(repository)?;
    // Validate the live worktree rather than assuming the index and filesystem
    // are identical. During a normal unstaged module move, `git ls-files`
    // still names the deleted source while the replacement module is
    // intentionally untracked. Missing index entries are excluded here and
    // the filesystem walk below adds their live replacements.
    let live_tracked: Vec<_> = tracked
        .into_iter()
        .filter(|path| fs::symlink_metadata(repository.join(path)).is_ok())
        .collect();
    let physical = inspect_physical_manifest_paths(repository, &live_tracked)?;
    if !physical.violations.is_empty() {
        return Err(format!(
            "tracked path contract violations:\n{}",
            physical
                .violations
                .iter()
                .map(|violation| format!("  - {violation}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let mut sources = BTreeSet::new();
    for path in live_tracked {
        if path.extension() != Some(OsStr::new("rs"))
            || !source_roots.iter().any(|root| path.starts_with(root))
        {
            continue;
        }
        let canonical = canonical_repository_relative(repository, &repository.join(&path))?;
        if !repository.join(&canonical).is_file() {
            return Err(format!(
                "tracked Rust source does not resolve to a file: {}",
                path.display()
            ));
        }
        sources.insert(normalize_relative(&path)?);
    }
    sources.extend(
        physical
            .symlinked_rust_sources
            .into_iter()
            .filter(|path| source_roots.iter().any(|root| path.starts_with(root))),
    );
    sources.extend(filesystem_rust_sources(repository, source_roots)?);
    Ok(sources)
}

fn filesystem_rust_sources(
    repository: &Path,
    source_roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut pending: Vec<_> = source_roots
        .iter()
        .map(|root| repository.join(root))
        .collect();
    let mut sources = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let entries = fs::read_dir(&path).map_err(|error| {
                format!("cannot read source directory '{}': {error}", path.display())
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    format!(
                        "cannot read entry in source directory '{}': {error}",
                        path.display()
                    )
                })?;
                let file_type = entry.file_type().map_err(|error| {
                    format!(
                        "cannot inspect source path '{}': {error}",
                        entry.path().display()
                    )
                })?;
                if file_type.is_dir() {
                    pending.push(entry.path());
                } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new("rs"))
                {
                    let entry_path = entry.path();
                    let relative = entry_path.strip_prefix(repository).map_err(|_| {
                        format!(
                            "source path is outside repository: {}",
                            entry_path.display()
                        )
                    })?;
                    sources.insert(normalize_relative(relative)?);
                }
            }
        }
    }
    Ok(sources)
}

fn query_kernel_sources(repository: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let source_roots = [PathBuf::from("src/query")].into_iter().collect();
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
                .filter(|path| path.starts_with("src/query")),
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

fn query_kernel_violations(
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
    let query_root = repository.join("src/query");
    fs::canonicalize(&query_root)
        .map_err(|error| format!("cannot canonicalize {}: {error}", query_root.display()))?;
    let canonical_repository = fs::canonicalize(repository)
        .map_err(|error| format!("cannot canonicalize {}: {error}", repository.display()))?;
    let root = PathBuf::from("src/query/mod.rs");
    let mut graph = QueryModuleGraph::default();
    if !sources.contains(&root) {
        graph
            .violations
            .insert("src/query/mod.rs is required as the single query module root".to_string());
        return Ok(graph);
    }

    let mut reachable = BTreeSet::new();
    let mut scanned = BTreeSet::new();
    let mut pending = VecDeque::from([QueryScanContext {
        path: root,
        module_dir: PathBuf::from("src/query"),
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
            graph
                .symbols
                .entry(full_scope)
                .or_default()
                .extend(module_level_query_names(&tokens, &scope));
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
                        "{}: #[path = {path:?}] is forbidden; query modules must follow the src/query file convention",
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
            "{} is not reachable from src/query/mod.rs through conventional mod declarations",
            unreachable.display()
        ));
    }
    Ok(graph)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestClassification {
    FirstParty,
    Fixture,
    Tooling,
    Vendor,
}

#[derive(Debug, PartialEq, Eq)]
struct PhysicalManifestLayout {
    manifests: BTreeSet<PathBuf>,
    symlinked_rust_sources: BTreeSet<PathBuf>,
    violations: BTreeSet<String>,
}

fn physical_manifest_layout(repository: &Path) -> Result<PhysicalManifestLayout, String> {
    let tracked = git_tracked_paths(repository)?;
    let live_tracked: Vec<_> = tracked
        .into_iter()
        .filter(|path| fs::symlink_metadata(repository.join(path)).is_ok())
        .collect();
    inspect_physical_manifest_paths(repository, &live_tracked)
}

fn git_tracked_paths(repository: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|error| {
            format!("cannot list tracked paths for Cargo manifest contract: {error}")
        })?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed while discovering Cargo manifests: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            std::str::from_utf8(bytes)
                .map(PathBuf::from)
                .map_err(|error| format!("git-tracked path is not UTF-8: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()
}

fn inspect_physical_manifest_paths(
    repository: &Path,
    tracked_paths: &[PathBuf],
) -> Result<PhysicalManifestLayout, String> {
    let canonical_repository = fs::canonicalize(repository)
        .map_err(|error| format!("cannot canonicalize {}: {error}", repository.display()))?;
    let mut candidates = BTreeSet::new();
    let mut symlinked_rust_sources = BTreeSet::new();
    let mut violations = BTreeSet::new();
    for tracked in tracked_paths {
        if tracked.file_name() == Some(OsStr::new("Cargo.toml")) {
            candidates.insert(normalize_relative(tracked)?);
        }
        let absolute = repository.join(tracked);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) => {
                violations.insert(format!(
                    "cannot inspect tracked path {}: {error}",
                    tracked.display()
                ));
                continue;
            }
        };
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let canonical = fs::canonicalize(&absolute).map_err(|error| {
            format!(
                "cannot resolve tracked symlink {}: {error}",
                tracked.display()
            )
        })?;
        if !canonical.starts_with(&canonical_repository) {
            violations.insert(format!(
                "tracked symlink {} resolves outside the repository to {}",
                tracked.display(),
                canonical.display()
            ));
            continue;
        }
        if canonical.is_dir() && canonical.join("Cargo.toml").is_file() {
            candidates.insert(normalize_relative(&tracked.join("Cargo.toml"))?);
        } else if canonical.file_name() == Some(OsStr::new("Cargo.toml")) {
            candidates.insert(normalize_relative(tracked)?);
        }
        if canonical.is_file()
            && (tracked.extension() == Some(OsStr::new("rs"))
                || canonical.extension() == Some(OsStr::new("rs")))
        {
            symlinked_rust_sources.insert(normalize_relative(tracked)?);
        } else if canonical.is_dir() {
            collect_symlinked_rust_sources(
                &canonical_repository,
                &canonical,
                tracked,
                &mut symlinked_rust_sources,
                &mut violations,
            )?;
        }
    }

    let expected: BTreeSet<_> = PR8_WORKSPACE_MANIFESTS.iter().map(PathBuf::from).collect();
    let mut manifests = BTreeSet::new();
    let mut canonical_owners = BTreeMap::<PathBuf, PathBuf>::new();
    for logical in candidates {
        if manifest_classification(&logical) != ManifestClassification::FirstParty {
            continue;
        }
        manifests.insert(logical.clone());
        let absolute = repository.join(&logical);
        let canonical = match fs::canonicalize(&absolute) {
            Ok(canonical) => canonical,
            Err(error) => {
                violations.insert(format!(
                    "cannot canonicalize tracked first-party manifest {}: {error}",
                    logical.display()
                ));
                continue;
            }
        };
        if !canonical.starts_with(&canonical_repository) {
            violations.insert(format!(
                "tracked first-party manifest {} resolves outside the repository to {}",
                logical.display(),
                canonical.display()
            ));
            continue;
        }
        if let Some(other) = canonical_owners.insert(canonical.clone(), logical.clone())
            && other != logical
        {
            violations.insert(format!(
                "tracked manifest symlink aliases the same physical crate: {} and {} -> {}",
                other.display(),
                logical.display(),
                canonical.display()
            ));
        }
        if !expected.contains(&logical) {
            violations.insert(format!(
                "additional tracked first-party Cargo package is forbidden by PR8: {} ({})",
                logical.display(),
                physical_manifest_description(&absolute)?
            ));
        }
    }

    for missing in expected.difference(&manifests) {
        violations.insert(format!(
            "required tracked first-party Cargo manifest is missing: {}",
            missing.display()
        ));
    }
    Ok(PhysicalManifestLayout {
        manifests,
        symlinked_rust_sources,
        violations,
    })
}

fn collect_symlinked_rust_sources(
    canonical_repository: &Path,
    physical_root: &Path,
    logical_root: &Path,
    sources: &mut BTreeSet<PathBuf>,
    violations: &mut BTreeSet<String>,
) -> Result<(), String> {
    let mut pending = VecDeque::from([(physical_root.to_path_buf(), logical_root.to_path_buf())]);
    let mut visited = BTreeSet::new();
    while let Some((physical, logical)) = pending.pop_front() {
        let canonical_directory = fs::canonicalize(&physical)
            .map_err(|error| format!("cannot canonicalize {}: {error}", physical.display()))?;
        if !visited.insert(canonical_directory.clone()) {
            continue;
        }
        for entry in fs::read_dir(&canonical_directory)
            .map_err(|error| format!("cannot read {}: {error}", canonical_directory.display()))?
        {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot read entry in {}: {error}",
                    canonical_directory.display()
                )
            })?;
            let canonical = fs::canonicalize(entry.path())
                .map_err(|error| format!("cannot resolve {}: {error}", entry.path().display()))?;
            let logical = logical.join(entry.file_name());
            if !canonical.starts_with(canonical_repository) {
                violations.insert(format!(
                    "tracked symlink descendant {} resolves outside the repository to {}",
                    logical.display(),
                    canonical.display()
                ));
            } else if canonical.is_dir() {
                pending.push_back((canonical, logical));
            } else if canonical.is_file() && canonical.extension() == Some(OsStr::new("rs")) {
                sources.insert(normalize_relative(&logical)?);
            }
        }
    }
    Ok(())
}

fn manifest_classification(path: &Path) -> ManifestClassification {
    let components: Vec<_> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect();
    if components.first() == Some(&"vendor") {
        ManifestClassification::Vendor
    } else if components.starts_with(&["tests", "fixtures"])
        || components.starts_with(&["eval", "hermetic", "fixtures"])
        || components.starts_with(&["evals", "agent_adoption", "fixture"])
    {
        ManifestClassification::Fixture
    } else if components
        .first()
        .is_some_and(|root| matches!(*root, ".git" | ".worktrees" | "target" | "node_modules"))
    {
        ManifestClassification::Tooling
    } else {
        ManifestClassification::FirstParty
    }
}

fn physical_manifest_description(manifest_path: &Path) -> Result<String, String> {
    let source = fs::read_to_string(manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let manifest: toml::Table = toml::from_str(&source)
        .map_err(|error| format!("cannot parse {}: {error}", manifest_path.display()))?;
    let package_name = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or("<virtual>");
    let mut targets = Vec::new();
    if let Some(lib) = manifest.get("lib").and_then(toml::Value::as_table) {
        targets.push(format!(
            "lib {} at {}",
            lib.get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or(package_name),
            lib.get("path")
                .and_then(toml::Value::as_str)
                .unwrap_or("src/lib.rs")
        ));
    }
    for (kind, key) in [("bin", "bin"), ("bench", "bench")] {
        if let Some(entries) = manifest.get(key).and_then(toml::Value::as_array) {
            for entry in entries.iter().filter_map(toml::Value::as_table) {
                targets.push(format!(
                    "{kind} {} at {}",
                    entry
                        .get("name")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("<default>"),
                    entry
                        .get("path")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("<default>")
                ));
            }
        }
    }
    Ok(if targets.is_empty() {
        format!("package {package_name}; default targets")
    } else {
        format!("package {package_name}; {}", targets.join(", "))
    })
}

#[test]
fn git_tracked_rust_sources_are_reachable_from_cargo_targets() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let layout = cargo_source_layout(repository).expect("discover Cargo workspace Rust targets");
    let reachable = resolve_reachable_sources(repository, &layout.target_roots)
        .expect("resolve Rust module/include graph");
    let tracked = git_tracked_rust_sources(repository, &layout.tracked_roots)
        .expect("list git-tracked workspace Rust sources");
    let allowlisted: BTreeSet<PathBuf> = INTENTIONAL_STANDALONE_RUST_INPUTS
        .iter()
        .map(|path| PathBuf::from(*path))
        .collect();
    let stale_allowlist: Vec<_> = allowlisted.difference(&tracked).collect();
    assert!(
        stale_allowlist.is_empty(),
        "standalone Rust input allowlist contains untracked or deleted paths: {stale_allowlist:?}"
    );
    let reachable_allowlist: Vec<_> = allowlisted.intersection(&reachable).collect();
    assert!(
        reachable_allowlist.is_empty(),
        "Rust inputs are now reachable and should leave the standalone allowlist: {reachable_allowlist:?}"
    );
    let orphaned: Vec<_> = tracked
        .difference(&reachable)
        .filter(|path| !allowlisted.contains(*path))
        .collect();

    assert!(
        orphaned.is_empty(),
        "git-tracked Rust files are not reachable from any Cargo target:\n{}\n\
         Register each file from a target/module root, or document a genuinely standalone source \
         input in INTENTIONAL_STANDALONE_RUST_INPUTS.",
        orphaned
            .iter()
            .map(|path| format!("  - {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn metadata_layout_includes_workspace_targets_and_scopes_tracked_sources() {
    let temporary = tempfile::tempdir().expect("create metadata fixture");
    let repository = temporary.path();
    let root_id = "path+file:///workspace#root@0.1.0";
    let domain_id = "path+file:///workspace/crates/domain#domain@0.1.0";
    let store_id = "path+file:///workspace/crates/store#store@0.1.0";
    let metadata = serde_json::json!({
        "packages": [
            {
                "id": root_id,
                "name": "tracedecay",
                "manifest_path": repository.join("Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("src/lib.rs") },
                    { "src_path": repository.join("src/main.rs") },
                    { "src_path": repository.join("build.rs") }
                ]
            },
            {
                "id": domain_id,
                "name": "tracedecay-domain",
                "manifest_path": repository.join("crates/tracedecay-domain/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-domain/src/lib.rs") },
                    { "src_path": repository.join("crates/tracedecay-domain/tests/boundary.rs") }
                ]
            },
            {
                "id": store_id,
                "name": "tracedecay-store",
                "manifest_path": repository.join("crates/tracedecay-store/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-store/src/lib.rs") }
                ]
            },
            {
                "id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
                "manifest_path": "/outside/registry/serde/Cargo.toml",
                "targets": [{ "src_path": "/outside/registry/serde/src/lib.rs" }]
            }
        ],
        "workspace_members": [root_id, domain_id, store_id]
    });

    let layout = parse_cargo_source_layout(
        repository,
        &serde_json::to_vec(&metadata).expect("serialize metadata fixture"),
    )
    .expect("parse metadata fixture");

    assert_eq!(
        layout.target_roots,
        [
            PathBuf::from("build.rs"),
            PathBuf::from("crates/tracedecay-domain/src/lib.rs"),
            PathBuf::from("crates/tracedecay-domain/tests/boundary.rs"),
            PathBuf::from("crates/tracedecay-store/src/lib.rs"),
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/main.rs"),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        layout.tracked_roots,
        [
            PathBuf::from("benches"),
            PathBuf::from("build.rs"),
            PathBuf::from("crates/tracedecay-domain"),
            PathBuf::from("crates/tracedecay-store"),
            PathBuf::from("examples"),
            PathBuf::from("src"),
            PathBuf::from("tests"),
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        layout.workspace_manifests,
        PR8_WORKSPACE_MANIFESTS.iter().map(PathBuf::from).collect()
    );
}

#[test]
fn scanner_follows_modules_path_attributes_and_literal_rust_includes() {
    let references = scan_references(
        r##"
        // mod commented_out;
        const TEXT: &str = "mod string_literal; include!(\"also_ignored.rs\");";
        #[cfg(test)]
        #[path = r#"alternate/scenario.rs"#]
        mod scenario;
        mod ordinary;
        mod inline {
            mod nested;
            include!("fragment.rs");
        }
        include_str!("fixture.rs");
        include_str!("not_rust.txt");
        "##,
    );

    assert!(references.contains(&SourceReference::Module {
        name: "scenario".to_string(),
        path: Some("alternate/scenario.rs".to_string()),
        inline_modules: Vec::new(),
    }));
    assert!(references.contains(&SourceReference::Module {
        name: "nested".to_string(),
        path: None,
        inline_modules: vec!["inline".to_string()],
    }));
    assert!(references.contains(&SourceReference::Include {
        path: "fragment.rs".to_string(),
        parse_as_rust: true,
        inline_modules: vec!["inline".to_string()],
    }));
    assert!(references.contains(&SourceReference::Include {
        path: "fixture.rs".to_string(),
        parse_as_rust: false,
        inline_modules: Vec::new(),
    }));
    assert!(!references.iter().any(|reference| {
        matches!(reference, SourceReference::Module { name, .. } if name == "commented_out" || name == "string_literal")
    }));
}

#[test]
fn resolver_exposes_a_forgotten_decomposed_test_scenario() {
    let temporary = tempfile::tempdir().expect("create resolver fixture");
    let repository = temporary.path();
    fs::create_dir_all(repository.join("tests/suite/registered")).unwrap();
    fs::write(repository.join("tests/suite/main.rs"), "mod registered;\n").unwrap();
    fs::write(
        repository.join("tests/suite/registered.rs"),
        "mod helper;\n",
    )
    .unwrap();
    fs::write(
        repository.join("tests/suite/registered/helper.rs"),
        "pub fn helper() {}\n",
    )
    .unwrap();
    fs::write(
        repository.join("tests/suite/forgotten_scenario.rs"),
        "#[test] fn silently_unregistered() {}\n",
    )
    .unwrap();

    let roots = [PathBuf::from("tests/suite/main.rs")].into_iter().collect();
    let reachable = resolve_reachable_sources(repository, &roots).unwrap();

    assert!(reachable.contains(Path::new("tests/suite/registered.rs")));
    assert!(reachable.contains(Path::new("tests/suite/registered/helper.rs")));
    assert!(!reachable.contains(Path::new("tests/suite/forgotten_scenario.rs")));
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
        ("bare forbidden type", "GlobalDb::open();", "globaldb"),
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
        "libsql",
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
        /* GlobalDb::open(); crate::dashboard::Dashboard::new(); */
        const PROSE: &str = "mcp::Server and crate::automation::run are not references";
        const RAW_PROSE: &str = r#"rusqlite::Connection and crate::transport::send"#;

        use {::serde::Serialize, std::collections::BTreeSet};
        use tracedecay_domain::session::SessionId;
        use tracedecay_store::memory::StorePort;
        use crate::query::temporal::ports::TemporalReadPort;

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
    fs::create_dir_all(repository.join("src/query")).unwrap();
    fs::write(
        repository.join("src/query/mod.rs"),
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
    fs::create_dir_all(repository.join("src/query/temporal")).unwrap();
    fs::write(repository.join("src/query/mod.rs"), "pub mod temporal;\n").unwrap();
    fs::write(
        repository.join("src/query/temporal/mod.rs"),
        "mod ports;\nuse self::ports::Port;\nstruct Kernel(Port);\n",
    )
    .unwrap();
    fs::write(
        repository.join("src/query/temporal/ports.rs"),
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
fn metadata_contract_rejects_package_aliases_extra_members_and_query_targets() {
    let temporary = tempfile::tempdir().expect("create metadata contract fixture");
    let repository = temporary.path();
    let root_id = "path+file:///workspace#root@0.1.0";
    let domain_id = "path+file:///workspace/crates/domain#domain@0.1.0";
    let store_id = "path+file:///workspace/crates/store#store@0.1.0";
    let neutral_id = "path+file:///workspace/components/engine#engine@0.1.0";
    let metadata = serde_json::json!({
        "packages": [
            {
                "id": root_id,
                "name": "tracedecay",
                "manifest_path": repository.join("Cargo.toml"),
                "dependencies": [
                    { "name": "sqlx", "rename": "serde" }
                ],
                "targets": [
                    {
                        "kind": ["lib"],
                        "name": "tracedecay",
                        "src_path": repository.join("src/lib.rs")
                    },
                    {
                        "kind": ["bin"],
                        "name": "temporal-kernel",
                        "src_path": repository.join("src/engine.rs")
                    },
                    {
                        "kind": ["example"],
                        "name": "neutral_example",
                        "src_path": repository.join("examples/neutral.rs")
                    },
                    {
                        "kind": ["test"],
                        "name": "neutral_test",
                        "src_path": repository.join("tests/neutral.rs")
                    },
                    {
                        "kind": ["custom-build"],
                        "name": "build-script-build",
                        "src_path": repository.join("build-neutral.rs")
                    }
                ]
            },
            {
                "id": domain_id,
                "name": "tracedecay-domain",
                "manifest_path": repository.join("crates/tracedecay-domain/Cargo.toml"),
                "targets": []
            },
            {
                "id": store_id,
                "name": "tracedecay-store",
                "manifest_path": repository.join("crates/tracedecay-store/Cargo.toml"),
                "dependencies": [
                    { "name": "mongodb", "rename": "serde_json" }
                ],
                "targets": []
            },
            {
                "id": neutral_id,
                "name": "engine",
                "manifest_path": repository.join("components/engine/Cargo.toml"),
                "targets": []
            }
        ],
        "workspace_members": [root_id, domain_id, store_id, neutral_id]
    });
    let layout = parse_cargo_source_layout(
        repository,
        &serde_json::to_vec(&metadata).expect("serialize metadata fixture"),
    )
    .expect("parse metadata fixture");

    for expected in [
        "alias serde",
        "package sqlx",
        "contract dependency serde_json -> mongodb",
        "temporal-kernel",
        "neutral_example",
        "neutral_test",
        "build-neutral.rs",
        "components/engine",
    ] {
        assert!(
            layout
                .pr8_violations
                .iter()
                .any(|violation| violation.contains(expected)),
            "metadata contract missed {expected}: {:?}",
            layout.pr8_violations
        );
    }
}

#[test]
fn physical_manifest_contract_classifies_paths_without_name_heuristics() {
    let temporary = tempfile::tempdir().expect("create physical manifest fixture");
    let repository = temporary.path();
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"tracedecay\"\nversion = \"0.1.0\"\n",
        ),
        (
            "crates/tracedecay-domain/Cargo.toml",
            "[package]\nname = \"tracedecay-domain\"\nversion = \"0.1.0\"\n",
        ),
        (
            "crates/tracedecay-store/Cargo.toml",
            "[package]\nname = \"tracedecay-store\"\nversion = \"0.1.0\"\n",
        ),
        (
            "components/engine/Cargo.toml",
            "[package]\nname = \"engine\"\nversion = \"0.1.0\"\n\
             [lib]\nname = \"query_engine\"\npath = \"src/core.rs\"\n",
        ),
        (
            "vendor/upstream/Cargo.toml",
            "[package]\nname = \"query-vendor\"\nversion = \"0.1.0\"\n",
        ),
        (
            "tests/fixtures/query-project/Cargo.toml",
            "[package]\nname = \"query-fixture\"\nversion = \"0.1.0\"\n",
        ),
    ];
    for (path, source) in files {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    let tracked = files
        .iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect::<Vec<_>>();
    let layout =
        inspect_physical_manifest_paths(repository, &tracked).expect("inspect tracked manifests");

    assert!(
        layout.violations.iter().any(|violation| {
            violation.contains("components/engine/Cargo.toml")
                && violation.contains("package engine")
                && violation.contains("lib query_engine")
        }),
        "neutral excluded package escaped the physical contract: {:?}",
        layout.violations
    );
    assert!(
        !layout
            .manifests
            .contains(Path::new("vendor/upstream/Cargo.toml"))
    );
    assert!(
        !layout
            .manifests
            .contains(Path::new("tests/fixtures/query-project/Cargo.toml"))
    );
}

#[cfg(unix)]
#[test]
fn physical_manifest_contract_rejects_symlinked_crates() {
    let temporary = tempfile::tempdir().expect("create symlinked manifest fixture");
    let repository = temporary.path();
    for path in PR8_WORKSPACE_MANIFESTS {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").unwrap();
    }
    fs::create_dir_all(repository.join("components")).unwrap();
    symlink(
        repository.join("crates/tracedecay-domain"),
        repository.join("components/engine"),
    )
    .unwrap();
    let mut tracked = PR8_WORKSPACE_MANIFESTS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    tracked.push(PathBuf::from("components/engine"));

    let layout =
        inspect_physical_manifest_paths(repository, &tracked).expect("inspect symlinked manifest");
    assert!(
        layout
            .violations
            .iter()
            .any(|violation| violation.contains("symlink aliases the same physical crate")),
        "symlinked crate escaped canonical-path inspection: {:?}",
        layout.violations
    );
}

#[cfg(unix)]
#[test]
fn physical_manifest_contract_rejects_outside_rust_symlinks() {
    let temporary = tempfile::tempdir().expect("create Rust symlink fixture");
    let repository = temporary.path().join("repository");
    fs::create_dir_all(repository.join("src/query")).unwrap();
    let outside = temporary.path().join("outside.rs");
    fs::write(&outside, "sqlx::Pool::connect();\n").unwrap();
    symlink(&outside, repository.join("src/query/linked.rs")).unwrap();

    let layout =
        inspect_physical_manifest_paths(&repository, &[PathBuf::from("src/query/linked.rs")])
            .expect("inspect tracked Rust symlink");
    assert!(
        layout
            .violations
            .iter()
            .any(|violation| violation.contains("outside the repository")),
        "outside Rust symlink escaped canonical inspection: {:?}",
        layout.violations
    );
}

#[cfg(unix)]
#[test]
fn physical_manifest_contract_discovers_inside_rust_symlinks() {
    let temporary = tempfile::tempdir().expect("create inside Rust symlink fixture");
    let repository = temporary.path();
    for path in PR8_WORKSPACE_MANIFESTS {
        let path = repository.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n").unwrap();
    }
    fs::create_dir_all(repository.join("src/query")).unwrap();
    fs::create_dir_all(repository.join("shared")).unwrap();
    fs::write(repository.join("src/query/mod.rs"), "mod safe;\n").unwrap();
    fs::write(repository.join("shared/safe.rs"), "pub struct Safe;\n").unwrap();
    symlink(
        repository.join("shared/safe.rs"),
        repository.join("src/query/safe.rs"),
    )
    .unwrap();
    let mut tracked = PR8_WORKSPACE_MANIFESTS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    tracked.push(PathBuf::from("src/query/safe.rs"));

    let layout =
        inspect_physical_manifest_paths(repository, &tracked).expect("inspect inside Rust symlink");
    assert!(
        layout.violations.is_empty(),
        "inside Rust symlink should be inspectable: {:?}",
        layout.violations
    );
    assert!(
        layout
            .symlinked_rust_sources
            .contains(Path::new("src/query/safe.rs"))
    );
    let sources = [
        PathBuf::from("src/query/mod.rs"),
        PathBuf::from("src/query/safe.rs"),
    ]
    .into_iter()
    .collect();
    assert!(
        query_kernel_violations(repository, &sources)
            .expect("inspect in-repository symlinked Rust source")
            .is_empty(),
        "in-repository Rust symlink target must be fully scanned"
    );
}

#[test]
fn temporal_kernel_sources_respect_dependency_boundary() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let physical =
        physical_manifest_layout(&repository).expect("inspect tracked physical Cargo manifests");
    assert!(
        physical.violations.is_empty(),
        "PR8 permits exactly the root/domain/store first-party Cargo packages:\n{}",
        physical
            .violations
            .iter()
            .map(|violation| format!("  - {violation}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let layout = cargo_source_layout(&repository).expect("inspect Cargo workspace membership");
    assert!(
        layout.pr8_violations.is_empty(),
        "PR8 workspace/dependency/target contract violations:\n{}",
        layout
            .pr8_violations
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

fn path_matches_forbidden_prefix(path: &[String], prefixes: &[&[&str]]) -> Option<String> {
    for prefix in prefixes {
        if path.len() >= prefix.len()
            && path
                .iter()
                .zip(prefix.iter())
                .all(|(segment, expected)| segment == *expected)
        {
            return Some(prefix.join("::"));
        }
    }
    None
}

fn forbidden_path_violations(source: &str, path: &Path, prefixes: &[&[&str]]) -> BTreeSet<String> {
    let tokens = tokenize(source);
    let mut violations = BTreeSet::new();
    for binding in scan_use_bindings(&tokens) {
        if let Some(forbidden) = path_matches_forbidden_prefix(&binding.path, prefixes) {
            violations.insert(format!(
                "{}: imports forbidden path {forbidden}",
                path.display()
            ));
        }
    }
    for binding in scan_extern_crate_bindings(&tokens) {
        if let Some(forbidden) = path_matches_forbidden_prefix(&binding.path, prefixes) {
            violations.insert(format!(
                "{}: extern crate forbidden path {forbidden}",
                path.display()
            ));
        }
    }
    for (_, qualified) in scan_qualified_paths(&tokens) {
        if let Some(forbidden) = path_matches_forbidden_prefix(&qualified, prefixes) {
            violations.insert(format!(
                "{}: references forbidden path {forbidden}",
                path.display()
            ));
        }
    }
    violations
}

fn scan_sources_for_forbidden_paths(
    repository: &Path,
    sources: &BTreeSet<PathBuf>,
    prefixes: &[&[&str]],
) -> Result<BTreeSet<String>, String> {
    let mut violations = BTreeSet::new();
    for path in sources {
        let absolute = repository.join(path);
        let source = fs::read_to_string(&absolute)
            .map_err(|error| format!("cannot read {}: {error}", absolute.display()))?;
        violations.extend(forbidden_path_violations(&source, path, prefixes));
    }
    Ok(violations)
}

#[test]
fn application_session_depends_on_ports_not_adapters() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = [PathBuf::from("src/application/session")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let sources =
        filesystem_rust_sources(&repository, &roots).expect("resolve application session sources");
    assert!(
        !sources.is_empty(),
        "application session sources must exist"
    );

    let forbidden: &[&[&str]] = &[
        &["crate", "global_db"],
        &["crate", "store"],
        &["crate", "daemon"],
        &["crate", "mcp"],
        &["crate", "sessions"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect application session sources");
    assert!(
        violations.is_empty(),
        "application/session must depend on ports/contracts, not adapters/runtimes:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn pr8_temporal_read_surfaces_cannot_import_refresh_or_writer_authorities() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = [
        PathBuf::from("src/application/session/retrieval.rs"),
        PathBuf::from("src/application/session/ports.rs"),
        PathBuf::from("src/application/session/types.rs"),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();

    let forbidden: &[&[&str]] = &[
        &["crate", "application", "session", "refresh"],
        &["super", "refresh"],
        &["crate", "global_db"],
        &["crate", "store"],
        &["crate", "daemon"],
        &["crate", "mcp"],
        &["crate", "sessions", "ingest"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect PR8 temporal read surfaces");
    assert!(
        violations.is_empty(),
        "PR8 temporal read surfaces must stay free of refresh/writer authorities:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );

    // Retrieval may race deadlines with tokio, but must not own refresh ports.
    let refresh_tokens = [
        "SessionRefreshStore",
        "begin_or_join_refresh",
        "wake_refresh",
    ];
    for path in &sources {
        if path.file_name().and_then(|name| name.to_str()) == Some("retrieval.rs") {
            let source = fs::read_to_string(repository.join(path)).expect("read retrieval.rs");
            for token in refresh_tokens {
                assert!(
                    !source.contains(token),
                    "retrieval.rs must not reference refresh authority token {token}"
                );
            }
        }
    }
}

#[test]
fn domain_session_contracts_are_runtime_and_store_free() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = [PathBuf::from("crates/tracedecay-domain/src/session.rs")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let forbidden: &[&[&str]] = &[
        &["tracedecay_store"],
        &["tracedecay"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
        &["std", "fs"],
        &["std", "net"],
        &["std", "process"],
        &["std", "thread"],
        &["std", "time", "Instant"],
        &["std", "time", "SystemTime"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect domain session contracts");
    assert!(
        violations.is_empty(),
        "domain session contracts must stay runtime/store free:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn store_session_contracts_are_adapter_free() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let roots = [PathBuf::from("crates/tracedecay-store/src/session")]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let sources =
        filesystem_rust_sources(&repository, &roots).expect("resolve store session sources");
    assert!(!sources.is_empty(), "store session sources must exist");

    let forbidden: &[&[&str]] = &[
        &["tracedecay"],
        &["libsql"],
        &["rusqlite"],
        &["sqlx"],
        &["tokio"],
        &["async_std"],
        &["std", "fs"],
        &["std", "net"],
        &["std", "process"],
        &["std", "thread"],
    ];
    let violations = scan_sources_for_forbidden_paths(&repository, &sources, forbidden)
        .expect("inspect store session contracts");
    assert!(
        violations.is_empty(),
        "store session contracts must stay adapter/runtime free:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}
