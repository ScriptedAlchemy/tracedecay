//! Canonical CLI/MCP wire contract for `tracedecay_admin_project`, the
//! internal operation first-party commands use to maintain the bookkeeping
//! the daemon keeps for a project or profile: its usage counter, registry
//! token accounting, gitignore status, automatic-fact receipts, and
//! automation scheduler reconciliation.
//!
//! Presentation-only transport keys such as `format` are removed before the
//! request body is decoded.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::FactCategoryV1;

/// Which owner's automation schedulers a reconcile covers.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationReconcileScope {
    /// The served project's scheduler, answered by the project's owner.
    Project,
    /// Every cached project scheduler of the profile, answered by the
    /// daemon's profile owner.
    Profile,
}

/// A terminal automatic-fact receipt state.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticFactReceiptStateV1 {
    Applied,
    Quarantined,
}

/// Every action is a struct variant: an internally tagged unit variant would
/// ignore keys beside its tag instead of refusing them.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdminProjectSurfaceRequestV1 {
    /// Read the project's local usage counter.
    CounterGet {},
    /// Reset the project's local usage counter to zero.
    CounterReset {},
    /// Record the project's saved-token total in the profile registry and
    /// read it back beside the other projects' total.
    StatusAccounting {},
    /// Read whether the project's store is gitignored.
    GitignoreStatus {},
    /// List terminal automatic-fact receipts.
    AutomaticFactReceiptList {
        /// Only receipts in this state (default: every state).
        state: Option<AutomaticFactReceiptStateV1>,
        limit: usize,
    },
    /// Read one automatic-fact receipt by its apply id.
    AutomaticFactReceiptView { id: String },
    /// Reconcile automation schedulers with the saved configuration.
    AutomationReconcile { scope: AutomationReconcileScope },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminProjectCounterV1 {
    pub counter: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminProjectCounterResetV1 {
    /// Always true: a reset that did not settle is a refusal.
    pub reset: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminProjectStatusAccountingV1 {
    pub tokens_saved: u64,
    /// Tokens the profile's other projects saved; null when they saved none.
    pub global_tokens_saved: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminProjectGitignoreStatusV1 {
    pub git_ignore: bool,
    /// The configuration revision the status was read at.
    pub revision_id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AutomaticFactReceiptAvailabilityV1 {
    Available,
}

/// The add-fact request an automatic apply recorded.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticFactAddRequestV1 {
    pub content: String,
    pub category: FactCategoryV1,
    pub source_label: Option<String>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub trust: f64,
    pub metadata: Value,
}

/// Automation evidence retained with the receipt.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticFactEvidenceV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<Value>,
}

/// One terminal automatic-fact apply receipt.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticFactReceiptV1 {
    pub apply_id: String,
    pub state: AutomaticFactReceiptStateV1,
    pub operation_id: String,
    pub add_fact_request: AutomaticFactAddRequestV1,
    pub evidence: AutomaticFactEvidenceV1,
    pub recorded_at_micros: i64,
    /// Present when the fact was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fact_id: Option<String>,
    /// Present when the fact was quarantined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticFactReceiptListV1 {
    pub availability: AutomaticFactReceiptAvailabilityV1,
    pub count: usize,
    pub receipts: Vec<AutomaticFactReceiptV1>,
    /// Continue after this apply id; null on the last page.
    pub next_after_apply_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticFactReceiptViewV1 {
    pub receipt: AutomaticFactReceiptV1,
}

/// What reconciling one automation scheduler did.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSchedulerReconcileOutcome {
    Started,
    RunningNotified,
    Exiting,
    Finished,
    Retiring,
    NotConfigured,
    LifecycleInactive,
    OwnerUnavailable,
}

/// A project whose scheduler is not cached reconciles when it next starts.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UncachedProjectReconcileOutcome {
    DeferredUntilProjectStartup,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationSchedulerOwnerReconcileOutcome {
    pub project_id: Option<String>,
    pub store_root: PathBuf,
    pub graph_db_path: PathBuf,
    pub scope_prefix: Option<String>,
    pub outcome: AutomationSchedulerReconcileOutcome,
}

/// The served project's scheduler reconcile.
#[derive(Clone, Debug, Deserialize, JsonSchema, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAutomationReconcileReport {
    /// Always `project`.
    pub scope: AutomationReconcileScope,
    pub outcome: AutomationSchedulerReconcileOutcome,
}

/// Every cached project scheduler of the profile, reconciled.
#[derive(Clone, Debug, Deserialize, JsonSchema, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileAutomationReconcileReport {
    /// Always `profile`.
    pub scope: AutomationReconcileScope,
    pub cached_owners: usize,
    pub outcomes: Vec<AutomationSchedulerOwnerReconcileOutcome>,
    pub uncached_projects: UncachedProjectReconcileOutcome,
}

/// One action's result, in the shape the action has always answered.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AdminProjectResultV1 {
    Counter(AdminProjectCounterV1),
    CounterReset(AdminProjectCounterResetV1),
    StatusAccounting(AdminProjectStatusAccountingV1),
    GitignoreStatus(AdminProjectGitignoreStatusV1),
    AutomaticFactReceiptList(AutomaticFactReceiptListV1),
    AutomaticFactReceiptView(Box<AutomaticFactReceiptViewV1>),
    ProjectAutomationReconcile(ProjectAutomationReconcileReport),
    ProfileAutomationReconcile(ProfileAutomationReconcileReport),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn requests_refuse_keys_outside_their_action() {
        for (request, detail) in [
            (
                json!({ "action": "counter_get", "project": "/elsewhere" }),
                "unknown field `project`",
            ),
            (
                json!({ "action": "automatic_fact_receipt_list", "state": " applied ", "limit": 5 }),
                "unknown variant ` applied `",
            ),
            (
                json!({ "action": "fact_apply", "id": "fact_1" }),
                "unknown variant `fact_apply`",
            ),
            (
                json!({ "action": "bench", "json": false, "max_nodes": 20 }),
                "unknown variant `bench`",
            ),
        ] {
            let error = serde_json::from_value::<AdminProjectSurfaceRequestV1>(request)
                .expect_err("refused request")
                .to_string();
            assert!(error.contains(detail), "{error}");
        }
        assert_eq!(
            serde_json::from_value::<AdminProjectSurfaceRequestV1>(json!({
                "action": "automatic_fact_receipt_list",
                "state": "quarantined",
                "limit": 5,
            }))
            .expect("typed request"),
            AdminProjectSurfaceRequestV1::AutomaticFactReceiptList {
                state: Some(AutomaticFactReceiptStateV1::Quarantined),
                limit: 5,
            }
        );
    }

    #[test]
    fn results_decode_back_to_their_action() {
        for (value, expected) in [
            (
                json!({ "counter": 3 }),
                AdminProjectResultV1::Counter(AdminProjectCounterV1 { counter: 3 }),
            ),
            (
                json!({ "tokens_saved": 7, "global_tokens_saved": null }),
                AdminProjectResultV1::StatusAccounting(AdminProjectStatusAccountingV1 {
                    tokens_saved: 7,
                    global_tokens_saved: None,
                }),
            ),
            (
                json!({ "scope": "project", "outcome": "started" }),
                AdminProjectResultV1::ProjectAutomationReconcile(
                    ProjectAutomationReconcileReport {
                        scope: AutomationReconcileScope::Project,
                        outcome: AutomationSchedulerReconcileOutcome::Started,
                    },
                ),
            ),
            (
                json!({
                    "scope": "profile",
                    "cached_owners": 0,
                    "outcomes": [],
                    "uncached_projects": "deferred_until_project_startup",
                }),
                AdminProjectResultV1::ProfileAutomationReconcile(
                    ProfileAutomationReconcileReport {
                        scope: AutomationReconcileScope::Profile,
                        cached_owners: 0,
                        outcomes: Vec::new(),
                        uncached_projects:
                            UncachedProjectReconcileOutcome::DeferredUntilProjectStartup,
                    },
                ),
            ),
        ] {
            let decoded: AdminProjectResultV1 =
                serde_json::from_value(value.clone()).expect("typed result");
            assert_eq!(decoded, expected);
            assert_eq!(serde_json::to_value(&decoded).expect("result JSON"), value);
        }
    }
}
