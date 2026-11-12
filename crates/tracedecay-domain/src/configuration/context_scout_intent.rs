//! Context Scout intent-pass vocabulary and configuration.
//!
//! The intent pass reads a bounded window of recent session turns and derives
//! deterministic action facts and prompt inferences from it. This module owns
//! only the intent vocabulary and the operator-configured bounds.

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

/// What made an intent a candidate: a deterministic action fact observed in
/// the turn, or a deterministic interpretation of the user's prompt.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum IntentDecisionSourceV1 {
    ActionFact,
    PromptInference,
}

/// Bounds on the session window one intent pass may read.
pub const INTENT_WINDOW_MAX_USER_TURNS_RANGE: std::ops::RangeInclusive<u8> = 1..=16;
pub const INTENT_WINDOW_MAX_MESSAGES_RANGE: std::ops::RangeInclusive<u8> = 1..=64;
pub const INTENT_WINDOW_MAX_BYTES_RANGE: std::ops::RangeInclusive<u32> = 256..=256 * 1024;
pub const INTENT_WINDOW_MAX_AGE_SECS_RANGE: std::ops::RangeInclusive<u32> = 1..=86_400;
pub const INTENT_WINDOW_MAX_FRESHEN_BUDGET_MS: u32 = 2_000;

pub const INTENT_MAX_PERMILLE: u16 = 1_000;

/// Bounds on the suggestion lifecycle the intent pass produces.
pub const INTENT_TTL_SECS_RANGE: std::ops::RangeInclusive<u32> = 1..=86_400;
pub const INTENT_MAX_DELIVERY_GAP_SECS: u32 = 86_400;
pub const INTENT_PROMPT_LOOKUP_DEADLINE_MS_RANGE: std::ops::RangeInclusive<u32> = 1..=5_000;

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

/// Operator configuration for the Context Scout intent pass.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutIntentSettingsV1 {
    pub enabled: bool,
    pub window: IntentWindowSettingsV1,
    /// Score a deterministic action fact contributes for its intent.
    pub action_fact_score_permille: u16,
    pub ttl_secs: u32,
    pub min_delivery_gap_secs: u32,
    pub prompt_lookup_deadline_ms: u32,
}

impl ContextScoutIntentSettingsV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.window.validate()?;
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
    /// so a changed window or lifecycle bound is never treated as the same pass.
    pub fn settings_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(INTENT_SETTINGS_DIGEST_DOMAIN, self))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn settings() -> ContextScoutIntentSettingsV1 {
        ContextScoutIntentSettingsV1 {
            enabled: true,
            window: IntentWindowSettingsV1 {
                max_user_turns: 3,
                max_messages: 12,
                max_bytes: 16 * 1024,
                max_age_secs: 1_800,
                freshen_budget_ms: 150,
            },
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
        settings().validate().unwrap();
        let mut settings = settings();
        settings.action_fact_score_permille = 1_001;
        assert!(settings.validate().is_err());
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
        changed.action_fact_score_permille = 650;
        assert_ne!(changed.settings_digest().unwrap(), digest);
        let mut disabled = settings();
        disabled.enabled = false;
        assert_ne!(disabled.settings_digest().unwrap(), digest);
    }
}
