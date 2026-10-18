//! The intent catalog: the user-visible text, skill, and expected tools for
//! every [`ContextScoutIntentKindV1`].
//!
//! This is the single source for both the hook hint surface and the Context
//! Scout intent pass. The text is mirrored by `evals/agent_adoption/grade.py`
//! and hint settlement credits a later call against `expected_tools`, so any
//! wording change here is a behavior change in both places.

use tracedecay_domain::configuration::ContextScoutIntentKindV1;

/// One cataloged intent.
#[derive(Debug, PartialEq, Eq)]
pub struct IntentCatalogEntryV1 {
    pub intent: ContextScoutIntentKindV1,
    /// Persisted analytics key; identical to the intent's serde key.
    pub key: &'static str,
    /// Human-readable name used in the escalation message prefix
    /// ("Repeated native <label> usage this session, ...").
    pub label: &'static str,
    pub skill: &'static str,
    pub message: &'static str,
    pub context: &'static str,
    /// Tools a later call credits as acting on this intent. Each name must be
    /// an exact token in `message` or `context`; [`IntentCatalogEntryV1::checked`]
    /// refuses the entry otherwise, so a slash-joined shorthand such as
    /// `tracedecay_context/search` cannot silently drop `tracedecay_search`.
    pub expected_tools: &'static [&'static str],
    pub nonblocking: bool,
}

const fn is_tool_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// True when `needle` appears in `haystack` as a whole tool token, not as a
/// slash-joined shorthand or a prefix of a longer name.
const fn contains_tool_token(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    let mut start = 0;
    while start + needle.len() <= haystack.len() {
        let mut offset = 0;
        while offset < needle.len() && haystack[start + offset] == needle[offset] {
            offset += 1;
        }
        if offset == needle.len() {
            let preceded = start > 0 && is_tool_token_byte(haystack[start - 1]);
            let followed = start + needle.len() < haystack.len()
                && is_tool_token_byte(haystack[start + needle.len()]);
            if !preceded && !followed {
                return true;
            }
        }
        start += 1;
    }
    false
}

impl IntentCatalogEntryV1 {
    const fn names_every_expected_tool(&self) -> bool {
        let mut index = 0;
        while index < self.expected_tools.len() {
            let tool = self.expected_tools[index];
            if !contains_tool_token(self.message, tool) && !contains_tool_token(self.context, tool)
            {
                return false;
            }
            index += 1;
        }
        true
    }

    const fn checked(self) -> Self {
        assert!(
            self.names_every_expected_tool(),
            "hint expected_tools must appear as exact tool tokens in message or context"
        );
        self
    }
}

#[cfg(test)]
mod expected_tool_token_tests {
    use super::contains_tool_token;

    #[test]
    fn slash_joined_shorthand_does_not_name_the_second_tool() {
        let prose =
            "pass project_path to tracedecay_context/search before scanning parent directories";
        assert!(contains_tool_token(prose, "tracedecay_context"));
        assert!(
            !contains_tool_token(prose, "tracedecay_search"),
            "a slash-joined shorthand must not count as naming tracedecay_search"
        );
    }
}

pub const INTENT_CATALOG: &[IntentCatalogEntryV1] = &[
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::Search,
        key: "search",
        label: "search",
        skill: "exploring-code",
        message: "For codebase search, route by what you're matching: literal/regex text -> tracedecay_grep; a code structure like a call shape or argument order -> tracedecay_ast_grep_search; symbol name -> tracedecay_search; concept -> tracedecay_context.",
        context: "tracedecay_grep runs a literal or regex content search over the indexed tree (pattern, fixed_strings, path_glob) and enriches each hit with its enclosing symbol; tracedecay_ast_grep_search matches a syntax-tree pattern in-process (e.g. `foo($$$)`, `if ($C) { $$$ }`) when a text regex cannot express the call/argument shape, then pair it with tracedecay_ast_grep_rewrite to change matches; tracedecay_search ranks symbols by name; tracedecay_context answers concept-level questions. Grep/ripgrep still fit prose and un-indexed files.",
        expected_tools: &[
            "tracedecay_grep",
            "tracedecay_ast_grep_search",
            "tracedecay_search",
            "tracedecay_context",
        ],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::SemanticSearch,
        key: "semantic_search",
        label: "semantic search",
        skill: "exploring-code",
        message: "For conceptual codebase questions, consider tracedecay_context.",
        context: "tracedecay_context answers concept-level queries from the pre-built code graph (add keywords to expand synonyms); tracedecay_search ranks symbols by name/keyword; tracedecay_grep matches a literal or regex string when you want exact text, not a concept.",
        expected_tools: &["tracedecay_context", "tracedecay_search", "tracedecay_grep"],
        nonblocking: true,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::FileRead,
        key: "file_read",
        label: "file read",
        skill: "exploring-code",
        message: "Before reading whole files, consider tracedecay_source_outline or tracedecay_source_body.",
        context: "tracedecay_source_outline gives a file's table of contents, and tracedecay_source_body returns one symbol's source for a node ID from tracedecay_find_exact_symbol or tracedecay_search, usually far cheaper than a full-file read. If you are opening the file only to find a string in it, tracedecay_grep locates the literal or regex match with its enclosing symbol instead.",
        expected_tools: &[
            "tracedecay_source_outline",
            "tracedecay_source_body",
            "tracedecay_find_exact_symbol",
            "tracedecay_grep",
        ],
        nonblocking: true,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::ToolDescriptorRead,
        key: "tool_descriptor_read",
        label: "tool descriptor read",
        skill: "tracing-functions",
        message: "This looks like a TraceDecay MCP tool descriptor; use the tool surface instead of reading schema JSON.",
        context: "Call the named tracedecay_* MCP tool directly when available, or use tool discovery for its schema; for function tracing that usually means tracedecay_find_exact_symbol plus tracedecay_callers/tracedecay_callees.",
        expected_tools: &[
            "tracedecay_find_exact_symbol",
            "tracedecay_callers",
            "tracedecay_callees",
        ],
        nonblocking: true,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::BroadRead,
        key: "broad_read",
        label: "broad read",
        skill: "exploring-code",
        message: "For broad codebase reading, consider starting with focused tracedecay context.",
        context: "tracedecay_context gathers relevant code slices without reading entire directories or the whole repository; tracedecay_grep sweeps the indexed tree for a literal or regex string when you are hunting for exact text rather than a concept.",
        expected_tools: &["tracedecay_context", "tracedecay_grep"],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::CallGraph,
        key: "call_graph",
        label: "call-graph",
        skill: "tracing-functions",
        message: "For function tracing, use the indexed call graph before grep/file reads.",
        context: "Resolve the symbol with tracedecay_find_exact_symbol or tracedecay_search, then use tracedecay_callers for who depends on it and tracedecay_callees for what it calls; use tracedecay_impact for broader dependents before opening files.",
        expected_tools: &[
            "tracedecay_find_exact_symbol",
            "tracedecay_search",
            "tracedecay_callers",
            "tracedecay_callees",
            "tracedecay_impact",
        ],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::Impact,
        key: "impact",
        label: "impact",
        skill: "assessing-impact",
        message: "For impact, affected-test, or blast-radius questions, use TraceDecay's dependency tools.",
        context: "Start with tracedecay_diff_context when you have changed files, tracedecay_impact for a resolved symbol, tracedecay_affected for affected tests, and tracedecay_test_map when you need direct test attribution.",
        expected_tools: &[
            "tracedecay_diff_context",
            "tracedecay_impact",
            "tracedecay_affected",
            "tracedecay_test_map",
        ],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::SymbolLookup,
        key: "symbol_lookup",
        label: "symbol lookup",
        skill: "exploring-code",
        message: "For symbol lookup, consider using tracedecay indexed symbol tools.",
        context: "tracedecay_context and tracedecay_node can locate definitions and nearby relationships from the code graph.",
        expected_tools: &["tracedecay_context", "tracedecay_node"],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::FileLookup,
        key: "file_lookup",
        label: "file lookup",
        skill: "exploring-code",
        message: "For finding files by role or path, consider using tracedecay_files.",
        context: "tracedecay_files can list indexed files and narrow file lookup before opening individual files.",
        expected_tools: &["tracedecay_files"],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::ProjectContext,
        key: "project_context",
        label: "project context",
        skill: "code-health",
        message: "For other repos or registered projects, consider TraceDecay project registry tools.",
        context: "tracedecay_project_list shows known projects; tracedecay_project_search can find a sibling repo by name/path/remote; pass project_path or project_id to tracedecay_context or tracedecay_search for cross-project code context before scanning parent directories.",
        expected_tools: &[
            "tracedecay_project_list",
            "tracedecay_project_search",
            "tracedecay_context",
            "tracedecay_search",
        ],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::SessionRecall,
        key: "session_recall",
        label: "session recall",
        skill: "managing-session-context",
        message: "For prior conversation context, consider TraceDecay session search.",
        context: "tracedecay_message_search searches ingested agent transcripts across providers; tracedecay_lcm_grep can search bounded raw-message snippets and summaries when you need session-level recall before re-discovering context.",
        expected_tools: &["tracedecay_message_search", "tracedecay_lcm_grep"],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::AtomicEdit,
        key: "atomic_edit",
        label: "atomic edit",
        skill: "editing-safely",
        message: "For safe mechanical edits, use TraceDecay's anchored edit tools.",
        context: "Use tracedecay_str_replace for one exact swap, tracedecay_multi_str_replace for an all-or-nothing batch, tracedecay_insert_at or tracedecay_insert_at_symbol for anchored insertion, tracedecay_replace_symbol for one resolved symbol, tracedecay_move_symbol to relocate one resolved symbol, and tracedecay_ast_grep_rewrite for structural rewrites.",
        expected_tools: &[
            "tracedecay_str_replace",
            "tracedecay_multi_str_replace",
            "tracedecay_insert_at",
            "tracedecay_insert_at_symbol",
            "tracedecay_ast_grep_rewrite",
            "tracedecay_replace_symbol",
            "tracedecay_move_symbol",
        ],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::TypeOrientation,
        key: "type_orientation",
        label: "type orientation",
        skill: "exploring-code",
        message: "For type, constructor, field, trait, or duplicate-logic questions, use TraceDecay's AST orientation tools.",
        context: "Use tracedecay_constructors for struct literal sites, tracedecay_field_sites for reads/writes, tracedecay_implementations for a trait's implementors or every body of a method name, and tracedecay_type_hierarchy for a trait/interface/class's full recursive implementor/extender tree.",
        expected_tools: &[
            "tracedecay_constructors",
            "tracedecay_field_sites",
            "tracedecay_implementations",
            "tracedecay_type_hierarchy",
        ],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::ExploreSubagent,
        key: "explore_subagent",
        label: "explore subagent",
        skill: "discovering-tracedecay",
        message: "For code research subagents, consider adding tracedecay MCP context before broad exploration.",
        context: "tracedecay_context can gather focused code context, while tracedecay_search, tracedecay_callers, and tracedecay_impact can answer common research questions without a broad scan.",
        expected_tools: &[
            "tracedecay_context",
            "tracedecay_search",
            "tracedecay_callers",
            "tracedecay_impact",
        ],
        nonblocking: true,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::SubagentStartContext,
        key: "subagent_start_context",
        label: "subagent start context",
        skill: "discovering-tracedecay",
        message: "For subagent handoff, include focused TraceDecay context instead of broad repo instructions.",
        context: "Use tracedecay_context, tracedecay_search, and tracedecay_impact to provide only the code graph slices the subagent needs; keep workflow depth in bundled skills.",
        expected_tools: &[
            "tracedecay_context",
            "tracedecay_search",
            "tracedecay_impact",
        ],
        nonblocking: true,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::BuildDiagnostics,
        key: "build_diagnostics",
        label: "build diagnostics",
        skill: "fixing-build-and-type-errors",
        message: "For build/type-check errors, use TraceDecay's diagnostics tools instead of parsing raw compiler output.",
        context: "tracedecay_diagnostics reads the diagnostics published for the current indexed generation and maps each error to its enclosing symbol; a TypeScript project's own tsc is run for it automatically, other toolchains publish by pasting compiler output into tracedecay_diagnose, which also adds caller/impact context so you fix the root cause, not just the line the compiler points at.",
        expected_tools: &["tracedecay_diagnostics", "tracedecay_diagnose"],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::ReviewChanges,
        key: "review_changes",
        label: "review changes",
        skill: "reviewing-changes",
        message: "For reviewing diffs or PR changes, use TraceDecay's change-context tools before raw diff reading.",
        context: "tracedecay_diff_context maps local changed files to touched symbols, dependents, and tests; tracedecay_pr_context does the same for a PR branch when available, so use GitHub only for review comments, metadata, and CI state.",
        expected_tools: &["tracedecay_diff_context", "tracedecay_pr_context"],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::MemoryStore,
        key: "memory_store",
        label: "memory store",
        skill: "project-memory",
        message: "For durable facts, prefer tracedecay_fact_store_add over hand-editing harness memory files.",
        context: "tracedecay_fact_store_add persists a trust-ranked project/user fact that survives across sessions and is recalled by tracedecay_context and tracedecay_fact_store_search; a memory markdown edit is only visible to the current harness. Keep secrets and unnecessary PII out of stored facts.",
        expected_tools: &["tracedecay_fact_store_add"],
        nonblocking: false,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::EditRedundancy,
        key: "edit_redundancy",
        label: "new-function edit",
        skill: "editing-safely",
        message: "You just added a new function-sized block. Search for an existing helper and continue; do not stop to ask whether the new block is acceptable.",
        context: "tracedecay_search finds existing helpers by name, signature, or body text and tracedecay_similar finds similarly named symbols; if a match exists, reuse or refactor the existing helper instead of keeping a second copy.",
        expected_tools: &["tracedecay_search", "tracedecay_similar"],
        nonblocking: true,
    }.checked(),
    IntentCatalogEntryV1 {
        intent: ContextScoutIntentKindV1::UnexpectedChanges,
        key: "unexpected_changes",
        label: "unexpected changes",
        skill: "investigating-unexpected-changes",
        message: "Unexpected commits or edits on your branch? Attribute them with the session-git index before reacting.",
        context: "tracedecay_sessions_for (git_ref commit/branch/worktree) names the sessions that produced or observed the change; tracedecay_commit_context and tracedecay_branch_diff show what moved; tracedecay_message_search recovers the acting session's intent. Do not force-push over another agent's work or theorize from blind git log.",
        expected_tools: &[
            "tracedecay_sessions_for",
            "tracedecay_commit_context",
            "tracedecay_branch_diff",
            "tracedecay_message_search",
        ],
        nonblocking: false,
    }.checked(),
];

/// The catalog entry for `intent`. Every intent has exactly one entry; the
/// unit tests enforce it.
pub fn entry(intent: ContextScoutIntentKindV1) -> &'static IntentCatalogEntryV1 {
    match INTENT_CATALOG.iter().find(|entry| entry.intent == intent) {
        Some(entry) => entry,
        None => unreachable!("every ContextScoutIntentKindV1 has a catalog entry"),
    }
}

/// Machine-readable expected-tool list for an intent `key` (the value stored
/// in `analytics_events.hint_category`), or `None` when the key is unknown.
pub fn expected_tools_for_key(key: &str) -> Option<&'static [&'static str]> {
    INTENT_CATALOG
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.expected_tools)
}

/// The context line with its bundled-skill trigger, exactly as the hook hint
/// surface renders it.
pub fn suggestion_context(intent: ContextScoutIntentKindV1) -> String {
    let entry = entry(intent);
    format!("{}\nSkill: tracedecay:{}.", entry.context, entry.skill)
}

/// The one-time stronger re-hint after repeated native usage of `intent` in
/// a session: `message` prefixed with the escalation sentence.
pub fn escalated_message(intent: ContextScoutIntentKindV1, message: &str) -> String {
    format!(
        "Repeated native {} usage this session. {message}",
        entry(intent).label
    )
}

/// The full model-visible suggestion for `intent`, byte-identical to the text
/// a hook appends for the same hint category.
pub fn suggestion_text(intent: ContextScoutIntentKindV1) -> String {
    format!(
        "tracedecay hint: {}\n{}",
        entry(intent).message,
        suggestion_context(intent)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_intent_has_exactly_one_entry_keyed_by_its_serde_key() {
        assert_eq!(INTENT_CATALOG.len(), ContextScoutIntentKindV1::ALL.len());
        let intents = INTENT_CATALOG
            .iter()
            .map(|entry| entry.intent)
            .collect::<BTreeSet<_>>();
        assert_eq!(intents.len(), INTENT_CATALOG.len());
        for intent in ContextScoutIntentKindV1::ALL {
            let entry = entry(intent);
            assert_eq!(entry.key, intent.as_key());
            assert_eq!(
                expected_tools_for_key(entry.key),
                Some(entry.expected_tools)
            );
            assert!(!entry.expected_tools.is_empty());
        }
        assert_eq!(expected_tools_for_key("unknown"), None);
    }

    #[test]
    fn suggestion_text_matches_the_hook_rendering() {
        let entry = entry(ContextScoutIntentKindV1::SessionRecall);
        assert_eq!(
            suggestion_text(ContextScoutIntentKindV1::SessionRecall),
            format!(
                "tracedecay hint: {}\n{}\nSkill: tracedecay:{}.",
                entry.message, entry.context, entry.skill
            )
        );
        for intent in ContextScoutIntentKindV1::ALL {
            let text = suggestion_text(intent);
            assert!(text.len() <= 900, "{intent:?} suggestion is too long");
            assert!(text.ends_with(&format!("Skill: tracedecay:{}.", entry_skill(intent))));
        }
    }

    fn entry_skill(intent: ContextScoutIntentKindV1) -> &'static str {
        entry(intent).skill
    }
}
