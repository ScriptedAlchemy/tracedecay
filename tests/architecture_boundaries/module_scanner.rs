//! Rust module/include scanner shared by the architecture boundary guards.
//!
//! Tokenizes Rust source well enough to follow `mod` declarations,
//! `#[path = ...]` attributes, and literal `include!`/`include_str!` of `.rs`
//! files, and resolves the set of sources reachable from Cargo target roots.

use rayon::prelude::*;
use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    Ident(String),
    StringLiteral(String),
    Punct(char),
}

/// Borrowed lexical tokens used by module reachability. This remains a
/// comment/literal-aware structural scan; it does not infer modules from source
/// text matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReferenceToken<'a> {
    Ident(&'a str),
    StringLiteral(&'a str, ReferenceStringKind),
    Punct(char),
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReferenceStringKind {
    Quoted,
    Raw { hashes: usize },
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
    let mut tokens = reference_tokens(source).peekable();
    let mut references = Vec::new();
    let mut inline_modules: Vec<(usize, String)> = Vec::new();
    let mut pending_path = None;
    let mut brace_depth = 0usize;

    while let Some(token) = tokens.next() {
        match token {
            ReferenceToken::Punct('#') if tokens.peek() == Some(&ReferenceToken::Punct('[')) => {
                tokens.next();
                if let Some(path) = consume_path_attribute(&mut tokens) {
                    pending_path = Some(path);
                }
            }
            ReferenceToken::Ident("mod") => {
                let Some(ReferenceToken::Ident(name)) = tokens.peek().copied() else {
                    continue;
                };
                tokens.next();
                if tokens.peek() == Some(&ReferenceToken::Punct(';')) {
                    tokens.next();
                    references.push(SourceReference::Module {
                        name: name.to_string(),
                        path: pending_path.take(),
                        inline_modules: inline_module_names(&inline_modules),
                    });
                } else if tokens.peek() == Some(&ReferenceToken::Punct('{')) {
                    tokens.next();
                    brace_depth += 1;
                    inline_modules.push((brace_depth, name.to_string()));
                    pending_path = None;
                }
            }
            ReferenceToken::Ident(macro_name @ ("include" | "include_str"))
                if tokens.peek() == Some(&ReferenceToken::Punct('!')) =>
            {
                tokens.next();
                if tokens.peek() != Some(&ReferenceToken::Punct('(')) {
                    continue;
                }
                tokens.next();
                let Some(ReferenceToken::StringLiteral(source, kind)) = tokens.peek().copied()
                else {
                    continue;
                };
                tokens.next();
                if let Some(path) = string_literal_value(source, kind)
                    && Path::new(&path).extension() == Some(OsStr::new("rs"))
                {
                    references.push(SourceReference::Include {
                        path,
                        parse_as_rust: macro_name == "include",
                        inline_modules: inline_module_names(&inline_modules),
                    });
                }
            }
            ReferenceToken::Punct('{') => {
                brace_depth += 1;
                pending_path = None;
            }
            ReferenceToken::Punct('}') => {
                while inline_modules
                    .last()
                    .is_some_and(|(depth, _)| *depth == brace_depth)
                {
                    inline_modules.pop();
                }
                brace_depth = brace_depth.saturating_sub(1);
                pending_path = None;
            }
            ReferenceToken::Punct(';') => pending_path = None,
            _ => {}
        }
    }

    references
}

fn reference_tokens(source: &str) -> ReferenceTokens<'_> {
    ReferenceTokens { source, offset: 0 }
}

struct ReferenceTokens<'a> {
    source: &'a str,
    offset: usize,
}

impl<'a> Iterator for ReferenceTokens<'a> {
    type Item = ReferenceToken<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.source.as_bytes();
        while self.offset < bytes.len() {
            let start = self.offset;
            match bytes[start] {
                byte if byte.is_ascii_whitespace() => {
                    self.offset += 1;
                    while self.offset < bytes.len() && bytes[self.offset].is_ascii_whitespace() {
                        self.offset += 1;
                    }
                }
                b'/' if bytes.get(start + 1) == Some(&b'/') => {
                    self.offset += 2;
                    while self.offset < bytes.len() && bytes[self.offset] != b'\n' {
                        self.offset += 1;
                    }
                }
                b'/' if bytes.get(start + 1) == Some(&b'*') => {
                    self.offset += 2;
                    let mut depth = 1usize;
                    while self.offset < bytes.len() && depth > 0 {
                        if bytes[self.offset] == b'/' && bytes.get(self.offset + 1) == Some(&b'*') {
                            depth += 1;
                            self.offset += 2;
                        } else if bytes[self.offset] == b'*'
                            && bytes.get(self.offset + 1) == Some(&b'/')
                        {
                            depth -= 1;
                            self.offset += 2;
                        } else {
                            self.offset += 1;
                        }
                    }
                }
                b'r' if let Some((end, hashes)) = raw_string_end(bytes, start) => {
                    self.offset = end;
                    return Some(ReferenceToken::StringLiteral(
                        &self.source[start..end],
                        ReferenceStringKind::Raw { hashes },
                    ));
                }
                b'"' => {
                    self.offset = quoted_string_end(bytes, start);
                    return Some(ReferenceToken::StringLiteral(
                        &self.source[start..self.offset],
                        ReferenceStringKind::Quoted,
                    ));
                }
                b'\'' if let Some(end) = char_literal_end(bytes, start) => {
                    self.offset = end;
                    return Some(ReferenceToken::Other);
                }
                b'r' if bytes.get(start + 1) == Some(&b'#')
                    && bytes
                        .get(start + 2)
                        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_') =>
                {
                    let ident_start = start + 2;
                    self.offset = ident_start + 1;
                    while self.offset < bytes.len()
                        && (bytes[self.offset].is_ascii_alphanumeric()
                            || bytes[self.offset] == b'_')
                    {
                        self.offset += 1;
                    }
                    return Some(ReferenceToken::Ident(
                        &self.source[ident_start..self.offset],
                    ));
                }
                byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                    self.offset += 1;
                    while self.offset < bytes.len()
                        && (bytes[self.offset].is_ascii_alphanumeric()
                            || bytes[self.offset] == b'_')
                    {
                        self.offset += 1;
                    }
                    return Some(ReferenceToken::Ident(&self.source[start..self.offset]));
                }
                byte if byte.is_ascii_digit() => {
                    self.offset += 1;
                    while self.offset < bytes.len()
                        && (bytes[self.offset].is_ascii_alphanumeric()
                            || matches!(bytes[self.offset], b'_' | b'.'))
                    {
                        self.offset += 1;
                    }
                    return Some(ReferenceToken::Other);
                }
                byte if byte.is_ascii() => {
                    self.offset += 1;
                    return Some(ReferenceToken::Punct(char::from(byte)));
                }
                _ => {
                    let character = self.source[start..].chars().next().expect("valid UTF-8");
                    self.offset += character.len_utf8();
                    return Some(ReferenceToken::Other);
                }
            }
        }
        None
    }
}

fn raw_string_end(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut quote = start + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let hashes = quote - start - 1;
    let mut cursor = quote + 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes) == Some(&bytes[start + 1..quote])
        {
            return Some((cursor + 1 + hashes, hashes));
        }
        cursor += 1;
    }
    Some((bytes.len(), hashes))
}

fn quoted_string_end(bytes: &[u8], start: usize) -> usize {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => return cursor + 1,
            b'\\' => cursor = (cursor + 2).min(bytes.len()),
            _ => cursor += 1,
        }
    }
    bytes.len()
}

fn string_literal_value(source: &str, kind: ReferenceStringKind) -> Option<String> {
    match kind {
        ReferenceStringKind::Quoted => {
            let (value, end) = quoted_string_at(source, 0);
            (end == source.len()).then_some(value)
        }
        ReferenceStringKind::Raw { hashes } => {
            let start = 2 + hashes;
            let end = source.len().checked_sub(1 + hashes)?;
            Some(source.get(start..end)?.to_string())
        }
    }
}

fn consume_path_attribute<'a>(
    tokens: &mut std::iter::Peekable<impl Iterator<Item = ReferenceToken<'a>>>,
) -> Option<String> {
    let mut body = [None; 3];
    let mut body_len = 0usize;
    let mut depth = 1usize;
    let mut simple = true;
    for token in tokens.by_ref() {
        match token {
            ReferenceToken::Punct('[') => {
                depth += 1;
                simple = false;
            }
            ReferenceToken::Punct(']') => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ if depth == 1 && body_len < body.len() => {
                body[body_len] = Some(token);
                body_len += 1;
            }
            _ if depth == 1 => simple = false,
            _ => {}
        }
    }
    if depth != 0 || !simple || body_len != body.len() {
        return None;
    }
    match body {
        [
            Some(ReferenceToken::Ident("path")),
            Some(ReferenceToken::Punct('=')),
            Some(ReferenceToken::StringLiteral(source, kind)),
        ] => string_literal_value(source, kind),
        _ => None,
    }
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

    while !pending.is_empty() {
        let batch: Vec<_> = pending
            .drain(..)
            .filter(|context| {
                reachable.insert(context.path.clone());
                scanned.insert(context.clone())
            })
            .collect();
        let parsed = module_scan_pool()?.install(|| {
            batch
                .into_par_iter()
                .map(|context| {
                    let absolute = repository.join(&context.path);
                    let source = fs::read_to_string(&absolute)
                        .map_err(|error| format!("cannot read {}: {error}", absolute.display()))?;
                    Ok((context, scan_references(&source)))
                })
                .collect::<Result<Vec<_>, String>>()
        })?;

        for (context, references) in parsed {
            for reference in references {
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
    }

    Ok(reachable)
}

fn module_scan_pool() -> Result<&'static rayon::ThreadPool, String> {
    static POOL: OnceLock<Result<rayon::ThreadPool, String>> = OnceLock::new();
    match POOL.get_or_init(|| {
        // Avoid spawning one worker per host CPU on large CI machines for a
        // bounded corpus of source files.
        let threads = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(8);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(|error| format!("cannot build bounded Rust module scanner pool: {error}"))
    }) {
        Ok(pool) => Ok(pool),
        Err(error) => Err(error.clone()),
    }
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
        /* outer /* mod nested_comment; */ comment */
        const TEXT: &str = "mod string_literal; include!(\"also_ignored.rs\");";
        const RAW_BYTES: &[u8] = br#"mod raw_byte_literal;"#;
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
        matches!(
            reference,
            SourceReference::Module { name, .. }
                if matches!(
                    name.as_str(),
                    "commented_out" | "nested_comment" | "string_literal" | "raw_byte_literal"
                )
        )
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
