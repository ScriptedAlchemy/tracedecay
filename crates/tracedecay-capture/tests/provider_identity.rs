use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tracedecay_capture::{claude, codex, cursor, cursor_composer};
use tracedecay_domain::ObservationSourceRangeV1;

#[test]
fn claude_native_identity_encoding_remains_byte_exact() {
    let native_path = [b'C', 0, b':', 0, b'\\', 0, 0x3d, 0xd8, 0x00, 0xde];

    assert_eq!(
        claude::encode_cursor_key("windows-utf16le", &native_path),
        "tracedecay-claude-cursor-v1-windows-utf16le-43003a005c003dd800de"
    );
    assert_eq!(
        claude::encode_source_id("windows-utf16le", &native_path),
        "tracedecay-claude-source-v1-windows-utf16le-43003a005c003dd800de"
    );
    assert_eq!(
        claude::observation_source_id(b"session-42"),
        "tracedecay-claude-observation-source-v1-sha256-79e3ce0c2602f9b58b30d97366d5c1dc656c28a248b3015f5a076d283d22028c"
    );
}

#[test]
fn codex_native_record_identity_uses_canonical_native_value() {
    let compact = json!({"type":"event_msg","payload":{"type":"agent_message","message":"ok"}});
    let reordered = json!({"payload":{"message":"ok","type":"agent_message"},"type":"event_msg"});

    assert_eq!(
        codex::codex_native_record_id("session-42", &compact).unwrap(),
        codex::codex_native_record_id("session-42", &reordered).unwrap()
    );
    assert_ne!(
        codex::codex_native_record_id("session-42", &compact).unwrap(),
        codex::codex_native_record_id("session-43", &compact).unwrap()
    );
}

#[test]
fn codex_checked_in_fixture_preserves_canonical_envelope() {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider_normalization/codex");
    let native: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("agent_message.input.json")).unwrap(),
    )
    .unwrap();
    let record_id = codex::codex_native_record_id("codex-golden-session", &native).unwrap();
    let actual = serde_json::to_value(
        codex::normalize_codex_observation(
            &native,
            "codex-golden-session",
            Some("codex-golden-session"),
            record_id.clone(),
            ObservationSourceRangeV1::new(0, 1).unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let expected: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("agent_message.expected_envelope.json"))
            .unwrap()
            .replace(
                "\"$STABLE_RECORD_ID\"",
                &serde_json::to_string(record_id.as_str()).unwrap(),
            ),
    )
    .unwrap();

    assert_eq!(actual, expected);
}

#[test]
fn cursor_checked_in_tool_fixture_preserves_projection() {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider_normalization/cursor");
    let native: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("tool_use.input.json")).unwrap(),
    )
    .unwrap();
    let expected: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("tool_use.expected_envelope.json")).unwrap(),
    )
    .unwrap();
    let record_id =
        cursor::observation_native_record_id("cursor", "cursor-tool-fixture", &native).unwrap();
    let actual = serde_json::to_value(
        cursor::normalize_cursor_observation(
            &native,
            "cursor-tool-fixture",
            record_id.clone(),
            ObservationSourceRangeV1::new(0, 64).unwrap(),
            None,
            None,
        )
        .unwrap(),
    )
    .unwrap();

    assert_eq!(actual["version"], expected["version"]);
    assert_eq!(actual["evidence"], expected["evidence"]);
    assert_eq!(actual["relations"]["message_id"], record_id.as_str());
    assert!(actual["facts"].as_array().unwrap().iter().any(|fact| {
        fact["kind"] == "tool_invocation"
            && fact["arguments"] == native["message"]["content"][1]["input"]
    }));
}

#[test]
fn cursor_composer_checked_in_bubble_preserves_projection() {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider_normalization/cursor_composer");
    let native: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("assistant_bubble.input.json")).unwrap(),
    )
    .unwrap();
    let expected: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("assistant_bubble.expected_envelope.json")).unwrap(),
    )
    .unwrap();
    let record_id = cursor_composer::cursor_composer_native_record_id("comp-1", "b-asst").unwrap();
    let actual = serde_json::to_value(
        cursor_composer::normalize_cursor_composer_observation(
            &native,
            "comp-1",
            record_id.clone(),
            ObservationSourceRangeV1::new(1, 2).unwrap(),
            1,
        )
        .unwrap(),
    )
    .unwrap();
    let actual_kinds = actual["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fact| fact["kind"].as_str().unwrap())
        .collect::<Vec<_>>();
    let expected_kinds = expected["fact_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(actual["evidence"], expected["evidence"]);
    assert_eq!(actual["relations"]["message_id"], record_id.as_str());
    assert_eq!(actual_kinds, expected_kinds);
}
