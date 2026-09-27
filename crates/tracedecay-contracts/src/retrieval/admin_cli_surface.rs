//! Canonical CLI/MCP wire contract for `tracedecay_admin_cli`, the profile
//! maintenance actions first-party CLI commands ask the daemon for.
//!
//! The request is tagged by `action`. Each result body is the JSON the action
//! has always answered, so the result is untagged: every body has its own
//! exact key set, which is what decodes it. Rows owned by a store crate the
//! contracts cannot name are carried exactly as that store serializes them.
//!
//! Presentation-only transport keys such as `format` are removed before the
//! request body is decoded.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::project_registry::{ProjectRegistrySummary, ProjectRepoGroup, PublicCodeProject};
use crate::session_sync::{
    SessionSyncAdmissionReceiptV1, SessionSyncOutcomeV1, SessionSyncSourceCoverageV1,
    SessionSyncSourceFrontierV1, SessionSyncStatsV1,
};
use crate::{IdempotencyKey, OperationTermination, RequestId};
use tracedecay_domain::UtcMicros;

const fn default_storage_report_page_limit() -> usize {
    8
}

/// What a cost or analytics action reads: the served project's ledgers and
/// sessions, or the whole profile's.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminCliScopeV1 {
    Project,
    Profile,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdminCliSurfaceRequestV1 {
    /// Savings and provider-usage cost over `range` (`today`, `7d`, ...).
    CostSummary {
        range: String,
        scope: AdminCliScopeV1,
    },
    /// Import every host's transcripts into the project's session store.
    SessionsImport {},
    /// Converge project sessions with Git history.
    SessionsGitSync {
        since: i64,
        limit_sessions: usize,
        dry_run: bool,
    },
    SessionsSyncStatus {
        idempotency_key: String,
    },
    SessionsSyncCancel {
        idempotency_key: String,
    },
    SessionsUnfinished {
        limit: usize,
    },
    /// Import hook analytics JSONL into the accounting ledger.
    AnalyticsSync {
        scope: AdminCliScopeV1,
    },
    AnalyticsDiagnostics {
        scope: AdminCliScopeV1,
        all: bool,
        no_sync: bool,
    },
    /// Record the served project's saved-token total.
    RegistryUpdate {
        tokens: u64,
    },
    RegistryList {
        limit: usize,
        query: Option<String>,
        /// Checkout to mark active when the caller's connection names no
        /// project.
        project_arg: Option<PathBuf>,
    },
    RegistryContext {
        /// Project path or alias; the caller's project when omitted.
        project_arg: Option<PathBuf>,
    },
    RegistryEmpty {},
    RegistryProjectTokens {
        project_args: Vec<PathBuf>,
    },
    /// Plan, or with `apply` execute, collection of registry rows whose
    /// checkout is gone.
    RegistryGc {
        prefix: Option<String>,
        apply: bool,
    },
    /// One project's storage (`project_id` with `project_root`), or one page
    /// of the profile's.
    StorageReport {
        project_id: Option<String>,
        project_root: Option<PathBuf>,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default = "default_storage_report_page_limit")]
        limit: usize,
    },
    GainQuery {
        project_arg: Option<PathBuf>,
        since: i64,
        history: bool,
    },
}

/// One action's answer.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum AdminCliResultV1 {
    CostSummary(AdminCliCostSummaryV1),
    SessionSync(AdminCliSessionSyncV1),
    SessionsUnfinished(AdminCliUnfinishedSessionsV1),
    AnalyticsSync(AdminCliAnalyticsImportV1),
    RegistryUpdate(AdminCliRegistryUpdateV1),
    RegistryList(AdminCliRegistryListV1),
    RegistryContext(AdminCliRegistryContextV1),
    RegistryEmpty(AdminCliRegistryEmptyV1),
    RegistryProjectTokens(AdminCliProjectTokensV1),
    RegistryGc(AdminCliRegistryGcV1),
    StorageReport(AdminCliStorageReportV1),
    GainHistory(AdminCliGainHistoryV1),
    GainTotal(AdminCliGainTotalV1),
    /// The diagnostics report is the analytics bridge's open document; it
    /// decodes last so it never claims another action's body.
    AnalyticsDiagnostics(Value),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliCostSummaryV1 {
    pub range: String,
    pub summary: AdminCliCostTotalsV1,
    pub today: AdminCliCostTodayV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliCostTotalsV1 {
    /// The provider-usage cost summary as the session memory prices it.
    pub provider_usage: Value,
    pub tokens_saved: u64,
    /// Saved over saved-plus-consumed; null when consumption is unmeasured.
    pub efficiency_ratio: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliCostTodayV1 {
    pub provider_usage: Value,
}

/// The daemon session-sync owner's answer.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdminCliSessionSyncV1 {
    Accepted {
        operation_id: RequestId,
        idempotency_key: IdempotencyKey,
        accepted_at: UtcMicros,
    },
    /// An identical sync was already running; this call joined it.
    Joined {
        operation_id: RequestId,
        idempotency_key: IdempotencyKey,
        accepted_at: UtcMicros,
    },
    Complete {
        operation_id: RequestId,
        idempotency_key: IdempotencyKey,
        coalesced_primary: Option<IdempotencyKey>,
        termination: OperationTermination,
        stats: SessionSyncStatsV1,
        coverage: Vec<SessionSyncSourceCoverageV1>,
        source_frontiers: Vec<SessionSyncSourceFrontierV1>,
        failure_codes: Vec<String>,
        completed_at: UtcMicros,
    },
    Cancelled,
    DeadlineExceeded,
    WrongScope,
    Unavailable {
        reason_code: String,
    },
}

impl From<SessionSyncOutcomeV1> for AdminCliSessionSyncV1 {
    fn from(outcome: SessionSyncOutcomeV1) -> Self {
        match outcome {
            SessionSyncOutcomeV1::Accepted(SessionSyncAdmissionReceiptV1 {
                operation_id,
                idempotency_key,
                accepted_at,
            }) => Self::Accepted {
                operation_id,
                idempotency_key,
                accepted_at,
            },
            SessionSyncOutcomeV1::Joined(SessionSyncAdmissionReceiptV1 {
                operation_id,
                idempotency_key,
                accepted_at,
            }) => Self::Joined {
                operation_id,
                idempotency_key,
                accepted_at,
            },
            SessionSyncOutcomeV1::Complete(receipt) => Self::Complete {
                operation_id: receipt.admission.operation_id,
                idempotency_key: receipt.admission.idempotency_key,
                coalesced_primary: receipt.coalesced_primary,
                termination: receipt.termination,
                stats: receipt.stats,
                coverage: receipt.coverage,
                source_frontiers: receipt.source_frontiers,
                failure_codes: receipt.failure_codes,
                completed_at: receipt.completed_at,
            },
            SessionSyncOutcomeV1::Cancelled => Self::Cancelled,
            SessionSyncOutcomeV1::DeadlineExceeded => Self::DeadlineExceeded,
            SessionSyncOutcomeV1::WrongScope => Self::WrongScope,
            SessionSyncOutcomeV1::Unavailable { reason_code } => Self::Unavailable {
                reason_code: reason_code.to_owned(),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliUnfinishedSessionsV1 {
    /// Workflow state rows exactly as the session store serializes them.
    pub items: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliAnalyticsImportV1 {
    pub imported: u64,
    /// One row per hook JSONL source: `path`, `imported`, `skipped`, `error`.
    pub sources: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliRegistryUpdateV1 {
    /// The total before this write; null when the ledger could not be read.
    pub previous: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_error: Option<String>,
    pub current: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdminCliRegistryListV1 {
    Ok {
        limit: usize,
        query: Option<String>,
        truncated: bool,
        summary: ProjectRegistrySummary,
        project_tree: Vec<ProjectRepoGroup>,
        projects: Vec<PublicCodeProject>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdminCliRegistryContextV1 {
    Ok {
        profile_id: String,
        project: Box<PublicCodeProject>,
        /// Alias rows exactly as the registry serializes them.
        aliases: Vec<Value>,
        /// Store rows exactly as the registry serializes them.
        stores: Vec<Value>,
    },
    /// The call named no project and its connection has none.
    Invalid {
        project: (),
    },
    NotFound {
        project: (),
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliRegistryEmptyV1 {
    pub empty: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliProjectTokensV1 {
    pub projects: Vec<AdminCliProjectTokenTotalV1>,
}

/// A project whose ledger could not be read reports a null total and the
/// reason, never a measured zero.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliProjectTokenTotalV1 {
    pub project: PathBuf,
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The registry collection plan, with its deletion counters filled when it
/// was applied.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliRegistryGcV1 {
    pub apply: bool,
    pub prefix: Option<String>,
    pub candidate_count: usize,
    pub metadata_candidate_count: usize,
    pub code_project_candidate_count: usize,
    pub storage_project_candidate_count: usize,
    pub protected_code_project_count: usize,
    pub deleted_count: usize,
    pub deleted_code_project_count: usize,
    pub deleted_storage_project_count: usize,
    pub candidate_paths: Vec<String>,
    /// Registry project rows exactly as the registry serializes them.
    pub candidates: Vec<Value>,
    pub protected_code_projects: Vec<Value>,
    pub storage_project_candidates: Vec<PathBuf>,
}

/// One storage report page, or one project's report.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliStorageReportV1 {
    pub profile_root: String,
    /// Report rows exactly as the retention report serializes them.
    pub stores: Vec<Value>,
    pub code_generation_retention: Vec<Value>,
    pub code_generation_retention_availability: Vec<Value>,
    pub unregistered_dir_count: usize,
    pub unregistered_bytes: u64,
    pub global_db_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_profile_size: Option<Value>,
    pub coverage: Value,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliGainHistoryV1 {
    pub history: Vec<AdminCliGainDayV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliGainDayV1 {
    /// Start-of-day epoch seconds (UTC).
    pub day: i64,
    pub saved_tokens: u64,
    pub calls: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminCliGainTotalV1 {
    pub saved_tokens: u64,
    pub calls: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_tool_catalog::ApplicationSurfaceOperation;

    use super::*;
    use crate::graph_tool::GraphToolResultV1;

    fn decoded(body: Value) -> AdminCliResultV1 {
        match GraphToolResultV1::from_result_value(ApplicationSurfaceOperation::AdminCli, body)
            .unwrap()
        {
            GraphToolResultV1::AdminCli(result) => *result,
            other => panic!("admin_cli decoded as {other:?}"),
        }
    }

    /// Each action's body decodes to that action's result, never to another
    /// action whose keys it shares, and serializes back byte-for-byte.
    #[test]
    fn every_action_body_decodes_to_its_own_result() {
        let bodies = [
            (
                "cost_summary",
                json!({
                    "range": "7d",
                    "summary": {"provider_usage": {"coverage": "unavailable"}, "tokens_saved": 5, "efficiency_ratio": null},
                    "today": {"provider_usage": {"coverage": "unavailable"}},
                }),
            ),
            (
                "session_sync",
                json!({"status": "unavailable", "reason_code": "session_sync_authority_unavailable"}),
            ),
            (
                "session_sync",
                json!({"status": "accepted", "operation_id": "session-sync.a", "idempotency_key": "session-sync.a", "accepted_at": 7}),
            ),
            ("sessions_unfinished", json!({"items": []})),
            ("analytics_sync", json!({"imported": 0, "sources": []})),
            ("registry_update", json!({"previous": 1, "current": 2})),
            (
                "registry_update",
                json!({"previous": null, "previous_error": "ledger locked", "current": 2}),
            ),
            (
                "registry_list",
                json!({
                    "status": "ok", "limit": 10, "query": null, "truncated": false,
                    "summary": {"project_count": 0, "repo_count": 0, "truncated": false},
                    "project_tree": [], "projects": [],
                }),
            ),
            (
                "registry_context",
                json!({"status": "not_found", "project": null}),
            ),
            (
                "registry_context",
                json!({"status": "invalid", "project": null}),
            ),
            ("registry_empty", json!({"empty": true})),
            (
                "registry_project_tokens",
                json!({"projects": [{"project": "/a", "tokens": 3}, {"project": "/b", "tokens": null, "error": "locked"}]}),
            ),
            (
                "registry_gc",
                json!({
                    "apply": false, "prefix": null, "candidate_count": 0,
                    "metadata_candidate_count": 0, "code_project_candidate_count": 0,
                    "storage_project_candidate_count": 0, "protected_code_project_count": 0,
                    "deleted_count": 0, "deleted_code_project_count": 0,
                    "deleted_storage_project_count": 0, "candidate_paths": [], "candidates": [],
                    "protected_code_projects": [], "storage_project_candidates": [],
                }),
            ),
            (
                "storage_report",
                json!({
                    "profile_root": "/profile", "stores": [], "code_generation_retention": [],
                    "code_generation_retention_availability": [], "unregistered_dir_count": 0,
                    "unregistered_bytes": 0, "global_db_bytes": 0,
                    "coverage": {"state": "complete", "next_cursor": null},
                }),
            ),
            (
                "gain_history",
                json!({"history": [{"day": 0, "saved_tokens": 1, "calls": 2}]}),
            ),
            ("gain_total", json!({"saved_tokens": 1, "calls": 2})),
            (
                "analytics_diagnostics",
                json!({"available": false, "source": "x"}),
            ),
        ];
        for (action, body) in bodies {
            let result = decoded(body.clone());
            let variant = match &result {
                AdminCliResultV1::CostSummary(_) => "cost_summary",
                AdminCliResultV1::SessionSync(_) => "session_sync",
                AdminCliResultV1::SessionsUnfinished(_) => "sessions_unfinished",
                AdminCliResultV1::AnalyticsSync(_) => "analytics_sync",
                AdminCliResultV1::RegistryUpdate(_) => "registry_update",
                AdminCliResultV1::RegistryList(_) => "registry_list",
                AdminCliResultV1::RegistryContext(_) => "registry_context",
                AdminCliResultV1::RegistryEmpty(_) => "registry_empty",
                AdminCliResultV1::RegistryProjectTokens(_) => "registry_project_tokens",
                AdminCliResultV1::RegistryGc(_) => "registry_gc",
                AdminCliResultV1::StorageReport(_) => "storage_report",
                AdminCliResultV1::GainHistory(_) => "gain_history",
                AdminCliResultV1::GainTotal(_) => "gain_total",
                AdminCliResultV1::AnalyticsDiagnostics(_) => "analytics_diagnostics",
            };
            assert_eq!(variant, action, "{body}");
            assert_eq!(serde_json::to_value(&result).unwrap(), body);
        }
    }

    #[test]
    fn session_sync_outcomes_keep_their_established_bodies() {
        assert_eq!(
            serde_json::to_value(AdminCliSessionSyncV1::from(
                SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_authority_unavailable",
                }
            ))
            .unwrap(),
            json!({"status": "unavailable", "reason_code": "session_sync_authority_unavailable"})
        );
        assert_eq!(
            serde_json::to_value(AdminCliSessionSyncV1::from(
                SessionSyncOutcomeV1::DeadlineExceeded
            ))
            .unwrap(),
            json!({"status": "deadline_exceeded"})
        );
    }

    #[test]
    fn requests_decode_their_actions_and_refuse_what_they_do_not_name() {
        assert_eq!(
            serde_json::from_value::<AdminCliSurfaceRequestV1>(
                json!({"action": "sessions_import"})
            )
            .unwrap(),
            AdminCliSurfaceRequestV1::SessionsImport {}
        );
        assert_eq!(
            serde_json::from_value::<AdminCliSurfaceRequestV1>(
                json!({"action": "storage_report", "project_id": null, "project_root": null})
            )
            .unwrap(),
            AdminCliSurfaceRequestV1::StorageReport {
                project_id: None,
                project_root: None,
                cursor: None,
                limit: 8,
            }
        );
        let refused = |body: Value| {
            serde_json::from_value::<AdminCliSurfaceRequestV1>(body)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            refused(json!({"action": "registry_empty", "project_root": "/elsewhere"})),
            "unknown field `project_root`, there are no fields"
        );
        assert_eq!(
            refused(json!({"action": "cost_summary", "range": "7d"})),
            "missing field `scope`"
        );
        assert_eq!(
            refused(json!({"action": "vacuum"})),
            "unknown variant `vacuum`, expected one of `cost_summary`, `sessions_import`, `sessions_git_sync`, `sessions_sync_status`, `sessions_sync_cancel`, `sessions_unfinished`, `analytics_sync`, `analytics_diagnostics`, `registry_update`, `registry_list`, `registry_context`, `registry_empty`, `registry_project_tokens`, `registry_gc`, `storage_report`, `gain_query`"
        );
    }
}
