//! Pure soft-hint decisions shared by hook adapters.
//!
//! This module intentionally returns model-visible text only. It does not deny,
//! rewrite, or otherwise decide permissions; adapters can choose how to surface
//! a returned hint for their own hook schema.

use std::collections::HashMap;
use std::path::Path;

use tracedecay_domain::HostIntegrationIdV1;
use tracedecay_domain::configuration::ContextScoutIntentKindV1;
use tracedecay_policy::hint_delivery::{
    HintDeliveryDecisionV1, HintDeliveryInputV1, decide_hint_delivery,
};
use tracedecay_policy::intent_action_facts::{ActionFactInputV1, first_action_fact};
use tracedecay_policy::intent_catalog;

/// A hook hint category is a Context Scout intent: one vocabulary for hints,
/// hint analytics, hint settlement, and Scout suggestions. The catalog text
/// lives in [`tracedecay_policy::intent_catalog`].
pub type HintCategory = ContextScoutIntentKindV1;

// Machine-readable expected-tool list for a hint-category `key` (the value
// stored in `analytics_events.hint_category`), or `None` when the key is
// unknown. Lets the hint-outcome correlator map an emitted hint to the tools
// that would satisfy it without re-parsing hint prose.
pub(crate) use tracedecay_policy::intent_catalog::expected_tools_for_key;

#[cfg(test)]
use tracedecay_policy::intent_catalog::INTENT_CATALOG as CATEGORY_SPECS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolHintInput {
    pub agent: HostIntegrationIdV1,
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
            agent: HostIntegrationIdV1::Cursor,
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
            message: intent_catalog::escalated_message(self.category, &self.message),
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
/// the budget silences the rest, the historical model gave a pathological
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
    #[tracing::instrument(name = "hosts.hooks.tool_hints.dedupe", level = "trace", skip_all)]
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
                *self.emitted.entry(session_id).or_default() += 1;
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
    /// `MAX_PERSISTED_HINT_ENTRIES`.
    pub fn load_or_default(path: &Path) -> Self {
        match Self::load(path) {
            Ok(loaded) if loaded.persisted_len() <= MAX_PERSISTED_HINT_ENTRIES => loaded,
            _ => Self::default(),
        }
    }

    /// Number of persisted (session, category) rows this state serializes to,
    /// the bound [`load_or_default`] enforces against stale-history growth.
    fn persisted_len(&self) -> usize {
        self.categories.len()
    }

    #[tracing::instrument(name = "hosts.hooks.tool_hints.load", level = "trace", skip_all)]
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let persisted = serde_json::from_str::<PersistedHints>(&content)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        Ok(Self::from_persisted(persisted))
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

    #[tracing::instrument(name = "hosts.hooks.tool_hints.save", level = "trace", skip_all)]
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

#[tracing::instrument(name = "hosts.hooks.tool_hints.decide", level = "trace", skip_all)]
pub fn decide_hint(input: &ToolHintInput) -> Option<ToolHint> {
    if !input.hints_enabled {
        return None;
    }

    classify_hint(input).map(hint_for_category)
}

#[tracing::instrument(name = "hosts.hooks.tool_hints.classify", level = "trace", skip_all)]
fn classify_hint(input: &ToolHintInput) -> Option<HintCategory> {
    first_action_fact(&input.action_fact_input())
}

impl ToolHintInput {
    /// Borrowed action-fact view of this hook event.
    pub(crate) fn action_fact_input(&self) -> ActionFactInputV1<'_> {
        ActionFactInputV1 {
            tool_name: self.tool_name.as_deref(),
            command: self.command.as_deref(),
            prompt: self.prompt.as_deref(),
            file_path: self.file_path.as_deref(),
            subagent_type: self.subagent_type.as_deref(),
            captured_output: self.captured_output.as_deref(),
            trusted_failure: self.trusted_failure,
            edit_text: self.edit_text.as_deref(),
        }
    }
}

fn hint_for_category(category: HintCategory) -> ToolHint {
    let entry = intent_catalog::entry(category);
    ToolHint {
        category,
        message: entry.message.to_string(),
        context: intent_catalog::suggestion_context(category),
        nonblocking: entry.nonblocking,
    }
}

#[cfg(test)]
fn category_skill(category: HintCategory) -> &'static str {
    intent_catalog::entry(category).skill
}

mod classifiers;
#[cfg(test)]
pub(super) use classifiers::is_harness_memory_path;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod evals;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests;
