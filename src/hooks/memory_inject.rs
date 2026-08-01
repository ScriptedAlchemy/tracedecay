//! Fact-store recall injection for agent lifecycle hooks.
//!
//! Renders bounded, trust-filtered "durable project memory" digests for
//! session-start hooks (Codex `SessionStart`/`SubagentStart`, Cursor
//! `sessionStart`) and prompt-relevance-gated recall blocks for Codex
//! `UserPromptSubmit`. Everything here is fail-open: any store error, missing
//! index, or disabled gate simply injects nothing.
//!
//! Char budgets are hard caps enforced at render time: session digests stay
//! within [`SESSION_DIGEST_CHAR_BUDGET`] and per-prompt injections within
//! [`PROMPT_RECALL_CHAR_BUDGET`]. Facts flagged secret-like by
//! [`crate::memory::hygiene::detect_secret_like`] are never injected, and fact
//! content is sanitized to a single plain-text line before rendering.

use std::collections::HashSet;
use std::hash::BuildHasher;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::memory::hygiene::detect_secret_like;
use crate::memory::types::{FactRecord, FactSearchResult};

/// Hard cap for the session-start "durable project memory" digest.
pub const SESSION_DIGEST_CHAR_BUDGET: usize = 2_000;
/// Hard cap for a per-prompt recall injection.
pub const PROMPT_RECALL_CHAR_BUDGET: usize = 800;
/// Top-K facts considered for the session digest.
pub const SESSION_DIGEST_FACT_COUNT: usize = 8;
/// Max facts injected per prompt.
pub const PROMPT_RECALL_FACT_COUNT: usize = 3;
/// Minimum trust for any injected fact.
pub const INJECTION_MIN_TRUST: f64 = 0.6;
/// Prompts shorter than this never trigger recall (greetings, "y", etc.).
const MIN_PROMPT_CHARS: usize = 12;
/// Per-fact line cap so one verbose fact cannot exhaust the budget.
const FACT_LINE_MAX_CHARS: usize = 240;
/// Lexical relevance floor for prompt-gated recall: a fact must either match
/// the FTS index or share this much token overlap with the prompt.
const PROMPT_RECALL_MIN_JACCARD: f64 = 0.15;
/// Combined-score floor for prompt-gated recall (relevance × trust × decay).
const PROMPT_RECALL_MIN_SCORE: f64 = 0.18;
/// Reset threshold for the persisted per-session seen-fact history, mirroring
/// `tool_hints::MAX_PERSISTED_HINT_ENTRIES`.
const MAX_PERSISTED_SEEN_ENTRIES: usize = 4_096;
const SEEN_FACTS_FILENAME: &str = "memory_inject_seen.json";
const USER_SEEN_FACTS_FILENAME: &str = "user_memory_inject_seen.json";

const DIGEST_HEADER: &str = "Durable project memory (tracedecay fact store; \
rate with tracedecay_fact_feedback — unhelpful for a wrong fact counts as much as helpful; \
correct via tracedecay_fact_store update):";
#[cfg(test)]
const PROMPT_RECALL_HEADER: &str = "Possibly relevant project memory \
(tracedecay fact store; rate with tracedecay_fact_feedback — unhelpful for wrong facts counts too):";
const USER_DIGEST_HEADER: &str = "Durable user memory (tracedecay user fact store; \
rate with tracedecay_fact_feedback using memory_scope=user):";
const USER_PROMPT_RECALL_HEADER: &str = "Possibly relevant user memory \
(tracedecay user fact store; rate with tracedecay_fact_feedback using memory_scope=user):";
const COMBINED_DIGEST_HEADER: &str = "Durable user and project memory \
(tracedecay fact stores; each fact includes its scope):";
const COMBINED_PROMPT_RECALL_HEADER: &str = "Possibly relevant user and project memory \
(tracedecay fact stores; each fact includes its scope):";

async fn daemon_fact_store(
    root: Option<&Path>,
    action: &str,
    query: Option<&str>,
    user_scope: bool,
    limit: usize,
) -> Option<serde_json::Value> {
    let mut args = serde_json::json!({
        "action": action,
        "format": "json",
        "limit": limit,
        "min_trust": INJECTION_MIN_TRUST,
    });
    if let Some(query) = query {
        args["query"] = serde_json::json!(query);
    }
    if user_scope {
        args["memory_scope"] = serde_json::json!("user");
    }
    match super::daemon_tool_json(root, "tracedecay_fact_store", args).await {
        Ok(value) => Some(value),
        Err(error) => {
            eprintln!("[tracedecay] memory hook daemon call failed: {error}");
            None
        }
    }
}

async fn daemon_fact_list(root: Option<&Path>, user_scope: bool) -> Vec<FactRecord> {
    daemon_fact_store(
        root,
        "list",
        None,
        user_scope,
        SESSION_DIGEST_FACT_COUNT * 4,
    )
    .await
    .and_then(|value| serde_json::from_value(value.get("facts")?.clone()).ok())
    .unwrap_or_default()
}

async fn daemon_fact_search(
    root: Option<&Path>,
    prompt: &str,
    user_scope: bool,
) -> Vec<FactSearchResult> {
    daemon_fact_store(
        root,
        "search",
        Some(prompt),
        user_scope,
        PROMPT_RECALL_FACT_COUNT * 4,
    )
    .await
    .and_then(|value| serde_json::from_value(value.get("facts")?.clone()).ok())
    .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Config gate
// ---------------------------------------------------------------------------

/// Whether fact injection is enabled: the `TRACEDECAY_MEMORY_INJECTION` env
/// var wins when set, otherwise the user-config flag (default on).
pub fn memory_injection_enabled() -> bool {
    injection_enabled_from(
        crate::config::brand_env("MEMORY_INJECTION").as_deref(),
        crate::user_config::UserConfig::load().memory_injection_enabled,
    )
}

/// Pure form of the gate for tests: env override beats the config flag.
pub fn injection_enabled_from(env_value: Option<&str>, config_flag: bool) -> bool {
    match env_value {
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => config_flag,
    }
}

// ---------------------------------------------------------------------------
// Sanitization
// ---------------------------------------------------------------------------

/// Collapses a fact's content to one sanitized plain-text line: control
/// characters and newlines become spaces, runs of whitespace collapse, and the
/// result is truncated to [`FACT_LINE_MAX_CHARS`] with an ellipsis. This keeps
/// injected context from smuggling markdown structure, ANSI sequences, or
/// multi-line prompt-injection payloads into the host's context block.
fn sanitize_fact_line(content: &str) -> String {
    let mut out = String::with_capacity(content.len().min(FACT_LINE_MAX_CHARS + 1));
    let mut last_was_space = true;
    for ch in content.chars() {
        let ch = if ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}' {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        out.push(ch);
        if out.chars().count() > FACT_LINE_MAX_CHARS {
            break;
        }
    }
    let mut out = out.trim_end().to_string();
    if out.chars().count() > FACT_LINE_MAX_CHARS {
        out = out.chars().take(FACT_LINE_MAX_CHARS - 1).collect();
        out = out.trim_end().to_string();
        out.push('…');
    }
    out
}

/// A fact is injectable when it is non-empty after sanitization and does not
/// look like a credential.
fn injectable(fact: &FactRecord) -> bool {
    !fact.content.trim().is_empty() && detect_secret_like(&fact.content).is_none()
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Picks the top-`k` digest facts from a trust-filtered candidate list:
/// secret-like facts are dropped, then one highest-trust fact per category is
/// taken first (category diversity), then remaining slots fill by trust with
/// newest-first tiebreak.
pub fn select_digest_facts(mut facts: Vec<FactRecord>, k: usize) -> Vec<FactRecord> {
    facts.retain(injectable);
    facts.sort_by(|left, right| {
        right
            .trust_score
            .total_cmp(&left.trust_score)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| right.fact_id.cmp(&left.fact_id))
    });
    let mut selected: Vec<FactRecord> = Vec::with_capacity(k.min(facts.len()));
    let mut seen_categories = HashSet::new();
    let mut deferred: Vec<FactRecord> = Vec::new();
    for fact in facts {
        if selected.len() >= k {
            break;
        }
        if seen_categories.insert(fact.category) {
            selected.push(fact);
        } else {
            deferred.push(fact);
        }
    }
    for fact in deferred {
        if selected.len() >= k {
            break;
        }
        selected.push(fact);
    }
    // Deterministic render order: trust desc, newest first.
    selected.sort_by(|left, right| {
        right
            .trust_score
            .total_cmp(&left.trust_score)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| right.fact_id.cmp(&left.fact_id))
    });
    selected
}

/// Filters prompt-recall search results down to facts that are actually
/// relevant to the prompt (lexical FTS match or meaningful token overlap and a
/// combined-score floor), excluding secret-like facts and anything in
/// `already_injected`. Returns at most `max` facts in score order.
pub fn select_prompt_recall_facts<S: BuildHasher>(
    results: Vec<FactSearchResult>,
    already_injected: &HashSet<i64, S>,
    max: usize,
) -> Vec<FactRecord> {
    let mut selected = Vec::new();
    for result in results {
        if selected.len() >= max {
            break;
        }
        let relevant = (result.fts_score > 0.0
            || result.jaccard_score >= PROMPT_RECALL_MIN_JACCARD)
            && result.score >= PROMPT_RECALL_MIN_SCORE;
        if !relevant || already_injected.contains(&result.fact.fact_id) || !injectable(&result.fact)
        {
            continue;
        }
        selected.push(result.fact);
    }
    selected
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn fact_line(fact: &FactRecord) -> String {
    format!(
        "- [{} #{} trust {:.2}] {}",
        fact.category.as_str(),
        fact.fact_id,
        fact.trust_score,
        sanitize_fact_line(&fact.content)
    )
}

/// Renders a header + fact-line block, dropping trailing facts that would
/// exceed `budget` chars. Returns the rendered text plus the ids actually
/// included, or `None` when no fact fits.
fn render_fact_block(
    header: &str,
    facts: &[FactRecord],
    budget: usize,
) -> Option<(String, Vec<i64>)> {
    if facts.is_empty() {
        return None;
    }
    let mut out = String::from(header);
    out.push('\n');
    let mut included = Vec::new();
    for fact in facts {
        let line = fact_line(fact);
        if out.chars().count() + line.chars().count() + 1 > budget {
            break;
        }
        out.push_str(&line);
        out.push('\n');
        included.push(fact.fact_id);
    }
    if included.is_empty() {
        return None;
    }
    Some((out, included))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemoryScope {
    User,
    Project,
}

impl MemoryScope {
    fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

fn select_scoped_facts(
    user: Vec<FactRecord>,
    project: Vec<FactRecord>,
    max: usize,
) -> Vec<(MemoryScope, FactRecord)> {
    let mut facts = user
        .into_iter()
        .map(|fact| (MemoryScope::User, fact))
        .chain(project.into_iter().map(|fact| (MemoryScope::Project, fact)))
        .collect::<Vec<_>>();
    facts.sort_by(|(left_scope, left), (right_scope, right)| {
        right
            .trust_score
            .total_cmp(&left.trust_score)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| {
                let rank = |scope| match scope {
                    MemoryScope::User => 0,
                    MemoryScope::Project => 1,
                };
                rank(*left_scope).cmp(&rank(*right_scope))
            })
    });
    let mut seen_content = HashSet::new();
    facts
        .into_iter()
        .filter(|(_, fact)| seen_content.insert(sanitize_fact_line(&fact.content)))
        .take(max)
        .collect()
}

fn render_scoped_fact_block(
    header: &str,
    facts: &[(MemoryScope, FactRecord)],
    budget: usize,
) -> Option<(String, Vec<i64>, Vec<i64>)> {
    if facts.is_empty() {
        return None;
    }
    let mut out = String::from(header);
    out.push('\n');
    let mut user_ids = Vec::new();
    let mut project_ids = Vec::new();
    for (scope, fact) in facts {
        let line = format!(
            "- [{} {} #{} trust {:.2}] {}",
            scope.label(),
            fact.category.as_str(),
            fact.fact_id,
            fact.trust_score,
            sanitize_fact_line(&fact.content)
        );
        if out.chars().count() + line.chars().count() + 1 > budget {
            break;
        }
        out.push_str(&line);
        out.push('\n');
        match scope {
            MemoryScope::User => user_ids.push(fact.fact_id),
            MemoryScope::Project => project_ids.push(fact.fact_id),
        }
    }
    if user_ids.is_empty() && project_ids.is_empty() {
        return None;
    }
    Some((out, user_ids, project_ids))
}

/// Renders the session-start "durable project memory" digest within `budget`.
#[cfg(test)]
pub fn render_memory_digest(facts: &[FactRecord], budget: usize) -> Option<String> {
    render_fact_block(DIGEST_HEADER, facts, budget).map(|(text, _)| text)
}

/// Renders the per-prompt recall block within `budget`.
#[cfg(test)]
pub fn render_prompt_recall(facts: &[FactRecord], budget: usize) -> Option<String> {
    render_fact_block(PROMPT_RECALL_HEADER, facts, budget).map(|(text, _)| text)
}

// ---------------------------------------------------------------------------
// Per-session seen-fact dedupe (mirrors tool_hints_seen.json)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PersistedSeenEntry {
    session_id: String,
    fact_id: i64,
}

/// Persisted `(session_id, fact_id)` pairs recording which facts were already
/// injected into a session, so `UserPromptSubmit` recall never repeats what
/// `SessionStart` (or an earlier prompt) already surfaced. Stored in
/// `.tracedecay/memory_inject_seen.json`, following the
/// `tool_hints_seen.json` pattern.
#[derive(Default)]
pub struct MemoryInjectSeen {
    seen: HashSet<(String, i64)>,
}

impl MemoryInjectSeen {
    pub fn load_or_default(path: &Path) -> Self {
        match Self::load(path) {
            Ok(loaded) if loaded.seen.len() <= MAX_PERSISTED_SEEN_ENTRIES => loaded,
            _ => Self::default(),
        }
    }

    fn load(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let entries: Vec<PersistedSeenEntry> = serde_json::from_str(&content).unwrap_or_default();
        Ok(Self {
            seen: entries
                .into_iter()
                .map(|entry| (entry.session_id, entry.fact_id))
                .collect(),
        })
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut entries: Vec<PersistedSeenEntry> = self
            .seen
            .iter()
            .map(|(session_id, fact_id)| PersistedSeenEntry {
                session_id: session_id.clone(),
                fact_id: *fact_id,
            })
            .collect();
        entries.sort_by(|left, right| {
            left.session_id
                .cmp(&right.session_id)
                .then_with(|| left.fact_id.cmp(&right.fact_id))
        });
        std::fs::write(path, serde_json::to_string(&entries)?)
    }

    pub fn seen_for_session(&self, session_id: &str) -> HashSet<i64> {
        self.seen
            .iter()
            .filter(|(session, _)| session == session_id)
            .map(|(_, fact_id)| *fact_id)
            .collect()
    }

    pub fn record(&mut self, session_id: &str, fact_ids: &[i64]) {
        for fact_id in fact_ids {
            self.seen.insert((session_id.to_string(), *fact_id));
        }
    }
}

fn seen_facts_path(root: &Path) -> Option<std::path::PathBuf> {
    let layout = crate::storage::resolve_layout_for_current_profile(root).ok()?;
    layout
        .data_root
        .is_dir()
        .then(|| layout.data_root.join(SEEN_FACTS_FILENAME))
}

/// Records `fact_ids` as seen for `session_id` in an already-loaded `seen`
/// state and persists it to `path`. No-op (and no write) when there is
/// nothing new to record.
fn commit_seen_facts(seen: &mut MemoryInjectSeen, path: &Path, session_id: &str, fact_ids: &[i64]) {
    if fact_ids.is_empty() {
        return;
    }
    seen.record(session_id, fact_ids);
    let _ = seen.save(path);
}

fn record_injected_facts(root: &Path, session_id: Option<&str>, fact_ids: &[i64]) {
    let (Some(session_id), Some(path)) = (session_id, seen_facts_path(root)) else {
        return;
    };
    if fact_ids.is_empty() {
        return;
    }
    let mut seen = MemoryInjectSeen::load_or_default(&path);
    commit_seen_facts(&mut seen, &path, session_id, fact_ids);
}

fn user_seen_facts_path() -> Option<std::path::PathBuf> {
    crate::storage::default_profile_root()
        .ok()
        .map(|root| root.join(USER_SEEN_FACTS_FILENAME))
}

fn record_injected_user_facts(session_id: Option<&str>, fact_ids: &[i64]) {
    let (Some(session_id), Some(path)) = (session_id, user_seen_facts_path()) else {
        return;
    };
    if fact_ids.is_empty() {
        return;
    }
    let mut seen = MemoryInjectSeen::load_or_default(&path);
    commit_seen_facts(&mut seen, &path, session_id, fact_ids);
}

// ---------------------------------------------------------------------------
// Async store-backed entry points (fail-open)
// ---------------------------------------------------------------------------

/// Loads trust-filtered digest candidates from the project's fact store
/// without bumping recall/access counters. Fail-open: any error yields an
/// empty list.
async fn digest_candidates(root: &Path) -> Vec<FactRecord> {
    daemon_fact_list(Some(root), false).await
}

/// Builds a session-start digest from the profile-level user memory store.
pub async fn user_session_memory_digest(session_id: Option<&str>) -> Option<String> {
    if !memory_injection_enabled() {
        return None;
    }
    let facts = daemon_fact_list(None, true).await;
    let facts = select_digest_facts(facts, SESSION_DIGEST_FACT_COUNT);
    let (text, included) =
        render_fact_block(USER_DIGEST_HEADER, &facts, SESSION_DIGEST_CHAR_BUDGET)?;
    record_injected_user_facts(session_id, &included);
    Some(text)
}

/// Recalls profile-level user facts for a projectless prompt.
pub async fn user_prompt_memory_recall(session_id: Option<&str>, prompt: &str) -> Option<String> {
    if !memory_injection_enabled() || prompt.trim().chars().count() < MIN_PROMPT_CHARS {
        return None;
    }
    let results = daemon_fact_search(None, prompt, true).await;
    let already_injected = match (session_id, user_seen_facts_path()) {
        (Some(session_id), Some(path)) => {
            MemoryInjectSeen::load_or_default(&path).seen_for_session(session_id)
        }
        _ => HashSet::new(),
    };
    let facts = select_prompt_recall_facts(results, &already_injected, PROMPT_RECALL_FACT_COUNT);
    let (text, included) =
        render_fact_block(USER_PROMPT_RECALL_HEADER, &facts, PROMPT_RECALL_CHAR_BUDGET)?;
    record_injected_user_facts(session_id, &included);
    Some(text)
}

/// Builds one bounded, provenance-labelled digest from user and active-project memory.
pub async fn combined_session_memory_digest(
    root: &Path,
    session_id: Option<&str>,
) -> Option<String> {
    if !memory_injection_enabled() {
        return None;
    }
    let project_digest = async {
        if crate::tracedecay::TraceDecay::is_initialized(root) {
            select_digest_facts(digest_candidates(root).await, SESSION_DIGEST_FACT_COUNT)
        } else {
            Vec::new()
        }
    };
    let user_digest = async {
        select_digest_facts(
            daemon_fact_list(Some(root), true).await,
            SESSION_DIGEST_FACT_COUNT,
        )
    };
    let (project, user) = tokio::join!(project_digest, user_digest);
    let facts = select_scoped_facts(user, project, SESSION_DIGEST_FACT_COUNT);
    let (text, user_ids, project_ids) =
        render_scoped_fact_block(COMBINED_DIGEST_HEADER, &facts, SESSION_DIGEST_CHAR_BUDGET)?;
    record_injected_user_facts(session_id, &user_ids);
    record_injected_facts(root, session_id, &project_ids);
    Some(text)
}

/// Recalls one bounded, provenance-labelled result set from user and active-project memory.
pub async fn combined_prompt_memory_recall(
    root: &Path,
    session_id: Option<&str>,
    prompt: &str,
) -> Option<String> {
    if !memory_injection_enabled() || prompt.trim().chars().count() < MIN_PROMPT_CHARS {
        return None;
    }
    // Loaded once and reused below to record newly-injected facts, instead of
    // reloading the same seen-facts file a second time after the recall.
    let mut project_seen_state = match (session_id, seen_facts_path(root)) {
        (Some(_), Some(path)) => Some((MemoryInjectSeen::load_or_default(&path), path)),
        _ => None,
    };
    let mut user_seen_state = match (session_id, user_seen_facts_path()) {
        (Some(_), Some(path)) => Some((MemoryInjectSeen::load_or_default(&path), path)),
        _ => None,
    };
    let project_seen = match (session_id, &project_seen_state) {
        (Some(session_id), Some((seen, _))) => seen.seen_for_session(session_id),
        _ => HashSet::new(),
    };
    let user_seen = match (session_id, &user_seen_state) {
        (Some(session_id), Some((seen, _))) => seen.seen_for_session(session_id),
        _ => HashSet::new(),
    };
    let (project_results, user_results) = tokio::join!(
        daemon_fact_search(Some(root), prompt, false),
        daemon_fact_search(Some(root), prompt, true),
    );
    let project =
        select_prompt_recall_facts(project_results, &project_seen, PROMPT_RECALL_FACT_COUNT);
    let user = select_prompt_recall_facts(user_results, &user_seen, PROMPT_RECALL_FACT_COUNT);
    let facts = select_scoped_facts(user, project, PROMPT_RECALL_FACT_COUNT);
    let (text, user_ids, project_ids) = render_scoped_fact_block(
        COMBINED_PROMPT_RECALL_HEADER,
        &facts,
        PROMPT_RECALL_CHAR_BUDGET,
    )?;
    if let Some(session_id) = session_id {
        if let Some((seen, path)) = user_seen_state.as_mut() {
            commit_seen_facts(seen, path, session_id, &user_ids);
        }
        if let Some((seen, path)) = project_seen_state.as_mut() {
            commit_seen_facts(seen, path, session_id, &project_ids);
        }
    }
    Some(text)
}

/// Prompt-shaped memory recall for a host's prompt-submit hook.
///
/// Codex, Cursor, and Kiro each carried a byte-identical copy of this: read the
/// prompt text, read the session id, resolve the project root, then route to
/// project-scoped or user-scoped recall. Only the root resolver genuinely
/// differs per host (identity-aware for Codex and Cursor, `cwd`-only for Kiro),
/// so it arrives as a lazily invoked closure — an event with no prompt still
/// costs no root resolution.
pub(super) async fn prompt_memory_recall<F, Fut>(
    parsed: &serde_json::Value,
    resolve_root: F,
) -> Option<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<std::path::PathBuf>>,
{
    let prompt = super::prompt_like_text(parsed)?;
    let session_id = super::event_session_id(parsed);
    match resolve_root().await {
        Some(root) => {
            Box::pin(combined_prompt_memory_recall(
                &root,
                session_id.as_deref(),
                &prompt,
            ))
            .await
        }
        None => user_prompt_memory_recall(session_id.as_deref(), &prompt).await,
    }
}

// ---------------------------------------------------------------------------
// Cursor materialized memory rule
// ---------------------------------------------------------------------------

/// Marker line identifying the generated Cursor memory rule; used for
/// idempotent regeneration and by uninstall/doctor tooling.
pub const CURSOR_MEMORY_RULE_MARKER: &str =
    "<!-- generated by tracedecay from the project fact store; do not edit by hand -->";

/// Renders the always-applied Cursor memory rule (`tracedecay-memory.mdc`)
/// from the project's fact store. With no facts the rule still renders as a
/// small static explainer, so install-time materialization is deterministic.
pub fn render_cursor_memory_rule(project_root: Option<&str>, facts: &[FactRecord]) -> String {
    let mut out = String::from(
        "---\ndescription: Durable project memory from the tracedecay fact store\nalwaysApply: true\n---\n\n",
    );
    out.push_str(CURSOR_MEMORY_RULE_MARKER);
    out.push_str("\n\n# Project memory (tracedecay)\n\n");
    if let Some(root) = project_root {
        out.push_str("Facts below were recorded for `");
        out.push_str(&sanitize_fact_line(root));
        out.push_str("`; disregard them when working in a different project.\n\n");
    }
    match render_fact_block(DIGEST_HEADER, facts, SESSION_DIGEST_CHAR_BUDGET) {
        Some((block, _)) => out.push_str(&block),
        None => out.push_str(
            "No durable facts stored yet. As decisions, preferences, and corrections \
             surface, store them with `tracedecay_fact_store` (action \"add\") and they \
             will appear here.\n",
        ),
    }
    out.push_str(
        "\nCurate via `tracedecay_fact_store` (update/remove), `tracedecay_fact_feedback`, \
         or the tracedecay dashboard.\n",
    );
    out
}

/// Renders Cursor's always-applied rule for a projectless session. Replacing
/// the previous project rule prevents facts from the last workspace leaking
/// into general chat.
pub fn render_cursor_user_memory_rule(facts: &[FactRecord]) -> String {
    let mut out = String::from(
        "---\ndescription: Durable user memory from the tracedecay user fact store\nalwaysApply: true\n---\n\n",
    );
    out.push_str(CURSOR_MEMORY_RULE_MARKER);
    out.push_str("\n\n# User memory (tracedecay)\n\n");
    match render_fact_block(USER_DIGEST_HEADER, facts, SESSION_DIGEST_CHAR_BUDGET) {
        Some((block, _)) => out.push_str(&block),
        None => out.push_str(
            "No durable user facts stored yet. Store lasting preferences with \
             `tracedecay_fact_store` using `memory_scope=user`.\n",
        ),
    }
    out.push_str(
        "\nCurate via `tracedecay_fact_store` or `tracedecay_fact_feedback` with \
         `memory_scope=user`.\n",
    );
    out
}

fn write_cursor_memory_rule_if_managed(rule_path: &Path, rendered: &str) -> bool {
    match std::fs::read_to_string(rule_path) {
        Ok(existing) if existing == rendered => return false,
        // Never overwrite a user-authored file at the managed path.
        Ok(existing) if !existing.contains(CURSOR_MEMORY_RULE_MARKER) => return false,
        _ => {}
    }
    crate::agents::safe_write_text_file(rule_path, rendered, None).is_ok()
}

/// Regenerates the materialized Cursor memory rule for `root`, writing only
/// when content changed (hash check) and only when the tracedecay Cursor
/// plugin is installed and the target file is absent or tracedecay-generated.
/// Workspaces without an initialized store still rewrite the managed rule to
/// an empty placeholder so stale facts from another workspace cannot remain
/// always-applied. Fail-open: returns whether the file was (re)written.
pub async fn regenerate_cursor_memory_rule(root: &Path) -> bool {
    if !memory_injection_enabled() {
        return false;
    }
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    regenerate_cursor_memory_rule_with_home(root, &home).await
}

/// Replaces Cursor's managed project rule with profile-level user memory for
/// a projectless session.
pub async fn regenerate_cursor_user_memory_rule() -> bool {
    if !memory_injection_enabled() {
        return false;
    }
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let rule_path = crate::agents::cursor::cursor_memory_rule_path(&home);
    let plugin_dir = crate::agents::cursor::cursor_plugin_install_dir(&home);
    if !plugin_dir.join(".cursor-plugin/plugin.json").exists() {
        return false;
    }
    let facts = daemon_fact_list(None, true).await;
    let facts = select_digest_facts(facts, SESSION_DIGEST_FACT_COUNT);
    write_cursor_memory_rule_if_managed(&rule_path, &render_cursor_user_memory_rule(&facts))
}

async fn regenerate_cursor_memory_rule_with_home(root: &Path, home: &Path) -> bool {
    let rule_path = crate::agents::cursor::cursor_memory_rule_path(home);
    let plugin_dir = crate::agents::cursor::cursor_plugin_install_dir(home);
    if !plugin_dir.join(".cursor-plugin/plugin.json").exists() {
        return false;
    }
    let facts = if crate::tracedecay::TraceDecay::is_initialized(root) {
        select_digest_facts(digest_candidates(root).await, SESSION_DIGEST_FACT_COUNT)
    } else {
        Vec::new()
    };
    let rendered = render_cursor_memory_rule(Some(root.to_string_lossy().as_ref()), &facts);
    write_cursor_memory_rule_if_managed(&rule_path, &rendered)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::memory::types::MemoryCategory;

    struct HomeEnvGuard(Option<std::ffi::OsString>);

    impl HomeEnvGuard {
        fn set(home: &Path) -> Self {
            let previous = std::env::var_os("HOME");
            // SAFETY: The shared hook test lock prevents concurrent tests from
            // observing this temporary process-environment override.
            unsafe { std::env::set_var("HOME", home) };
            Self(previous)
        }
    }

    impl Drop for HomeEnvGuard {
        fn drop(&mut self) {
            // SAFETY: The lock used by the caller remains held through drop.
            unsafe {
                if let Some(previous) = self.0.take() {
                    std::env::set_var("HOME", previous);
                } else {
                    std::env::remove_var("HOME");
                }
            }
        }
    }

    fn fact(fact_id: i64, category: MemoryCategory, trust: f64, content: &str) -> FactRecord {
        FactRecord {
            fact_id,
            content: content.to_string(),
            category,
            tags: Vec::new(),
            entities: Vec::new(),
            trust_score: trust,
            source: None,
            retrieval_count: 0,
            access_count: 0,
            helpful_count: 0,
            unhelpful_count: 0,
            created_at: 1_000 + fact_id,
            updated_at: 1_000 + fact_id,
            last_retrieved_at: None,
            last_recalled_at: None,
            last_feedback_at: None,
            metadata: serde_json::Value::Null,
        }
    }

    fn search_result(fact: FactRecord, score: f64, fts: f64, jaccard: f64) -> FactSearchResult {
        FactSearchResult {
            trust_score: fact.trust_score,
            fact,
            score,
            fts_score: fts,
            jaccard_score: jaccard,
            holographic_score: 0.5,
            why: None,
        }
    }

    #[test]
    fn digest_renders_header_ids_categories_and_trust() {
        let facts = vec![
            fact(7, MemoryCategory::Decision, 0.9, "Use pnpm for installs"),
            fact(
                3,
                MemoryCategory::UserPref,
                0.8,
                "Prefer nextest over cargo test",
            ),
        ];
        let digest = render_memory_digest(&facts, SESSION_DIGEST_CHAR_BUDGET).unwrap();
        assert!(digest.starts_with(DIGEST_HEADER));
        assert!(digest.contains("[decision #7 trust 0.90] Use pnpm for installs"));
        assert!(digest.contains("[user_pref #3 trust 0.80] Prefer nextest over cargo test"));
    }

    #[test]
    fn combined_digest_labels_scopes_deduplicates_content_and_stays_bounded() {
        let user = vec![
            fact(1, MemoryCategory::UserPref, 0.95, "Prefer concise answers"),
            fact(2, MemoryCategory::General, 0.80, "Shared fact"),
        ];
        let project = vec![
            fact(1, MemoryCategory::Decision, 0.90, "Use SQLite"),
            fact(3, MemoryCategory::General, 0.70, "Shared fact"),
        ];
        let selected = select_scoped_facts(user, project, 8);
        let (text, user_ids, project_ids) =
            render_scoped_fact_block(COMBINED_DIGEST_HEADER, &selected, 300).unwrap();

        assert!(text.contains("[user user_pref #1 trust 0.95] Prefer concise answers"));
        assert!(text.contains("[project decision #1 trust 0.90] Use SQLite"));
        assert_eq!(text.matches("Shared fact").count(), 1);
        assert!(text.chars().count() <= 300);
        assert_eq!(user_ids, vec![1, 2]);
        assert_eq!(project_ids, vec![1]);
    }

    #[test]
    fn digest_enforces_char_budget_by_dropping_trailing_facts() {
        let facts: Vec<FactRecord> = (0..40)
            .map(|i| {
                fact(
                    i,
                    MemoryCategory::Project,
                    0.9,
                    &format!("fact {i} {}", "x".repeat(150)),
                )
            })
            .collect();
        let digest = render_memory_digest(&facts, SESSION_DIGEST_CHAR_BUDGET).unwrap();
        assert!(
            digest.chars().count() <= SESSION_DIGEST_CHAR_BUDGET,
            "digest must stay within budget, got {}",
            digest.chars().count()
        );
        assert!(digest.contains("fact 0"));
        assert!(
            !digest.contains("fact 39"),
            "trailing facts must be dropped"
        );
    }

    #[test]
    fn digest_returns_none_when_nothing_fits_or_no_facts() {
        assert!(render_memory_digest(&[], SESSION_DIGEST_CHAR_BUDGET).is_none());
        let facts = vec![fact(1, MemoryCategory::General, 0.9, "some fact")];
        assert!(render_memory_digest(&facts, 10).is_none());
    }

    #[test]
    fn secret_like_facts_are_never_selected() {
        let facts = vec![
            fact(
                1,
                MemoryCategory::Tool,
                0.99,
                concat!("api_", "key=", "0000000000000000"),
            ),
            fact(
                2,
                MemoryCategory::Tool,
                0.7,
                "Use pnpm rather than npm for installs",
            ),
        ];
        let selected = select_digest_facts(facts, 8);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].fact_id, 2);
    }

    #[test]
    fn digest_selection_is_category_diverse_then_trust_ordered() {
        let facts = vec![
            fact(1, MemoryCategory::Decision, 0.95, "d1"),
            fact(2, MemoryCategory::Decision, 0.94, "d2"),
            fact(3, MemoryCategory::Decision, 0.93, "d3"),
            fact(4, MemoryCategory::UserPref, 0.70, "p1"),
            fact(5, MemoryCategory::Tool, 0.65, "t1"),
        ];
        let selected = select_digest_facts(facts, 3);
        let ids: Vec<i64> = selected.iter().map(|f| f.fact_id).collect();
        // One per category first (1, 4, 5), rendered in trust order.
        assert_eq!(ids, vec![1, 4, 5]);
    }

    #[test]
    fn digest_selection_fills_remaining_slots_by_trust() {
        let facts = vec![
            fact(1, MemoryCategory::Decision, 0.95, "d1"),
            fact(2, MemoryCategory::Decision, 0.94, "d2"),
            fact(3, MemoryCategory::UserPref, 0.70, "p1"),
        ];
        let ids: Vec<i64> = select_digest_facts(facts, 3)
            .iter()
            .map(|f| f.fact_id)
            .collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn prompt_recall_requires_lexical_relevance_and_score() {
        let results = vec![
            // FTS hit above the score floor: kept.
            search_result(
                fact(1, MemoryCategory::Decision, 0.9, "relevant"),
                0.3,
                0.6,
                0.05,
            ),
            // No FTS hit, weak overlap: dropped even with decent score.
            search_result(
                fact(2, MemoryCategory::Decision, 0.9, "irrelevant"),
                0.3,
                0.0,
                0.05,
            ),
            // Overlap above the jaccard floor but combined score too low: dropped.
            search_result(
                fact(3, MemoryCategory::Decision, 0.9, "weak"),
                0.05,
                0.0,
                0.4,
            ),
            // Jaccard-only relevance with a clearing score: kept.
            search_result(
                fact(4, MemoryCategory::Tool, 0.8, "overlap"),
                0.25,
                0.0,
                0.3,
            ),
        ];
        let ids: Vec<i64> = select_prompt_recall_facts(results, &HashSet::new(), 3)
            .iter()
            .map(|f| f.fact_id)
            .collect();
        assert_eq!(ids, vec![1, 4]);
    }

    #[test]
    fn prompt_recall_dedupes_against_already_injected_and_caps_count() {
        let results: Vec<FactSearchResult> = (1..=6)
            .map(|i| {
                search_result(
                    fact(i, MemoryCategory::Project, 0.9, &format!("fact {i}")),
                    0.4,
                    0.5,
                    0.3,
                )
            })
            .collect();
        let already: HashSet<i64> = [1, 2].into_iter().collect();
        let ids: Vec<i64> = select_prompt_recall_facts(results, &already, PROMPT_RECALL_FACT_COUNT)
            .iter()
            .map(|f| f.fact_id)
            .collect();
        assert_eq!(ids, vec![3, 4, 5]);
    }

    #[test]
    fn prompt_recall_render_respects_budget() {
        let facts: Vec<FactRecord> = (0..10)
            .map(|i| {
                fact(
                    i,
                    MemoryCategory::Project,
                    0.9,
                    &format!("prompt fact {i} {}", "y".repeat(200)),
                )
            })
            .collect();
        let text = render_prompt_recall(&facts, PROMPT_RECALL_CHAR_BUDGET).unwrap();
        assert!(text.chars().count() <= PROMPT_RECALL_CHAR_BUDGET);
        assert!(text.starts_with(PROMPT_RECALL_HEADER));
    }

    #[test]
    fn sanitize_collapses_newlines_control_chars_and_truncates() {
        let sanitized = sanitize_fact_line("line one\nline two\t\u{1b}[31mred\u{1b}[0m   spaced");
        assert_eq!(sanitized, "line one line two [31mred [0m spaced");
        let long = sanitize_fact_line(&"z".repeat(500));
        assert!(long.chars().count() <= FACT_LINE_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn gate_env_override_beats_config() {
        assert!(injection_enabled_from(None, true));
        assert!(!injection_enabled_from(None, false));
        assert!(!injection_enabled_from(Some("0"), true));
        assert!(!injection_enabled_from(Some("false"), true));
        assert!(!injection_enabled_from(Some("off"), true));
        assert!(injection_enabled_from(Some("1"), false));
        assert!(injection_enabled_from(Some("true"), false));
    }

    #[test]
    fn seen_file_round_trips_and_scopes_by_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory_inject_seen.json");
        let mut seen = MemoryInjectSeen::load_or_default(&path);
        seen.record("session-a", &[1, 2]);
        seen.record("session-b", &[3]);
        seen.save(&path).unwrap();

        let reloaded = MemoryInjectSeen::load_or_default(&path);
        assert_eq!(
            reloaded.seen_for_session("session-a"),
            [1, 2].into_iter().collect()
        );
        assert_eq!(
            reloaded.seen_for_session("session-b"),
            [3].into_iter().collect()
        );
    }

    #[test]
    fn cursor_memory_rule_is_always_applied_and_budgeted() {
        let facts = vec![fact(
            9,
            MemoryCategory::Decision,
            0.88,
            "Ship digests via hooks",
        )];
        let rule = render_cursor_memory_rule(Some("/home/user/proj"), &facts);
        assert!(rule.starts_with("---\n"));
        assert!(rule.contains("alwaysApply: true"));
        assert!(rule.contains(CURSOR_MEMORY_RULE_MARKER));
        assert!(rule.contains("[decision #9 trust 0.88] Ship digests via hooks"));
        assert!(rule.contains("/home/user/proj"));

        let empty = render_cursor_memory_rule(None, &[]);
        assert!(empty.contains("No durable facts stored yet"));
        assert!(empty.contains("alwaysApply: true"));

        let user = render_cursor_user_memory_rule(&facts);
        assert!(user.contains("# User memory (tracedecay)"));
        assert!(user.contains("Ship digests via hooks"));
        assert!(!user.contains("/home/user/proj"));
        assert!(user.contains("memory_scope=user"));
    }

    #[tokio::test]
    async fn cursor_memory_rule_clears_stale_facts_when_store_missing() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();

        let rule_path = crate::agents::cursor::cursor_memory_rule_path(home.path());
        let plugin_dir = crate::agents::cursor::cursor_plugin_install_dir(home.path());
        std::fs::create_dir_all(plugin_dir.join(".cursor-plugin")).unwrap();
        std::fs::write(plugin_dir.join(".cursor-plugin/plugin.json"), "{}").unwrap();
        std::fs::create_dir_all(rule_path.parent().unwrap()).unwrap();
        let stale_rule = [
            CURSOR_MEMORY_RULE_MARKER,
            "\n\n# Project memory (tracedecay)\n\n- [decision #9 trust 0.90] stale fact from another workspace\n",
        ]
        .concat();
        std::fs::write(&rule_path, stale_rule).unwrap();

        let rewritten = regenerate_cursor_memory_rule_with_home(root.path(), home.path()).await;
        let content = std::fs::read_to_string(&rule_path).unwrap();

        assert!(rewritten, "managed stale rule should be rewritten");
        assert!(content.contains(CURSOR_MEMORY_RULE_MARKER));
        assert!(content.contains("No durable facts stored yet"));
        assert!(!content.contains("stale fact from another workspace"));
    }

    #[test]
    fn projectless_cursor_memory_uses_the_plugin_install_directory() {
        crate::hooks::run_with_test_env_lock(async {
            let home = tempfile::tempdir().unwrap();
            let _home_guard = HomeEnvGuard::set(home.path());
            let plugin_dir = crate::agents::cursor::cursor_plugin_install_dir(home.path());
            std::fs::create_dir_all(plugin_dir.join(".cursor-plugin")).unwrap();
            std::fs::write(
                plugin_dir.join(".cursor-plugin/plugin.json"),
                r#"{"name":"tracedecay"}"#,
            )
            .unwrap();

            assert!(
                regenerate_cursor_user_memory_rule().await,
                "projectless Cursor memory should regenerate when the plugin is installed"
            );
            let rule_path = crate::agents::cursor::cursor_memory_rule_path(home.path());
            let content = std::fs::read_to_string(rule_path).unwrap();
            assert!(content.contains("# User memory (tracedecay)"));
        });
    }
}
