use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::{CancellationSignal, Deadline, RequestId, ResolvedScope, now_micros};
use tracedecay_domain::UtcMicros;

use super::verified_query_test_support::{
    ImmediateAdmission, ImmediateProjection, admit_context, assert_route, fixture_scope,
    fixture_store, graph_operation,
};
use super::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionPort, CodeGraphReadAdmissionRequest,
    CodeGraphReadRequest, CodeGraphSourceAuthorityPort, CodeGraphSourceBindRequest,
    VerifiedCodeGraphRead, VerifiedGraphQueryRequest, open_verified_graph_query,
};

const SCOPE_TAG: &str = "verified-query-deadline";

struct DelayedAdmission {
    scope: ResolvedScope,
    delay: Duration,
}

impl CodeGraphReadAdmissionPort for DelayedAdmission {
    fn admit<'a>(
        &'a self,
        request: CodeGraphReadAdmissionRequest<'a>,
    ) -> super::CodeGraphReadAdmissionFuture<'a> {
        let scope = self.scope.clone();
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            Ok(admit_context(&request, &scope))
        })
    }
}

struct CancelWaitingAdmission {
    scope: ResolvedScope,
}

impl CodeGraphReadAdmissionPort for CancelWaitingAdmission {
    fn admit<'a>(
        &'a self,
        request: CodeGraphReadAdmissionRequest<'a>,
    ) -> super::CodeGraphReadAdmissionFuture<'a> {
        let scope = self.scope.clone();
        Box::pin(async move {
            request.cancellation.cancelled().await;
            Ok(admit_context(&request, &scope))
        })
    }
}

struct DelayedProjection {
    scope: ResolvedScope,
    store: Arc<tracedecay_code_index::graph_projection::CodeGraphProjectionStore>,
    delay: Duration,
}

impl CodeGraphProjectionReadPort for DelayedProjection {
    fn open<'a>(&'a self, _request: CodeGraphReadRequest<'a>) -> super::CodeGraphReadFuture<'a> {
        let scope = self.scope.clone();
        let store = Arc::clone(&self.store);
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            VerifiedCodeGraphRead::new(scope, store, super::CodeGraphReadFreshnessV1::Current)
        })
    }
}

struct PendingAdmission;

impl CodeGraphReadAdmissionPort for PendingAdmission {
    fn admit<'a>(
        &'a self,
        _request: CodeGraphReadAdmissionRequest<'a>,
    ) -> super::CodeGraphReadAdmissionFuture<'a> {
        Box::pin(std::future::pending())
    }
}

struct PendingProjection;

impl CodeGraphProjectionReadPort for PendingProjection {
    fn open<'a>(&'a self, _request: CodeGraphReadRequest<'a>) -> super::CodeGraphReadFuture<'a> {
        Box::pin(std::future::pending())
    }
}

struct PendingSourceBind;

impl CodeGraphSourceAuthorityPort for PendingSourceBind {
    fn bind<'a>(
        &'a self,
        _request: CodeGraphSourceBindRequest<'a>,
    ) -> super::CodeGraphSourceBindFuture<'a> {
        Box::pin(std::future::pending())
    }
}

struct CancelWaitingProjection {
    scope: ResolvedScope,
    store: Arc<tracedecay_code_index::graph_projection::CodeGraphProjectionStore>,
}

impl CodeGraphProjectionReadPort for CancelWaitingProjection {
    fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> super::CodeGraphReadFuture<'a> {
        let scope = self.scope.clone();
        let store = Arc::clone(&self.store);
        Box::pin(async move {
            if let Some(signal) = request.live_cancellation {
                signal.cancelled().await;
            } else {
                std::future::pending::<()>().await;
            }
            VerifiedCodeGraphRead::new(scope, store, super::CodeGraphReadFreshnessV1::Current)
        })
    }
}

fn short_deadline() -> Deadline {
    Deadline::new(UtcMicros(now_micros().0.saturating_add(20_000))).expect("deadline")
}

async fn expect_open_error(
    admission: &dyn CodeGraphReadAdmissionPort,
    projection: &dyn CodeGraphProjectionReadPort,
    source: Option<&dyn CodeGraphSourceAuthorityPort>,
    deadline: Deadline,
    cancellation: &CancellationSignal,
    request_tag: &str,
) -> tracedecay_domain::errors::TraceDecayError {
    let operation = graph_operation();
    match open_verified_graph_query(
        admission,
        projection,
        VerifiedGraphQueryRequest::new(
            &operation,
            RequestId::new(request_tag).expect("request"),
            deadline,
            cancellation,
        ),
        source,
    )
    .await
    {
        Ok(_) => panic!("open must fail for {request_tag}"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn delayed_admission_returns_exact_timed_out() {
    let admission = DelayedAdmission {
        scope: fixture_scope(SCOPE_TAG),
        delay: Duration::from_millis(80),
    };
    let projection = ImmediateProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
    };
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.admit-timeout").expect("signal");
    let error = expect_open_error(
        &admission,
        &projection,
        None,
        short_deadline(),
        &cancellation,
        "request.verified-query-deadline.admit-timeout",
    )
    .await;
    assert_route(error, "code-graph-timed-out");
}

#[tokio::test]
async fn delayed_projection_returns_exact_timed_out() {
    let admission = ImmediateAdmission {
        scope: fixture_scope(SCOPE_TAG),
    };
    let projection = DelayedProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
        delay: Duration::from_millis(80),
    };
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.proj-timeout").expect("signal");
    let error = expect_open_error(
        &admission,
        &projection,
        None,
        short_deadline(),
        &cancellation,
        "request.verified-query-deadline.proj-timeout",
    )
    .await;
    assert_route(error, "code-graph-timed-out");
}

#[tokio::test]
async fn cancellation_during_admission_wait_returns_exact_cancelled() {
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.admit-cancel").expect("signal");
    let cancel = cancellation.clone();
    let admission = CancelWaitingAdmission {
        scope: fixture_scope(SCOPE_TAG),
    };
    let projection = ImmediateProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
    };
    let open = expect_open_error(
        &admission,
        &projection,
        None,
        deadline,
        &cancellation,
        "request.verified-query-deadline.admit-cancel",
    );
    let cancel_task = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel(now_micros());
    };
    let (error, _) = tokio::join!(open, cancel_task);
    assert_route(error, "code-graph-cancelled");
}

#[tokio::test]
async fn cancellation_during_projection_wait_returns_exact_cancelled() {
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.proj-cancel").expect("signal");
    let cancel = cancellation.clone();
    let admission = ImmediateAdmission {
        scope: fixture_scope(SCOPE_TAG),
    };
    let projection = CancelWaitingProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
    };
    let open = expect_open_error(
        &admission,
        &projection,
        None,
        deadline,
        &cancellation,
        "request.verified-query-deadline.proj-cancel",
    );
    let cancel_task = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel(now_micros());
    };
    let (error, _) = tokio::join!(open, cancel_task);
    assert_route(error, "code-graph-cancelled");
}

#[tokio::test]
async fn pending_admission_returns_exact_timed_out() {
    let admission = PendingAdmission;
    let projection = ImmediateProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
    };
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.pending-admit-timeout")
            .expect("signal");
    let error = expect_open_error(
        &admission,
        &projection,
        None,
        short_deadline(),
        &cancellation,
        "request.verified-query-deadline.pending-admit-timeout",
    )
    .await;
    assert_route(error, "code-graph-timed-out");
}

#[tokio::test]
async fn pending_projection_returns_exact_timed_out() {
    let admission = ImmediateAdmission {
        scope: fixture_scope(SCOPE_TAG),
    };
    let projection = PendingProjection;
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.pending-proj-timeout")
            .expect("signal");
    let error = expect_open_error(
        &admission,
        &projection,
        None,
        short_deadline(),
        &cancellation,
        "request.verified-query-deadline.pending-proj-timeout",
    )
    .await;
    assert_route(error, "code-graph-timed-out");
}

#[tokio::test]
async fn pending_source_bind_returns_exact_timed_out() {
    let admission = ImmediateAdmission {
        scope: fixture_scope(SCOPE_TAG),
    };
    let projection = ImmediateProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
    };
    let bind = PendingSourceBind;
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.pending-bind-timeout")
            .expect("signal");
    let error = expect_open_error(
        &admission,
        &projection,
        Some(&bind),
        short_deadline(),
        &cancellation,
        "request.verified-query-deadline.pending-bind-timeout",
    )
    .await;
    assert_route(error, "code-graph-timed-out");
}

#[tokio::test]
async fn pending_admission_returns_exact_cancelled() {
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.pending-admit-cancel")
            .expect("signal");
    let cancel = cancellation.clone();
    let admission = PendingAdmission;
    let projection = ImmediateProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
    };
    let open = expect_open_error(
        &admission,
        &projection,
        None,
        deadline,
        &cancellation,
        "request.verified-query-deadline.pending-admit-cancel",
    );
    let cancel_task = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel(now_micros());
    };
    let (error, _) = tokio::join!(open, cancel_task);
    assert_route(error, "code-graph-cancelled");
}

#[tokio::test]
async fn pending_projection_returns_exact_cancelled() {
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.pending-proj-cancel")
            .expect("signal");
    let cancel = cancellation.clone();
    let admission = ImmediateAdmission {
        scope: fixture_scope(SCOPE_TAG),
    };
    let projection = PendingProjection;
    let open = expect_open_error(
        &admission,
        &projection,
        None,
        deadline,
        &cancellation,
        "request.verified-query-deadline.pending-proj-cancel",
    );
    let cancel_task = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel(now_micros());
    };
    let (error, _) = tokio::join!(open, cancel_task);
    assert_route(error, "code-graph-cancelled");
}

#[tokio::test]
async fn pending_source_bind_returns_exact_cancelled() {
    let deadline = Deadline::new(UtcMicros(i64::MAX)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.verified-query-deadline.pending-bind-cancel")
            .expect("signal");
    let cancel = cancellation.clone();
    let admission = ImmediateAdmission {
        scope: fixture_scope(SCOPE_TAG),
    };
    let projection = ImmediateProjection {
        scope: fixture_scope(SCOPE_TAG),
        store: fixture_store(SCOPE_TAG),
    };
    let bind = PendingSourceBind;
    let open = expect_open_error(
        &admission,
        &projection,
        Some(&bind),
        deadline,
        &cancellation,
        "request.verified-query-deadline.pending-bind-cancel",
    );
    let cancel_task = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel(now_micros());
    };
    let (error, _) = tokio::join!(open, cancel_task);
    assert_route(error, "code-graph-cancelled");
}
