use std::fs;

use tracedecay_sqlite_parity_protocol::{ErrorCode, ErrorPayload, ResponseOutcome};

use crate::{service::handle_request_bytes, snapshot};

use super::support::{fixture, request_value};

#[test]
fn protocol_rejects_versions_options_sql_and_profile_paths() {
    let fixture = fixture();
    let mut unsupported = request_value(
        &fixture.path,
        "version",
        serde_json::json!({ "type": "metadata" }),
    );
    unsupported["protocol_version"] = serde_json::json!(2);
    assert!(matches!(
        handle_request_bytes(&serde_json::to_vec(&unsupported).unwrap()).outcome,
        ResponseOutcome::Error {
            error: ErrorPayload {
                code: ErrorCode::UnsupportedProtocolVersion,
                ..
            }
        }
    ));
    for invalid_command in [
        serde_json::json!({ "type": "sql", "sql": "DELETE FROM observations" }),
        serde_json::json!({ "type": "metadata", "writable": true }),
    ] {
        let invalid = request_value(&fixture.path, "invalid", invalid_command);
        assert!(matches!(
            handle_request_bytes(&serde_json::to_vec(&invalid).unwrap()).outcome,
            ResponseOutcome::Error {
                error: ErrorPayload {
                    code: ErrorCode::InvalidRequest,
                    ..
                }
            }
        ));
    }

    for (command, expected_code) in [
        (
            serde_json::json!({
                "type": "session_store_count",
                "family": "lcm",
                "table": "observations"
            }),
            ErrorCode::InvalidStoreFamily,
        ),
        (
            serde_json::json!({
                "type": "session_store_page",
                "family": "observation",
                "table": "observations",
                "cursor": null,
                "limit": 0
            }),
            ErrorCode::InvalidPageLimit,
        ),
        (
            serde_json::json!({
                "type": "session_store_page",
                "family": "observation",
                "table": "observations",
                "cursor": null,
                "limit": 101
            }),
            ErrorCode::InvalidPageLimit,
        ),
        (
            serde_json::json!({
                "type": "session_store_page",
                "family": "observation",
                "table": "observations",
                "cursor": { "table": "lcm_raw_messages", "store_id": 1 },
                "limit": 10
            }),
            ErrorCode::InvalidPageCursor,
        ),
    ] {
        let invalid = request_value(&fixture.path, "invalid-session-store", command);
        let ResponseOutcome::Error { error } =
            handle_request_bytes(&serde_json::to_vec(&invalid).unwrap()).outcome
        else {
            panic!("invalid session-store request unexpectedly succeeded");
        };
        assert_eq!(error.code, expected_code);
    }

    let directory = tempfile::tempdir().expect("temp profile parent");
    let profile = directory.path().join(".tracedecay");
    fs::create_dir(&profile).expect("create profile directory");
    let live = profile.join("tracedecay.db");
    fs::write(&live, b"not opened").expect("create protected file");
    let error = snapshot::validate_copied_path(&live).expect_err("profile path must be rejected");
    assert_eq!(error.code, ErrorCode::RefusedLiveProfile);

    let custom_profile = directory.path().join("custom-profile-root");
    fs::create_dir(&custom_profile).expect("create custom profile root");
    let custom_live = custom_profile.join("global.db");
    fs::write(&custom_live, b"not opened").expect("create custom protected file");
    let canonical_live = fs::canonicalize(&custom_live).expect("canonical custom live path");
    let error = snapshot::reject_protected_profile_path(&canonical_live, &[custom_profile])
        .expect_err("configured profile root must be rejected");
    assert_eq!(error.code, ErrorCode::RefusedLiveProfile);
}
