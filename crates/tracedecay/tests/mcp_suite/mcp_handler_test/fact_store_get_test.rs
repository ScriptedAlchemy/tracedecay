#![cfg(feature = "test-transport")]

//! Behavioral proof for `tracedecay_fact_store_get`.
//!
//! Callers ask for one fact by id. The tool returns that stored fact and the
//! trust events that explain its score, or a typed refusal. It does not invent
//! a payload for a missing id, a different memory scope, or another project.

use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use super::memory_facts_test::{
    FactStoreCrossProjectFixture, close_test_graph, fact_store_cross_project_fixture, setup_project,
};
use crate::support::{extract_real_server_text, handle_real_server_tool_call_raw};

async fn call_tool(server: &McpServer, tool_name: &str, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, tool_name, arguments).await
}

/// The document an MCP client reads: the JSON-RPC error, or the parsed tool text.
fn client_document(response: &Value) -> Value {
    if !response["error"].is_null() {
        return response["error"].clone();
    }
    let text = extract_real_server_text(&response["result"]);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_fact_store_get returned non-JSON tool text: {error}: {text}")
    })
}

fn stored_fact_id(added: &Value) -> String {
    let payload = client_document(added);
    payload
        .pointer("/result/fact/fact/fact_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("fact add did not return an available fact id: {payload}"))
        .to_owned()
}

fn history_observed(entry: &Value) -> Value {
    json!({
        "action": entry["action"].clone(),
        "old_trust_millionths": entry["old_trust_millionths"].clone(),
        "new_trust_millionths": entry["new_trust_millionths"].clone(),
        "source_label": entry["source_label"].clone(),
        "reason": entry["reason"].clone(),
        "details_availability": entry["details_availability"].clone(),
    })
}

fn assert_problem(document: &Value, kind: &str) {
    assert_eq!(
        document["problem"]["kind"], kind,
        "expected a typed {kind} refusal, got {document}"
    );
    assert!(
        document.pointer("/outcome/value/payload").is_none(),
        "a {kind} refusal must not carry a fact payload: {document}"
    );
}

async fn project_id(server: &McpServer) -> String {
    server
        .cg()
        .await
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("registered project id")
        .to_owned()
}

#[tokio::test]
async fn fact_store_get_returns_the_stored_fact_and_its_trust_history() {
    let fixture = setup_project().await;
    let server = Arc::clone(&fixture.server);
    let content = "The Phoenix release train ships on the first Monday";

    let added = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": content,
            "category": "decision",
            "tags": ["release", "calendar"],
            "entities": ["release desk", "calendar owner"],
            "source_label": "operator desk"
        }),
    )
    .await;
    let fact_id = stored_fact_id(&added);

    let fetched = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id.clone()}),
        )
        .await,
    );
    assert_eq!(fetched["fact"]["kind"], "available", "{fetched}");
    assert_eq!(fetched["fact"]["fact"]["fact_id"], fact_id);
    assert_eq!(fetched["fact"]["fact"]["content"], content);
    assert_eq!(fetched["fact"]["fact"]["category"], "decision");
    assert_eq!(
        fetched["fact"]["fact"]["tags"],
        json!(["calendar", "release"]),
        "stored tags are canonicalized, not request order: {fetched}"
    );
    assert_eq!(
        fetched["fact"]["fact"]["entities"],
        json!(["calendar owner", "release desk"])
    );
    assert_eq!(fetched["fact"]["fact"]["source_label"], "operator desk");
    assert_eq!(fetched["fact"]["fact"]["owner"]["kind"], "project");
    assert_eq!(fetched["fact"]["fact"]["source"]["kind"], "application");
    assert_eq!(fetched["fact"]["fact"]["trust_score_millionths"], 500_000);
    assert_eq!(fetched["fact"]["fact"]["telemetry"]["helpful_count"], 0);
    assert_eq!(fetched["fact"]["fact"]["telemetry"]["unhelpful_count"], 0);
    assert_eq!(fetched["trust_history"], json!([]));

    call_tool(
        &server,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id.clone(),
            "action": "helpful",
            "source_label": "release review",
            "reason": "matched the release note"
        }),
    )
    .await;
    let after_helpful = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id.clone()}),
        )
        .await,
    );
    assert_eq!(after_helpful["fact"]["fact"]["content"], content);
    assert_eq!(
        after_helpful["fact"]["fact"]["trust_score_millionths"], 550_000,
        "{after_helpful}"
    );
    assert_eq!(
        after_helpful["fact"]["fact"]["telemetry"]["helpful_count"],
        1
    );
    assert_eq!(
        after_helpful["fact"]["fact"]["telemetry"]["unhelpful_count"],
        0
    );
    let helpful_history = after_helpful["trust_history"]
        .as_array()
        .expect("trust history");
    assert_eq!(
        helpful_history
            .iter()
            .map(history_observed)
            .collect::<Vec<_>>(),
        vec![json!({
            "action": "helpful",
            "old_trust_millionths": 500_000,
            "new_trust_millionths": 550_000,
            "source_label": "release review",
            "reason": "matched the release note",
            "details_availability": "available"
        })]
    );

    call_tool(
        &server,
        "tracedecay_fact_feedback",
        json!({
            "fact_id": fact_id.clone(),
            "action": "unhelpful",
            "source_label": "release review",
            "reason": "the date was wrong"
        }),
    )
    .await;
    let after_unhelpful = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id}),
        )
        .await,
    );
    assert_eq!(after_unhelpful["fact"]["fact"]["content"], content);
    assert_eq!(
        after_unhelpful["fact"]["fact"]["trust_score_millionths"], 450_000,
        "{after_unhelpful}"
    );
    assert_eq!(
        after_unhelpful["fact"]["fact"]["telemetry"]["helpful_count"],
        1
    );
    assert_eq!(
        after_unhelpful["fact"]["fact"]["telemetry"]["unhelpful_count"],
        1
    );
    let history = after_unhelpful["trust_history"]
        .as_array()
        .expect("trust history");
    assert_eq!(
        history.iter().map(history_observed).collect::<Vec<_>>(),
        vec![
            json!({
                "action": "helpful",
                "old_trust_millionths": 500_000,
                "new_trust_millionths": 550_000,
                "source_label": "release review",
                "reason": "matched the release note",
                "details_availability": "available"
            }),
            json!({
                "action": "unhelpful",
                "old_trust_millionths": 550_000,
                "new_trust_millionths": 450_000,
                "source_label": "release review",
                "reason": "the date was wrong",
                "details_availability": "available"
            }),
        ]
    );

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn fact_store_get_refuses_a_missing_id_and_a_different_memory_scope() {
    let fixture = setup_project().await;
    let server = Arc::clone(&fixture.server);
    let project_content = "Project routing stays on the active project graph";
    let user_content = "The operator wants concise technical answers";

    let project_added = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": project_content,
            "category": "project"
        }),
    )
    .await;
    let project_fact_id = stored_fact_id(&project_added);
    let user_added = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({
            "content": user_content,
            "category": "user_pref",
            "memory_scope": "user"
        }),
    )
    .await;
    let user_fact_id = stored_fact_id(&user_added);

    let project_read = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": project_fact_id.clone()}),
        )
        .await,
    );
    assert_eq!(project_read["fact"]["fact"]["content"], project_content);
    assert_eq!(project_read["fact"]["fact"]["category"], "project");
    assert_eq!(project_read["fact"]["fact"]["owner"]["kind"], "project");

    let user_read = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({
                "fact_id": user_fact_id.clone(),
                "memory_scope": "user"
            }),
        )
        .await,
    );
    assert_eq!(user_read["fact"]["fact"]["content"], user_content);
    assert_eq!(user_read["fact"]["fact"]["category"], "user_pref");
    assert_eq!(user_read["fact"]["fact"]["owner"]["kind"], "profile");

    let missing = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": "fact.never-stored"}),
        )
        .await,
    );
    assert_problem(&missing, "not_found_or_not_authorized");
    assert!(
        !missing.to_string().contains(project_content),
        "a missing id must not return the stored project fact: {missing}"
    );

    let project_as_user = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({
                "fact_id": project_fact_id,
                "memory_scope": "user"
            }),
        )
        .await,
    );
    assert_problem(&project_as_user, "not_found_or_not_authorized");

    let user_as_project = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": user_fact_id}),
        )
        .await,
    );
    assert_problem(&user_as_project, "not_found_or_not_authorized");

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn fact_store_get_reads_only_the_selected_registered_project() {
    let fixture: FactStoreCrossProjectFixture = fact_store_cross_project_fixture().await;
    let target_project_id = project_id(&fixture.target_server).await;
    let active_content = "Active project fact must not leak through a target read";
    let target_content = "Target project fact is readable only by its registered selector";

    let active_added = call_tool(
        &fixture.active_server,
        "tracedecay_fact_store_add",
        json!({
            "content": active_content,
            "category": "project"
        }),
    )
    .await;
    let active_fact_id = stored_fact_id(&active_added);
    let target_added = call_tool(
        &fixture.target_server,
        "tracedecay_fact_store_add",
        json!({
            "content": target_content,
            "category": "project"
        }),
    )
    .await;
    let target_fact_id = stored_fact_id(&target_added);

    let selected = client_document(
        &call_tool(
            &fixture.active_server,
            "tracedecay_fact_store_get",
            json!({
                "fact_id": target_fact_id.clone(),
                "project_selector": {"project_id": target_project_id.clone()}
            }),
        )
        .await,
    );
    assert_eq!(selected["fact"]["kind"], "available", "{selected}");
    assert_eq!(selected["fact"]["fact"]["fact_id"], target_fact_id);
    assert_eq!(selected["fact"]["fact"]["content"], target_content);
    assert_eq!(
        selected["fact"]["fact"]["owner"]["project_id"],
        target_project_id
    );
    assert!(
        !selected.to_string().contains(active_content),
        "the selected read must not include the active project's fact: {selected}"
    );

    let unselected = client_document(
        &call_tool(
            &fixture.active_server,
            "tracedecay_fact_store_get",
            json!({"fact_id": target_fact_id}),
        )
        .await,
    );
    assert_problem(&unselected, "not_found_or_not_authorized");

    let active_under_target = client_document(
        &call_tool(
            &fixture.active_server,
            "tracedecay_fact_store_get",
            json!({
                "fact_id": active_fact_id,
                "project_selector": {"project_id": target_project_id}
            }),
        )
        .await,
    );
    assert_problem(&active_under_target, "not_found_or_not_authorized");

    let missing_project = client_document(
        &call_tool(
            &fixture.active_server,
            "tracedecay_fact_store_get",
            json!({
                "fact_id": "fact.never-stored",
                "project_selector": {"project_id": "project.missing"}
            }),
        )
        .await,
    );
    assert_problem(&missing_project, "not_found_or_not_authorized");
    assert!(
        !missing_project.to_string().contains(active_content),
        "an unresolved selector must not fall back to the active project: {missing_project}"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn fact_store_get_rejects_a_request_that_is_not_one_fact_id() {
    let fixture = setup_project().await;
    let server = Arc::clone(&fixture.server);
    let content = "A well-formed get still returns this stored sentence";
    let added = call_tool(
        &server,
        "tracedecay_fact_store_add",
        json!({"content": content, "category": "tool"}),
    )
    .await;
    let fact_id = stored_fact_id(&added);
    let stored = client_document(
        &call_tool(
            &server,
            "tracedecay_fact_store_get",
            json!({"fact_id": fact_id}),
        )
        .await,
    );
    assert_eq!(stored["fact"]["fact"]["content"], content);
    assert_eq!(stored["fact"]["fact"]["category"], "tool");

    for arguments in [
        json!({}),
        json!({"fact_id": 41}),
        json!({"fact_id": "fact.never-stored", "category": "decision"}),
        json!({"fact_id": "fact.never-stored", "min_trust": 0.5}),
    ] {
        let refused =
            client_document(&call_tool(&server, "tracedecay_fact_store_get", arguments).await);
        assert_problem(&refused, "invalid_request");
        assert!(
            !refused.to_string().contains(content),
            "a rejected get must not return the stored fact: {refused}"
        );
    }

    close_test_graph(fixture).await;
}
