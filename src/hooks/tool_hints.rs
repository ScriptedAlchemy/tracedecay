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

impl HintAgent {
    pub(crate) fn as_key(self) -> &'static str {
        match self {
            HintAgent::Claude => "claude",
            HintAgent::Cursor => "cursor",
            HintAgent::Codex => "codex",
            HintAgent::Kiro => "kiro",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HintCategory {
    Search,
    SemanticSearch,
    FileRead,
    ToolDescriptorRead,
    BroadRead,
    CallGraph,
    Impact,
    SymbolLookup,
    FileLookup,
    ProjectContext,
    SessionRecall,
    AtomicEdit,
    TypeOrientation,
    ExploreSubagent,
    SubagentStartContext,
}

impl HintCategory {
    pub(crate) fn as_key(self) -> &'static str {
        match self {
            HintCategory::Search => "search",
            HintCategory::SemanticSearch => "semantic_search",
            HintCategory::FileRead => "file_read",
            HintCategory::ToolDescriptorRead => "tool_descriptor_read",
            HintCategory::BroadRead => "broad_read",
            HintCategory::CallGraph => "call_graph",
            HintCategory::Impact => "impact",
            HintCategory::SymbolLookup => "symbol_lookup",
            HintCategory::FileLookup => "file_lookup",
            HintCategory::ProjectContext => "project_context",
            HintCategory::SessionRecall => "session_recall",
            HintCategory::AtomicEdit => "atomic_edit",
            HintCategory::TypeOrientation => "type_orientation",
            HintCategory::ExploreSubagent => "explore_subagent",
            HintCategory::SubagentStartContext => "subagent_start_context",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "search" => Some(HintCategory::Search),
            "semantic_search" => Some(HintCategory::SemanticSearch),
            "file_read" => Some(HintCategory::FileRead),
            "tool_descriptor_read" => Some(HintCategory::ToolDescriptorRead),
            "broad_read" => Some(HintCategory::BroadRead),
            "call_graph" => Some(HintCategory::CallGraph),
            "impact" => Some(HintCategory::Impact),
            "symbol_lookup" => Some(HintCategory::SymbolLookup),
            "file_lookup" => Some(HintCategory::FileLookup),
            "project_context" => Some(HintCategory::ProjectContext),
            "session_recall" => Some(HintCategory::SessionRecall),
            "atomic_edit" => Some(HintCategory::AtomicEdit),
            "type_orientation" => Some(HintCategory::TypeOrientation),
            "explore_subagent" => Some(HintCategory::ExploreSubagent),
            "subagent_start_context" => Some(HintCategory::SubagentStartContext),
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
            "For code research subagents, consider adding tracedecay MCP context before broad exploration.",
            "tracedecay_context can gather focused code context, while tracedecay_search, tracedecay_callers, and tracedecay_impact can answer common research questions without a broad scan.",
            true,
        ));
    }

    if is_semantic_search_tool(input) {
        return Some(hint(
            HintCategory::SemanticSearch,
            "For conceptual codebase questions, consider tracedecay_context.",
            "tracedecay_context answers concept-level queries from the pre-built code graph (add keywords to expand synonyms); tracedecay_search ranks symbols by name/keyword.",
            true,
        ));
    }

    let text = combined_text(input);
    if asks_for_session_recall(&text) {
        return Some(hint(
            HintCategory::SessionRecall,
            "For prior conversation context, consider TraceDecay session search.",
            "tracedecay_message_search searches ingested agent transcripts across providers; tracedecay_lcm_grep can search bounded raw-message snippets and summaries when you need session-level recall before re-discovering context.",
            false,
        ));
    }

    if asks_for_project_context(&text)
        || input
            .command
            .as_deref()
            .is_some_and(is_project_discovery_command)
    {
        return Some(hint(
            HintCategory::ProjectContext,
            "For other repos or registered projects, consider TraceDecay project registry tools.",
            "tracedecay_project_list shows known projects; tracedecay_project_search can find a sibling repo by name/path/remote; pass project_path or project_id to tracedecay_context/search for cross-project code context before scanning parent directories.",
            false,
        ));
    }

    if asks_for_call_graph(&text) {
        return Some(hint(
            HintCategory::CallGraph,
            "For function tracing, use the indexed call graph before grep/file reads.",
            "Resolve the symbol with tracedecay_find_exact_symbol or tracedecay_search, then use tracedecay_callers for who depends on it and tracedecay_callees for what it calls; use tracedecay_impact for broader dependents before opening files.",
            false,
        ));
    }

    if asks_for_impact(&text) {
        return Some(hint(
            HintCategory::Impact,
            "For impact, affected-test, or blast-radius questions, use TraceDecay's dependency tools.",
            "Start with tracedecay_diff_context when you have changed files, tracedecay_impact for a resolved symbol, tracedecay_affected for affected tests, and tracedecay_test_map when you need direct test attribution.",
            false,
        ));
    }

    if asks_for_atomic_edit(&text) {
        return Some(hint(
            HintCategory::AtomicEdit,
            "For safe mechanical edits, use TraceDecay's anchored edit tools.",
            "Use tracedecay_multi_str_replace for all-or-nothing anchored replacements, tracedecay_ast_grep_rewrite for structural rewrites, and tracedecay_replace_symbol when replacing one resolved symbol.",
            false,
        ));
    }

    if asks_for_type_orientation(&text) {
        return Some(hint(
            HintCategory::TypeOrientation,
            "For type, constructor, field, trait, or duplicate-logic questions, use TraceDecay's AST orientation tools.",
            "Use tracedecay_constructors for struct literal sites, tracedecay_field_sites for reads/writes, tracedecay_impls or tracedecay_implementations for trait methods, and tracedecay_redundancy before adding similar helpers.",
            false,
        ));
    }

    if input
        .command
        .as_deref()
        .is_some_and(is_shell_search_command)
    {
        return Some(hint(
            HintCategory::Search,
            "For codebase search, consider using tracedecay_search or tracedecay_context.",
            "tracedecay_search uses the existing index for code search; tracedecay_context can gather focused surrounding context when a text search is only a starting point.",
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
            "For codebase search, consider using tracedecay_search or tracedecay_context.",
            "tracedecay_search uses the existing index for code search; tracedecay_context can gather focused surrounding context when a text search is only a starting point.",
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
            "For finding files by role or path, consider using tracedecay_files.",
            "tracedecay_files can list indexed files and narrow file lookup before opening individual files.",
            false,
        ));
    }

    if is_tracedecay_tool_descriptor_read(input) {
        return Some(hint(
            HintCategory::ToolDescriptorRead,
            "This looks like a TraceDecay MCP tool descriptor; use the tool surface instead of reading schema JSON.",
            "Call the named tracedecay_* MCP tool directly when available, or use tool discovery for its schema; for function tracing that usually means tracedecay_find_exact_symbol plus tracedecay_callers/tracedecay_callees.",
            true,
        ));
    }

    if is_single_file_read(input) {
        return Some(hint(
            HintCategory::FileRead,
            "Before reading whole files, consider tracedecay_outline, tracedecay_body, or tracedecay_read.",
            "tracedecay_outline gives a file's table of contents, tracedecay_body returns one symbol's source, and tracedecay_read (mode: \"lines\") slices a range — usually far cheaper than a full-file read.",
            true,
        ));
    }

    if asks_for_broad_read(&text) {
        return Some(hint(
            HintCategory::BroadRead,
            "For broad codebase reading, consider starting with focused tracedecay context.",
            "tracedecay_context can gather relevant code slices without reading entire directories or the whole repository.",
            false,
        ));
    }

    if asks_for_symbol_lookup(&text) {
        return Some(hint(
            HintCategory::SymbolLookup,
            "For symbol lookup, consider using tracedecay indexed symbol tools.",
            "tracedecay_context and tracedecay_node can locate definitions and nearby relationships from the code graph.",
            false,
        ));
    }

    if asks_for_file_lookup(&text) {
        return Some(hint(
            HintCategory::FileLookup,
            "For finding files by role or path, consider using tracedecay_files.",
            "tracedecay_files can list indexed files and narrow file lookup before opening individual files.",
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

fn is_tracedecay_tool_descriptor_read(input: &ToolHintInput) -> bool {
    let is_read_tool = input
        .tool_name
        .as_deref()
        .is_some_and(|name| matches_normalized(name, &["readfile", "read_file", "read"]));
    is_read_tool
        && input.file_path.as_deref().is_some_and(|path| {
            (path.contains("/tools/tracedecay_") || path.contains("\\tools\\tracedecay_"))
                && std::path::Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
}

/// Matches Cursor's semantic/codebase-search tool names. Cursor's hooks docs do
/// not enumerate a matcher value for semantic search, so the post-tool-use hook
/// runs unmatched and this predicate recognizes the tool names Cursor has
/// reported for it (`SemanticSearch`, `codebase_search`, `Codebase Search`).
fn is_semantic_search_tool(input: &ToolHintInput) -> bool {
    input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(
            name,
            &[
                "semanticsearch",
                "semantic_search",
                "codebasesearch",
                "codebase_search",
            ],
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
    // The quote/escape-aware parser shared with hooks.rs: quoted arguments
    // stay single tokens, so a pattern like `grep "needle -r" file` can no
    // longer leak a fake `-r` flag (the old split_whitespace misparse).
    let tokens = super::shell_words(command);
    let Some(first) = tokens.first() else {
        return false;
    };
    // Tolerate a leading subshell paren (`(grep -r foo)`), which the shell
    // parser keeps attached to the first word.
    let program = first.trim_start_matches('(').to_ascii_lowercase();
    match program.as_str() {
        "rg" | "ripgrep" => true,
        "grep" => tokens
            .iter()
            .skip(1)
            .any(|token| is_recursive_grep_flag(token)),
        _ => false,
    }
}

fn is_project_discovery_command(command: &str) -> bool {
    let tokens = super::shell_words(command);
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = first.trim_start_matches('(').to_ascii_lowercase();
    match program.as_str() {
        "find" | "fd" | "fdfind" => tokens
            .iter()
            .skip(1)
            .any(|token| is_parent_or_projects_path(token)),
        "rg" | "ripgrep" | "grep" => tokens
            .iter()
            .skip(1)
            .any(|token| is_parent_or_projects_path(token)),
        _ => false,
    }
}

fn is_parent_or_projects_path(token: &str) -> bool {
    let token = token.trim_matches(|c| matches!(c, '(' | ')' | '"' | '\''));
    token == ".."
        || token.starts_with("../")
        || token.contains("/../")
        || token.contains("/projects/")
        || token.ends_with("/projects")
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
            "trace function",
            "trace the function",
            "trace functions",
            "trace the functions",
            "function trace",
            "find callers",
            "find caller",
            "find callees",
            "find callee",
            "who calls",
            "what calls",
            "callers of",
            "caller of",
            "called by",
            "call graph",
            "call path",
            "call chain",
            "callees of",
            "uses of",
            "depend on",
            "depends on",
            "what depends",
        ],
    )
}

fn asks_for_impact(text: &str) -> bool {
    contains_any(
        text,
        &[
            "impact",
            "blast radius",
            "change risk",
            "change-risk",
            "affected tests",
            "affected files",
            "test map",
            "test_map",
            "what files are affected",
            "what code is affected",
            "which tests",
            "what tests",
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

fn asks_for_project_context(text: &str) -> bool {
    mentions_external_project_scope(text) || asks_for_repo_discovery(text)
}

fn mentions_external_project_scope(text: &str) -> bool {
    contains_any(
        text,
        &[
            "another repo",
            "another repository",
            "other repo",
            "other repository",
            "external repo",
            "external repository",
            "sibling repo",
            "sibling repository",
            "neighbor repo",
            "neighbor repository",
            "nearby repo",
            "nearby repository",
            "next door",
            "registered project",
            "project registry",
            "project listing",
            "project list",
            "project search",
            "cross-project",
            "cross project",
            "orchestrator repo",
            "orchestrator repository",
        ],
    )
}

fn asks_for_repo_discovery(text: &str) -> bool {
    !mentions_current_project_scope(text)
        && contains_any(text, &[" repo", " repository"])
        && contains_any(text, &["find", "locate", "where", "which"])
}

fn mentions_current_project_scope(text: &str) -> bool {
    contains_any(
        text,
        &[
            "this repo",
            "this repository",
            "current repo",
            "current repository",
            "current workspace",
            "this workspace",
            "in repo",
            "in repository",
            "in the repo",
            "in the repository",
            "inside repo",
            "inside the repo",
        ],
    )
}

fn asks_for_session_recall(text: &str) -> bool {
    contains_any(
        text,
        &[
            "where did we",
            "what did we",
            "when did we",
            "did we talk",
            "talk about",
            "discuss before",
            "mentioned before",
            "prior conversation",
            "previous conversation",
            "earlier conversation",
            "session search",
            "session recall",
            "conversation history",
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

fn asks_for_atomic_edit(text: &str) -> bool {
    contains_any(
        text,
        &[
            "edit safely",
            "safe edit",
            "mechanical edit",
            "mechanical rewrite",
            "replace this everywhere",
            "replace everywhere",
            "rewrite structurally",
            "structural rewrite",
            "ast-grep",
            "ast grep",
            "multi_str_replace",
            "ast_grep_rewrite",
        ],
    )
}

fn asks_for_type_orientation(text: &str) -> bool {
    contains_any(
        text,
        &[
            "constructor sites",
            "constructors",
            "struct literal",
            "field use",
            "field uses",
            "field reads",
            "field writes",
            "trait impl",
            "trait impls",
            "trait implementations",
            "implementors",
            "impl blocks",
            "duplicate logic",
            "redundant",
            "similar helper",
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
            assert!(hint.context.contains("tracedecay_context"), "{name}");
            assert!(hint.nonblocking, "semantic-search hints must stay soft");
        }
    }

    #[test]
    fn parent_directory_find_gets_project_registry_hint() {
        let hint = decide_hint(&ToolHintInput {
            tool_name: Some("shell".to_string()),
            command: Some("find .. -maxdepth 3 -type f -iname '*runner*'".to_string()),
            prompt: Some(
                "Find where the clean-ci Windows runner orchestrator is defined".to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "project_context");
        assert!(hint.context.contains("tracedecay_project_list"));
        assert!(hint.context.contains("tracedecay_project_search"));
    }

    #[test]
    fn external_repo_shell_search_prefers_project_registry_hint() {
        let hint = decide_hint(&ToolHintInput {
            tool_name: Some("shell".to_string()),
            command: Some("rg -n \"proxmox|windows|runner|clean-ci\" .".to_string()),
            prompt: Some(
                "Find the runner orchestrator repo and update its Windows boxes".to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "project_context");
        assert!(hint.message.contains("registered projects"));
    }

    #[test]
    fn current_repo_shell_search_keeps_normal_search_hint() {
        let hint = decide_hint(&ToolHintInput {
            tool_name: Some("shell".to_string()),
            command: Some("rg -n \"runner\" .".to_string()),
            prompt: Some("Search this repo for the runner implementation".to_string()),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "search");
        assert!(hint.context.contains("tracedecay_search"));
    }

    #[test]
    fn trace_function_prompts_get_call_graph_ladder_before_generic_search() {
        let hint = decide_hint(&ToolHintInput {
            tool_name: Some("shell".to_string()),
            command: Some("rg -n \"setup_project\" tests/mcp_handler_test.rs".to_string()),
            prompt: Some(
                "Use TraceDecay to trace the function and find callers of setup_project"
                    .to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "call_graph");
        assert!(hint.context.contains("tracedecay_find_exact_symbol"));
        assert!(hint.context.contains("tracedecay_callers"));
        assert!(hint.context.contains("tracedecay_callees"));
    }

    #[test]
    fn dependency_fixture_prompts_get_call_graph_ladder() {
        let hint = decide_hint(&ToolHintInput {
            prompt: Some(
                "Which tests still depend on setup_project instead of setup_empty_project?"
                    .to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "call_graph");
        assert!(hint.context.contains("tracedecay_callers"));
        assert!(hint.context.contains("tracedecay_impact"));
    }

    #[test]
    fn affected_test_prompts_get_test_mapping_ladder() {
        let hint = decide_hint(&ToolHintInput {
            prompt: Some(
                "Find affected tests and blast radius for this refactor before running cargo"
                    .to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "impact");
        assert!(hint.context.contains("tracedecay_diff_context"));
        assert!(hint.context.contains("tracedecay_affected"));
        assert!(hint.context.contains("tracedecay_test_map"));
    }

    #[test]
    fn mechanical_edit_prompts_get_atomic_edit_ladder() {
        let hint = decide_hint(&ToolHintInput {
            prompt: Some(
                "Use ast-grep for a mechanical rewrite and replace this everywhere safely"
                    .to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "atomic_edit");
        assert!(hint.context.contains("tracedecay_multi_str_replace"));
        assert!(hint.context.contains("tracedecay_ast_grep_rewrite"));
    }

    #[test]
    fn type_orientation_prompts_get_ast_graph_ladder() {
        let hint = decide_hint(&ToolHintInput {
            prompt: Some(
                "Find constructor sites, field writes, trait impls, and duplicate logic"
                    .to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "type_orientation");
        assert!(hint.context.contains("tracedecay_constructors"));
        assert!(hint.context.contains("tracedecay_field_sites"));
        assert!(hint.context.contains("tracedecay_redundancy"));
    }

    #[test]
    fn prior_conversation_prompt_gets_session_recall_hint() {
        let hint = decide_hint(&ToolHintInput {
            prompt: Some(
                "Where did we talk about clean-ci and the runner orchestrator before?".to_string(),
            ),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category.as_key(), "session_recall");
        assert!(hint.context.contains("tracedecay_message_search"));
        assert!(hint.context.contains("tracedecay_lcm_grep"));
    }

    #[test]
    fn single_file_read_gets_a_soft_outline_hint() {
        let mut input = input_for_tool("Read");
        input.file_path = Some("src/lib.rs".to_string());
        let hint = decide_hint(&input).unwrap();
        assert_eq!(hint.category, HintCategory::FileRead);
        assert!(hint.message.contains("tracedecay_outline"));
        assert!(hint.nonblocking, "read hints must stay soft");
    }

    #[test]
    fn tracedecay_tool_schema_reads_get_direct_tool_hint() {
        let mut input = input_for_tool("ReadFile");
        input.file_path = Some(
            "/home/zack/.cursor/projects/repo/mcps/plugin-tracedecay/tools/tracedecay_callers.json"
                .to_string(),
        );
        let hint = decide_hint(&input).unwrap();

        assert_eq!(hint.category, HintCategory::ToolDescriptorRead);
        assert!(hint.message.contains("tool descriptor"));
        assert!(hint.context.contains("tracedecay_callers"));
        assert!(hint.context.contains("tracedecay_callees"));
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
        assert!(dedupe.should_emit("s1", HintCategory::ToolDescriptorRead));
        assert!(dedupe.should_emit("s2", HintCategory::Search));
    }

    #[test]
    fn descriptor_reads_dedupe_separately_from_source_file_reads() {
        let mut dedupe = ToolHintDedupe::default();
        assert!(dedupe.should_emit("s1", HintCategory::FileRead));
        assert!(dedupe.should_emit("s1", HintCategory::ToolDescriptorRead));
        assert!(!dedupe.should_emit("s1", HintCategory::FileRead));
        assert!(!dedupe.should_emit("s1", HintCategory::ToolDescriptorRead));
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
    fn shell_search_classification_honors_quoting() {
        assert!(is_shell_search_command("rg foo src/"));
        assert!(is_shell_search_command("grep -r foo ."));
        assert!(is_shell_search_command("grep --recursive foo ."));
        assert!(is_shell_search_command("(grep -r foo .)"));
        // Quoted multi-word pattern: still a recursive grep.
        assert!(is_shell_search_command("grep -r \"foo bar\" src/"));
        // A flag-looking string INSIDE quotes is data, not a flag — the old
        // split_whitespace parser misclassified this as recursive.
        assert!(!is_shell_search_command("grep \"needle -r\" file.txt"));
        assert!(!is_shell_search_command("grep foo file.txt"));
        assert!(!is_shell_search_command("cat file.txt"));
        assert!(!is_shell_search_command(""));
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
