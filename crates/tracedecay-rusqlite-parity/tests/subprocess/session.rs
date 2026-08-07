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
        "source_cursors",
        "sessions",
        "session_messages",
        "session_schema_migrations",
        "lcm_raw_messages",
        "session_temporal_schema_migrations",
        "session_temporal_generations",
        "session_temporal_observation_effects",
        "session_temporal_projection_receipts",
        "session_occurrences",
        "session_assertions",
        "session_summary_nodes",
        "memory_v2_facts",
        "memory_v2_current_facts",
        "memory_v2_assertions",
        "memory_v2_lineage_events",
        "retrieval_anchors",
        "generation_diagnostics",
        "diagnostic_generation_publications",
        "configuration_revisions",
        "configuration_entries",
        "configuration_mutation_receipts",
        "configuration_audit_events",
    ] {
        let family = match table {
            "source_cursors" => "observation",
            "sessions" | "session_messages" => "transcript",
            "session_schema_migrations" | "lcm_raw_messages" => "lcm",
            "session_summary_nodes" => "summary",
            "memory_v2_facts"
            | "memory_v2_current_facts"
            | "memory_v2_assertions"
            | "memory_v2_lineage_events"
            | "retrieval_anchors" => "fact",
            "generation_diagnostics" | "diagnostic_generation_publications" => "diagnostics",
            "configuration_revisions"
            | "configuration_entries"
            | "configuration_mutation_receipts"
            | "configuration_audit_events" => "configuration",
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
