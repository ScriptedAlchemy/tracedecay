use super::*;
use serde_json::json;
use tracedecay_contracts::retrieval::HookIngestTranscriptRequestV1;

#[test]
fn decode_keeps_the_session_payload_and_drops_only_presentation_keys() {
    let request = decode_hook_runtime_request(&json!({
        "action": "ingest_transcript",
        "provider": "codex",
        "user_scope": true,
        "session_id": "codex-session",
        "format": "json",
    }))
    .unwrap();
    assert_eq!(
        request,
        HookRuntimeSurfaceRequestV1::IngestTranscript(HookIngestTranscriptRequestV1 {
            provider: "codex".to_owned(),
            user_scope: true,
            session_id: Some("codex-session".to_owned()),
            event_json: None,
            messages: None,
            max_new_bytes: None,
        })
    );
    assert_eq!(
        decode_hook_runtime_request(&json!({"action": "reset_counter", "reset": true}))
            .unwrap_err()
            .to_string(),
        "config error: invalid arguments for tracedecay_hook_runtime: unknown field `reset`, there are no fields"
    );
}

#[test]
fn required_field_rejects_missing_and_empty_values() {
    assert!(required_field(None, "session_id").is_err());
    assert!(required_field(Some(""), "session_id").is_err());
    assert_eq!(required_field(Some("s"), "session_id").unwrap(), "s");
}

#[test]
fn session_authority_roles_fail_closed_independently() {
    let none = SessionAuthorities::default();
    assert!(required_project_db(&none).is_err());
    assert!(required_user_db(&none).is_err());
}
