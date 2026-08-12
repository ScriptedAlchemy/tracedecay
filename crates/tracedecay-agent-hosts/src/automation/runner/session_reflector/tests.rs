use super::{SessionFactCurationOutcome, session_fact_ledger_summary, validation_repairs_summary};
use serde_json::json;

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

#[test]
fn durable_session_fact_summaries_hash_instead_of_persisting_payloads() {
    let hostile_secret = "sk-live-ledger-do-not-persist";
    let proposed = vec![json!({"content": hostile_secret})];
    let accepted = vec![json!({"content": hostile_secret, "status": "accepted"})];
    let quarantined = vec![json!({"content": hostile_secret, "reason": hostile_secret})];

    let summary = session_fact_ledger_summary(&proposed, &accepted, &accepted, &quarantined)
        .expect("structural fact summary");
    let repairs = validation_repairs_summary(&[json!({"repair": hostile_secret})])
        .expect("structural repair summary");

    assert_eq!(summary.pointer("/proposed/count"), Some(&json!(1)));
    assert_eq!(summary.pointer("/quarantined/count"), Some(&json!(1)));
    assert!(summary.pointer("/accepted/sha256").is_some());
    assert_eq!(repairs.pointer("/count"), Some(&json!(1)));
    assert!(repairs.pointer("/sha256").is_some());
    assert!(
        !serde_json::to_string(&summary)
            .unwrap()
            .contains(hostile_secret)
    );
    assert!(
        !serde_json::to_string(&repairs)
            .unwrap()
            .contains(hostile_secret)
    );
}
