use std::time::Duration;

use tracedecay_contracts::{CancellationSignal, Deadline, RequestId, ResolvedScope, now_micros};
use tracedecay_domain::UtcMicros;

use super::verified_query_test_support::{
    ImmediateProjection, admit_context, assert_route, fixture_scope, fixture_store, graph_operation,
};
use super::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionPort, CodeGraphReadAdmissionRequest,
    VerifiedGraphQueryRequest, open_verified_graph_query,
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

fn short_deadline() -> Deadline {
    Deadline::new(UtcMicros(now_micros().0.saturating_add(20_000))).expect("deadline")
}

async fn expect_open_error(
    admission: &dyn CodeGraphReadAdmissionPort,
    projection: &dyn CodeGraphProjectionReadPort,
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
        None,
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
        short_deadline(),
        &cancellation,
        "request.verified-query-deadline.admit-timeout",
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
