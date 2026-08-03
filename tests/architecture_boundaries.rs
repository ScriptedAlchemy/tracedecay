use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const REPOSITORY_SOURCE_ROOTS: &[&str] = &["src", "tests", "examples", "benches"];

const ARCHITECTURE_NON_ROOT_MEMBERS: &[&str] = &[
    "tracedecay-agent-hosts",
    "tracedecay-automation",
    "tracedecay-capture",
    "tracedecay-code-extraction",
    "tracedecay-code-index",
    "tracedecay-dashboard-api",
    "tracedecay-domain",
    "tracedecay-jsonrpc",
    "tracedecay-lsp",
    "tracedecay-migrate",
    "tracedecay-runtime-core",
    "tracedecay-sessions",
    "tracedecay-usecases",
];

const ARCHITECTURE_OMITTED_MEMBERS: &[&str] = &[
    "tracedecay-api",
    "tracedecay-application",
    "tracedecay-global-db",
    "tracedecay-host-integration",
    "tracedecay-hooks",
    "tracedecay-policy",
    "tracedecay-query",
    "tracedecay-sdk",
    "tracedecay-search-eval",
    "tracedecay-semantic",
    "tracedecay-temporal-query",
    "tracedecay-rusqlite-parity",
    "tracedecay-rusqlite-runtime",
    "tracedecay-sqlite-parity-protocol",
    "tracedecay-store",
    "tracedecay-tool-catalog",
];

const ARCHITECTURE_LAYERS: &[(&str, u8)] = &[
    ("tracedecay", 5),
    ("tracedecay-agent-hosts", 4),
    ("tracedecay-dashboard-api", 4),
    ("tracedecay-migrate", 3),
    ("tracedecay-sessions", 3),
    ("tracedecay-usecases", 3),
    ("tracedecay-runtime-core", 2),
    ("tracedecay-code-extraction", 1),
    ("tracedecay-code-index", 1),
    ("tracedecay-lsp", 1),
    ("tracedecay-automation", 0),
    ("tracedecay-capture", 0),
    ("tracedecay-domain", 0),
    ("tracedecay-jsonrpc", 0),
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
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    rename: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    src_path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct CargoSourceLayout {
    target_roots: BTreeSet<PathBuf>,
    tracked_roots: BTreeSet<PathBuf>,
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

    for package in packages {
        if !workspace_members.contains(&package.id) {
            continue;
        }
        let manifest_path = metadata_path_relative(
            repository,
            &package.manifest_path,
            "workspace package manifest",
        )?;
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("manifest has no parent: {}", manifest_path.display()))?;
        if !package_root.as_os_str().is_empty() {
            tracked_roots.insert(package_root.to_path_buf());
        }

        for target in package.targets {
            target_roots.insert(metadata_path_relative(
                repository,
                &target.src_path,
                "Cargo target source",
            )?);
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

    Ok(CargoSourceLayout {
        target_roots,
        tracked_roots,
    })
}

fn cargo_architecture_contract(repository: &Path) -> Result<BTreeSet<String>, String> {
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

    parse_architecture_contract(repository, &output.stdout)
}

fn parse_architecture_contract(
    repository: &Path,
    metadata_json: &[u8],
) -> Result<BTreeSet<String>, String> {
    let CargoMetadata {
        packages,
        workspace_members,
    } = serde_json::from_slice(metadata_json)
        .map_err(|error| format!("cannot parse cargo metadata: {error}"))?;
    let packages_by_id: BTreeMap<&str, &CargoPackage> = packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect();
    let mut violations = BTreeSet::new();
    let mut workspace_packages = Vec::new();

    for member_id in &workspace_members {
        match packages_by_id.get(member_id.as_str()) {
            Some(package) => workspace_packages.push(*package),
            None => {
                violations.insert(format!(
                    "workspace member id is missing from cargo metadata: {member_id}"
                ));
            }
        }
    }

    let root_manifest = repository.join("Cargo.toml");
    let root_package = workspace_packages
        .iter()
        .copied()
        .find(|package| package.manifest_path == root_manifest);
    let root_name = root_package.map_or("tracedecay", |package| package.name.as_str());
    if root_package.is_none() {
        violations.insert(format!(
            "workspace root manifest is not a cargo metadata member: {}",
            root_manifest.display()
        ));
    } else if root_name != "tracedecay" {
        violations.insert(format!(
            "workspace root package is named `{root_name}`, expected `tracedecay`"
        ));
    }

    let actual_non_root: BTreeSet<&str> = workspace_packages
        .iter()
        .filter(|package| package.manifest_path != root_manifest)
        .map(|package| package.name.as_str())
        .collect();
    let expected_non_root: BTreeSet<&str> = ARCHITECTURE_NON_ROOT_MEMBERS.iter().copied().collect();

    for name in expected_non_root.difference(&actual_non_root) {
        violations.insert(format!("missing expected workspace member: {name}"));
    }
    for name in actual_non_root.difference(&expected_non_root) {
        violations.insert(format!("unexpected non-root workspace member: {name}"));
    }
    for omitted in ARCHITECTURE_OMITTED_MEMBERS {
        if actual_non_root.contains(omitted) {
            violations.insert(format!("omitted workspace member is present: {omitted}"));
        }
    }

    let layers: BTreeMap<&str, u8> = ARCHITECTURE_LAYERS.iter().copied().collect();
    for package in &workspace_packages {
        let source_name = package.name.as_str();
        let Some(&source_layer) = layers.get(source_name) else {
            violations.insert(format!(
                "workspace package has no architecture layer: {source_name}"
            ));
            continue;
        };
        let is_root = package.manifest_path == root_manifest;

        for dependency in &package.dependencies {
            let dependency_name = dependency.name.as_str();
            let dependency_kind = dependency.kind.as_deref().unwrap_or("normal");
            let is_production = !matches!(dependency.kind.as_deref(), Some("dev"));

            if dependency_name.eq_ignore_ascii_case(root_name) && !is_root {
                violations.insert(format!(
                    "non-root package `{source_name}` depends directly on workspace root `{root_name}`"
                ));
            }
            let dependency_alias = dependency.rename.as_deref().unwrap_or_default();
            if dependency_name.to_ascii_lowercase().contains("rusqlite")
                || dependency_alias.to_ascii_lowercase().contains("rusqlite")
            {
                violations.insert(format!(
                    "package `{source_name}` has forbidden direct dependency `{dependency_name}` ({dependency_kind})"
                ));
            }

            let Some(&dependency_layer) = layers.get(dependency_name) else {
                continue;
            };
            if is_production && dependency_layer > source_layer {
                violations.insert(format!(
                    "upward production edge: `{source_name}` (L{source_layer}) -> `{dependency_name}` (L{dependency_layer})"
                ));
            }
        }
    }

    Ok(violations)
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
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["ls-files", "-z", "--"])
        .args(source_roots)
        .output();
    let Ok(output) = output else {
        return filesystem_rust_sources(repository, source_roots);
    };
    if !output.status.success() {
        return filesystem_rust_sources(repository, source_roots);
    }

    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            let path = std::str::from_utf8(bytes)
                .map_err(|error| format!("git-tracked path is not UTF-8: {error}"))?;
            normalize_relative(Path::new(path))
        })
        .filter_map(|result| match result {
            Ok(path)
                if path.extension() == Some(OsStr::new("rs"))
                    && repository.join(&path).is_file() =>
            {
                Some(Ok(path))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
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
    let metadata = serde_json::json!({
        "packages": [
            {
                "id": root_id,
                "manifest_path": repository.join("Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("src/lib.rs") },
                    { "src_path": repository.join("src/main.rs") },
                    { "src_path": repository.join("build.rs") }
                ]
            },
            {
                "id": domain_id,
                "manifest_path": repository.join("crates/tracedecay-domain/Cargo.toml"),
                "targets": [
                    { "src_path": repository.join("crates/tracedecay-domain/src/lib.rs") },
                    { "src_path": repository.join("crates/tracedecay-domain/tests/boundary.rs") }
                ]
            },
            {
                "id": "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
                "manifest_path": "/outside/registry/serde/Cargo.toml",
                "targets": [{ "src_path": "/outside/registry/serde/src/lib.rs" }]
            }
        ],
        "workspace_members": [root_id, domain_id]
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
            PathBuf::from("examples"),
            PathBuf::from("src"),
            PathBuf::from("tests"),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn workspace_architecture_contract() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let violations = cargo_architecture_contract(repository)
        .expect("discover Cargo workspace architecture metadata");
    assert!(
        violations.is_empty(),
        "workspace architecture contract violations (sorted):\n{}",
        violations
            .iter()
            .map(|violation| format!("  - {violation}"))
            .collect::<Vec<_>>()
            .join("\n")
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
