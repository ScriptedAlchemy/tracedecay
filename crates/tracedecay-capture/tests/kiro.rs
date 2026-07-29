use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tracedecay_capture::kiro::{KiroSnapshotMessage, snapshot_native_payload, stable_message_id};

#[test]
fn native_message_identity_remains_byte_exact() {
    assert_eq!(
        stable_message_id(
            "sess-1",
            Some("native-xyz"),
            "assistant",
            None,
            0,
            "ignored-for-native",
        ),
        "sess-1:native-xyz"
    );
    assert_eq!(
        stable_message_id("a:b", Some("c"), "assistant", None, 0, "ignored"),
        "kiro.message-id.v2.64a57d187fba9d5573d1351f63d86cc18249c4c669b1664d5af21b0a4b455532"
    );
    assert_eq!(
        stable_message_id("a", Some("b:c"), "assistant", None, 0, "ignored"),
        "kiro.message-id.v2.0500de5ef18c4cc47e647e2ac7760bd9ec19cf928add1f14a6ca8103cc9be6fd"
    );
}

#[test]
fn derived_message_identity_remains_byte_exact() {
    assert_eq!(
        stable_message_id(
            "sess-1",
            None,
            "assistant",
            Some(1_800_000_000),
            0,
            "stable body",
        ),
        "kiro.derived-message.v3.077fae8332facaa5376bf9de2b34fb6b605cd5483278eaa558842064406765c0"
    );
}

#[test]
fn checked_in_workspace_fixture_preserves_private_kernel_payload() {
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider_normalization/kiro");
    let input: Value = serde_json::from_str(
        &fs::read_to_string(fixture_root.join("workspace_session.input.json")).unwrap(),
    )
    .unwrap();
    let assistant = &input["messages"][1];
    let session_id = input["sessionId"].as_str().unwrap();
    let role = assistant["role"].as_str().unwrap();
    let timestamp = assistant["timestamp"].as_i64().map(|value| value / 1_000);
    let text = assistant["content"].as_str().unwrap();
    let message_id = stable_message_id(session_id, None, role, timestamp, 0, text);
    assert_eq!(
        message_id,
        "kiro.derived-message.v3.d55a77dc82280ffc352356e69eaa1da66fe65f35eac5f65d6604ffa9269d5e7b"
    );

    let actual = snapshot_native_payload(KiroSnapshotMessage {
        session_id,
        message_id: &message_id,
        role,
        timestamp,
        ordinal: 1,
        text,
        kind: Some("message"),
        model: input["modelId"].as_str(),
    });

    assert_eq!(
        actual,
        json!({
            "provider": "kiro",
            "session_id": "sess-golden",
            "message_id":
                "kiro.derived-message.v3.d55a77dc82280ffc352356e69eaa1da66fe65f35eac5f65d6604ffa9269d5e7b",
            "role": "assistant",
            "timestamp": 1_800_000_010_i64,
            "ordinal": 1,
            "kind": "message",
            "model": "claude-sonnet-4.6",
            "text": "The billing pipeline regression is fixed.",
        })
    );
    let encoded = actual.to_string();
    for forbidden in [
        "source_path",
        "metadata",
        "reasoning",
        "tool_calls",
        "git",
        "workflow",
    ] {
        assert!(
            !encoded.contains(forbidden),
            "{forbidden} must remain absent"
        );
    }
}
