use super::SessionFactCurationOutcome;

#[test]
fn session_fact_receipt_outcome_tracks_terminal_effects_and_retry_need() {
    let cases = [
        ((0, 0, 0, false), SessionFactCurationOutcome::NoCandidate),
        ((2, 2, 0, false), SessionFactCurationOutcome::Applied),
        ((0, 0, 2, false), SessionFactCurationOutcome::Quarantined),
        ((2, 1, 1, false), SessionFactCurationOutcome::Partial),
        ((2, 1, 0, true), SessionFactCurationOutcome::Partial),
        ((2, 0, 0, true), SessionFactCurationOutcome::Retry),
    ];

    for ((admitted, applied, quarantined, retry_required), expected) in cases {
        assert_eq!(
            SessionFactCurationOutcome::classify(admitted, applied, quarantined, retry_required,),
            expected,
        );
    }
}
