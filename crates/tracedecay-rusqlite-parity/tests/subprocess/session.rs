use serde_json::json;

use crate::support::{fixture, invoke, request};

#[test]
fn subprocess_reports_closed_session_store_counts_schema_and_keyset_pages() {
    let fixture = fixture();

    let count = invoke(&request(
        &fixture.path,
        json!({
            "type": "session_store_count",
            "family": "observation",
            "table": "observations"
        }),
    ));
    assert_eq!(count["status"], "ok");
    assert_eq!(count["output"]["row_count"], 2);

    let schema = invoke(&request(
        &fixture.path,
        json!({
            "type": "session_store_schema",
            "family": "transcript",
            "table": "session_messages"
        }),
    ));
    assert_eq!(schema["status"], "ok");
    assert_eq!(schema["output"]["exists"], true);
    assert_eq!(schema["output"]["columns"][0]["name"], "provider");
    assert_eq!(
        schema["output"]["foreign_keys"].as_array().unwrap().len(),
        2
    );

    let first = invoke(&request(
        &fixture.path,
        json!({
            "type": "session_store_page",
            "family": "observation",
            "table": "observations",
            "cursor": null,
            "limit": 1
        }),
    ));
    assert_eq!(first["status"], "ok");
    assert_eq!(first["output"]["order_columns"], json!(["sequence"]));
    assert_eq!(
        first["output"]["rows"][0]["observation_id"],
        "observation-1"
    );
    assert!(
        first["output"]["rows"][0]["row_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:"))
    );
    let second = invoke(&request(
        &fixture.path,
        json!({
            "type": "session_store_page",
            "family": "observation",
            "table": "observations",
            "cursor": first["output"]["next_cursor"].clone(),
            "limit": 1
        }),
    ));
    assert_eq!(
        second["output"]["rows"][0]["observation_id"],
        "observation-2"
    );
    assert!(second["output"]["next_cursor"].is_null());

    for table in [
        "sessions",
        "session_messages",
        "session_schema_migrations",
        "lcm_raw_messages",
        "session_temporal_schema_migrations",
        "session_temporal_generations",
        "session_temporal_observation_effects",
    ] {
        let family = match table {
            "sessions" | "session_messages" => "transcript",
            "session_schema_migrations" | "lcm_raw_messages" => "lcm",
            _ => "temporal",
        };
        let response = invoke(&request(
            &fixture.path,
            json!({
                "type": "session_store_page",
                "family": family,
                "table": table,
                "cursor": null,
                "limit": 10
            }),
        ));
        assert_eq!(response["status"], "ok", "table {table}: {response:#}");
        assert_eq!(response["output"]["table"], table);
    }
}
