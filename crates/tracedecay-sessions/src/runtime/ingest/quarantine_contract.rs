use std::collections::BTreeSet;
use std::future::Future;

use crate::admission::test_support::MemoryHostAdmission;
use crate::admission::{HostAdmissionOutcome, HostAdmissionStatus};

use super::failure::ProviderRunOutcome;

pub(crate) const REFUSED_REASON: &str = "observation_cursor_advance_ledger_disagreement";

pub(crate) fn refuse_session(admission: &MemoryHostAdmission, session: &'static str) {
    admission.fail_captures_for_session(
        session,
        HostAdmissionOutcome {
            status: HostAdmissionStatus::Degraded,
            retryable: false,
            reason_code: Some(REFUSED_REASON),
            recovery: None,
            cause: None,
        },
    );
}

pub(crate) fn admitted_sessions(admission: &MemoryHostAdmission) -> BTreeSet<String> {
    admission
        .observations()
        .iter()
        .map(|stored| {
            stored
                .observation()
                .source()
                .session_id()
                .as_str()
                .to_owned()
        })
        .collect()
}

fn refusal_reports(outcome: &ProviderRunOutcome) -> usize {
    outcome
        .failures
        .iter()
        .filter(|failure| failure.reason_code == REFUSED_REASON)
        .count()
}

pub(crate) async fn assert_refused_source_contract<P, F>(
    admission: &MemoryHostAdmission,
    sibling_session: &str,
    mut pass: P,
    change_refused_source: impl FnOnce(),
) where
    P: FnMut() -> F,
    F: Future<Output = ProviderRunOutcome>,
{
    let first = pass().await;
    let reports = first
        .failures
        .iter()
        .map(|failure| (failure.reason_code, failure.retryable))
        .collect::<Vec<_>>();
    assert_eq!(
        reports,
        [(REFUSED_REASON, false)],
        "the first pass reports the refused source once and nothing else"
    );
    assert!(
        admitted_sessions(admission).contains(sibling_session),
        "the healthy sibling {sibling_session} must be admitted in the same pass: {:?}",
        admitted_sessions(admission)
    );
    let attempts = admission.failed_session_capture_attempts();
    assert!(
        attempts > 0,
        "the first pass must attempt the refused source"
    );

    for pass_number in 2..=3 {
        let outcome = pass().await;
        assert_eq!(
            refusal_reports(&outcome),
            0,
            "pass {pass_number} re-reported the unchanged refused source: {:?}",
            outcome.failures
        );
        assert_eq!(
            admission.failed_session_capture_attempts(),
            attempts,
            "pass {pass_number} attempted the unchanged refused source again"
        );
    }

    change_refused_source();
    let mut reports = 0;
    for _ in 0..8 {
        reports += refusal_reports(&pass().await);
        if admission.failed_session_capture_attempts() > attempts {
            break;
        }
    }
    assert!(
        admission.failed_session_capture_attempts() > attempts,
        "a changed source must leave quarantine and be attempted again"
    );
    assert_eq!(reports, 1, "the changed source is reported once");
}
