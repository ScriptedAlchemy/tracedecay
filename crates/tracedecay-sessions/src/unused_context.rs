//! Measure how much returned tool context later session turns never reuse.
//!
//! The meter matches MCP trailers: `chars/4`. A unit is used when a later
//! turn in the same session opens, edits, quotes, or cites it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::Value;
use tracedecay_contracts::retrieval::{
    AdminCliUnusedContextExampleV1, AdminCliUnusedContextReportV1,
    AdminCliUnusedContextToolRatioV1, UnusedContextUsageKindV1,
};

const METER: &str = "chars_div_4";
const PREVIEW_CHARS: usize = 160;
const MIN_QUOTE_CHARS: usize = 20;
const MIN_SHORT_QUOTE_CHARS: usize = 12;
const DEFAULT_EXAMPLE_LIMIT: usize = 3;

/// One hydrated session message in store order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEvent {
    pub provider: String,
    pub session_id: String,
    pub message_id: String,
    pub store_id: i64,
    pub role: String,
    pub kind: Option<String>,
    pub tool_names: Option<String>,
    pub content: String,
    pub metadata_json: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnusedContextOptions {
    pub example_limit: usize,
}

impl Default for UnusedContextOptions {
    fn default() -> Self {
        Self {
            example_limit: DEFAULT_EXAMPLE_LIMIT,
        }
    }
}

#[derive(Clone, Debug)]
struct ContextUnit {
    tool: String,
    provider: String,
    session_id: String,
    message_id: String,
    store_id: i64,
    path: Option<String>,
    symbol: Option<String>,
    content: String,
}

#[derive(Clone, Debug)]
struct ScoredUnit {
    unit: ContextUnit,
    usage: Vec<UnusedContextUsageKindV1>,
    evidence: Option<String>,
}

/// Score returned tool context against later opens, edits, quotes, and cites.
#[must_use]
pub fn measure_unused_context(
    events: &[TimelineEvent],
    options: UnusedContextOptions,
) -> AdminCliUnusedContextReportV1 {
    let example_limit = options.example_limit.max(1);
    let mut sessions = BTreeSet::new();
    let mut by_session: BTreeMap<(String, String), Vec<&TimelineEvent>> = BTreeMap::new();
    for event in events {
        sessions.insert((event.provider.clone(), event.session_id.clone()));
        by_session
            .entry((event.provider.clone(), event.session_id.clone()))
            .or_default()
            .push(event);
    }

    let mut scored = Vec::new();
    let mut tool_results_scanned = 0_u64;
    for session_events in by_session.values_mut() {
        session_events.sort_by_key(|event| {
            (
                event.store_id,
                event.ordinal_hint(),
                event.message_id.as_str(),
            )
        });
        let results = pair_tool_results(session_events);
        tool_results_scanned += results.len() as u64;
        for result in results {
            let units = extract_units(&result);
            for unit in units {
                scored.push(score_unit(&unit, session_events));
            }
        }
    }

    assemble_report(
        events.len() as u64,
        sessions.len() as u64,
        tool_results_scanned,
        scored,
        example_limit,
    )
}

impl TimelineEvent {
    fn ordinal_hint(&self) -> i64 {
        self.store_id
    }
}

struct ToolResult {
    tool: String,
    provider: String,
    session_id: String,
    message_id: String,
    store_id: i64,
    arguments: Value,
    content: String,
}

fn pair_tool_results(events: &[&TimelineEvent]) -> Vec<ToolResult> {
    let mut results = Vec::new();
    let mut pending: VecDeque<(String, Value, i64)> = VecDeque::new();
    for event in events {
        if !is_tool_result(event) {
            if let Some((tool, arguments)) = invocation_from_event(event) {
                pending.push_back((tool, arguments, event.store_id));
            }
            continue;
        }
        let paired_tool = pending
            .pop_front()
            .map(|(tool, arguments, _)| (tool, arguments))
            .or_else(|| infer_tool_from_content(&event.content).map(|tool| (tool, Value::Null)));
        let Some((tool, arguments)) = paired_tool else {
            continue;
        };
        if is_usage_only_tool(&tool) {
            continue;
        }
        results.push(ToolResult {
            tool,
            provider: event.provider.clone(),
            session_id: event.session_id.clone(),
            message_id: event.message_id.clone(),
            store_id: event.store_id,
            arguments,
            content: event.content.clone(),
        });
    }
    results
}

fn is_tool_result(event: &TimelineEvent) -> bool {
    let kind = event.kind.as_deref().unwrap_or("");
    event.role.eq_ignore_ascii_case("tool") || kind.eq_ignore_ascii_case("tool_result")
}

fn invocation_from_event(event: &TimelineEvent) -> Option<(String, Value)> {
    let metadata = parse_json_object(event.metadata_json.as_deref().unwrap_or(""));
    if let Some(name) = first_tool_name_from_metadata(&metadata) {
        let arguments = arguments_from_metadata(&metadata)
            .or_else(|| parse_json_value(&event.content))
            .unwrap_or(Value::Null);
        return Some((normalize_tool_name(&name), arguments));
    }
    if let Some(name) = first_named_tool(event.tool_names.as_deref()) {
        let arguments = parse_json_value(&event.content).unwrap_or(Value::Null);
        return Some((normalize_tool_name(&name), arguments));
    }
    if let Some(name) = infer_tool_from_content(&event.content) {
        let arguments = parse_json_value(&event.content).unwrap_or(Value::Null);
        return Some((name, arguments));
    }
    None
}

fn first_named_tool(tool_names: Option<&str>) -> Option<String> {
    tool_names?
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn first_tool_name_from_metadata(metadata: &Value) -> Option<String> {
    if let Some(name) = metadata
        .pointer("/active_replay/tool_calls/0/name")
        .and_then(Value::as_str)
    {
        return Some(name.to_owned());
    }
    if let Some(name) = metadata
        .pointer("/active_replay/tool_calls/0/function/name")
        .and_then(Value::as_str)
    {
        return Some(name.to_owned());
    }
    if let Some(name) = metadata
        .pointer("/tool_calls/0/function/name")
        .and_then(Value::as_str)
    {
        return Some(name.to_owned());
    }
    if let Some(name) = metadata
        .pointer("/tool_calls/0/name")
        .and_then(Value::as_str)
    {
        return Some(name.to_owned());
    }
    metadata
        .pointer("/tool_events/0/tool_name")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn arguments_from_metadata(metadata: &Value) -> Option<Value> {
    metadata
        .pointer("/active_replay/tool_calls/0/arguments")
        .cloned()
        .or_else(|| {
            metadata
                .pointer("/tool_calls/0/function/arguments")
                .cloned()
        })
        .or_else(|| metadata.pointer("/tool_calls/0/arguments").cloned())
        .map(|value| match value {
            Value::String(text) => parse_json_value(&text).unwrap_or(Value::String(text)),
            other => other,
        })
}

fn infer_tool_from_content(content: &str) -> Option<String> {
    let value = parse_json_value(content)?;
    if value.get("search_matches").is_some() || value.get("related_symbols").is_some() {
        return Some("context".to_owned());
    }
    if value.get("files").is_some() && value.get("layout").is_some() {
        return Some("files".to_owned());
    }
    if value.get("match_count").is_some() && value.get("results").is_some() {
        return Some("grep".to_owned());
    }
    if value.get("query").and_then(Value::as_str) == Some("hunks") {
        return Some("git_hunks".to_owned());
    }
    if value.get("callers").is_some() {
        return Some("callers".to_owned());
    }
    if value.get("results").and_then(Value::as_array).is_some() && value.get("display").is_none() {
        if value.pointer("/results/0/display").is_some()
            || value.pointer("/results/0/candidate").is_some()
        {
            return Some("search".to_owned());
        }
    }
    None
}

fn normalize_tool_name(name: &str) -> String {
    let trimmed = name.trim();
    let stripped = trimmed
        .strip_prefix("mcp__tracedecay__")
        .or_else(|| trimmed.strip_prefix("tracedecay_"))
        .unwrap_or(trimmed);
    match stripped.to_ascii_lowercase().as_str() {
        "search" | "code_search" | "symbol_search" => "search".to_owned(),
        "context" => "context".to_owned(),
        "files" | "read" | "read_file" | "source_body" | "open" => "files".to_owned(),
        "grep" | "ast_grep" => "grep".to_owned(),
        "callers" | "code_callers" => "callers".to_owned(),
        "git_hunks" | "hunks" | "application_git_hunks" => "git_hunks".to_owned(),
        other => other.to_owned(),
    }
}

fn is_usage_only_tool(tool: &str) -> bool {
    matches!(
        tool,
        "edit"
            | "write"
            | "strreplace"
            | "str_replace"
            | "multi_str_replace"
            | "insert_at"
            | "apply_patch"
            | "edit_file"
            | "write_file"
            | "update_file"
    )
}

fn extract_units(result: &ToolResult) -> Vec<ContextUnit> {
    let json = parse_json_value(&result.content);
    let mut units = match result.tool.as_str() {
        "search" => json.as_ref().map(search_units).unwrap_or_default(),
        "context" => json.as_ref().map(context_units).unwrap_or_default(),
        "files" => json.as_ref().map(files_units).unwrap_or_default(),
        "grep" => json
            .as_ref()
            .map(grep_units)
            .unwrap_or_else(|| grep_text_units(&result.content)),
        "callers" => json.as_ref().map(callers_units).unwrap_or_default(),
        "git_hunks" => json
            .as_ref()
            .map(git_hunk_units)
            .unwrap_or_else(|| diff_text_units(&result.content)),
        _ => json
            .as_ref()
            .map(generic_json_units)
            .unwrap_or_else(|| generic_text_units(&result.content)),
    };
    if units.is_empty() {
        units.push(unit_from_parts(
            argument_path(&result.arguments),
            None,
            result.content.clone(),
        ));
    }
    for unit in &mut units {
        unit.tool = result.tool.clone();
        unit.provider = result.provider.clone();
        unit.session_id = result.session_id.clone();
        unit.message_id = result.message_id.clone();
        unit.store_id = result.store_id;
        if unit.path.is_none() {
            unit.path = argument_path(&result.arguments);
        }
    }
    units
}

fn unit_from_parts(path: Option<String>, symbol: Option<String>, content: String) -> ContextUnit {
    ContextUnit {
        tool: String::new(),
        provider: String::new(),
        session_id: String::new(),
        message_id: String::new(),
        store_id: 0,
        path,
        symbol,
        content,
    }
}

fn search_units(value: &Value) -> Vec<ContextUnit> {
    let rows = value
        .get("results")
        .or_else(|| value.get("hits"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let path = string_field(&row, &["display.path", "path", "file", "candidate.path"]);
            let symbol = string_field(
                &row,
                &[
                    "display.qualified_name",
                    "display.name",
                    "qualified_name",
                    "name",
                    "symbol",
                ],
            );
            unit_from_parts(path, symbol, compact_json(&row))
        })
        .collect()
}

fn context_units(value: &Value) -> Vec<ContextUnit> {
    let mut units = Vec::new();
    for key in ["symbols", "related_symbols", "search_matches", "code"] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            for item in items {
                let path = string_field(item, &["file", "path"]);
                let symbol = string_field(item, &["qualified_name", "name", "symbol"]);
                let content = item
                    .get("code")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| compact_json(item));
                units.push(unit_from_parts(path, symbol, content));
            }
        }
    }
    units
}

fn files_units(value: &Value) -> Vec<ContextUnit> {
    value
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|file| {
            let path = string_field(file, &["path", "file"]);
            unit_from_parts(path, None, compact_json(file))
        })
        .collect()
}

fn grep_units(value: &Value) -> Vec<ContextUnit> {
    value
        .get("results")
        .or_else(|| value.get("matches"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|hit| {
            let path = string_field(hit, &["file", "path"]);
            let symbol = string_field(hit, &["symbol", "name"]);
            let text = hit
                .get("text")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| compact_json(hit));
            unit_from_parts(path, symbol, text)
        })
        .collect()
}

fn grep_text_units(content: &str) -> Vec<ContextUnit> {
    content
        .lines()
        .filter_map(|line| {
            let (path, rest) = line.split_once(':')?;
            if path.is_empty() || !path.contains('.') && !path.contains('/') {
                return None;
            }
            Some(unit_from_parts(
                Some(normalize_path(path)),
                None,
                rest.to_owned(),
            ))
        })
        .collect()
}

fn callers_units(value: &Value) -> Vec<ContextUnit> {
    let rows = value
        .get("callers")
        .or_else(|| value.get("results"))
        .or_else(|| value.get("symbols"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let path = string_field(&row, &["file", "path"]);
            let symbol = string_field(&row, &["qualified_name", "name", "symbol"]);
            unit_from_parts(path, symbol, compact_json(&row))
        })
        .collect()
}

fn git_hunk_units(value: &Value) -> Vec<ContextUnit> {
    if let Some(hunks) = value
        .pointer("/result/value/hunks")
        .and_then(Value::as_array)
    {
        return hunks
            .iter()
            .map(|hunk| {
                let path = string_field(hunk, &["hunk.path", "path", "file"]);
                unit_from_parts(path, None, compact_json(hunk))
            })
            .collect();
    }
    if let Some(files) = value
        .pointer("/result/value/files")
        .and_then(Value::as_array)
    {
        return files
            .iter()
            .map(|file| {
                let path = string_field(file, &["path", "file"]);
                unit_from_parts(path, None, compact_json(file))
            })
            .collect();
    }
    generic_json_units(value)
}

fn diff_text_units(content: &str) -> Vec<ContextUnit> {
    let mut units = Vec::new();
    let mut current_path = None;
    let mut current = String::new();
    for line in content.lines() {
        if let Some(path) = line
            .strip_prefix("+++ b/")
            .or_else(|| line.strip_prefix("+++ "))
        {
            if !current.is_empty() {
                units.push(unit_from_parts(
                    current_path.take(),
                    None,
                    std::mem::take(&mut current),
                ));
            }
            current_path = Some(normalize_path(path.trim()));
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() || current_path.is_some() {
        units.push(unit_from_parts(current_path, None, current));
    }
    units
}

fn generic_json_units(value: &Value) -> Vec<ContextUnit> {
    for key in [
        "results", "hits", "files", "matches", "hunks", "symbols", "callers",
    ] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            return items
                .iter()
                .map(|item| {
                    unit_from_parts(
                        string_field(item, &["path", "file"]),
                        string_field(item, &["qualified_name", "name", "symbol"]),
                        compact_json(item),
                    )
                })
                .collect();
        }
    }
    if let Some(path) = string_field(value, &["path", "file"]) {
        return vec![unit_from_parts(
            Some(path),
            string_field(value, &["name", "symbol"]),
            compact_json(value),
        )];
    }
    Vec::new()
}

fn generic_text_units(content: &str) -> Vec<ContextUnit> {
    let paths = extract_paths(content);
    if paths.len() == 1 {
        return vec![unit_from_parts(
            paths.into_iter().next(),
            None,
            content.to_owned(),
        )];
    }
    if paths.is_empty() {
        return Vec::new();
    }
    paths
        .into_iter()
        .map(|path| unit_from_parts(Some(path), None, content.to_owned()))
        .collect()
}

fn score_unit(unit: &ContextUnit, events: &[&TimelineEvent]) -> ScoredUnit {
    let later: Vec<&&TimelineEvent> = events
        .iter()
        .filter(|event| event.store_id > unit.store_id)
        .collect();
    let later_text = later
        .iter()
        .map(|event| event.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut usage = Vec::new();
    let mut evidence = None;

    if let Some(path) = unit.path.as_deref() {
        for event in &later {
            if is_tool_result(event) {
                continue;
            }
            let Some((tool, arguments)) = invocation_from_event(event) else {
                continue;
            };
            let Some(later_path) = argument_path(&arguments).or_else(|| {
                event
                    .content
                    .lines()
                    .next()
                    .filter(|line| looks_like_path(line))
                    .map(normalize_path)
            }) else {
                continue;
            };
            if !paths_match(path, &later_path) {
                continue;
            }
            if is_open_tool(&tool) {
                push_usage(&mut usage, UnusedContextUsageKindV1::Opened);
                evidence = Some(format!("opened {later_path}"));
            }
            if is_edit_tool(&tool) {
                push_usage(&mut usage, UnusedContextUsageKindV1::Edited);
                evidence = Some(format!("edited {later_path}"));
            }
        }
        if text_cites_path(&later_text, path) {
            push_usage(&mut usage, UnusedContextUsageKindV1::Cited);
            evidence.get_or_insert_with(|| format!("cited {path}"));
        }
    }
    if let Some(symbol) = unit.symbol.as_deref()
        && distinctive_symbol(symbol)
        && contains_token(&later_text, symbol)
    {
        push_usage(&mut usage, UnusedContextUsageKindV1::Cited);
        evidence.get_or_insert_with(|| format!("cited {symbol}"));
    }
    if let Some(quote) = distinctive_quote(&unit.content, &later_text) {
        push_usage(&mut usage, UnusedContextUsageKindV1::Quoted);
        evidence = Some(format!("quoted {quote}"));
    }

    ScoredUnit {
        unit: unit.clone(),
        usage,
        evidence,
    }
}

fn push_usage(usage: &mut Vec<UnusedContextUsageKindV1>, kind: UnusedContextUsageKindV1) {
    if !usage.contains(&kind) {
        usage.push(kind);
    }
}

fn is_open_tool(tool: &str) -> bool {
    matches!(
        tool,
        "files" | "read" | "read_file" | "source_body" | "open" | "cat"
    )
}

fn is_edit_tool(tool: &str) -> bool {
    is_usage_only_tool(tool)
}

fn distinctive_quote(content: &str, later_text: &str) -> Option<String> {
    let mut candidates: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| line.chars().count() >= MIN_QUOTE_CHARS && !is_noise_line(line))
        .collect();
    if candidates.is_empty() {
        candidates = content
            .lines()
            .map(str::trim)
            .filter(|line| line.chars().count() >= MIN_SHORT_QUOTE_CHARS && !is_noise_line(line))
            .collect();
    }
    candidates
        .into_iter()
        .find(|line| later_text.contains(*line))
        .map(ToOwned::to_owned)
}

fn is_noise_line(line: &str) -> bool {
    let stripped = line.trim_matches(|ch: char| {
        ch == '{'
            || ch == '}'
            || ch == '['
            || ch == ']'
            || ch == ','
            || ch == '"'
            || ch.is_whitespace()
    });
    stripped.is_empty() || stripped == "null" || looks_like_path(line)
}

fn text_cites_path(text: &str, path: &str) -> bool {
    let normalized = normalize_path(path);
    if text.contains(&normalized) {
        return true;
    }
    let basename = normalized.rsplit('/').next().unwrap_or(&normalized);
    distinctive_basename(basename) && contains_token(text, basename)
}

fn distinctive_basename(name: &str) -> bool {
    name.len() >= 8 || name.contains('_') || name.contains('.')
}

fn distinctive_symbol(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.len() < 4 {
        return false;
    }
    !matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "this"
            | "that"
            | "true"
            | "false"
            | "null"
            | "none"
            | "self"
            | "data"
            | "file"
            | "path"
            | "name"
            | "kind"
            | "text"
            | "line"
            | "main"
            | "test"
            | "type"
            | "func"
    )
}

fn contains_token(text: &str, token: &str) -> bool {
    text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != ':' && ch != '.')
        .any(|part| part == token)
        || text.contains(token)
}

fn argument_path(arguments: &Value) -> Option<String> {
    string_field(arguments, &["path", "file", "target", "filename"]).or_else(|| {
        arguments
            .get("paths")
            .and_then(Value::as_array)
            .and_then(|paths| paths.first().and_then(Value::as_str).map(normalize_path))
    })
}

fn paths_match(left: &str, right: &str) -> bool {
    let left = normalize_path(left);
    let right = normalize_path(right);
    left == right || left.ends_with(&right) || right.ends_with(&left)
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .trim_matches('"')
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_owned()
}

fn looks_like_path(value: &str) -> bool {
    let value = value.trim();
    value.contains('/') || value.contains('\\') || value.contains('.')
}

fn extract_paths(content: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for token in content.split(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | '`' | ',' | ';' | '(' | ')')
    }) {
        if looks_like_path(token)
            && (token.contains('/')
                || token.ends_with(".rs")
                || token.ends_with(".ts")
                || token.ends_with(".py"))
        {
            let path = normalize_path(token);
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

fn string_field(value: &Value, pointers: &[&str]) -> Option<String> {
    for pointer in pointers {
        let found = if pointer.contains('.') {
            let json_pointer = format!("/{}", pointer.replace('.', "/"));
            value.pointer(&json_pointer).and_then(Value::as_str)
        } else {
            value.get(*pointer).and_then(Value::as_str)
        };
        if let Some(text) = found.filter(|text| !text.is_empty()) {
            return Some(normalize_path(text));
        }
    }
    None
}

fn parse_json_value(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

fn parse_json_object(text: &str) -> Value {
    parse_json_value(text).unwrap_or(Value::Null)
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.chars().count().div_ceil(4)).unwrap_or(u64::MAX)
}

fn ratio(unused: u64, returned: u64) -> Option<f64> {
    if returned == 0 {
        None
    } else {
        Some(unused as f64 / returned as f64)
    }
}

fn preview(text: &str) -> String {
    let mut preview = text.chars().take(PREVIEW_CHARS).collect::<String>();
    if text.chars().count() > PREVIEW_CHARS {
        preview.push('…');
    }
    preview.replace('\n', " ")
}

fn assemble_report(
    messages_scanned: u64,
    sessions_scanned: u64,
    tool_results_scanned: u64,
    scored: Vec<ScoredUnit>,
    example_limit: usize,
) -> AdminCliUnusedContextReportV1 {
    let mut tools: BTreeMap<String, AdminCliUnusedContextToolRatioV1> = BTreeMap::new();
    let mut examples_by_tool: BTreeMap<String, Vec<AdminCliUnusedContextExampleV1>> =
        BTreeMap::new();
    let mut returned_tokens = 0;
    let mut used_tokens = 0;
    let mut unused_tokens = 0;
    let mut returned_bytes = 0;
    let mut used_bytes = 0;
    let mut unused_bytes = 0;

    for scored in scored {
        let tokens = estimate_tokens(&scored.unit.content);
        let bytes = scored.unit.content.len() as u64;
        let used = !scored.usage.is_empty();
        returned_tokens += tokens;
        returned_bytes += bytes;
        if used {
            used_tokens += tokens;
            used_bytes += bytes;
        } else {
            unused_tokens += tokens;
            unused_bytes += bytes;
        }
        let tool = tools.entry(scored.unit.tool.clone()).or_insert_with(|| {
            AdminCliUnusedContextToolRatioV1 {
                tool: scored.unit.tool.clone(),
                result_count: 0,
                unit_count: 0,
                returned_tokens: 0,
                used_tokens: 0,
                unused_tokens: 0,
                returned_bytes: 0,
                used_bytes: 0,
                unused_bytes: 0,
                unused_ratio: None,
            }
        });
        tool.unit_count += 1;
        tool.returned_tokens += tokens;
        tool.returned_bytes += bytes;
        if used {
            tool.used_tokens += tokens;
            tool.used_bytes += bytes;
        } else {
            tool.unused_tokens += tokens;
            tool.unused_bytes += bytes;
        }
        examples_by_tool
            .entry(scored.unit.tool.clone())
            .or_default()
            .push(AdminCliUnusedContextExampleV1 {
                tool: scored.unit.tool.clone(),
                used,
                usage: scored.usage,
                provider: scored.unit.provider,
                session_id: scored.unit.session_id,
                message_id: scored.unit.message_id,
                path: scored.unit.path,
                symbol: scored.unit.symbol,
                preview: preview(&scored.unit.content),
                evidence: scored.evidence,
                returned_tokens: tokens,
                returned_bytes: bytes,
            });
    }

    for tool in tools.values_mut() {
        tool.unused_ratio = ratio(tool.unused_tokens, tool.returned_tokens);
    }
    // result_count is unit groups approximated by counting examples' message ids.
    let mut result_ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (tool, examples) in &examples_by_tool {
        for example in examples {
            result_ids
                .entry(tool.clone())
                .or_default()
                .insert(example.message_id.clone());
        }
    }
    for (tool, ids) in result_ids {
        if let Some(row) = tools.get_mut(&tool) {
            row.result_count = ids.len() as u64;
        }
    }

    let mut examples = Vec::new();
    for (tool, mut rows) in examples_by_tool {
        rows.sort_by_key(|example| {
            (
                example.used,
                example.session_id.clone(),
                example.message_id.clone(),
            )
        });
        let unused = rows
            .iter()
            .filter(|example| !example.used)
            .take(example_limit);
        let used = rows
            .iter()
            .filter(|example| example.used)
            .take(example_limit);
        let mut selected: Vec<_> = unused.chain(used).cloned().collect();
        if selected.len() < example_limit {
            for row in rows {
                if selected.len() >= example_limit {
                    break;
                }
                if !selected.iter().any(|existing| {
                    existing.message_id == row.message_id && existing.preview == row.preview
                }) {
                    selected.push(row);
                }
            }
        }
        let _ = tool;
        examples.extend(selected);
    }
    examples.sort_by(|left, right| {
        left.tool
            .cmp(&right.tool)
            .then(left.used.cmp(&right.used))
            .then(left.session_id.cmp(&right.session_id))
    });

    AdminCliUnusedContextReportV1 {
        meter: METER.to_owned(),
        sessions_scanned,
        messages_scanned,
        tool_results_scanned,
        returned_tokens,
        used_tokens,
        unused_tokens,
        returned_bytes,
        used_bytes,
        unused_bytes,
        unused_ratio: ratio(unused_tokens, returned_tokens),
        tools: tools.into_values().collect(),
        examples,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(
        store_id: i64,
        role: &str,
        kind: &str,
        tool_names: Option<&str>,
        content: &str,
        metadata: Option<&str>,
    ) -> TimelineEvent {
        TimelineEvent {
            provider: "cursor".to_owned(),
            session_id: "session-1".to_owned(),
            message_id: format!("msg-{store_id}"),
            store_id,
            role: role.to_owned(),
            kind: Some(kind.to_owned()),
            tool_names: tool_names.map(ToOwned::to_owned),
            content: content.to_owned(),
            metadata_json: metadata.map(ToOwned::to_owned),
        }
    }

    fn measure(events: &[TimelineEvent]) -> AdminCliUnusedContextReportV1 {
        measure_unused_context(events, UnusedContextOptions { example_limit: 3 })
    }

    fn tool_row<'a>(
        report: &'a AdminCliUnusedContextReportV1,
        tool: &str,
    ) -> &'a AdminCliUnusedContextToolRatioV1 {
        report
            .tools
            .iter()
            .find(|row| row.tool == tool)
            .unwrap_or_else(|| panic!("missing tool {tool}: {:?}", report.tools))
    }

    #[test]
    fn search_hit_later_opened_is_used_and_sibling_is_unused() {
        let used_path = "crates/tracedecay/src/lib.rs";
        let ignored_path = "crates/tracedecay-cli/src/main.rs";
        let result = json!({
            "results": [
                {"display": {"name": "load_session", "qualified_name": "lcm::load_session", "path": used_path}},
                {"display": {"name": "dispatch", "qualified_name": "cli::dispatch", "path": ignored_path}}
            ]
        });
        let report = measure(&[
            event(
                1,
                "assistant",
                "tool_call",
                Some("tracedecay_search"),
                "{}",
                None,
            ),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
            event(
                3,
                "assistant",
                "tool_call",
                Some("read_file"),
                &json!({"path": used_path}).to_string(),
                None,
            ),
        ]);
        let search = tool_row(&report, "search");
        assert_eq!(search.unit_count, 2);
        assert!(search.used_tokens > 0);
        assert!(search.unused_tokens > 0);
        assert!(search.unused_ratio.unwrap() > 0.0);
        assert!(
            report
                .examples
                .iter()
                .any(|example| example.path.as_deref() == Some(used_path) && example.used)
        );
        assert!(
            report
                .examples
                .iter()
                .any(|example| example.path.as_deref() == Some(ignored_path) && !example.used)
        );
    }

    #[test]
    fn grep_line_later_quoted_is_used() {
        let match_text = "pub async fn load_session(conn: &impl QueryExecutor)";
        let result = json!({
            "results": [{"file": "crates/tracedecay-lcm/src/query/session.rs", "line": 9, "text": match_text}],
            "match_count": 1
        });
        let report = measure(&[
            event(
                1,
                "assistant",
                "tool_call",
                Some("tracedecay_grep"),
                "{}",
                None,
            ),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
            event(
                3,
                "assistant",
                "message",
                None,
                &format!("I will reuse {match_text} as the walker."),
                None,
            ),
        ]);
        let grep = tool_row(&report, "grep");
        assert_eq!(grep.unused_tokens, 0);
        assert!(grep.used_tokens > 0);
        assert_eq!(
            report.examples[0].usage,
            vec![UnusedContextUsageKindV1::Quoted]
        );
    }

    #[test]
    fn files_listing_later_edited_one_path() {
        let result = json!({
            "layout": "flat",
            "files": [
                {"path": "src/alpha.rs", "symbols": 2, "bytes": 40},
                {"path": "src/beta.rs", "symbols": 1, "bytes": 20}
            ]
        });
        let report = measure(&[
            event(
                1,
                "assistant",
                "tool_call",
                Some("tracedecay_files"),
                "{}",
                None,
            ),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
            event(
                3,
                "assistant",
                "file_edit",
                Some("str_replace"),
                &json!({"path": "src/alpha.rs", "old": "a", "new": "b"}).to_string(),
                None,
            ),
        ]);
        let files = tool_row(&report, "files");
        assert_eq!(files.unit_count, 2);
        assert!(files.used_tokens > 0);
        assert!(files.unused_tokens > 0);
    }

    #[test]
    fn context_symbol_later_cited_and_related_symbol_ignored() {
        let result = json!({
            "symbols": [{"name": "measure_unused_context", "qualified_name": "unused_context::measure_unused_context", "file": "src/unused_context.rs"}],
            "related_symbols": [{"name": "unrelated_helper", "qualified_name": "other::unrelated_helper", "file": "src/other.rs"}]
        });
        let report = measure(&[
            event(
                1,
                "assistant",
                "tool_call",
                Some("tracedecay_context"),
                "{}",
                None,
            ),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
            event(
                3,
                "assistant",
                "message",
                None,
                "Call measure_unused_context on the stored timeline.",
                None,
            ),
        ]);
        let context = tool_row(&report, "context");
        assert_eq!(context.unit_count, 2);
        assert!(context.used_tokens > 0);
        assert!(context.unused_tokens > 0);
    }

    #[test]
    fn callers_later_cited_by_path() {
        let result = json!({
            "callers": [
                {"name": "handle_sessions", "qualified_name": "cli::handle_sessions", "file": "crates/tracedecay-cli/src/sessions_cmd.rs"},
                {"name": "ignored_caller", "qualified_name": "other::ignored_caller", "file": "crates/other/src/lib.rs"}
            ]
        });
        let report = measure(&[
            event(
                1,
                "assistant",
                "tool_call",
                Some("tracedecay_callers"),
                "{}",
                None,
            ),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
            event(
                3,
                "assistant",
                "message",
                None,
                "The CLI entry is crates/tracedecay-cli/src/sessions_cmd.rs.",
                None,
            ),
        ]);
        let callers = tool_row(&report, "callers");
        assert_eq!(callers.unit_count, 2);
        assert!(callers.used_tokens > 0);
        assert!(callers.unused_tokens > 0);
    }

    #[test]
    fn git_hunks_later_edited_matching_path() {
        let result = json!({
            "query": "hunks",
            "result": {
                "value": {
                    "hunks": [
                        {"hunk": {"path": "src/gain.rs"}, "digest": "aaa"},
                        {"hunk": {"path": "src/unused.rs"}, "digest": "bbb"}
                    ]
                }
            }
        });
        let report = measure(&[
            event(1, "assistant", "tool_call", Some("git_hunks"), "{}", None),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
            event(
                3,
                "assistant",
                "file_edit",
                Some("apply_patch"),
                &json!({"path": "src/gain.rs"}).to_string(),
                None,
            ),
        ]);
        let hunks = tool_row(&report, "git_hunks");
        assert_eq!(hunks.unit_count, 2);
        assert!(hunks.used_tokens > 0);
        assert!(hunks.unused_tokens > 0);
    }

    #[test]
    fn common_word_is_not_a_cite_and_sessions_do_not_leak() {
        let result = json!({
            "results": [{"display": {"name": "data", "qualified_name": "data", "path": "src/a.rs"}}]
        });
        let foreign = TimelineEvent {
            session_id: "session-2".to_owned(),
            message_id: "foreign".to_owned(),
            store_id: 9,
            ..event(
                9,
                "assistant",
                "message",
                None,
                "I opened src/a.rs after reading data.",
                None,
            )
        };
        let report = measure(&[
            event(1, "assistant", "tool_call", Some("search"), "{}", None),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
            foreign,
        ]);
        let search = tool_row(&report, "search");
        assert_eq!(search.used_tokens, 0);
        assert!(search.unused_tokens > 0);
        assert_eq!(report.sessions_scanned, 2);
    }

    #[test]
    fn empty_timeline_is_complete_zero_not_fabricated_success_ratio() {
        let report = measure(&[]);
        assert_eq!(report.sessions_scanned, 0);
        assert_eq!(report.tool_results_scanned, 0);
        assert!(report.tools.is_empty());
        assert_eq!(report.unused_ratio, None);
        assert_eq!(report.meter, "chars_div_4");
    }

    #[test]
    fn meter_matches_chars_div_four() {
        let result =
            json!({"files": [{"path": "only.rs", "symbols": 0, "bytes": 1}], "layout": "flat"});
        let report = measure(&[
            event(1, "assistant", "tool_call", Some("files"), "{}", None),
            event(2, "tool", "tool_result", None, &result.to_string(), None),
        ]);
        let expected = estimate_tokens(&result.to_string());
        assert_eq!(tool_row(&report, "files").returned_tokens, expected);
        assert_eq!(
            expected,
            u64::try_from(result.to_string().chars().count().div_ceil(4)).unwrap()
        );
    }

    fn representative_corpus() -> Vec<TimelineEvent> {
        let mut events = Vec::new();
        let mut store_id = 1_i64;
        let mut push = |role: &str, kind: &str, tool: Option<&str>, content: String| {
            events.push(TimelineEvent {
                provider: "cursor".to_owned(),
                session_id: "td-unused-context".to_owned(),
                message_id: format!("msg-{store_id}"),
                store_id,
                role: role.to_owned(),
                kind: Some(kind.to_owned()),
                tool_names: tool.map(ToOwned::to_owned),
                content,
                metadata_json: None,
            });
            store_id += 1;
        };

        let search = json!({
            "results": [
                {"display": {"name": "load_session", "qualified_name": "lcm::load_session", "path": "crates/tracedecay-lcm/src/query/session.rs"}},
                {"display": {"name": "dispatch_command", "qualified_name": "cli::dispatch_command", "path": "crates/tracedecay-cli/src/main.rs"}},
                {"display": {"name": "pair_tool_results", "qualified_name": "unused_context::pair_tool_results", "path": "crates/tracedecay-sessions/src/unused_context.rs"}},
                {"display": {"name": "forgotten_search_hit", "qualified_name": "other::forgotten_search_hit", "path": "crates/other/src/forgotten.rs"}},
                {"display": {"name": "unused_index_probe", "qualified_name": "index::unused_index_probe", "path": "crates/index/src/probe.rs"}},
                {"display": {"name": "dead_ranker", "qualified_name": "rank::dead_ranker", "path": "crates/rank/src/dead.rs"}}
            ]
        });
        push(
            "assistant",
            "tool_call",
            Some("tracedecay_search"),
            "{}".to_owned(),
        );
        push("tool", "tool_result", None, search.to_string());

        let context = json!({
            "symbols": [
                {"name": "measure_unused_context", "qualified_name": "unused_context::measure_unused_context", "file": "crates/tracedecay-sessions/src/unused_context.rs"},
                {"name": "ignored_context_symbol", "qualified_name": "other::ignored_context_symbol", "file": "crates/other/src/context.rs"}
            ],
            "related_symbols": [
                {"name": "estimate_tokens", "qualified_name": "unused_context::estimate_tokens", "file": "crates/tracedecay-sessions/src/unused_context.rs"},
                {"name": "unused_related", "qualified_name": "other::unused_related", "file": "crates/other/src/related.rs"}
            ],
            "search_matches": [
                {"name": "TimelineEvent", "qualified_name": "unused_context::TimelineEvent", "file": "crates/tracedecay-sessions/src/unused_context.rs"},
                {"name": "unused_match", "qualified_name": "other::unused_match", "file": "crates/other/src/match.rs"}
            ]
        });
        push(
            "assistant",
            "tool_call",
            Some("tracedecay_context"),
            "{}".to_owned(),
        );
        push("tool", "tool_result", None, context.to_string());

        let files = json!({
            "layout": "flat",
            "files": [
                {"path": "crates/tracedecay-sessions/src/unused_context.rs", "symbols": 12, "bytes": 4000},
                {"path": "crates/tracedecay-cli/src/sessions_cmd.rs", "symbols": 8, "bytes": 2000},
                {"path": "crates/tracedecay-mcp/src/handlers/unused_context.rs", "symbols": 4, "bytes": 1500},
                {"path": "crates/other/src/alpha.rs", "symbols": 1, "bytes": 40},
                {"path": "crates/other/src/beta.rs", "symbols": 1, "bytes": 40},
                {"path": "crates/other/src/gamma.rs", "symbols": 1, "bytes": 40}
            ]
        });
        push(
            "assistant",
            "tool_call",
            Some("tracedecay_files"),
            "{}".to_owned(),
        );
        push("tool", "tool_result", None, files.to_string());

        let grep = json!({
            "results": [
                {"file": "crates/tracedecay-sessions/src/unused_context.rs", "line": 80, "text": "pub fn measure_unused_context(events: &[TimelineEvent], options: UnusedContextOptions)"},
                {"file": "crates/tracedecay-cli/src/sessions_cmd.rs", "line": 244, "text": "async fn handle_sessions_unused_context("},
                {"file": "crates/tracedecay-lcm/src/query/session.rs", "line": 9, "text": "pub async fn load_session(conn: &impl QueryExecutor)"},
                {"file": "crates/other/src/alpha.rs", "line": 1, "text": "fn never_quoted_alpha_helper() {}"},
                {"file": "crates/other/src/beta.rs", "line": 1, "text": "fn never_quoted_beta_helper() {}"},
                {"file": "crates/other/src/gamma.rs", "line": 1, "text": "fn never_quoted_gamma_helper() {}"}
            ],
            "match_count": 6
        });
        push(
            "assistant",
            "tool_call",
            Some("tracedecay_grep"),
            "{}".to_owned(),
        );
        push("tool", "tool_result", None, grep.to_string());

        let callers = json!({
            "callers": [
                {"name": "handle_sessions_action", "qualified_name": "cli::handle_sessions_action", "file": "crates/tracedecay-cli/src/sessions_cmd.rs"},
                {"name": "sessions_unused_context", "qualified_name": "handlers::sessions_unused_context", "file": "crates/tracedecay-mcp/src/handlers/unused_context.rs"},
                {"name": "dispatch_command", "qualified_name": "cli::dispatch_command", "file": "crates/tracedecay-cli/src/main.rs"},
                {"name": "ignored_caller_one", "qualified_name": "other::ignored_caller_one", "file": "crates/other/src/callers_a.rs"},
                {"name": "ignored_caller_two", "qualified_name": "other::ignored_caller_two", "file": "crates/other/src/callers_b.rs"},
                {"name": "ignored_caller_three", "qualified_name": "other::ignored_caller_three", "file": "crates/other/src/callers_c.rs"}
            ]
        });
        push(
            "assistant",
            "tool_call",
            Some("tracedecay_callers"),
            "{}".to_owned(),
        );
        push("tool", "tool_result", None, callers.to_string());

        let hunks = json!({
            "query": "hunks",
            "result": {
                "value": {
                    "hunks": [
                        {"hunk": {"path": "crates/tracedecay-sessions/src/unused_context.rs"}, "digest": "h1"},
                        {"hunk": {"path": "crates/tracedecay-cli/src/sessions_cmd.rs"}, "digest": "h2"},
                        {"hunk": {"path": "crates/tracedecay-mcp/src/handlers/unused_context.rs"}, "digest": "h3"},
                        {"hunk": {"path": "crates/other/src/hunk_a.rs"}, "digest": "h4"},
                        {"hunk": {"path": "crates/other/src/hunk_b.rs"}, "digest": "h5"},
                        {"hunk": {"path": "crates/other/src/hunk_c.rs"}, "digest": "h6"}
                    ]
                }
            }
        });
        push("assistant", "tool_call", Some("git_hunks"), "{}".to_owned());
        push("tool", "tool_result", None, hunks.to_string());

        push(
            "assistant",
            "tool_call",
            Some("read_file"),
            json!({"path": "crates/tracedecay-lcm/src/query/session.rs"}).to_string(),
        );
        push(
            "assistant",
            "tool_call",
            Some("read_file"),
            json!({"path": "crates/tracedecay-cli/src/main.rs"}).to_string(),
        );
        push(
            "assistant",
            "file_edit",
            Some("str_replace"),
            json!({"path": "crates/tracedecay-sessions/src/unused_context.rs"}).to_string(),
        );
        push(
            "assistant",
            "file_edit",
            Some("str_replace"),
            json!({"path": "crates/tracedecay-cli/src/sessions_cmd.rs"}).to_string(),
        );
        push(
            "assistant",
            "file_edit",
            Some("apply_patch"),
            json!({"path": "crates/tracedecay-mcp/src/handlers/unused_context.rs"}).to_string(),
        );
        push(
            "assistant",
            "message",
            None,
            "Reuse pub fn measure_unused_context(events: &[TimelineEvent], options: UnusedContextOptions) and async fn handle_sessions_unused_context( plus pub async fn load_session(conn: &impl QueryExecutor). Also cite unused_context::estimate_tokens and unused_context::TimelineEvent and handlers::sessions_unused_context plus cli::handle_sessions_action and cli::dispatch_command.".to_owned(),
        );
        events
    }

    #[test]
    fn representative_corpus_has_three_spot_checks_per_named_tool() {
        let report = measure(&representative_corpus());
        for tool in ["search", "context", "files", "grep", "callers", "git_hunks"] {
            let row = tool_row(&report, tool);
            assert!(
                row.used_tokens > 0,
                "{tool} should have used tokens: {row:?}"
            );
            assert!(
                row.unused_tokens > 0,
                "{tool} should have unused tokens: {row:?}"
            );
            let examples = report
                .examples
                .iter()
                .filter(|example| example.tool == tool)
                .count();
            assert!(
                examples >= 3,
                "{tool} should have 3+ spot-checks, got {examples}"
            );
        }
        assert!(report.unused_ratio.unwrap() > 0.0);
    }
}
