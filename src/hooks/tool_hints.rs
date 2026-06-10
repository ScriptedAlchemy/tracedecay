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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HintCategory {
    Search,
    SemanticSearch,
    FileRead,
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
            HintCategory::SemanticSearch => "semantic_search",
            HintCategory::FileRead => "file_read",
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
            "semantic_search" => Some(HintCategory::SemanticSearch),
            "file_read" => Some(HintCategory::FileRead),
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

/// Upper bound on persisted (session, category) pairs. The file accrues a
/// handful of entries per session; past this bound it is stale history from
/// long-dead sessions, so the store resets rather than growing forever.
const MAX_PERSISTED_HINT_ENTRIES: usize = 4096;

#[derive(Debug, Default)]
pub struct ToolHintDedupe {
    seen: HashSet<(String, HintCategory)>,
}

impl ToolHintDedupe {
    pub fn should_emit(&mut self, session_id: impl Into<String>, category: HintCategory) -> bool {
        self.seen.insert((session_id.into(), category))
    }

    /// Loads the dedupe set from `path`, tolerating a missing file (empty set)
    /// and resetting when the persisted history exceeds
    /// [`MAX_PERSISTED_HINT_ENTRIES`].
    pub fn load_or_default(path: &Path) -> Self {
        match Self::load(path) {
            Ok(loaded) if loaded.seen.len() <= MAX_PERSISTED_HINT_ENTRIES => loaded,
            _ => Self::default(),
        }
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
    if !input.hints_enabled {
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

    if is_semantic_search_tool(input) {
        return Some(hint(
            HintCategory::SemanticSearch,
            "For conceptual codebase questions, consider tokensave_context.",
            "tokensave_context answers concept-level queries from the pre-built code graph (add keywords to expand synonyms); tokensave_search ranks symbols by name/keyword.",
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

    if is_single_file_read(input) {
        return Some(hint(
            HintCategory::FileRead,
            "Before reading whole files, consider tokensave_outline, tokensave_body, or tokensave_read.",
            "tokensave_outline gives a file's table of contents, tokensave_body returns one symbol's source, and tokensave_read (mode: \"lines\") slices a range — usually far cheaper than a full-file read.",
            true,
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

/// Matches Cursor's semantic/codebase-search tool names. Cursor's hooks docs do
/// not enumerate a matcher value for semantic search, so the post-tool-use hook
/// runs unmatched and this predicate recognizes the tool names Cursor has
/// reported for it (`SemanticSearch`, `codebase_search`, `Codebase Search`).
fn is_semantic_search_tool(input: &ToolHintInput) -> bool {
    input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(
            name,
            &["semanticsearch", "semantic_search", "codebasesearch", "codebase_search"],
        )
    })
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
        Some("rg" | "ripgrep") => true,
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn input_for_tool(tool_name: &str) -> ToolHintInput {
        ToolHintInput {
            tool_name: Some(tool_name.to_string()),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        }
    }

    #[test]
    fn semantic_search_tools_get_a_context_hint() {
        for name in ["SemanticSearch", "codebase_search", "Codebase Search"] {
            let hint = decide_hint(&input_for_tool(name)).unwrap();
            assert_eq!(hint.category, HintCategory::SemanticSearch, "{name}");
            assert!(hint.context.contains("tokensave_context"), "{name}");
            assert!(hint.nonblocking, "semantic-search hints must stay soft");
        }
    }

    #[test]
    fn single_file_read_gets_a_soft_outline_hint() {
        let mut input = input_for_tool("Read");
        input.file_path = Some("src/lib.rs".to_string());
        let hint = decide_hint(&input).unwrap();
        assert_eq!(hint.category, HintCategory::FileRead);
        assert!(hint.message.contains("tokensave_outline"));
        assert!(hint.nonblocking, "read hints must stay soft");
    }

    #[test]
    fn read_without_file_path_gets_no_hint() {
        assert!(decide_hint(&input_for_tool("Read")).is_none());
    }

    #[test]
    fn dedupe_emits_each_category_once_per_session() {
        let mut dedupe = ToolHintDedupe::default();
        assert!(dedupe.should_emit("s1", HintCategory::Search));
        assert!(!dedupe.should_emit("s1", HintCategory::Search));
        assert!(dedupe.should_emit("s1", HintCategory::FileRead));
        assert!(dedupe.should_emit("s2", HintCategory::Search));
    }

    #[test]
    fn dedupe_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/tool_hints_seen.json");

        let mut dedupe = ToolHintDedupe::load_or_default(&path);
        assert!(dedupe.should_emit("s1", HintCategory::Search));
        dedupe.save(&path).unwrap();

        let mut reloaded = ToolHintDedupe::load_or_default(&path);
        assert!(
            !reloaded.should_emit("s1", HintCategory::Search),
            "persisted (session, category) pairs must suppress re-emission"
        );
        assert!(reloaded.should_emit("s1", HintCategory::FileRead));
    }

    #[test]
    fn dedupe_load_tolerates_missing_and_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let mut dedupe = ToolHintDedupe::load_or_default(&missing);
        assert!(dedupe.should_emit("s1", HintCategory::Search));

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, "not json").unwrap();
        let mut dedupe = ToolHintDedupe::load_or_default(&corrupt);
        assert!(dedupe.should_emit("s1", HintCategory::Search));
    }
}
