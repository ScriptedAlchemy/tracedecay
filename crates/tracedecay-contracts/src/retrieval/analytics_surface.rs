//! Canonical CLI/MCP wire contract for `tracedecay_analytics`, the read-only
//! adoption rollup over durable analytics events, the fact-store funnel and
//! the automation run ledger.
//!
//! Presentation-only transport keys such as `format` and the routed
//! `project_selector` are removed before the request body is decoded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::observability::{CostsReadModelV1, ObservatoryReadModelV1};

/// Smallest lookback window, in days.
pub const ANALYTICS_MIN_WINDOW_DAYS: u32 = 1;
/// Largest lookback window, in days.
pub const ANALYTICS_MAX_WINDOW_DAYS: u32 = 365;
/// Lookback window when the caller names none, in days.
pub const ANALYTICS_DEFAULT_WINDOW_DAYS: u32 = 14;

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsScopeV1 {
    /// Analytics events of the resolved project.
    #[default]
    Project,
    /// Analytics events of every registered project.
    All,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsSectionV1 {
    Tools,
    Hints,
    Facts,
    Automation,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsSurfaceRequestV1 {
    /// "project" (default) scopes analytics_events to the resolved project;
    /// "all" reports across every registered project.
    pub scope: Option<AnalyticsScopeV1>,
    /// Lookback window in days for events and automation runs (default: 14,
    /// 1-365).
    #[schemars(range(min = 1, max = 365))]
    pub window_days: Option<u32>,
    /// Optional filter to a single section. Omit to return all sections.
    pub section: Option<AnalyticsSectionV1>,
}

/// The settled status of an analytics report.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsReportStatusV1 {
    Ok,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsResultV1 {
    pub status: AnalyticsReportStatusV1,
    pub scope: AnalyticsScopeV1,
    /// The canonical analytics project key; `None` for `scope: "all"`.
    pub project_id: Option<String>,
    pub project_root: String,
    pub window_days: u32,
    pub since: i64,
    pub event_count: i64,
    pub event_count_truncated: bool,
    /// Present only when no section filter was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observatory: Option<ObservatoryReadModelV1>,
    /// Present only when no section filter was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub costs: Option<CostsReadModelV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<AnalyticsToolsSectionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<AnalyticsHintsPayloadV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facts: Option<AnalyticsFactsSectionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<AnalyticsAutomationSectionV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsTierCountsV1 {
    pub tier: String,
    pub calls: i64,
    pub errors: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsTopToolV1 {
    pub tool_name: String,
    pub tier: String,
    pub calls: i64,
    pub errors: i64,
}

/// A recorded event name that resolved to a canonical tool under another
/// spelling.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsCanonicalCallNameV1 {
    pub event_name: String,
    pub canonical_tool_name: String,
    pub calls: i64,
    pub errors: i64,
}

/// A recorded event name with no advertised canonical tool.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsEventCallNameV1 {
    pub event_name: String,
    pub calls: i64,
    pub errors: i64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsZeroCallToolsV1 {
    pub count: usize,
    pub sample: Vec<String>,
    pub sample_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsToolsSectionV1 {
    pub available: bool,
    pub tiers: Vec<AnalyticsTierCountsV1>,
    pub top_tools: Vec<AnalyticsTopToolV1>,
    pub raw_distinct_event_name_count: usize,
    pub called_available_defined_tool_count: usize,
    pub available_defined_tool_count: usize,
    pub maximal_defined_tool_count: usize,
    pub aliased_call_names: Vec<AnalyticsCanonicalCallNameV1>,
    pub bound_internal_call_names: Vec<AnalyticsEventCallNameV1>,
    pub unavailable_public_call_names: Vec<AnalyticsCanonicalCallNameV1>,
    pub unknown_or_retired_call_names: Vec<AnalyticsEventCallNameV1>,
    pub zero_call_available_defined_tools: AnalyticsZeroCallToolsV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct AnalyticsHintCategoryV1 {
    pub category: String,
    pub emitted: i64,
    pub followed: i64,
    pub ignored: i64,
    pub suppressed: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct AnalyticsHintsPayloadV1 {
    pub available: bool,
    pub source: String,
    #[serde(default)]
    pub error: Option<String>,
    pub by_category: Vec<AnalyticsHintCategoryV1>,
}

/// The fact-store funnel of the resolved project, or why it is unavailable.
///
/// The variants are distinguished by their exact field sets, so each one
/// denies unknown fields and the most specific shape is tried first.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum AnalyticsFactsSectionV1 {
    Available(AnalyticsFactFunnelV1),
    ProjectUnavailable(AnalyticsProjectSectionUnavailableV1),
    Unavailable(AnalyticsSectionUnavailableV1),
}

/// `available` is always `true`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsFactFunnelV1 {
    pub available: bool,
    pub project_root: String,
    pub facts: u64,
    pub retrievals: u64,
    pub facts_retrieved: u64,
    pub helpful_feedback: u64,
    pub unhelpful_feedback: u64,
    pub facts_rated: u64,
}

/// A section the report could not read. `available` is always `false`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsSectionUnavailableV1 {
    pub available: bool,
    pub reason: String,
}

impl AnalyticsSectionUnavailableV1 {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: reason.into(),
        }
    }
}

/// A project-scoped section whose store failed to read. `available` is
/// always `false`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsProjectSectionUnavailableV1 {
    pub available: bool,
    pub reason: String,
    pub project_root: String,
}

/// The automation run outcomes of the resolved project's ledger, or why the
/// ledger is unavailable.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum AnalyticsAutomationSectionV1 {
    Available(AnalyticsAutomationOutcomesV1),
    LedgerUnavailable(AnalyticsLedgerUnavailableV1),
    Unavailable(AnalyticsSectionUnavailableV1),
}

/// `available` is always `true`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsAutomationOutcomesV1 {
    pub available: bool,
    pub dashboard_root: String,
    pub records_considered: usize,
    pub records_in_window: usize,
    pub records_truncated: bool,
    pub by_job: Vec<AnalyticsAutomationJobOutcomesV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsAutomationJobOutcomesV1 {
    pub job: String,
    pub succeeded: i64,
    pub failed: i64,
    pub skipped: i64,
    pub other: i64,
}

/// The ledger exists but failed to read. `available` is always `false`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsLedgerUnavailableV1 {
    pub available: bool,
    pub reason: String,
    pub dashboard_root: String,
}
