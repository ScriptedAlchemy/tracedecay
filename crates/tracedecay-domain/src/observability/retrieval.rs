use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::CoverageStateV1;
use super::execution::validate_revision;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalQueryObservedV1 {
    pub query_family: String,
    pub enabled_lanes: Vec<String>,
    pub candidate_budget: u64,
    pub context_budget: u64,
    pub token_budget: u64,
    pub answered: bool,
    pub source_coverage: CoverageStateV1,
    pub lane_coverage: CoverageStateV1,
}

impl RetrievalQueryObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        const FAMILIES: &[&str] = &[
            "exact_technical",
            "phrase",
            "natural_language",
            "typo",
            "temporal",
            "graph",
            "task_session",
            "diagnostic",
            "no_answer",
            "unknown",
        ];
        if !FAMILIES.contains(&self.query_family.as_str())
            || !valid_retriever_lanes(&self.enabled_lanes)
        {
            return Err("retrieval_query_dimensions");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalPlannerObservedV1 {
    pub planner_revision: String,
    pub requested_lanes: Vec<String>,
    pub admitted_lanes: Vec<String>,
    pub abstained: bool,
}

impl RetrievalPlannerObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        validate_revision(&self.planner_revision)?;
        if !valid_retriever_lanes(&self.requested_lanes)
            || !valid_retriever_lanes(&self.admitted_lanes)
            || self
                .admitted_lanes
                .iter()
                .any(|lane| !self.requested_lanes.contains(lane))
        {
            return Err("retrieval_planner_lanes");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrieverObservedV1 {
    pub retriever_kind: String,
    pub profile_revision: String,
    pub requested_candidates: u64,
    pub consumed_candidates: u64,
    pub eligible_candidates: u64,
    pub returned_candidates: u64,
    pub unique_contributions: u64,
}

impl RetrieverObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        validate_revision(&self.profile_revision)?;
        if !valid_retriever_lane(&self.retriever_kind)
            || self.consumed_candidates > self.requested_candidates
            || self.returned_candidates > self.eligible_candidates
            || self.unique_contributions > self.returned_candidates
        {
            return Err("retriever_counts");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalSynthesisObservedV1 {
    pub candidate_count: u64,
    pub context_count: u64,
    pub context_tokens: u64,
    pub abstained: bool,
}

impl RetrievalSynthesisObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if self.context_count > self.candidate_count {
            return Err("retrieval_synthesis_counts");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalSourceObservedV1 {
    pub source_kind: String,
    pub eligible: u64,
    pub observed: u64,
    pub denied: u64,
    pub unknown: u64,
}

impl RetrievalSourceObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        const SOURCE_KINDS: &[&str] = &[
            "code",
            "session",
            "memory",
            "fact",
            "work",
            "git",
            "diagnostic",
            "external",
            "unknown",
        ];
        if !SOURCE_KINDS.contains(&self.source_kind.as_str())
            || self
                .observed
                .saturating_add(self.denied)
                .saturating_add(self.unknown)
                > self.eligible
        {
            return Err("retrieval_source");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextOutcomeObservedV1 {
    pub outcome: String,
    pub independently_observed: bool,
    pub censored: bool,
}

impl ContextOutcomeObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        const OUTCOMES: &[&str] = &[
            "context_supplied",
            "evidence_cited",
            "independently_verified_use",
            "no_use_observed",
            "unknown",
        ];
        if !OUTCOMES.contains(&self.outcome.as_str())
            || (self.independently_observed && self.censored)
        {
            return Err("context_outcome");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RetrievalAblationObservedV1 {
    pub descriptor_revision: String,
    pub baseline_value: f64,
    pub candidate_value: f64,
    pub unit: String,
    pub coverage: CoverageStateV1,
}

impl RetrievalAblationObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        validate_revision(&self.descriptor_revision)?;
        if !self.baseline_value.is_finite()
            || !self.candidate_value.is_finite()
            || !matches!(
                self.unit.as_str(),
                "ratio" | "seconds" | "microseconds" | "bytes" | "events"
            )
        {
            return Err("retrieval_ablation");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdoptionEligibilityObservedV1 {
    pub capability: String,
    pub eligible: u64,
    pub enabled: u64,
    pub available: u64,
}

impl AdoptionEligibilityObservedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        const CAPABILITIES: &[&str] = &[
            "retrieval",
            "context_scout",
            "feedback",
            "automation",
            "work",
            "workflow",
            "git",
            "lsp",
            "hooks",
            "mcp",
            "dashboard",
            "analytics",
        ];
        if !CAPABILITIES.contains(&self.capability.as_str())
            || self.enabled > self.eligible
            || self.available > self.enabled
        {
            return Err("adoption_eligibility");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdoptionOutcomeLinkedV1 {
    pub invoked: u64,
    pub terminal: u64,
    pub independently_useful: u64,
    pub repeat_useful: u64,
    pub censored: u64,
    pub unknown: u64,
}

impl AdoptionOutcomeLinkedV1 {
    pub(super) fn validate(&self) -> Result<(), &'static str> {
        if self.terminal > self.invoked
            || self.independently_useful > self.terminal
            || self.repeat_useful > self.independently_useful
            || self
                .terminal
                .saturating_add(self.censored)
                .saturating_add(self.unknown)
                > self.invoked
        {
            return Err("adoption_outcome");
        }
        Ok(())
    }
}

fn valid_retriever_lanes(lanes: &[String]) -> bool {
    lanes.len() <= 7
        && lanes.iter().all(|lane| valid_retriever_lane(lane))
        && lanes
            .iter()
            .enumerate()
            .all(|(index, lane)| !lanes[..index].contains(lane))
}

fn valid_retriever_lane(lane: &str) -> bool {
    matches!(
        lane,
        "exact_literal"
            | "lexical"
            | "semantic"
            | "graph"
            | "temporal"
            | "task_session"
            | "diagnostic"
    )
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsModeV1 {
    Off,
    LocalOnly,
    AggregateShare,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalyticsConsentChangedV1 {
    pub previous: AnalyticsModeV1,
    pub current: AnalyticsModeV1,
    pub share_staging_age_seconds: Option<u64>,
}
