//! Pure soft-hint decisions shared by hook adapters.
//!
//! This module intentionally returns model-visible text only. It does not deny,
//! rewrite, or otherwise decide permissions; adapters can choose how to surface
//! a returned hint for their own hook schema.

use std::collections::HashMap;
use std::path::Path;

pub use tracedecay_domain::HostIntegrationIdV1 as HintAgent;
use tracedecay_policy::hint_delivery::{
    HintDeliveryDecisionV1, HintDeliveryInputV1, decide_hint_delivery,
};

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
    ReviewChanges,
    MemoryStore,
    EditRedundancy,
    UnexpectedChanges,
}

impl HintCategory {
    fn spec(self) -> &'static HintCategorySpec {
        match CATEGORY_SPECS.iter().find(|spec| spec.category == self) {
            Some(spec) => spec,
            None => unreachable!("every HintCategory has a spec"),
        }
    }

    pub(crate) fn as_key(self) -> &'static str {
        self.spec().key
    }

    /// Human-readable name used in the escalation message prefix
    /// ("Repeated native <label> usage this session — ...").
    fn label(self) -> &'static str {
        self.spec().label
    }

    fn from_key(key: &str) -> Option<Self> {
        CATEGORY_SPECS
            .iter()
            .find(|spec| spec.key == key)
            .map(|spec| spec.category)
    }

    /// The tracedecay MCP tools this category steers toward. Used by the
    /// hint-outcome correlator (`super::hint_outcomes`) to decide whether a
    /// fired tool satisfied a hint.
    pub(crate) fn expected_tools(self) -> &'static [&'static str] {
        self.spec().expected_tools
    }
}

/// Machine-readable expected-tool list for a hint-category `key` (the value
/// stored in `analytics_events.hint_category`), or `None` when the key is
/// unknown. Lets the hint-outcome correlator map an emitted hint to the tools
/// that would satisfy it without re-parsing hint prose.
pub(crate) fn expected_tools_for_key(key: &str) -> Option<&'static [&'static str]> {
    HintCategory::from_key(key).map(HintCategory::expected_tools)
}

struct HintCategorySpec {
    category: HintCategory,
    key: &'static str,
    label: &'static str,
    skill: &'static str,
    message: &'static str,
    context: &'static str,
    /// Machine-readable list of the tracedecay MCP tools this hint steers the
    /// model toward, derived from the tools named in `context`. The
    /// hint-outcome correlator (`super::hint_outcomes`) treats a hint as
    /// "acted" when one of these tools fires in the session after the hint,
    /// instead of re-parsing the prose. Keep in sync with `context`.
    expected_tools: &'static [&'static str],
    nonblocking: bool,
}

const CATEGORY_SPECS: &[HintCategorySpec] = &[
    HintCategorySpec {
        category: HintCategory::Search,
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
    },
    HintCategorySpec {
        category: HintCategory::SemanticSearch,
        key: "semantic_search",
        label: "semantic search",
        skill: "exploring-code",
        message: "For conceptual codebase questions, consider tracedecay_context.",
        context: "tracedecay_context answers concept-level queries from the pre-built code graph (add keywords to expand synonyms); tracedecay_search ranks symbols by name/keyword; tracedecay_grep matches a literal or regex string when you want exact text, not a concept.",
        expected_tools: &["tracedecay_context", "tracedecay_search", "tracedecay_grep"],
        nonblocking: true,
    },
    HintCategorySpec {
        category: HintCategory::FileRead,
        key: "file_read",
        label: "file read",
        skill: "exploring-code",
        message: "Before reading whole files, consider tracedecay_outline, tracedecay_body, or tracedecay_read.",
        context: "tracedecay_outline gives a file's table of contents, tracedecay_body returns one symbol's source, and tracedecay_read (mode: \"lines\") slices a range — usually far cheaper than a full-file read. If you are opening the file only to find a string in it, tracedecay_grep locates the literal or regex match with its enclosing symbol instead.",
        expected_tools: &[
            "tracedecay_outline",
            "tracedecay_body",
            "tracedecay_read",
            "tracedecay_grep",
        ],
        nonblocking: true,
    },
    HintCategorySpec {
        category: HintCategory::ToolDescriptorRead,
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
    },
    HintCategorySpec {
        category: HintCategory::BroadRead,
        key: "broad_read",
        label: "broad read",
        skill: "exploring-code",
        message: "For broad codebase reading, consider starting with focused tracedecay context.",
        context: "tracedecay_context gathers relevant code slices without reading entire directories or the whole repository; tracedecay_grep sweeps the indexed tree for a literal or regex string when you are hunting for exact text rather than a concept.",
        expected_tools: &["tracedecay_context", "tracedecay_grep"],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::CallGraph,
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
    },
    HintCategorySpec {
        category: HintCategory::Impact,
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
    },
    HintCategorySpec {
        category: HintCategory::SymbolLookup,
        key: "symbol_lookup",
        label: "symbol lookup",
        skill: "exploring-code",
        message: "For symbol lookup, consider using tracedecay indexed symbol tools.",
        context: "tracedecay_context and tracedecay_node can locate definitions and nearby relationships from the code graph.",
        expected_tools: &["tracedecay_context", "tracedecay_node"],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::FileLookup,
        key: "file_lookup",
        label: "file lookup",
        skill: "exploring-code",
        message: "For finding files by role or path, consider using tracedecay_files.",
        context: "tracedecay_files can list indexed files and narrow file lookup before opening individual files.",
        expected_tools: &["tracedecay_files"],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::ProjectContext,
        key: "project_context",
        label: "project context",
        skill: "code-health",
        message: "For other repos or registered projects, consider TraceDecay project registry tools.",
        context: "tracedecay_project_list shows known projects; tracedecay_project_search can find a sibling repo by name/path/remote; pass project_path or project_id to tracedecay_context/search for cross-project code context before scanning parent directories.",
        expected_tools: &[
            "tracedecay_project_list",
            "tracedecay_project_search",
            "tracedecay_context",
            "tracedecay_search",
        ],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::SessionRecall,
        key: "session_recall",
        label: "session recall",
        skill: "managing-session-context",
        message: "For prior conversation context, consider TraceDecay session search.",
        context: "tracedecay_message_search searches ingested agent transcripts across providers; tracedecay_lcm_grep can search bounded raw-message snippets and summaries when you need session-level recall before re-discovering context.",
        expected_tools: &["tracedecay_message_search", "tracedecay_lcm_grep"],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::AtomicEdit,
        key: "atomic_edit",
        label: "atomic edit",
        skill: "editing-safely",
        message: "For safe mechanical edits, use TraceDecay's anchored edit tools.",
        context: "Use tracedecay_str_replace for one exact swap, tracedecay_multi_str_replace for an all-or-nothing batch, tracedecay_insert_at or tracedecay_insert_at_symbol for anchored insertion, tracedecay_replace_symbol for one resolved symbol, and tracedecay_ast_grep_rewrite for structural rewrites.",
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
    },
    HintCategorySpec {
        category: HintCategory::TypeOrientation,
        key: "type_orientation",
        label: "type orientation",
        skill: "exploring-code",
        message: "For type, constructor, field, trait, or duplicate-logic questions, use TraceDecay's AST orientation tools.",
        context: "Use tracedecay_constructors for struct literal sites, tracedecay_field_sites for reads/writes, tracedecay_impls or tracedecay_implementations for trait methods, tracedecay_type_hierarchy for a trait/interface/class's full recursive implementor/extender tree, and tracedecay_redundancy before adding similar helpers.",
        expected_tools: &[
            "tracedecay_constructors",
            "tracedecay_field_sites",
            "tracedecay_impls",
            "tracedecay_implementations",
            "tracedecay_type_hierarchy",
            "tracedecay_redundancy",
        ],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::ExploreSubagent,
        key: "explore_subagent",
        label: "explore subagent",
        skill: "using-tracedecay",
        message: "For code research subagents, consider adding tracedecay MCP context before broad exploration.",
        context: "tracedecay_context can gather focused code context, while tracedecay_search, tracedecay_callers, and tracedecay_impact can answer common research questions without a broad scan.",
        expected_tools: &[
            "tracedecay_context",
            "tracedecay_search",
            "tracedecay_callers",
            "tracedecay_impact",
        ],
        nonblocking: true,
    },
    HintCategorySpec {
        category: HintCategory::SubagentStartContext,
        key: "subagent_start_context",
        label: "subagent start context",
        skill: "using-tracedecay",
        message: "For subagent handoff, include focused TraceDecay context instead of broad repo instructions.",
        context: "Use tracedecay_context, tracedecay_search, and tracedecay_impact to provide only the code graph slices the subagent needs; keep workflow depth in bundled skills.",
        expected_tools: &[
            "tracedecay_context",
            "tracedecay_search",
            "tracedecay_impact",
        ],
        nonblocking: true,
    },
    HintCategorySpec {
        category: HintCategory::BuildDiagnostics,
        key: "build_diagnostics",
        label: "build diagnostics",
        skill: "fixing-build-and-type-errors",
        message: "For build/type-check errors, use TraceDecay's diagnostics tools instead of parsing raw compiler output.",
        context: "tracedecay_diagnostics runs (or reads) the project's diagnostics and maps each error to its enclosing symbol; tracedecay_diagnose adds caller/impact context for a specific failure so you fix the root cause, not just the line the compiler points at.",
        expected_tools: &["tracedecay_diagnostics", "tracedecay_diagnose"],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::ReviewChanges,
        key: "review_changes",
        label: "review changes",
        skill: "reviewing-changes",
        message: "For reviewing diffs or PR changes, use TraceDecay's change-context tools before raw diff reading.",
        context: "tracedecay_diff_context maps local changed files to touched symbols, dependents, and tests; tracedecay_pr_context does the same for a PR branch when available, so use GitHub only for review comments, metadata, and CI state.",
        expected_tools: &["tracedecay_diff_context", "tracedecay_pr_context"],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::MemoryStore,
        key: "memory_store",
        label: "memory store",
        skill: "project-memory",
        message: "For durable facts, prefer tracedecay_fact_store_add over hand-editing harness memory files.",
        context: "tracedecay_fact_store_add persists a trust-ranked project/user fact that survives across sessions and is recalled by tracedecay_context and tracedecay_recall; a memory markdown edit is only visible to the current harness. Keep secrets and unnecessary PII out of stored facts.",
        expected_tools: &["tracedecay_fact_store_add"],
        nonblocking: false,
    },
    HintCategorySpec {
        category: HintCategory::EditRedundancy,
        key: "edit_redundancy",
        label: "new-function edit",
        skill: "editing-safely",
        message: "You just added a new function-sized block; before moving on, confirm it does not duplicate logic that already exists.",
        context: "tracedecay_redundancy surfaces near-duplicate function bodies and tracedecay_similar finds structurally similar code; if a match exists, reuse or refactor the existing helper instead of keeping a second copy.",
        expected_tools: &["tracedecay_redundancy", "tracedecay_similar"],
        nonblocking: true,
    },
    HintCategorySpec {
        category: HintCategory::UnexpectedChanges,
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
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHintInput {
    pub agent: HintAgent,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub command: Option<String>,
    pub prompt: Option<String>,
    pub subagent_type: Option<String>,
    pub file_path: Option<String>,
    /// Host-captured tool output from top-level `tool_response`/`tool_output`
    /// fields. Never populate this from `tool_input` or shell command text.
    pub captured_output: Option<String>,
    /// The host authenticated this as a non-interrupt, non-timeout tool error.
    /// Command text alone must never set this flag.
    pub trusted_failure: bool,
    /// Text an edit tool adds (Write `content`, Edit `new_string`, the joined
    /// `MultiEdit` `new_string`s). Used only to detect a newly added
    /// function-sized body for the [`HintCategory::EditRedundancy`] nudge; other
    /// surfaces leave it `None`.
    pub edit_text: Option<String>,
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
            captured_output: None,
            trusted_failure: false,
            edit_text: None,
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
    /// Evaluates one hint candidate through the canonical policy evaluator
    /// ([`decide_hint_delivery`]) and applies the resulting per-session budget
    /// and per-category escalation state mutations. The policy decision is
    /// returned unchanged; rendering happens only from it.
    pub fn decide(
        &mut self,
        session_id: impl Into<String>,
        category: HintCategory,
    ) -> HintDeliveryDecisionV1 {
        let session_id = session_id.into();
        let delivered_in_session = *self.emitted.get(&session_id).unwrap_or(&0);
        let state = self
            .categories
            .entry((session_id.clone(), category))
            .or_default();

        let decision = decide_hint_delivery(HintDeliveryInputV1 {
            category_was_delivered: state.hinted,
            escalation_was_delivered: state.escalated,
            triggers_after_delivery: state.triggers_after_hint,
            delivered_in_session,
            session_limit: MAX_HINTS_PER_SESSION,
            escalation_threshold: ESCALATION_TRIGGER_THRESHOLD,
        });
        match decision {
            HintDeliveryDecisionV1::Deliver => {
                *self.emitted.entry(session_id).or_default() += 1;
                state.hinted = true;
            }
            HintDeliveryDecisionV1::DeliverEscalation => {
                state.triggers_after_hint = state.triggers_after_hint.saturating_add(1);
                state.escalated = true;
            }
            HintDeliveryDecisionV1::SuppressDuplicate => {
                if state.hinted && !state.escalated {
                    state.triggers_after_hint = state.triggers_after_hint.saturating_add(1);
                }
            }
            HintDeliveryDecisionV1::SuppressBudget => {}
        }
        decision
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

    classify_hint(input).map(hint_for_category)
}

fn classify_hint(input: &ToolHintInput) -> Option<HintCategory> {
    let facts = HintRequestFacts::new(input);
    CLASSIFICATION_RULES
        .iter()
        .find(|rule| (rule.matches)(&facts))
        .map(|rule| rule.category)
}

struct HintRequestFacts<'a> {
    input: &'a ToolHintInput,
    text: String,
    prompt_text: String,
    command: Option<&'a str>,
    tool_name: Option<&'a str>,
    command_is_shell_search: bool,
    prompt_has_diagnostic: bool,
    captured_output_has_diagnostic: bool,
    trusted_build_failure: bool,
}

impl<'a> HintRequestFacts<'a> {
    fn new(input: &'a ToolHintInput) -> Self {
        let text = combined_text(input);
        let prompt_text = input
            .prompt
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let command = input.command.as_deref();
        let prompt_has_diagnostic = looks_like_pasted_diagnostic(&prompt_text);
        let captured_output_has_diagnostic = input
            .captured_output
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
            .is_some_and(looks_like_pasted_diagnostic);
        let trusted_build_failure =
            input.trusted_failure && command.is_some_and(is_build_or_typecheck_command);
        Self {
            input,
            text,
            prompt_text,
            command,
            tool_name: input.tool_name.as_deref(),
            command_is_shell_search: command.is_some_and(is_shell_text_search_command),
            prompt_has_diagnostic,
            captured_output_has_diagnostic,
            trusted_build_failure,
        }
    }
}

type ClassificationPredicate = fn(&HintRequestFacts<'_>) -> bool;

struct ClassificationRule {
    category: HintCategory,
    matches: ClassificationPredicate,
}

const CLASSIFICATION_RULES: &[ClassificationRule] = &[
    ClassificationRule {
        category: HintCategory::ExploreSubagent,
        matches: |facts| is_explore_subagent(facts.input),
    },
    ClassificationRule {
        category: HintCategory::SubagentStartContext,
        matches: |facts| is_subagent_context_handoff(facts.input),
    },
    ClassificationRule {
        category: HintCategory::SemanticSearch,
        matches: |facts| is_semantic_search_tool(facts.input),
    },
    ClassificationRule {
        category: HintCategory::UnexpectedChanges,
        matches: |facts| signals_unexpected_change(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::SessionRecall,
        matches: |facts| asks_for_session_recall(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::ProjectContext,
        matches: |facts| {
            asks_for_project_context(&facts.text)
                || facts.command.is_some_and(is_project_discovery_command)
        },
    },
    ClassificationRule {
        category: HintCategory::CallGraph,
        matches: |facts| asks_for_call_graph(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::Impact,
        matches: |facts| asks_for_impact(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::BuildDiagnostics,
        matches: |facts| {
            (!facts.command_is_shell_search && asks_for_build_diagnostics(&facts.prompt_text))
                || facts.prompt_has_diagnostic
                || facts.captured_output_has_diagnostic
                || facts.trusted_build_failure
        },
    },
    ClassificationRule {
        category: HintCategory::AtomicEdit,
        matches: |facts| asks_for_atomic_edit(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::ReviewChanges,
        matches: |facts| {
            asks_for_review_changes(&facts.text)
                || facts
                    .command
                    .is_some_and(|command| is_diff_review_command(command, &facts.text))
        },
    },
    ClassificationRule {
        category: HintCategory::TypeOrientation,
        matches: |facts| asks_for_type_orientation(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::Search,
        matches: |facts| asks_for_text_search(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::MemoryStore,
        matches: |facts| is_memory_store_edit(facts.input),
    },
    ClassificationRule {
        category: HintCategory::EditRedundancy,
        matches: |facts| is_redundancy_candidate_edit(facts.input),
    },
    ClassificationRule {
        category: HintCategory::FileLookup,
        matches: |facts| {
            facts
                .tool_name
                .is_some_and(|name| matches_normalized(name, &["glob", "listdir", "list_dir"]))
                || facts.command.is_some_and(is_file_lookup_command)
        },
    },
    ClassificationRule {
        category: HintCategory::FileRead,
        matches: |facts| facts.command.is_some_and(is_shell_file_read_command),
    },
    ClassificationRule {
        category: HintCategory::Search,
        matches: |facts| facts.command.is_some_and(is_shell_search_command),
    },
    ClassificationRule {
        category: HintCategory::Search,
        matches: |facts| {
            facts
                .tool_name
                .is_some_and(|name| matches_normalized(name, &["grep", "search"]))
        },
    },
    ClassificationRule {
        category: HintCategory::ToolDescriptorRead,
        matches: |facts| is_tracedecay_tool_descriptor_read(facts.input),
    },
    ClassificationRule {
        category: HintCategory::FileRead,
        matches: |facts| is_single_file_read(facts.input),
    },
    ClassificationRule {
        category: HintCategory::BroadRead,
        matches: |facts| asks_for_broad_read(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::SymbolLookup,
        matches: |facts| asks_for_symbol_lookup(&facts.text),
    },
    ClassificationRule {
        category: HintCategory::FileLookup,
        matches: |facts| asks_for_file_lookup(&facts.text),
    },
];

fn hint_for_category(category: HintCategory) -> ToolHint {
    let spec = category.spec();
    ToolHint {
        category,
        message: spec.message.to_string(),
        context: format!("{}\nSkill: tracedecay:{}.", spec.context, spec.skill),
        nonblocking: spec.nonblocking,
    }
}

#[cfg(test)]
fn category_skill(category: HintCategory) -> &'static str {
    category.spec().skill
}

mod classifiers;
#[cfg(test)]
pub(super) use classifiers::is_harness_memory_path;
use classifiers::{
    asks_for_atomic_edit, asks_for_broad_read, asks_for_build_diagnostics, asks_for_call_graph,
    asks_for_file_lookup, asks_for_impact, asks_for_project_context, asks_for_review_changes,
    asks_for_session_recall, asks_for_symbol_lookup, asks_for_text_search,
    asks_for_type_orientation, combined_text, is_build_or_typecheck_command,
    is_diff_review_command, is_explore_subagent, is_file_lookup_command, is_memory_store_edit,
    is_project_discovery_command, is_redundancy_candidate_edit, is_semantic_search_tool,
    is_shell_file_read_command, is_shell_search_command, is_shell_text_search_command,
    is_single_file_read, is_subagent_context_handoff, is_tracedecay_tool_descriptor_read,
    looks_like_pasted_diagnostic, matches_normalized, signals_unexpected_change,
};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod evals;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
