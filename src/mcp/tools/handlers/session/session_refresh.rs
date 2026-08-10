//! Transport-neutral session refresh service contract retained for the
//! application-owned session surface.

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;
use tracedecay_application::RequestContext;
use tracedecay_domain::{SessionSourceCoverageV1, UtcMicros};
use tracedecay_usecases::session::{SessionRefreshTarget, SessionRequestBinding};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionRefreshAction {
    Begin,
    Status,
    Cancel,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionRefreshCommand {
    pub(crate) action: SessionRefreshAction,
    pub(crate) context: RequestContext,
    pub(crate) binding: SessionRequestBinding,
    pub(crate) target: SessionRefreshTarget,
    pub(crate) handle: Option<String>,
}

pub(crate) type SessionRefreshServiceFuture<'a> =
    Pin<Box<dyn Future<Output = SessionRefreshServiceOutcome> + Send + 'a>>;

pub(crate) trait SessionRefreshServicePort: Send + Sync {
    fn execute(&self, command: SessionRefreshCommand) -> SessionRefreshServiceFuture<'_>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshProgressView {
    pub(crate) operation_id: String,
    pub(crate) session_id: String,
    pub(crate) frontier: SessionRefreshFrontierView,
    pub(crate) coverage: SessionRefreshCoverageView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) source_coverage: Vec<SessionSourceCoverageV1>,
    pub(crate) committed_batches: u64,
    pub(crate) committed_records: u64,
    pub(crate) updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshReceiptView {
    pub(crate) operation_id: String,
    pub(crate) session_id: String,
    pub(crate) frontier: SessionRefreshFrontierView,
    pub(crate) coverage: SessionRefreshCoverageView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) source_coverage: Vec<SessionSourceCoverageV1>,
    pub(crate) state: String,
    pub(crate) failure_code: Option<String>,
    pub(crate) terminal_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshFrontierView {
    pub(crate) observed_through: u64,
    pub(crate) committed_through: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshCoverageView {
    pub(crate) visible: u64,
    pub(crate) hidden: u64,
    pub(crate) unknown: u64,
    pub(crate) redacted: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionRefreshServiceOutcome {
    Started {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    Joined {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    Busy,
    Running(Option<SessionRefreshProgressView>),
    Complete(SessionRefreshReceiptView),
    Failed(SessionRefreshReceiptView),
    Cancelled(SessionRefreshReceiptView),
    Denied,
    WrongScope,
    Stale,
    NotFound,
    Aborted,
    DeadlineExceeded,
    Unavailable,
}

pub(crate) fn utc_micros_value(value: UtcMicros) -> i64 {
    value.0
}
