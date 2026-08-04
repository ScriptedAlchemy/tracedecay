use std::fs;

use serde_json::json;

use crate::support::{fixture, invoke, missing_copied_database, request, request_for_database};

#[test]
fn subprocess_never_creates_missing_files_or_accepts_write_sql() {
    let fixture = fixture();
    let missing = fixture.path.parent().unwrap().join("missing.db");
    let response = invoke(&request_for_database(
        missing_copied_database(&missing),
        json!({ "type": "metadata" }),
    ));
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"]["code"], "invalid_path");
    assert!(!missing.exists());

    let before = fs::read(&fixture.path).expect("fixture before invalid command");
    let response = invoke(&request(
        &fixture.path,
        json!({ "type": "sql", "sql": "DELETE FROM observations" }),
    ));
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"]["code"], "invalid_request");
    let response = invoke(&request(
        &fixture.path,
        json!({
            "type": "session_store_page",
            "family": "observation",
            "table": "observations",
            "cursor": { "table": "lcm_raw_messages", "store_id": 1 },
            "limit": 10
        }),
    ));
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"]["code"], "invalid_page_cursor");
    assert_eq!(
        before,
        fs::read(&fixture.path).expect("fixture after invalid command")
    );
}

#[test]
fn subprocess_rejects_unknown_fields_and_closed_command_semantic_errors() {
    let fixture = fixture();
    let before = fs::read(&fixture.path).expect("fixture before invalid requests");

    let mut unknown_field = request(&fixture.path, json!({ "type": "metadata" }));
    unknown_field["command"]["unexpected"] = json!(true);
    let response = invoke(&unknown_field);
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"]["code"], "invalid_request");

    let response = invoke(&request(
        &fixture.path,
        json!({
            "type": "session_store_count",
            "family": "lcm",
            "table": "observations"
        }),
    ));
    assert_eq!(response["status"], "error");
    assert_eq!(response["error"]["code"], "invalid_store_family");
    assert_eq!(
        before,
        fs::read(&fixture.path).expect("fixture after invalid requests")
    );
}
