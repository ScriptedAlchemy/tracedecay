//! Rust module/include scanner shared by the architecture boundary guards.
//!
//! Tokenizes Rust source well enough to follow `mod` declarations,
//! `#[path = ...]` attributes, and literal `include!`/`include_str!` of `.rs`
//! files, and resolves the set of sources reachable from Cargo target roots.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    Ident(String),
    StringLiteral(String),
    Punct(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceReference {
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

pub(crate) fn tokenize(source: &str) -> Vec<Token> {
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

pub(crate) fn scan_references(source: &str) -> Vec<SourceReference> {
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

pub(crate) fn matching_delimiter(
    tokens: &[Token],
    start: usize,
    open: char,
    close: char,
) -> Option<usize> {
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

pub(crate) fn token_is_ident(token: Option<&Token>, expected: &str) -> bool {
    matches!(token, Some(Token::Ident(value)) if value == expected)
}

fn inline_module_names(modules: &[(usize, String)]) -> Vec<String> {
    modules.iter().map(|(_, name)| name.clone()).collect()
}

pub(crate) fn normalize_identifier(value: &str) -> String {
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

pub(crate) fn resolve_reachable_sources(
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

pub(crate) fn normalize_relative(path: &Path) -> Result<PathBuf, String> {
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
