//! Pure soft-hint decisions shared by hook adapters.
//!
//! This module intentionally returns model-visible text only. It does not deny,
//! rewrite, or otherwise decide permissions; adapters can choose how to surface
//! a returned hint for their own hook schema.

use std::collections::HashMap;
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
    BuildDiagnostics,
    MemoryStore,
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
            HintCategory::BuildDiagnostics => "build_diagnostics",
            HintCategory::MemoryStore => "memory_store",
        }
    }

    /// Human-readable name used in the escalation message prefix
    /// ("Repeated native <label> usage this session — ...").
    fn label(self) -> &'static str {
        match self {
            HintCategory::Search => "search",
            HintCategory::SemanticSearch => "semantic search",
            HintCategory::FileRead => "file read",
            HintCategory::ToolDescriptorRead => "tool descriptor read",
            HintCategory::BroadRead => "broad read",
            HintCategory::CallGraph => "call-graph",
            HintCategory::Impact => "impact",
            HintCategory::SymbolLookup => "symbol lookup",
            HintCategory::FileLookup => "file lookup",
            HintCategory::ProjectContext => "project context",
            HintCategory::SessionRecall => "session recall",
            HintCategory::AtomicEdit => "atomic edit",
            HintCategory::TypeOrientation => "type orientation",
            HintCategory::ExploreSubagent => "explore subagent",
            HintCategory::SubagentStartContext => "subagent start context",
            HintCategory::BuildDiagnostics => "build diagnostics",
            HintCategory::MemoryStore => "memory store",
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
            "build_diagnostics" => Some(HintCategory::BuildDiagnostics),
            "memory_store" => Some(HintCategory::MemoryStore),
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

impl ToolHint {
    /// Returns the stronger re-hint variant used for the one-time escalation
    /// after repeated native usage of this category in a session. Prefixes the
    /// base message; context and softness are preserved.
    #[must_use]
    pub fn escalated(&self) -> ToolHint {
        ToolHint {
            category: self.category,
            message: format!(
                "Repeated native {} usage this session — {}",
                self.category.label(),
                self.message
            ),
            context: self.context.clone(),
            nonblocking: self.nonblocking,
        }
    }
}

/// Upper bound on persisted (session, category) pairs. The file accrues a
/// handful of entries per session; past this bound it is stale history from
/// long-dead sessions, so the store resets rather than growing forever.
const MAX_PERSISTED_HINT_ENTRIES: usize = 4096;

/// At most this many hints surface across all categories in one session. A
/// session that trips many native patterns still gets only a few nudges before
/// the budget silences the rest — the historical model gave a pathological
/// session up to one hint per category (15) with no cap.
pub const MAX_HINTS_PER_SESSION: usize = 3;

/// After a category has been hinted, its native pattern must fire this many
/// more times in the same session before the single stronger escalation hint is
/// allowed. Chosen so a session that keeps reaching for the same native tool
/// gets exactly one louder reminder, then permanent silence for that category.
pub const ESCALATION_TRIGGER_THRESHOLD: u32 = 3;

/// Outcome of a single hint-candidate decision. Mirrors the terminal-event
/// vocabulary the analytics layer records: `Emit`/`Escalate` surface a hint,
/// while the `Suppressed*` variants drop it for distinct reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintDecision {
    /// First surfacing of this category in the session; counts against the
    /// per-session budget.
    Emit,
    /// The one-time stronger re-hint after repeated native usage. Does not count
    /// against the budget (it is the escalation allowance).
    Escalate,
    /// This category already surfaced (or already escalated) and is now silent,
    /// or its escalation threshold is not yet reached.
    SuppressedDuplicate,
    /// The per-session hint budget is exhausted, so a not-yet-seen category is
    /// held back.
    SuppressedBudget,
}

/// Per (session, category) escalation bookkeeping.
#[derive(Debug, Clone, Copy, Default)]
struct CategoryState {
    /// Whether this category has already surfaced an initial hint.
    hinted: bool,
    /// Native-pattern fires observed after the initial hint. Escalation unlocks
    /// once this reaches [`ESCALATION_TRIGGER_THRESHOLD`].
    triggers_after_hint: u32,
    /// Whether the single stronger escalation hint has already been spent.
    escalated: bool,
}

#[derive(Debug, Default)]
pub struct ToolHintDedupe {
    /// Number of budget-counting hints already emitted per session.
    emitted: HashMap<String, usize>,
    /// Per (session, category) escalation state.
    categories: HashMap<(String, HintCategory), CategoryState>,
}

impl ToolHintDedupe {
    /// Decides what to do with one hint candidate, updating per-session budget
    /// and per-category escalation counters. This is the impure dedupe layer;
    /// the pure text decision lives in [`decide_hint`].
    pub fn decide(
        &mut self,
        session_id: impl Into<String>,
        category: HintCategory,
    ) -> HintDecision {
        let session_id = session_id.into();
        let state = self
            .categories
            .entry((session_id.clone(), category))
            .or_default();

        if !state.hinted {
            // First time this category fires. Gate on the session budget.
            let emitted = self.emitted.entry(session_id).or_default();
            if *emitted >= MAX_HINTS_PER_SESSION {
                return HintDecision::SuppressedBudget;
            }
            *emitted += 1;
            state.hinted = true;
            return HintDecision::Emit;
        }

        if state.escalated {
            // Already spent the single escalation allowance for this category.
            return HintDecision::SuppressedDuplicate;
        }

        state.triggers_after_hint += 1;
        if state.triggers_after_hint >= ESCALATION_TRIGGER_THRESHOLD {
            state.escalated = true;
            return HintDecision::Escalate;
        }

        HintDecision::SuppressedDuplicate
    }

    /// Loads the dedupe state from `path`, tolerating a missing file (empty
    /// state) and resetting when the persisted history exceeds
    /// [`MAX_PERSISTED_HINT_ENTRIES`].
    pub fn load_or_default(path: &Path) -> Self {
        match Self::load(path) {
            Ok(loaded) if loaded.persisted_len() <= MAX_PERSISTED_HINT_ENTRIES => loaded,
            _ => Self::default(),
        }
    }

    /// Number of persisted (session, category) rows this state serializes to —
    /// the bound [`load_or_default`] enforces against stale-history growth.
    fn persisted_len(&self) -> usize {
        self.categories.len()
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        // v2: {"version":2, "sessions":[...], "categories":[...]}. v1: a bare
        // array of {session_id, category}. Probe for the versioned object first,
        // then fall back to the legacy array so old files load losslessly.
        if let Ok(persisted) = serde_json::from_str::<PersistedHints>(&content) {
            return Ok(Self::from_persisted(persisted));
        }
        let entries: Vec<PersistedHintEntry> = serde_json::from_str(&content).unwrap_or_default();
        Ok(Self::from_v1_entries(entries))
    }

    fn from_v1_entries(entries: Vec<PersistedHintEntry>) -> Self {
        // A v1 entry records that the category was hinted once; it carries no
        // budget or escalation counters. Reconstruct `hinted` state and
        // per-session emitted counts so a v1->v2 load preserves suppression.
        let mut dedupe = Self::default();
        for entry in entries {
            let Some(category) = HintCategory::from_key(&entry.category) else {
                continue;
            };
            let already = dedupe
                .categories
                .insert(
                    (entry.session_id.clone(), category),
                    CategoryState {
                        hinted: true,
                        triggers_after_hint: 0,
                        escalated: false,
                    },
                )
                .is_some();
            if !already {
                *dedupe.emitted.entry(entry.session_id).or_default() += 1;
            }
        }
        dedupe
    }

    fn from_persisted(persisted: PersistedHints) -> Self {
        let mut categories = HashMap::new();
        for entry in persisted.categories {
            let Some(category) = HintCategory::from_key(&entry.category) else {
                continue;
            };
            categories.insert(
                (entry.session_id, category),
                CategoryState {
                    hinted: entry.hinted,
                    triggers_after_hint: entry.triggers_after_hint,
                    escalated: entry.escalated,
                },
            );
        }
        let emitted = persisted
            .sessions
            .into_iter()
            .map(|session| (session.session_id, session.emitted))
            .collect();
        Self {
            emitted,
            categories,
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut sessions: Vec<PersistedSession> = self
            .emitted
            .iter()
            .map(|(session_id, emitted)| PersistedSession {
                session_id: session_id.clone(),
                emitted: *emitted,
            })
            .collect();
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        let mut categories: Vec<PersistedCategory> = self
            .categories
            .iter()
            .map(|((session_id, category), state)| PersistedCategory {
                session_id: session_id.clone(),
                category: category.as_key().to_string(),
                hinted: state.hinted,
                triggers_after_hint: state.triggers_after_hint,
                escalated: state.escalated,
            })
            .collect();
        categories.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then_with(|| a.category.cmp(&b.category))
        });
        let persisted = PersistedHints {
            version: HINT_STORE_VERSION,
            sessions,
            categories,
        };
        let json = serde_json::to_string_pretty(&persisted).map_err(std::io::Error::other)?;
        std::fs::write(path, format!("{json}\n"))
    }
}

/// Schema version written by [`ToolHintDedupe::save`]. v1 was a bare entry array.
const HINT_STORE_VERSION: u32 = 2;

/// v2 persisted schema: per-session budget counters plus per (session, category)
/// escalation state. `#[serde(default)]` keeps forward/partial reads lossless.
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedHints {
    version: u32,
    #[serde(default)]
    sessions: Vec<PersistedSession>,
    #[serde(default)]
    categories: Vec<PersistedCategory>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedSession {
    session_id: String,
    emitted: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedCategory {
    session_id: String,
    category: String,
    #[serde(default)]
    hinted: bool,
    #[serde(default)]
    triggers_after_hint: u32,
    #[serde(default)]
    escalated: bool,
}

/// Legacy v1 entry: a bare `{session_id, category}` pair meaning "this category
/// was hinted once this session". Still parsed so old stores migrate losslessly.
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
            "tracedecay_context answers concept-level queries from the pre-built code graph (add keywords to expand synonyms); tracedecay_search ranks symbols by name/keyword; tracedecay_grep matches a literal or regex string when you want exact text, not a concept.",
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
        .is_some_and(is_build_diagnostics_command)
    {
        return Some(hint(
            HintCategory::BuildDiagnostics,
            "For build/type-check errors, use TraceDecay's diagnostics tools instead of parsing raw compiler output.",
            "tracedecay_diagnostics runs (or reads) the project's diagnostics and maps each error to its enclosing symbol; tracedecay_diagnose adds caller/impact context for a specific failure so you fix the root cause, not just the line the compiler points at.",
            false,
        ));
    }

    if is_memory_store_edit(input) {
        return Some(hint(
            HintCategory::MemoryStore,
            "For durable facts, prefer tracedecay_fact_store over hand-editing harness memory files.",
            "tracedecay_fact_store persists a trust-ranked project/user fact that survives across sessions and is recalled by tracedecay_context and tracedecay_recall; a memory markdown edit is only visible to the current harness. Keep secrets and unnecessary PII out of stored facts.",
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
            "For codebase search, route by what you're matching: literal/regex text -> tracedecay_grep; symbol name -> tracedecay_search; concept -> tracedecay_context.",
            "tracedecay_grep runs a literal or regex content search over the indexed tree (pattern, fixed_strings, path_glob) and enriches each hit with its enclosing symbol; tracedecay_search ranks symbols by name; tracedecay_context answers concept-level questions. Grep/ripgrep still fit prose and un-indexed files.",
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
            "For codebase search, route by what you're matching: literal/regex text -> tracedecay_grep; symbol name -> tracedecay_search; concept -> tracedecay_context.",
            "tracedecay_grep runs a literal or regex content search over the indexed tree (pattern, fixed_strings, path_glob) and enriches each hit with its enclosing symbol; tracedecay_search ranks symbols by name; tracedecay_context answers concept-level questions. Grep/ripgrep still fit prose and un-indexed files.",
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
            "tracedecay_outline gives a file's table of contents, tracedecay_body returns one symbol's source, and tracedecay_read (mode: \"lines\") slices a range — usually far cheaper than a full-file read. If you are opening the file only to find a string in it, tracedecay_grep locates the literal or regex match with its enclosing symbol instead.",
            true,
        ));
    }

    if asks_for_broad_read(&text) {
        return Some(hint(
            HintCategory::BroadRead,
            "For broad codebase reading, consider starting with focused tracedecay context.",
            "tracedecay_context gathers relevant code slices without reading entire directories or the whole repository; tracedecay_grep sweeps the indexed tree for a literal or regex string when you are hunting for exact text rather than a concept.",
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

/// Classifies a shell command as a build/type-check invocation whose output the
/// model is about to parse by hand: `cargo check|build|clippy|test`, a bare
/// `tsc`, `npx tsc`, or `pyright`. Quote-aware like the other shell classifiers
/// so a needle such as `grep "cargo check"` is data, not a program.
fn is_build_diagnostics_command(command: &str) -> bool {
    let tokens = super::shell_words(command);
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = first.trim_start_matches('(').to_ascii_lowercase();
    // The program name without any directory prefix (e.g. `/usr/bin/tsc` -> `tsc`).
    let base = program.rsplit(['/', '\\']).next().unwrap_or(&program);
    match base {
        "cargo" => tokens.iter().skip(1).any(|token| {
            matches!(
                token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                "check" | "build" | "clippy" | "test"
            )
        }),
        "tsc" | "pyright" | "pyright-python" => true,
        "npx" | "pnpm" | "yarn" | "bunx" => tokens.iter().skip(1).any(|token| {
            matches!(
                token.trim_start_matches('(').to_ascii_lowercase().as_str(),
                "tsc" | "pyright"
            )
        }),
        _ => false,
    }
}

/// True when a Write/Edit event targets a harness-memory location where a
/// durable fact belongs in `TraceDecay` memory instead: `*/.claude/**/memory/*.md`,
/// any `MEMORY.md`, or any `CLAUDE.md`.
fn is_memory_store_edit(input: &ToolHintInput) -> bool {
    let is_edit_tool = input.tool_name.as_deref().is_some_and(|name| {
        matches_normalized(name, &["write", "edit", "multiedit", "notebookedit"])
    });
    is_edit_tool
        && input
            .file_path
            .as_deref()
            .is_some_and(is_harness_memory_path)
}

/// Matches the harness-memory file locations that should route durable facts to
/// `tracedecay_fact_store`. Normalizes `\\` to `/` so Windows paths match too.
pub(super) fn is_harness_memory_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let file_name = normalized.rsplit('/').next().unwrap_or(&normalized);
    if file_name.eq_ignore_ascii_case("MEMORY.md") || file_name.eq_ignore_ascii_case("CLAUDE.md") {
        return true;
    }
    // `*/.claude/**/memory/*.md`: a `.claude` segment somewhere above a `memory`
    // directory that directly holds the `.md` file.
    let is_markdown = std::path::Path::new(file_name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
    if !is_markdown {
        return false;
    }
    let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    // The file's parent directory must be `memory`, and some ancestor `.claude`.
    let Some(parent_idx) = segments.len().checked_sub(2) else {
        return false;
    };
    if !segments[parent_idx].eq_ignore_ascii_case("memory") {
        return false;
    }
    segments[..parent_idx]
        .iter()
        .any(|segment| segment.eq_ignore_ascii_case(".claude"))
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
            assert!(
                hint.context.contains("tracedecay_grep"),
                "semantic-search hint must route literal text to tracedecay_grep: {name}"
            );
            assert!(hint.nonblocking, "semantic-search hints must stay soft");
        }
    }

    #[test]
    fn grep_tool_search_routes_literal_matches_to_grep() {
        for name in ["Grep", "search"] {
            let hint = decide_hint(&input_for_tool(name)).unwrap();
            assert_eq!(hint.category, HintCategory::Search, "{name}");
            assert!(
                hint.message.contains("tracedecay_grep"),
                "search hint must lead with grep routing: {name}"
            );
            assert!(hint.context.contains("tracedecay_grep"), "{name}");
            assert!(hint.context.contains("tracedecay_search"), "{name}");
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
        assert!(
            hint.context.contains("tracedecay_grep"),
            "literal/regex search must route to tracedecay_grep"
        );
        assert!(
            hint.message.contains("tracedecay_grep"),
            "search hint must lead with grep routing for literal patterns"
        );
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
        assert!(
            hint.context.contains("tracedecay_grep"),
            "reading a file to find a string should route to tracedecay_grep"
        );
        assert!(hint.nonblocking, "read hints must stay soft");
    }

    #[test]
    fn broad_read_prompts_route_literal_hunts_to_grep() {
        let hint = decide_hint(&ToolHintInput {
            prompt: Some("Read every file in the entire codebase to find the flag".to_string()),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        })
        .unwrap();

        assert_eq!(hint.category, HintCategory::BroadRead);
        assert!(hint.context.contains("tracedecay_context"));
        assert!(
            hint.context.contains("tracedecay_grep"),
            "broad-read hint must route literal string hunts to tracedecay_grep"
        );
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
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::FileRead),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::ToolDescriptorRead),
            HintDecision::Emit
        );
        // Fresh session gets its own budget.
        assert_eq!(
            dedupe.decide("s2", HintCategory::Search),
            HintDecision::Emit
        );
    }

    #[test]
    fn descriptor_reads_dedupe_separately_from_source_file_reads() {
        let mut dedupe = ToolHintDedupe::default();
        assert_eq!(
            dedupe.decide("s1", HintCategory::FileRead),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::ToolDescriptorRead),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::FileRead),
            HintDecision::SuppressedDuplicate
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::ToolDescriptorRead),
            HintDecision::SuppressedDuplicate
        );
    }

    #[test]
    fn per_session_budget_caps_total_hints() {
        let mut dedupe = ToolHintDedupe::default();
        // Three distinct categories fit the budget.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::FileRead),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::Impact),
            HintDecision::Emit
        );
        // The fourth distinct category is held back by the budget, not dedupe.
        assert_eq!(
            dedupe.decide("s1", HintCategory::CallGraph),
            HintDecision::SuppressedBudget
        );
        // A different session is unaffected by s1's exhausted budget.
        assert_eq!(
            dedupe.decide("s2", HintCategory::CallGraph),
            HintDecision::Emit
        );
    }

    #[test]
    fn escalation_fires_exactly_once_after_repeated_triggers() {
        let mut dedupe = ToolHintDedupe::default();
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );
        // Repeat fires below the threshold stay silent.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
        // Third post-hint fire unlocks the single escalation.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Escalate
        );
        // Everything after escalation is permanently silent.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
    }

    #[test]
    fn escalation_does_not_count_against_the_budget() {
        let mut dedupe = ToolHintDedupe::default();
        // Exhaust the budget with three categories, then escalate the first.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::FileRead),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::Impact),
            HintDecision::Emit
        );
        for _ in 0..(ESCALATION_TRIGGER_THRESHOLD - 1) {
            assert_eq!(
                dedupe.decide("s1", HintCategory::Search),
                HintDecision::SuppressedDuplicate
            );
        }
        // Escalation is allowed even though the budget is spent.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Escalate
        );
    }

    #[test]
    fn escalated_hint_prefixes_the_base_message() {
        let base = hint(HintCategory::Search, "use tracedecay_grep", "context", true);
        let escalated = base.escalated();
        assert!(escalated
            .message
            .starts_with("Repeated native search usage this session — "));
        assert!(escalated.message.contains("use tracedecay_grep"));
        assert_eq!(escalated.category, base.category);
        assert_eq!(escalated.context, base.context);
        assert_eq!(escalated.nonblocking, base.nonblocking);
    }

    #[test]
    fn dedupe_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/tool_hints_seen.json");

        let mut dedupe = ToolHintDedupe::load_or_default(&path);
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );
        dedupe.save(&path).unwrap();

        let mut reloaded = ToolHintDedupe::load_or_default(&path);
        assert_eq!(
            reloaded.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate,
            "persisted (session, category) pairs must suppress re-emission"
        );
        assert_eq!(
            reloaded.decide("s1", HintCategory::FileRead),
            HintDecision::Emit
        );
    }

    #[test]
    fn save_writes_versioned_v2_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_hints_seen.json");
        let mut dedupe = ToolHintDedupe::default();
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );
        dedupe.save(&path).unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["version"], 2);
        assert!(value["sessions"].is_array());
        assert!(value["categories"].is_array());
    }

    #[test]
    fn v1_store_migrates_to_v2_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_hints_seen.json");
        // Legacy v1 file: a bare array of {session_id, category}.
        std::fs::write(
            &path,
            r#"[{"session_id":"s1","category":"search"},{"session_id":"s1","category":"file_read"}]"#,
        )
        .unwrap();

        let mut dedupe = ToolHintDedupe::load_or_default(&path);
        // v1 categories load as already-hinted: they suppress, not re-emit.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::FileRead),
            HintDecision::SuppressedDuplicate
        );
        // The two migrated hints already count against s1's budget, so only one
        // more distinct category can emit before the cap.
        assert_eq!(
            dedupe.decide("s1", HintCategory::Impact),
            HintDecision::Emit
        );
        assert_eq!(
            dedupe.decide("s1", HintCategory::CallGraph),
            HintDecision::SuppressedBudget
        );

        // Persisting rewrites the file in v2 shape.
        dedupe.save(&path).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["version"], 2);

        // Reload from v2 preserves the migrated suppression state.
        let mut reloaded = ToolHintDedupe::load_or_default(&path);
        assert_eq!(
            reloaded.decide("s1", HintCategory::Search),
            HintDecision::SuppressedDuplicate
        );
    }

    #[test]
    fn oversized_store_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_hints_seen.json");
        // A v1 array beyond the persisted bound must reset to an empty state.
        let entries: Vec<String> = (0..=MAX_PERSISTED_HINT_ENTRIES)
            .map(|i| format!(r#"{{"session_id":"s{i}","category":"search"}}"#))
            .collect();
        std::fs::write(&path, format!("[{}]", entries.join(","))).unwrap();

        let mut dedupe = ToolHintDedupe::load_or_default(&path);
        // Reset means s0's category is treated as never hinted.
        assert_eq!(
            dedupe.decide("s0", HintCategory::Search),
            HintDecision::Emit
        );
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
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, "not json").unwrap();
        let mut dedupe = ToolHintDedupe::load_or_default(&corrupt);
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDecision::Emit
        );
    }

    fn shell_input(command: &str) -> ToolHintInput {
        ToolHintInput {
            tool_name: Some("Bash".to_string()),
            command: Some(command.to_string()),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        }
    }

    #[test]
    fn build_commands_get_a_diagnostics_hint() {
        for command in [
            "cargo check",
            "cargo build --release",
            "cargo clippy --all-targets",
            "cargo test hooks::",
            "tsc --noEmit",
            "npx tsc -p tsconfig.json",
            "pnpm tsc",
            "pyright src/",
            "/usr/bin/tsc",
        ] {
            let hint = decide_hint(&shell_input(command)).unwrap_or_else(|| {
                panic!("{command} must produce a build-diagnostics hint");
            });
            assert_eq!(hint.category, HintCategory::BuildDiagnostics, "{command}");
            assert!(
                hint.context.contains("tracedecay_diagnostics"),
                "{command} hint must point at tracedecay_diagnostics"
            );
            assert!(
                hint.context.contains("tracedecay_diagnose"),
                "{command} hint must point at tracedecay_diagnose"
            );
        }
    }

    #[test]
    fn non_build_shell_commands_do_not_get_a_diagnostics_hint() {
        // A recursive grep is still a search hint, not a build-diagnostics one.
        assert_eq!(
            decide_hint(&shell_input("grep -r foo src/"))
                .unwrap()
                .category,
            HintCategory::Search
        );
        // Non-build cargo subcommands and unrelated programs are not classified.
        assert!(!is_build_diagnostics_command("cargo run"));
        assert!(!is_build_diagnostics_command("cargo fmt"));
        assert!(!is_build_diagnostics_command("npm install"));
        // A build word inside a quoted arg is data, not the program.
        assert!(!is_build_diagnostics_command(
            "grep \"cargo check\" log.txt"
        ));
        assert!(!is_build_diagnostics_command(""));
    }

    fn edit_input(tool_name: &str, file_path: &str) -> ToolHintInput {
        ToolHintInput {
            tool_name: Some(tool_name.to_string()),
            file_path: Some(file_path.to_string()),
            session_id: Some("session-1".to_string()),
            ..ToolHintInput::default()
        }
    }

    #[test]
    fn memory_file_edits_get_a_fact_store_hint() {
        for (tool, path) in [
            ("Write", "/home/zack/.claude/projects/foo/memory/MEMORY.md"),
            ("Edit", "/home/zack/.claude/projects/foo/memory/pr-flow.md"),
            ("Write", "/repo/MEMORY.md"),
            ("Edit", "/home/zack/.claude/CLAUDE.md"),
            ("Write", "project/CLAUDE.md"),
        ] {
            let hint = decide_hint(&edit_input(tool, path))
                .unwrap_or_else(|| panic!("{tool} {path} must produce a memory-store hint"));
            assert_eq!(hint.category, HintCategory::MemoryStore, "{tool} {path}");
            assert!(
                hint.message.contains("tracedecay_fact_store"),
                "{tool} {path} hint must point at tracedecay_fact_store"
            );
        }
    }

    #[test]
    fn non_memory_edits_get_no_memory_store_hint() {
        // A regular source edit is not a memory location — and edit tools have no
        // other hint branch, so no hint at all.
        assert!(decide_hint(&edit_input("Write", "src/lib.rs")).is_none());
        // A markdown file in a non-`.claude` `memory` dir does not match.
        assert!(!is_harness_memory_path("/repo/docs/memory/notes.md"));
        // `.claude` present but the file is not directly under a `memory` dir.
        assert!(!is_harness_memory_path(
            "/home/zack/.claude/memory/sub/notes.md"
        ));
        // A non-markdown file under `.claude/**/memory/` does not match.
        assert!(!is_harness_memory_path(
            "/home/zack/.claude/projects/foo/memory/data.json"
        ));
        // Positive controls.
        assert!(is_harness_memory_path(
            "/home/zack/.claude/projects/foo/memory/notes.md"
        ));
        assert!(is_harness_memory_path("/anywhere/MEMORY.md"));
        assert!(is_harness_memory_path("/anywhere/CLAUDE.md"));
        // Windows-style separators normalize.
        assert!(is_harness_memory_path(
            "C:\\Users\\z\\.claude\\projects\\foo\\memory\\notes.md"
        ));
    }
}
