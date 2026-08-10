use super::*;

#[test]
fn required_str_rejects_missing_and_empty_values() {
    assert!(required_str(&json!({}), "action").is_err());
    assert!(required_str(&json!({ "action": "" }), "action").is_err());
    assert_eq!(
        required_str(&json!({ "action": "reset_counter" }), "action").unwrap(),
        "reset_counter"
    );
}

#[test]
fn projectless_runtime_rejects_project_database_actions() {
    assert!(!projectless_action_allowed("reset_counter", &json!({})));
    assert!(projectless_action_allowed(
        "codex_stop",
        &json!({ "session_id": "codex-terminal" }),
    ));
    assert!(!projectless_action_allowed(
        "ingest_transcript",
        &json!({ "user_scope": false }),
    ));
    assert!(projectless_action_allowed(
        "ingest_transcript",
        &json!({ "user_scope": true }),
    ));
    assert!(!projectless_action_allowed(
        "claude_compact",
        &json!({ "user_scope": false }),
    ));
    assert!(projectless_action_allowed(
        "claude_compact",
        &json!({ "user_scope": true }),
    ));
}

#[test]
fn session_authority_roles_fail_closed_independently() {
    let none = SessionAuthorities::default();
    assert!(required_project_db(none).is_err());
    assert!(required_user_db(none).is_err());
}

#[tokio::test]
async fn terminal_review_operation_stops_when_its_receipt_is_superseded() {
    let cancellation = tracedecay_usecases::observation::ObservationCancellation::default();
    cancellation.cancel();

    let output = super::terminal::await_terminal_operation(&cancellation, async {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        "completed"
    })
    .await;

    assert!(output.is_none());
}
