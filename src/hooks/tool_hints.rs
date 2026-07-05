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

mod classifiers;
pub(super) use classifiers::is_harness_memory_path;
use classifiers::{
    asks_for_atomic_edit, asks_for_broad_read, asks_for_call_graph, asks_for_file_lookup,
    asks_for_impact, asks_for_project_context, asks_for_session_recall, asks_for_symbol_lookup,
    asks_for_type_orientation, combined_text, is_build_diagnostics_command, is_explore_subagent,
    is_memory_store_edit, is_project_discovery_command, is_semantic_search_tool,
    is_shell_search_command, is_single_file_read, is_tracedecay_tool_descriptor_read,
    matches_normalized,
};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
