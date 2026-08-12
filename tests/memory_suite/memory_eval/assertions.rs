use serde::Deserialize;
use tracedecay_application::retained_surfaces::FactFeedbackActionV1;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(super) enum Assertion {
    FactCount {
        name: String,
        op: CompareOp,
        value: i64,
        #[serde(default)]
        phase: AssertionPhase,
    },
    SourceCount {
        name: String,
        source: String,
        op: CompareOp,
        value: i64,
        #[serde(default)]
        phase: AssertionPhase,
    },
    ContentCount {
        name: String,
        contains: String,
        op: CompareOp,
        value: i64,
        #[serde(default)]
        phase: AssertionPhase,
    },
    SourceTrust {
        name: String,
        source: String,
        op: CompareOp,
        value: f64,
        #[serde(default)]
        phase: AssertionPhase,
    },
    RetrievalTotal {
        name: String,
        source: String,
        op: CompareOp,
        value: i64,
        #[serde(default)]
        phase: AssertionPhase,
    },
    FeedbackHistory {
        name: String,
        source: String,
        action: FactFeedbackActionV1,
        op: CompareOp,
        value: i64,
        #[serde(default)]
        phase: AssertionPhase,
    },
    CurationRemovesSource {
        name: String,
        source: String,
        expected: bool,
        #[serde(default)]
        phase: AssertionPhase,
    },
    SearchRank {
        name: String,
        query: String,
        top_fact_source: String,
        min_rank_gap: usize,
        #[serde(default = "default_search_limit")]
        limit: usize,
        #[serde(default)]
        phase: AssertionPhase,
    },
    SearchSource {
        name: String,
        query: String,
        source: String,
        #[serde(default = "default_search_limit")]
        limit: usize,
        #[serde(default)]
        phase: AssertionPhase,
    },
}

fn default_search_limit() -> usize {
    5
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub(super) enum CompareOp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Deserialize, Clone, Copy, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum AssertionPhase {
    #[default]
    Both,
    WellBehavedOnly,
    ViolationOnly,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Phase {
    WellBehaved,
    Violation,
}

pub(super) fn should_skip_assertion(phase: Phase, assertion_phase: AssertionPhase) -> bool {
    matches!(
        (phase, assertion_phase),
        (Phase::Violation, AssertionPhase::WellBehavedOnly)
            | (Phase::WellBehaved, AssertionPhase::ViolationOnly)
    )
}

pub(super) struct AssertionOutcome {
    pub(super) name: String,
    pub(super) passed: bool,
    pub(super) detail: String,
}
