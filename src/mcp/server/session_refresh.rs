//! Daemon-backed session-refresh service exposed to the MCP tool layer:
//! scope authorization, scheduler wake-up, handle bookkeeping, and view
//! mapping for the `SessionRefreshServicePort` implementation.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::PoisonError;

use sha2::{Digest, Sha256};
use tracedecay_application::RequestContext;
use tracedecay_domain::ProjectId;

use crate::application::session::{
    AuthorizationGrantId, SessionAuthorizationError, SessionAuthorizationGrant,
    SessionRefreshConfiguration, SessionRefreshHandle, SessionRefreshOutcome,
    SessionRefreshSchedulerError, SessionRefreshSchedulerPort, SessionRefreshService,
    SessionRequestBinding, SessionScopeAuthorizationRequest, SessionScopeAuthorizer,
};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::{
    SessionRefreshCommand, SessionRefreshCoverageView, SessionRefreshFrontierView,
    SessionRefreshProgressView, SessionRefreshReceiptView, SessionRefreshServiceOutcome,
    SessionRefreshServicePort, utc_micros_value,
};
use crate::store::GlobalDbSessionTemporalStore;

const SESSION_REFRESH_PROJECTOR_VERSION: &str = "session-temporal-projector.v1";
const SESSION_REFRESH_CONFIG_VERSION: &str = "session-refresh-config.v1";
const MAX_SESSION_REFRESH_HANDLES: usize = 1_024;

struct DaemonSessionRefreshAuthorizer<'a> {
    expected_project_id: Option<&'a str>,
}

impl SessionScopeAuthorizer for DaemonSessionRefreshAuthorizer<'_> {
    fn authorize(
        &self,
        context: &RequestContext,
        binding: &SessionRequestBinding,
        request: &SessionScopeAuthorizationRequest,
    ) -> std::result::Result<SessionAuthorizationGrant, SessionAuthorizationError> {
        if request.identity().project_id().map(ProjectId::as_str) != self.expected_project_id {
            return Err(SessionAuthorizationError::WrongScope);
        }
        SessionAuthorizationGrant::issue(
            AuthorizationGrantId::new("grant.mcp.session-refresh")?,
            1,
            context,
            binding,
            request,
        )
    }
}

#[derive(Clone)]
struct DaemonSessionRefreshWake(
    crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
);

impl SessionRefreshSchedulerPort for DaemonSessionRefreshWake {
    fn wake(&self) -> std::result::Result<(), SessionRefreshSchedulerError> {
        self.0.wake();
        Ok(())
    }
}

pub(crate) struct DaemonSessionRefreshService {
    database: Arc<RegisteredGlobalDb>,
    wake: DaemonSessionRefreshWake,
    expected_project_id: Option<String>,
    handles: std::sync::Mutex<HashMap<String, SessionRefreshHandle>>,
}

enum SessionRefreshHandleLookup {
    Found(SessionRefreshHandle),
    Stale,
    NotFound,
}

impl DaemonSessionRefreshService {
    pub(crate) fn new(
        database: Arc<RegisteredGlobalDb>,
        wake: crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
        expected_project_id: Option<String>,
    ) -> Self {
        Self {
            database,
            wake: DaemonSessionRefreshWake(wake),
            expected_project_id,
            handles: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn service(
        &self,
    ) -> Option<
        SessionRefreshService<
            DaemonSessionRefreshAuthorizer<'_>,
            GlobalDbSessionTemporalStore<'_>,
            &DaemonSessionRefreshWake,
        >,
    > {
        Some(SessionRefreshService::new(
            DaemonSessionRefreshAuthorizer {
                expected_project_id: self.expected_project_id.as_deref(),
            },
            GlobalDbSessionTemporalStore::new(self.database.as_ref()),
            &self.wake,
            SessionRefreshConfiguration::new(
                SESSION_REFRESH_PROJECTOR_VERSION,
                SESSION_REFRESH_CONFIG_VERSION,
            )
            .ok()?,
        ))
    }

    fn handle(&self, token: &str) -> SessionRefreshHandleLookup {
        if !is_session_refresh_handle_token(token) {
            return missing_session_refresh_handle_lookup(token);
        }
        match self
            .handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(token)
            .cloned()
        {
            Some(handle) => SessionRefreshHandleLookup::Found(handle),
            None => missing_session_refresh_handle_lookup(token),
        }
    }

    fn remember(&self, handle: &SessionRefreshHandle) -> String {
        let mut digest = Sha256::new();
        digest.update(b"tracedecay.mcp.session-refresh.handle.v1\0");
        digest.update(handle.operation_id().as_str().as_bytes());
        digest.update(handle.join_digest().as_bytes());
        digest.update(handle.caller_idempotency_digest().as_bytes());
        let token = format!("srh_{}", hex::encode(digest.finalize()));
        let mut handles = self.handles.lock().unwrap_or_else(PoisonError::into_inner);
        if handles.len() >= MAX_SESSION_REFRESH_HANDLES
            && !handles.contains_key(&token)
            && let Some(evicted) = handles.keys().next().cloned()
        {
            handles.remove(&evicted);
        }
        handles.insert(token.clone(), handle.clone());
        token
    }

    async fn execute_command(
        &self,
        command: SessionRefreshCommand,
    ) -> SessionRefreshServiceOutcome {
        let Some(service) = self.service() else {
            return SessionRefreshServiceOutcome::Unavailable;
        };
        let outcome = match command.action {
            crate::mcp::tools::SessionRefreshAction::Begin => {
                service
                    .begin_or_join(&command.context, &command.binding, command.target)
                    .await
            }
            crate::mcp::tools::SessionRefreshAction::Status => {
                let handle = match command.handle.as_deref().map(|token| self.handle(token)) {
                    Some(SessionRefreshHandleLookup::Found(handle)) => handle,
                    Some(SessionRefreshHandleLookup::Stale) => {
                        return SessionRefreshServiceOutcome::Stale;
                    }
                    Some(SessionRefreshHandleLookup::NotFound) | None => {
                        return SessionRefreshServiceOutcome::NotFound;
                    }
                };
                service
                    .status(&command.context, &command.binding, &handle)
                    .await
            }
            crate::mcp::tools::SessionRefreshAction::Cancel => {
                let handle = match command.handle.as_deref().map(|token| self.handle(token)) {
                    Some(SessionRefreshHandleLookup::Found(handle)) => handle,
                    Some(SessionRefreshHandleLookup::Stale) => {
                        return SessionRefreshServiceOutcome::Stale;
                    }
                    Some(SessionRefreshHandleLookup::NotFound) | None => {
                        return SessionRefreshServiceOutcome::NotFound;
                    }
                };
                service
                    .cancel(&command.context, &command.binding, &handle)
                    .await
            }
        };
        self.public_outcome(outcome)
    }

    fn public_outcome(&self, outcome: SessionRefreshOutcome) -> SessionRefreshServiceOutcome {
        match outcome {
            SessionRefreshOutcome::Started(handle) => {
                let token = self.remember(&handle);
                SessionRefreshServiceOutcome::Started {
                    operation_id: handle.operation_id().as_str().to_string(),
                    handle: token,
                    accepted_at: utc_micros_value(handle.accepted_at()),
                }
            }
            SessionRefreshOutcome::Joined(handle) => {
                let token = self.remember(&handle);
                SessionRefreshServiceOutcome::Joined {
                    operation_id: handle.operation_id().as_str().to_string(),
                    handle: token,
                    accepted_at: utc_micros_value(handle.accepted_at()),
                }
            }
            SessionRefreshOutcome::Busy => SessionRefreshServiceOutcome::Busy,
            SessionRefreshOutcome::Running(progress) => {
                SessionRefreshServiceOutcome::Running(progress.as_ref().map(refresh_progress_view))
            }
            SessionRefreshOutcome::Complete(receipt) => {
                SessionRefreshServiceOutcome::Complete(refresh_receipt_view(&receipt))
            }
            SessionRefreshOutcome::Failed(receipt) => {
                SessionRefreshServiceOutcome::Failed(refresh_receipt_view(&receipt))
            }
            SessionRefreshOutcome::Cancelled(receipt) => {
                SessionRefreshServiceOutcome::Cancelled(refresh_receipt_view(&receipt))
            }
            SessionRefreshOutcome::Denied => SessionRefreshServiceOutcome::Denied,
            SessionRefreshOutcome::WrongScope => SessionRefreshServiceOutcome::WrongScope,
            SessionRefreshOutcome::Stale => SessionRefreshServiceOutcome::Stale,
            SessionRefreshOutcome::NotFound => SessionRefreshServiceOutcome::NotFound,
            SessionRefreshOutcome::Aborted => SessionRefreshServiceOutcome::Aborted,
            SessionRefreshOutcome::DeadlineExceeded => {
                SessionRefreshServiceOutcome::DeadlineExceeded
            }
            SessionRefreshOutcome::Unavailable => SessionRefreshServiceOutcome::Unavailable,
        }
    }
}

fn is_session_refresh_handle_token(token: &str) -> bool {
    token.strip_prefix("srh_").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn missing_session_refresh_handle_lookup(token: &str) -> SessionRefreshHandleLookup {
    if is_session_refresh_handle_token(token) {
        SessionRefreshHandleLookup::Stale
    } else {
        SessionRefreshHandleLookup::NotFound
    }
}

#[cfg(test)]
#[test]
fn session_refresh_handle_tokens_are_closed_and_non_leaking() {
    let valid = format!("srh_{}", "a".repeat(64));
    assert!(is_session_refresh_handle_token(&valid));
    assert!(!is_session_refresh_handle_token("refresh-handle"));
    assert!(!is_session_refresh_handle_token(&format!(
        "srh_{}",
        "z".repeat(64)
    )));
    assert!(!is_session_refresh_handle_token(&format!(
        "srh_{}",
        "a".repeat(63)
    )));
    assert!(matches!(
        missing_session_refresh_handle_lookup(&valid),
        SessionRefreshHandleLookup::Stale
    ));
    assert!(matches!(
        missing_session_refresh_handle_lookup("refresh-handle"),
        SessionRefreshHandleLookup::NotFound
    ));
}

impl SessionRefreshServicePort for DaemonSessionRefreshService {
    fn execute<'a>(
        &'a self,
        command: SessionRefreshCommand,
    ) -> Pin<Box<dyn Future<Output = SessionRefreshServiceOutcome> + Send + 'a>> {
        Box::pin(async move { self.execute_command(command).await })
    }
}

fn refresh_frontier_view(
    frontier: tracedecay_store::SessionRefreshFrontierV1,
) -> SessionRefreshFrontierView {
    SessionRefreshFrontierView {
        observed_through: frontier.observed_through(),
        committed_through: frontier.committed_through(),
    }
}

fn refresh_coverage_view(
    coverage: &tracedecay_domain::TemporalCoverageCountsV1,
) -> SessionRefreshCoverageView {
    SessionRefreshCoverageView {
        visible: coverage.visible,
        hidden: coverage.hidden,
        unknown: coverage.unknown,
        redacted: coverage.redacted,
    }
}

fn refresh_progress_view(
    progress: &tracedecay_store::SessionRefreshProgressV1,
) -> SessionRefreshProgressView {
    SessionRefreshProgressView {
        operation_id: progress.operation_id().as_str().to_string(),
        session_id: progress.session_id().as_str().to_string(),
        frontier: refresh_frontier_view(progress.frontier()),
        coverage: refresh_coverage_view(progress.coverage()),
        source_coverage: progress
            .source_coverage()
            .map(|receipt| receipt.sources().to_vec())
            .unwrap_or_default(),
        committed_batches: progress.committed_batches(),
        committed_records: progress.committed_records(),
        updated_at: utc_micros_value(progress.updated_at()),
    }
}

fn refresh_receipt_view(
    receipt: &tracedecay_store::SessionRefreshReceiptV1,
) -> SessionRefreshReceiptView {
    SessionRefreshReceiptView {
        operation_id: receipt.operation_id().as_str().to_string(),
        session_id: receipt.session_id().as_str().to_string(),
        frontier: refresh_frontier_view(receipt.frontier()),
        coverage: refresh_coverage_view(receipt.coverage()),
        source_coverage: receipt
            .source_coverage()
            .map(|coverage| coverage.sources().to_vec())
            .unwrap_or_default(),
        state: match receipt.state() {
            tracedecay_store::SessionRefreshTerminalStateV1::Complete => "complete",
            tracedecay_store::SessionRefreshTerminalStateV1::Failed => "failed",
            tracedecay_store::SessionRefreshTerminalStateV1::Cancelled => "cancelled",
        }
        .to_string(),
        failure_code: receipt.failure_code().map(|code| code.as_str().to_string()),
        terminal_at: utc_micros_value(receipt.terminal_at()),
    }
}
