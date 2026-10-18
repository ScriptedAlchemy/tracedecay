//! Context Scout intent-pass vocabulary and configuration.
//!
//! The intent pass reads a bounded window of recent session turns, asks a
//! configured decision engine one typed yes/no question per intent, and
//! combines the answers with deterministic action facts. This module owns only
//! the intent vocabulary and the operator-configured bounds. The engine URL
//! and model are never defaulted: an intent pass without an explicit engine is
//! not configured.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::DomainError;
use crate::research::{ManifestDigest, canonical_sha256};

const INTENT_SETTINGS_DIGEST_DOMAIN: &str = "tracedecay.context-scout.intent-settings.v1";

/// One user intent the Scout can recognize. The serialized keys are the
/// historical hook hint category keys, so persisted hint analytics, hint
/// settlement, and evals keep one vocabulary.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutIntentKindV1 {
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

impl ContextScoutIntentKindV1 {
    pub const ALL: [Self; 20] = [
        Self::Search,
        Self::SemanticSearch,
        Self::FileRead,
        Self::ToolDescriptorRead,
        Self::BroadRead,
        Self::CallGraph,
        Self::Impact,
        Self::SymbolLookup,
        Self::FileLookup,
        Self::ProjectContext,
        Self::SessionRecall,
        Self::AtomicEdit,
        Self::TypeOrientation,
        Self::ExploreSubagent,
        Self::SubagentStartContext,
        Self::BuildDiagnostics,
        Self::ReviewChanges,
        Self::MemoryStore,
        Self::EditRedundancy,
        Self::UnexpectedChanges,
    ];

    /// The serialized key, identical to the serde representation.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::SemanticSearch => "semantic_search",
            Self::FileRead => "file_read",
            Self::ToolDescriptorRead => "tool_descriptor_read",
            Self::BroadRead => "broad_read",
            Self::CallGraph => "call_graph",
            Self::Impact => "impact",
            Self::SymbolLookup => "symbol_lookup",
            Self::FileLookup => "file_lookup",
            Self::ProjectContext => "project_context",
            Self::SessionRecall => "session_recall",
            Self::AtomicEdit => "atomic_edit",
            Self::TypeOrientation => "type_orientation",
            Self::ExploreSubagent => "explore_subagent",
            Self::SubagentStartContext => "subagent_start_context",
            Self::BuildDiagnostics => "build_diagnostics",
            Self::ReviewChanges => "review_changes",
            Self::MemoryStore => "memory_store",
            Self::EditRedundancy => "edit_redundancy",
            Self::UnexpectedChanges => "unexpected_changes",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_key() == key)
    }
}

/// What made an intent a candidate: the decision engine's answer, a
/// deterministic action fact observed in the turn, or both.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum IntentDecisionSourceV1 {
    Model,
    ActionFact,
    Both,
}

/// Bounds on the decision-engine round trip. `timeout_ms` bounds one request;
/// `max_concurrent` bounds in-flight requests across every session.
pub const DECISION_ENGINE_TIMEOUT_MS_RANGE: std::ops::RangeInclusive<u32> = 20..=5_000;
pub const DECISION_ENGINE_MAX_CONCURRENT_RANGE: std::ops::RangeInclusive<u8> = 1..=8;
pub const DECISION_ENGINE_MAX_STATE_BYTES_RANGE: std::ops::RangeInclusive<u32> = 1..=1024 * 1024;
const DECISION_ENGINE_MAX_BASE_URL_BYTES: usize = 2_048;
const DECISION_ENGINE_MAX_MODEL_BYTES: usize = 256;

/// Bounds on the session window one intent pass may read.
pub const INTENT_WINDOW_MAX_USER_TURNS_RANGE: std::ops::RangeInclusive<u8> = 1..=16;
pub const INTENT_WINDOW_MAX_MESSAGES_RANGE: std::ops::RangeInclusive<u8> = 1..=64;
pub const INTENT_WINDOW_MAX_BYTES_RANGE: std::ops::RangeInclusive<u32> = 256..=256 * 1024;
pub const INTENT_WINDOW_MAX_AGE_SECS_RANGE: std::ops::RangeInclusive<u32> = 1..=86_400;
pub const INTENT_WINDOW_MAX_FRESHEN_BUDGET_MS: u32 = 2_000;

/// Bounds on one configured question.
pub const INTENT_QUESTION_MAX_INSTRUCTION_BYTES: usize = 512;
pub const INTENT_MAX_PERMILLE: u16 = 1_000;

/// Bounds on the suggestion lifecycle the intent pass produces.
pub const INTENT_TTL_SECS_RANGE: std::ops::RangeInclusive<u32> = 1..=86_400;
pub const INTENT_MAX_DELIVERY_GAP_SECS: u32 = 86_400;
pub const INTENT_PROMPT_LOOKUP_DEADLINE_MS_RANGE: std::ops::RangeInclusive<u32> = 1..=5_000;

/// Threshold the reference question set starts from.
pub const INTENT_REFERENCE_THRESHOLD_PERMILLE: u16 = 600;

/// The decision engine the intent pass asks. There is no default endpoint or
/// model; both come only from explicit operator configuration.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionEngineSettingsV1 {
    pub base_url: String,
    pub model: Option<String>,
    pub timeout_ms: u32,
    pub max_concurrent: u8,
    pub max_state_bytes: u32,
}

impl DecisionEngineSettingsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        let field = "context scout decision engine";
        let url = self.base_url.as_str();
        let has_scheme_and_host = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'));
        if !has_scheme_and_host
            || url.len() > DECISION_ENGINE_MAX_BASE_URL_BYTES
            || url
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || !DECISION_ENGINE_TIMEOUT_MS_RANGE.contains(&self.timeout_ms)
            || !DECISION_ENGINE_MAX_CONCURRENT_RANGE.contains(&self.max_concurrent)
            || !DECISION_ENGINE_MAX_STATE_BYTES_RANGE.contains(&self.max_state_bytes)
            || self.model.as_deref().is_some_and(|model| {
                model.trim().is_empty()
                    || model.len() > DECISION_ENGINE_MAX_MODEL_BYTES
                    || model.chars().any(char::is_control)
            })
        {
            return Err(DomainError::NonCanonical { field });
        }
        Ok(())
    }
}

/// Bounds on the recent-session window one intent pass reads.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntentWindowSettingsV1 {
    pub max_user_turns: u8,
    pub max_messages: u8,
    pub max_bytes: u32,
    pub max_age_secs: u32,
    /// Time the pass may wait for the transcript store to ingest the newest
    /// turn before it reads the window as it stands.
    pub freshen_budget_ms: u32,
}

impl IntentWindowSettingsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if !INTENT_WINDOW_MAX_USER_TURNS_RANGE.contains(&self.max_user_turns)
            || !INTENT_WINDOW_MAX_MESSAGES_RANGE.contains(&self.max_messages)
            || self.max_messages < self.max_user_turns
            || !INTENT_WINDOW_MAX_BYTES_RANGE.contains(&self.max_bytes)
            || !INTENT_WINDOW_MAX_AGE_SECS_RANGE.contains(&self.max_age_secs)
            || self.freshen_budget_ms > INTENT_WINDOW_MAX_FRESHEN_BUDGET_MS
        {
            return Err(DomainError::NonCanonical {
                field: "context scout intent window",
            });
        }
        Ok(())
    }
}

/// One yes/no question asked of the decision engine for one intent. The
/// intent is a candidate when the engine's probability reaches
/// `threshold_permille` and, when set, exceeds the runner-up by
/// `margin_permille`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IntentQuestionSettingsV1 {
    pub intent: ContextScoutIntentKindV1,
    pub instructions: String,
    pub threshold_permille: u16,
    pub margin_permille: Option<u16>,
}

impl IntentQuestionSettingsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.instructions.trim().is_empty()
            || self.instructions.len() > INTENT_QUESTION_MAX_INSTRUCTION_BYTES
            || self.instructions.chars().any(char::is_control)
            || self.threshold_permille > INTENT_MAX_PERMILLE
            || self
                .margin_permille
                .is_some_and(|margin| margin > INTENT_MAX_PERMILLE)
        {
            return Err(DomainError::NonCanonical {
                field: "context scout intent question",
            });
        }
        Ok(())
    }
}

/// Operator configuration for the Context Scout intent pass.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutIntentSettingsV1 {
    pub enabled: bool,
    pub engine: DecisionEngineSettingsV1,
    pub window: IntentWindowSettingsV1,
    pub questions: Vec<IntentQuestionSettingsV1>,
    /// Score a deterministic action fact contributes for its intent.
    pub action_fact_score_permille: u16,
    pub ttl_secs: u32,
    pub min_delivery_gap_secs: u32,
    pub prompt_lookup_deadline_ms: u32,
}

impl ContextScoutIntentSettingsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.engine.validate()?;
        self.window.validate()?;
        let mut intents = BTreeSet::new();
        for question in &self.questions {
            question.validate()?;
            if !intents.insert(question.intent) {
                return Err(DomainError::NonCanonical {
                    field: "context scout intent questions",
                });
            }
        }
        if self.action_fact_score_permille > INTENT_MAX_PERMILLE
            || !INTENT_TTL_SECS_RANGE.contains(&self.ttl_secs)
            || self.min_delivery_gap_secs > INTENT_MAX_DELIVERY_GAP_SECS
            || !INTENT_PROMPT_LOOKUP_DEADLINE_MS_RANGE.contains(&self.prompt_lookup_deadline_ms)
        {
            return Err(DomainError::NonCanonical {
                field: "context scout intent settings",
            });
        }
        Ok(())
    }

    /// Behavior digest of this block. Runtime configuration pins compare it
    /// so a changed question set or engine is never treated as the same pass.
    pub fn settings_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(INTENT_SETTINGS_DIGEST_DOMAIN, self))
    }

    /// The starting question set for the model-judged intents. Tool-shape
    /// intents (search, file reads, subagent handoffs, edits) are decided by
    /// deterministic action facts and have no question.
    pub fn reference_questions() -> Vec<IntentQuestionSettingsV1> {
        REFERENCE_QUESTIONS
            .iter()
            .map(|(intent, instructions)| IntentQuestionSettingsV1 {
                intent: *intent,
                instructions: (*instructions).to_owned(),
                threshold_permille: INTENT_REFERENCE_THRESHOLD_PERMILLE,
                margin_permille: None,
            })
            .collect()
    }
}

const REFERENCE_QUESTIONS: [(ContextScoutIntentKindV1, &str); 10] = [
    (
        ContextScoutIntentKindV1::SessionRecall,
        "Is the user asking to look up or reuse something from an earlier conversation, a previous session, or another agent's transcript?",
    ),
    (
        ContextScoutIntentKindV1::MemoryStore,
        "Did the user state a durable preference, convention, or fact about this project or their workflow that should be remembered across sessions?",
    ),
    (
        ContextScoutIntentKindV1::CallGraph,
        "Is the user asking who calls, what is called by, or what depends on a specific function, method, or symbol?",
    ),
    (
        ContextScoutIntentKindV1::Impact,
        "Is the user asking what a change would affect or break, such as its blast radius, dependent code, or the tests that cover it?",
    ),
    (
        ContextScoutIntentKindV1::ReviewChanges,
        "Is the user asking to review, summarize, or address feedback on a diff, a pull request, or a set of changed files?",
    ),
    (
        ContextScoutIntentKindV1::BuildDiagnostics,
        "Is the user trying to locate or fix a compiler, type-check, lint, or build error?",
    ),
    (
        ContextScoutIntentKindV1::AtomicEdit,
        "Is the user asking for one mechanical change applied consistently across many sites, such as a rename, a signature change, or a structural refactor?",
    ),
    (
        ContextScoutIntentKindV1::TypeOrientation,
        "Is the user asking how a type is constructed, implemented, extended, or used, such as its constructor sites, trait implementations, field uses, or type hierarchy?",
    ),
    (
        ContextScoutIntentKindV1::ProjectContext,
        "Is the user asking about the overall architecture or design of this project, or about a repository or project other than the current one?",
    ),
    (
        ContextScoutIntentKindV1::UnexpectedChanges,
        "Is the user reporting commits, file changes, or branch history that they did not make and cannot account for?",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> DecisionEngineSettingsV1 {
        DecisionEngineSettingsV1 {
            base_url: "http://127.0.0.1:8088".to_owned(),
            model: Some("intent-small".to_owned()),
            timeout_ms: 250,
            max_concurrent: 2,
            max_state_bytes: 64 * 1024,
        }
    }

    fn settings() -> ContextScoutIntentSettingsV1 {
        ContextScoutIntentSettingsV1 {
            enabled: true,
            engine: engine(),
            window: IntentWindowSettingsV1 {
                max_user_turns: 3,
                max_messages: 12,
                max_bytes: 16 * 1024,
                max_age_secs: 1_800,
                freshen_budget_ms: 150,
            },
            questions: ContextScoutIntentSettingsV1::reference_questions(),
            action_fact_score_permille: 700,
            ttl_secs: 600,
            min_delivery_gap_secs: 30,
            prompt_lookup_deadline_ms: 200,
        }
    }

    #[test]
    fn intent_keys_round_trip_through_serde() {
        for kind in ContextScoutIntentKindV1::ALL {
            let encoded = serde_json::to_value(kind).unwrap();
            assert_eq!(encoded, serde_json::Value::String(kind.as_key().to_owned()));
            assert_eq!(
                ContextScoutIntentKindV1::from_key(kind.as_key()),
                Some(kind)
            );
        }
        assert_eq!(ContextScoutIntentKindV1::from_key("unknown"), None);
        let unique = ContextScoutIntentKindV1::ALL
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), ContextScoutIntentKindV1::ALL.len());
    }

    #[test]
    fn reference_settings_validate() {
        let settings = settings();
        settings.validate().unwrap();
        assert_eq!(settings.questions.len(), 10);
        assert!(settings.questions.iter().all(
            |question| question.threshold_permille == 600 && question.margin_permille.is_none()
        ));
    }

    #[test]
    fn duplicate_question_intents_are_refused() {
        let mut settings = settings();
        let duplicate = settings.questions[0].clone();
        settings.questions.push(duplicate);
        assert!(settings.validate().is_err());
    }

    #[test]
    fn question_instructions_are_bounded_and_control_free() {
        for instructions in [
            String::new(),
            "   ".to_owned(),
            "a".repeat(INTENT_QUESTION_MAX_INSTRUCTION_BYTES + 1),
            "Is this\na question?".to_owned(),
        ] {
            let mut settings = settings();
            settings.questions[0].instructions = instructions;
            assert!(settings.validate().is_err());
        }
        let mut settings = settings();
        settings.questions[0].instructions = "a".repeat(INTENT_QUESTION_MAX_INSTRUCTION_BYTES);
        settings.validate().unwrap();
    }

    #[test]
    fn thresholds_and_margins_are_permille() {
        let mut settings = settings();
        settings.questions[0].threshold_permille = 1_001;
        assert!(settings.validate().is_err());
        let mut settings = self::settings();
        settings.questions[0].margin_permille = Some(1_001);
        assert!(settings.validate().is_err());
        let mut settings = self::settings();
        settings.action_fact_score_permille = 1_001;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn engine_bounds_are_enforced() {
        let cases: [fn(&mut DecisionEngineSettingsV1); 9] = [
            |engine| engine.timeout_ms = 19,
            |engine| engine.timeout_ms = 5_001,
            |engine| engine.max_concurrent = 0,
            |engine| engine.max_concurrent = 9,
            |engine| engine.max_state_bytes = 0,
            |engine| engine.base_url = "127.0.0.1:8088".to_owned(),
            |engine| engine.base_url = "http://".to_owned(),
            |engine| engine.base_url = "http://host name".to_owned(),
            |engine| engine.model = Some(" ".to_owned()),
        ];
        for mutate in cases {
            let mut settings = settings();
            mutate(&mut settings.engine);
            assert!(settings.validate().is_err(), "{:?}", settings.engine);
        }
        let mut settings = settings();
        settings.engine.timeout_ms = 20;
        settings.engine.max_concurrent = 8;
        settings.engine.model = None;
        settings.validate().unwrap();
    }

    #[test]
    fn window_bounds_are_enforced() {
        let cases: [fn(&mut IntentWindowSettingsV1); 6] = [
            |window| window.max_user_turns = 0,
            |window| window.max_messages = 2,
            |window| window.max_bytes = 0,
            |window| window.max_age_secs = 0,
            |window| window.freshen_budget_ms = INTENT_WINDOW_MAX_FRESHEN_BUDGET_MS + 1,
            |window| window.max_messages = 65,
        ];
        for mutate in cases {
            let mut settings = settings();
            mutate(&mut settings.window);
            assert!(settings.validate().is_err(), "{:?}", settings.window);
        }
    }

    #[test]
    fn lifecycle_bounds_are_enforced() {
        let cases: [fn(&mut ContextScoutIntentSettingsV1); 4] = [
            |settings| settings.ttl_secs = 0,
            |settings| settings.min_delivery_gap_secs = INTENT_MAX_DELIVERY_GAP_SECS + 1,
            |settings| settings.prompt_lookup_deadline_ms = 0,
            |settings| settings.prompt_lookup_deadline_ms = 5_001,
        ];
        for mutate in cases {
            let mut settings = settings();
            mutate(&mut settings);
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn settings_digest_tracks_every_behavioral_field() {
        let base = settings();
        let digest = base.settings_digest().unwrap();
        assert_eq!(digest, settings().settings_digest().unwrap());
        let mut changed = settings();
        changed.questions[0].threshold_permille = 650;
        assert_ne!(changed.settings_digest().unwrap(), digest);
        let mut disabled = settings();
        disabled.enabled = false;
        assert_ne!(disabled.settings_digest().unwrap(), digest);
    }
}
