//! Transport-neutral session refresh service contract retained for the
//! application-owned session surface.

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use tracedecay_application::RequestContext;
use tracedecay_domain::{SessionSourceCoverageV1, UtcMicros};

use super::refresh::SessionRefreshTarget;
use super::types::SessionRequestBinding;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRefreshAction {
    Begin,
    Status,
    Cancel,
}

#[derive(Clone, Debug)]
pub struct SessionRefreshCommand {
    pub action: SessionRefreshAction,
    pub context: RequestContext,
    pub binding: SessionRequestBinding,
    pub target: SessionRefreshTarget,
    pub handle: Option<String>,
}

pub type SessionRefreshServiceFuture<'a> =
    Pin<Box<dyn Future<Output = SessionRefreshServiceOutcome> + Send + 'a>>;

pub trait SessionRefreshServicePort: Send + Sync {
    fn execute(&self, command: SessionRefreshCommand) -> SessionRefreshServiceFuture<'_>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionRefreshProgressView {
    pub operation_id: String,
    pub session_id: String,
    pub frontier: SessionRefreshFrontierView,
    pub coverage: SessionRefreshCoverageView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_coverage: Vec<SessionSourceCoverageV1>,
    pub committed_batches: u64,
    pub committed_records: u64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionRefreshReceiptView {
    pub operation_id: String,
    pub session_id: String,
    pub frontier: SessionRefreshFrontierView,
    pub coverage: SessionRefreshCoverageView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_coverage: Vec<SessionSourceCoverageV1>,
    pub state: String,
    pub failure_code: Option<String>,
    pub terminal_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SessionRefreshFrontierView {
    pub observed_through: u64,
    pub committed_through: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SessionRefreshCoverageView {
    pub visible: u64,
    pub hidden: u64,
    pub unknown: u64,
    pub redacted: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionRefreshServiceOutcome {
    Started {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    StartedReconciliationRequired {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    Joined {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    JoinedReconciliationRequired {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    Busy,
    Running(Option<SessionRefreshProgressView>),
    Complete(SessionRefreshReceiptView),
    Failed(SessionRefreshReceiptView),
    Cancelled(SessionRefreshReceiptView),
    CancelledReconciliationRequired(SessionRefreshReceiptView),
    Denied,
    WrongScope,
    Stale,
    NotFound,
    Aborted,
    DeadlineExceeded,
    Unavailable,
}

pub fn utc_micros_value(value: UtcMicros) -> i64 {
    value.0
}
