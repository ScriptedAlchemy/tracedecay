//! Pure soft-hint decisions shared by hook adapters.
//!
//! This module intentionally returns model-visible text only. It does not deny,
//! rewrite, or otherwise decide permissions; adapters can choose how to surface
//! a returned hint for their own hook schema.

use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HintAgent {
    Claude,
    Cursor,
    Codex,
    Kiro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HintCategory {
    Search,
    BroadRead,
    CallGraph,
    Impact,
    SymbolLookup,
    FileLookup,
    ExploreSubagent,
}

impl HintCategory {
    fn as_key(self) -> &'static str {
        match self {
            HintCategory::Search => "search",
            HintCategory::BroadRead => "broad_read",
            HintCategory::CallGraph => "call_graph",
            HintCategory::Impact => "impact",
            HintCategory::SymbolLookup => "symbol_lookup",
            HintCategory::FileLookup => "file_lookup",
            HintCategory::ExploreSubagent => "explore_subagent",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "search" => Some(HintCategory::Search),
            "broad_read" => Some(HintCategory::BroadRead),
            "call_graph" => Some(HintCategory::CallGraph),
            "impact" => Some(HintCategory::Impact),
            "symbol_lookup" => Some(HintCategory::SymbolLookup),
            "file_lookup" => Some(HintCategory::FileLookup),
            "explore_subagent" => Some(HintCategory::ExploreSubagent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHintInput {
    pub agent: HintAgent,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub command: Option<String>,
    pub prompt: Option<String>,
    pub subagent_type: Option<String>,
    pub file_path: Option<String>,
    pub hints_enabled: bool,
}

impl Default for ToolHintInput {
    fn default() -> Self {
        Self {
            agent: HintAgent::Cursor,
            session_id: None,
            tool_name: None,
            command: None,
            prompt: None,
            subagent_type: None,
            file_path: None,
            hints_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHint {
    pub category: HintCategory,
    pub message: String,
    pub context: String,
    pub nonblocking: bool,
}

#[derive(Debug, Default)]
pub struct ToolHintDedupe {
    seen: HashSet<(String, HintCategory)>,
}

impl ToolHintDedupe {
    pub fn should_emit(&mut self, session_id: impl Into<String>, category: HintCategory) -> bool {
        self.seen.insert((session_id.into(), category))
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let entries: Vec<PersistedHintEntry> = serde_json::from_str(&content).unwrap_or_default();
        let seen = entries
            .into_iter()
            .filter_map(|entry| {
                let category = HintCategory::from_key(&entry.category)?;
                Some((entry.session_id, category))
            })
            .collect();
        Ok(Self { seen })
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut entries: Vec<PersistedHintEntry> = self
            .seen
            .iter()
            .map(|(session_id, category)| PersistedHintEntry {
                session_id: session_id.clone(),
                category: category.as_key().to_string(),
            })
            .collect();
        entries.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then_with(|| a.category.cmp(&b.category))
        });
        let json = serde_json::to_string_pretty(&entries).map_err(std::io::Error::other)?;
        std::fs::write(path, format!("{json}\n"))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedHintEntry {
    session_id: String,
    category: String,
}

pub fn decide_hint(input: &ToolHintInput) -> Option<ToolHint> {
    if !input.hints_enabled || is_single_file_read(input) {
        return None;
    }

    if is_explore_subagent(input) {
        return Some(hint(
            HintCategory::ExploreSubagent,
            "For code research subagents, consider adding tokensave MCP context before broad exploration.",
            "tokensave_context can gather focused code context, while tokensave_search, tokensave_callers, and tokensave_impact can answer common research questions without a broad scan.",
            true,
        ));
    }

    if input
        .command
        .as_deref()
        .is_some_and(is_shell_search_command)
    {
        return Some(hint(
            HintCategory::Search,
            "For codebase search, consider using tokensave_search or tokensave_context.",
            "tokensave_search uses the existing index for code search; tokensave_context can gather focused surrounding context when a text search is only a starting point.",
            false,
        ));
    }

    if input
        .tool_name
        .as_deref()
        .is_some_and(|name| matches_normalized(name, &["grep", "search"]))
    {
        return Some(hint(
            HintCategory::Search,
            "For codebase search, consider using tokensave_search or tokensave_context.",
            "tokensave_search uses the existing index for code search; tokensave_context can gather focused surrounding context when a text search is only a starting point.",
            false,
        ));
    }

    if input
        .tool_name
        .as_deref()
        .is_some_and(|name| matches_normalized(name, &["glob"]))
    {
        return Some(hint(
            HintCategory::FileLookup,
            "For finding files by role or path, consider using tokensave_files.",
            "tokensave_files can list indexed files and narrow file lookup before opening individual files.",
            false,
        ));
    }

    let text = combined_text(input);
    if text.is_empty() {
        return None;
    }

    if asks_for_call_graph(&text) {
        return Some(hint(
            HintCategory::CallGraph,
            "For caller or callee questions, consider using the indexed call graph.",
            "tokensave_callers answers who calls a symbol; tokensave_callees answers what a symbol calls.",
            false,
        ));
    }

    if asks_for_impact(&text) {
        return Some(hint(
            HintCategory::Impact,
            "For impact or change-risk questions, consider using tokensave impact tools.",
            "tokensave_impact and tokensave_affected can identify related code and likely affected files from the index.",
            false,
        ));
    }

    if asks_for_broad_read(&text) {
        return Some(hint(
            HintCategory::BroadRead,
            "For broad codebase reading, consider starting with focused tokensave context.",
            "tokensave_context can gather relevant code slices without reading entire directories or the whole repository.",
            false,
        ));
    }

    if asks_for_symbol_lookup(&text) {
        return Some(hint(
            HintCategory::SymbolLookup,
            "For symbol lookup, consider using tokensave indexed symbol tools.",
            "tokensave_context and tokensave_node can locate definitions and nearby relationships from the code graph.",
            false,
        ));
    }

    if asks_for_file_lookup(&text) {
        return Some(hint(
            HintCategory::FileLookup,
            "For finding files by role or path, consider using tokensave_files.",
            "tokensave_files can list indexed files and narrow file lookup before opening individual files.",
            false,
        ));
    }

    None
}

fn hint(category: HintCategory, message: &str, context: &str, nonblocking: bool) -> ToolHint {
    ToolHint {
        category,
        message: message.to_string(),
        context: context.to_string(),
        nonblocking,
    }
}

fn is_single_file_read(input: &ToolHintInput) -> bool {
    let is_read_tool = input
        .tool_name
        .as_deref()
        .is_some_and(|name| matches_normalized(name, &["readfile", "read_file", "read"]));
    is_read_tool
        && input
            .file_path
            .as_deref()
            .is_some_and(|path| !path.is_empty())
        && input.command.as_deref().unwrap_or_default().is_empty()
        && input.prompt.as_deref().unwrap_or_default().is_empty()
        && input
            .subagent_type
            .as_deref()
            .unwrap_or_default()
            .is_empty()
}

fn is_explore_subagent(input: &ToolHintInput) -> bool {
    let is_subagent_tool = input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(name, &["subagent", "agent", "task", "subagentstart"])
    });
    let is_explore_type = input
        .subagent_type
        .as_deref()
        .is_some_and(|kind| matches_normalized(kind, &["explore", "research", "code_research"]));

    is_subagent_tool && is_explore_type
}

fn is_shell_search_command(command: &str) -> bool {
    let tokens = shell_words(command);
    match tokens.first().map(String::as_str) {
        Some("rg") | Some("ripgrep") => true,
        Some("grep") => tokens
            .iter()
            .skip(1)
            .any(|token| is_recursive_grep_flag(token)),
        _ => false,
    }
}

fn shell_words(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|part| {
            part.trim_matches(|c: char| matches!(c, '"' | '\'' | '(' | ')' | ';' | ','))
                .to_ascii_lowercase()
        })
        .filter(|part| !part.is_empty())
        .collect()
}

fn is_recursive_grep_flag(token: &str) -> bool {
    if token == "--recursive" {
        return true;
    }
    if token.starts_with("--") {
        return false;
    }
    token
        .strip_prefix('-')
        .is_some_and(|flags| flags.chars().any(|c| c == 'r'))
}

fn combined_text(input: &ToolHintInput) -> String {
    [input.prompt.as_deref(), input.command.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase()
}

fn asks_for_call_graph(text: &str) -> bool {
    contains_any(
        text,
        &[
            "who calls",
            "callers of",
            "caller of",
            "called by",
            "call graph",
            "call path",
            "call chain",
            "callees of",
        ],
    )
}

fn asks_for_impact(text: &str) -> bool {
    contains_any(
        text,
        &[
            "impact",
            "change risk",
            "change-risk",
            "affected files",
            "what files are affected",
            "what code is affected",
        ],
    )
}

fn asks_for_broad_read(text: &str) -> bool {
    contains_any(
        text,
        &[
            "read every",
            "full contents",
            "entire codebase",
            "whole codebase",
            "scan the codebase",
            "scan the entire",
        ],
    )
}

fn asks_for_symbol_lookup(text: &str) -> bool {
    contains_any(
        text,
        &[
            "symbol lookup",
            "find definition",
            "where is defined",
            "where is this defined",
        ],
    )
}

fn asks_for_file_lookup(text: &str) -> bool {
    contains_any(
        text,
        &["find files", "which files", "list files", "file lookup"],
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn matches_normalized(value: &str, expected: &[&str]) -> bool {
    let normalized = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_ascii_lowercase();
    expected.iter().any(|candidate| normalized == *candidate)
}
