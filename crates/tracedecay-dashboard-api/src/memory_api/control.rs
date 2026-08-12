//! Live dashboard request lifecycle controls for canonical memory reads.

use std::sync::Arc;

use tracedecay_store::FactReadControl;

use crate::DashboardHttpRequestControlV1;
use crate::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardScopeV1,
};

pub(crate) fn fact_read_control(control: &DashboardHttpRequestControlV1) -> FactReadControl {
    let cancellation = control.cancellation().clone();
    let deadline = control.deadline();
    FactReadControl::new(Arc::new(move || {
        cancellation.is_cancelled()
            || deadline.is_elapsed_at(crate::application::context::application_observed_at())
    }))
}

pub(super) fn request_deadline_elapsed(control: &DashboardHttpRequestControlV1) -> bool {
    control
        .deadline()
        .is_elapsed_at(crate::application::context::application_observed_at())
}

pub(crate) fn request_terminal_state(
    control: &DashboardHttpRequestControlV1,
) -> Option<DashboardDomainStateV1> {
    if request_deadline_elapsed(control) {
        Some(DashboardDomainStateV1::TimedOut)
    } else if control.cancellation().is_cancelled() {
        Some(DashboardDomainStateV1::Cancelled)
    } else {
        None
    }
}

pub(super) fn read_error_envelope<T>(
    scope: DashboardScopeV1,
    control: &DashboardHttpRequestControlV1,
    payload: T,
    reason: impl Into<String>,
) -> DashboardEnvelopeV1<T> {
    let reason = reason.into();
    let domain_state = match request_terminal_state(control) {
        Some(state @ (DashboardDomainStateV1::TimedOut | DashboardDomainStateV1::Cancelled)) => {
            state
        }
        _ => return DashboardEnvelopeV1::error(scope, payload, reason),
    };
    let mut coverage = DashboardCoverageV1::unknown();
    coverage.omission_reasons.push(reason);
    DashboardEnvelopeV1::new(
        scope,
        domain_state,
        coverage,
        DashboardFreshnessV1::unknown(),
        payload,
    )
}

pub(crate) fn terminal_read_code(state: DashboardDomainStateV1) -> (&'static str, &'static str) {
    match state {
        DashboardDomainStateV1::TimedOut => (
            "request_deadline_elapsed",
            "dashboard request deadline elapsed",
        ),
        _ => ("request_cancelled", "dashboard request was cancelled"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn request_control(
        cancellation: tracedecay_application::CancellationSignal,
    ) -> DashboardHttpRequestControlV1 {
        DashboardHttpRequestControlV1 {
            request_id: tracedecay_application::RequestId::new(
                "request.dashboard-memory-read-control-test",
            )
            .expect("request identity"),
            deadline: tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(i64::MAX))
                .expect("request deadline"),
            cancellation,
            observed_at: tracedecay_domain::UtcMicros(1),
        }
    }

    #[test]
    fn fact_read_control_observes_the_live_http_cancellation_signal() {
        let cancellation = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-memory-read-control-test",
        )
        .expect("cancellation signal");
        let control = request_control(cancellation.clone());
        let read_control = fact_read_control(&control);

        assert!(!read_control.interrupted());
        assert!(cancellation.cancel(tracedecay_domain::UtcMicros(2)));
        assert!(read_control.interrupted());
    }

    #[test]
    fn fact_read_control_observes_the_admitted_http_deadline() {
        let cancellation = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-memory-read-deadline-test",
        )
        .expect("cancellation signal");
        let mut control = request_control(cancellation);
        control.deadline = tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(0))
            .expect("elapsed deadline");

        assert!(request_deadline_elapsed(&control));
        assert!(fact_read_control(&control).interrupted());
    }

    #[test]
    fn read_error_envelope_preserves_cancelled_and_timed_out_states() {
        let cancellation = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-memory-read-envelope-test",
        )
        .expect("cancellation signal");
        let control = request_control(cancellation.clone());
        assert!(cancellation.cancel(tracedecay_domain::UtcMicros(2)));
        let cancelled = read_error_envelope(
            DashboardScopeV1 {
                project_id: None,
                storage_mode: "project_local".to_owned(),
                store_root: "/fixture".to_owned(),
            },
            &control,
            None::<Value>,
            "cancelled fixture",
        );
        assert_eq!(cancelled.domain_state, DashboardDomainStateV1::Cancelled);

        let cancellation = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-memory-timeout-envelope-test",
        )
        .expect("cancellation signal");
        let mut control = request_control(cancellation);
        control.deadline = tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(0))
            .expect("elapsed deadline");
        let timed_out = read_error_envelope(
            DashboardScopeV1 {
                project_id: None,
                storage_mode: "project_local".to_owned(),
                store_root: "/fixture".to_owned(),
            },
            &control,
            None::<Value>,
            "timed out fixture",
        );
        assert_eq!(timed_out.domain_state, DashboardDomainStateV1::TimedOut);
    }
}
