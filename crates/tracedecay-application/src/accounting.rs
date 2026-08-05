//! Transport-neutral accounting and analytics authority contracts.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::{ProjectId, UserProfileId};

use crate::{CancellationSignal, Deadline, OperationReceipt, OperationTermination, RequestId};

/// Exact durable identity served by one accounting authority.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountingScopeV1 {
    pub profile_id: UserProfileId,
    pub project: Option<AccountingProjectScopeV1>,
}

/// Exact registered project identity and accounting key.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountingProjectScopeV1 {
    pub project_id: ProjectId,
    pub canonical_project_key: String,
}

/// Closed accounting operations owned by the daemon authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountingOperationV1 {
    CostSummary { range: String },
    AnalyticsSync,
    AnalyticsDiagnostics { all_projects: bool, no_sync: bool },
    StatusAccounting,
}

impl AccountingOperationV1 {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::CostSummary { .. } => "cost_summary",
            Self::AnalyticsSync => "analytics_sync",
            Self::AnalyticsDiagnostics { .. } => "analytics_diagnostics",
            Self::StatusAccounting => "status_accounting",
        }
    }
}

/// Live admission controls forwarded unchanged by transport adapters.
#[derive(Clone, Debug)]
pub struct AccountingInvocationV1 {
    pub request_id: RequestId,
    pub deadline: Deadline,
    pub cancellation: CancellationSignal,
    pub operation: AccountingOperationV1,
}

/// Source families consulted by accounting operations.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingSourceV1 {
    AccountingLedger,
    TurnTranscript,
    HookAnalytics,
    ProjectSessions,
    ProfileSessions,
}

/// Truthful coverage for one consulted source family.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountingSourceCoverageV1 {
    pub source: AccountingSourceV1,
    pub state: AccountingSourceStateV1,
    pub reason: Option<String>,
}

/// A source is never represented as a successful empty set when unavailable.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingSourceStateV1 {
    Complete,
    Partial,
    Unavailable,
    NotRequested,
}

/// Storage property that makes repeated ingestion safe.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingIngestGuaranteeV1 {
    NotApplicable,
    DurableSourceCursor,
}

/// Receipt and coverage returned with every served accounting payload.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountingResponseV1 {
    pub scope: AccountingScopeV1,
    pub payload: Value,
    pub coverage: Vec<AccountingSourceCoverageV1>,
    pub ingest_guarantee: AccountingIngestGuaranteeV1,
    pub receipt: OperationReceipt,
}

/// Typed terminal state. Missing authorities and partial sources never collapse
/// into a successful empty payload.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "value")]
pub enum AccountingOutcomeV1 {
    Complete(AccountingResponseV1),
    Partial(AccountingResponseV1),
    Cancelled {
        scope: AccountingScopeV1,
        receipt: OperationReceipt,
    },
    TimedOut {
        scope: AccountingScopeV1,
        receipt: OperationReceipt,
    },
    Unavailable {
        scope: AccountingScopeV1,
        reason: String,
        coverage: Vec<AccountingSourceCoverageV1>,
        receipt: OperationReceipt,
    },
}

impl AccountingOutcomeV1 {
    pub const fn termination(&self) -> OperationTermination {
        match self {
            Self::Complete(_) => OperationTermination::Completed,
            Self::Partial(_) => OperationTermination::Partial,
            Self::Cancelled { .. } => OperationTermination::Cancelled,
            Self::TimedOut { .. } => OperationTermination::TimedOut,
            Self::Unavailable { .. } => OperationTermination::Unavailable,
        }
    }
}

pub type AccountingFuture<'a> = Pin<Box<dyn Future<Output = AccountingOutcomeV1> + Send + 'a>>;

/// Daemon-owned accounting authority. Adapters may parse requests and render
/// outcomes but never receive its databases or source paths.
pub trait AccountingAuthorityPort: Send + Sync {
    fn invoke<'a>(&'a self, invocation: AccountingInvocationV1) -> AccountingFuture<'a>;
}
