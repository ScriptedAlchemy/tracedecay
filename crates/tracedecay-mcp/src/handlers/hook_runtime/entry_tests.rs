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
    for value in [None, Some("")] {
        assert_eq!(
            required_field(value, "session_id").unwrap_err().to_string(),
            "config error: missing required parameter `session_id`"
        );
    }
    assert_eq!(required_field(Some("s"), "session_id").unwrap(), "s");
}

#[test]
fn session_authority_roles_fail_closed_independently() {
    let none = SessionAuthorities::default();
    assert_eq!(
        required_project_db(&none)
            .err()
            .map(|error| error.to_string()),
        Some("config error: daemon project session database is unavailable".to_owned())
    );
    assert_eq!(
        required_user_db(&none).err().map(|error| error.to_string()),
        Some("config error: daemon user session database is unavailable".to_owned())
    );
}
