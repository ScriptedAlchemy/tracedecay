//! Behavioral proof of `tracedecay_fact_store_list` through the production MCP
//! `tools/call` path.
//!
//! List is an identity-ordered page of current facts. Retired facts (removed
//! or superseded) leave the page; category and trust filters drop non-matches;
//! `after_fact_id` resumes after the previous page's last id. Malformed
//! selectors fail at decode; out-of-range bounds fail as an invalid-request
//! problem. Generated ids and timestamps are not pinned.

use super::memory_facts_test::{
    close_test_graph, fact_store_server, invoke_production_tool, setup_project,
};
use crate::support::{extract_real_server_text, handle_real_server_tool_call_raw};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const LOW_TRUST_CONTENT: &str = "List proof: the decision trains on Tuesdays";
const TOOL_CONTENT: &str = "List proof: the rebuild tool refuses unsigned payloads";
const PROJECT_CONTENT: &str = "List proof: Project Phoenix stores facts by identity";
const USER_CONTENT: &str = "List proof: the operator wants one-line answers";

async fn list(fixture: &super::memory_facts_test::FactStoreMcpFixture, arguments: Value) -> Value {
    invoke_production_tool(fixture, "tracedecay_fact_store_list", arguments)
        .await
        .expect("tracedecay_fact_store_list")
}

async fn add(fixture: &super::memory_facts_test::FactStoreMcpFixture, arguments: Value) -> Value {
    let payload = invoke_production_tool(fixture, "tracedecay_fact_store_add", arguments)
        .await
        .expect("tracedecay_fact_store_add");
    assert_eq!(payload["outcome"], "committed");
    let result = &payload["result"];
    assert_eq!(result["disposition"], "added");
    assert_eq!(result["fact"]["kind"], "available");
    result.clone()
}

fn added_fact_id(result: &Value) -> String {
    result["fact"]["fact"]["fact_id"]
        .as_str()
        .unwrap_or_else(|| panic!("added fact id: {result}"))
        .to_owned()
}

fn by_content(payload: &Value) -> BTreeMap<&str, &Value> {
    payload["facts"]
        .as_array()
        .expect("list facts")
        .iter()
        .map(|projection| {
            assert_eq!(projection["kind"], "available", "{payload}");
            let fact = &projection["fact"];
            let content = fact["content"].as_str().expect("fact content");
            (content, fact)
        })
        .collect()
}

fn assert_identity_order(payload: &Value) {
    let ids: Vec<&str> = payload["facts"]
        .as_array()
        .expect("list facts")
        .iter()
        .map(|projection| {
            projection["fact"]["fact_id"]
                .as_str()
                .expect("listed fact id")
        })
        .collect();
    let mut ordered = ids.clone();
    ordered.sort_unstable();
    assert_eq!(ids, ordered, "list pages in fact_id order: {payload}");
}

fn assert_project_fact(
    fact: &Value,
    project_id: &str,
    category: &str,
    trust_millionths: u64,
    tags: Value,
    entities: Value,
    source_label: Option<&str>,
    metadata: Value,
) {
    assert_eq!(
        fact["owner"],
        json!({"kind": "project", "project_id": project_id})
    );
    assert_eq!(fact["category"], category);
    assert_eq!(fact["trust_score_millionths"], json!(trust_millionths));
    assert_eq!(fact["tags"], tags);
    assert_eq!(fact["entities"], entities);
    assert_eq!(fact["source"]["kind"], "application");
    assert_eq!(fact["source_label"], json!(source_label));
    assert_eq!(fact["metadata"], metadata);
    assert_eq!(fact["telemetry"]["retrieval_count"], json!(0));
    assert_eq!(fact["telemetry"]["access_count"], json!(0));
    assert_eq!(fact["telemetry"]["helpful_count"], json!(0));
    assert_eq!(fact["telemetry"]["unhelpful_count"], json!(0));
    assert_eq!(fact["telemetry"]["last_retrieved_at"], Value::Null);
    assert_eq!(fact["telemetry"]["last_recalled_at"], Value::Null);
    assert_eq!(fact["telemetry"]["last_feedback_at"], Value::Null);
}

async fn project_id(fixture: &super::memory_facts_test::FactStoreMcpFixture) -> String {
    fact_store_server(fixture)
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
async fn fact_store_list_pages_current_facts_by_identity_and_filters() {
    let fixture = setup_project().await;
    let project_id = project_id(&fixture).await;

    let empty = list(&fixture, json!({})).await;
    assert_eq!(
        empty["owner"],
        json!({"kind": "project", "project_id": project_id})
    );
    assert_eq!(empty["facts"], json!([]));
    assert_eq!(empty["next_after_fact_id"], Value::Null);

    add(
        &fixture,
        json!({
            "content": LOW_TRUST_CONTENT,
            "category": "decision",
            "trust": 0.2,
            "tags": ["zeta", "alpha"],
            "entities": ["Train schedule"],
            "source_label": "list-proof-low"
        }),
    )
    .await;
    add(
        &fixture,
        json!({
            "content": TOOL_CONTENT,
            "category": "tool",
            "entities": ["Rebuild tool"]
        }),
    )
    .await;
    let project = add(
        &fixture,
        json!({
            "content": PROJECT_CONTENT,
            "category": "project",
            "trust": 0.9,
            "tags": ["memory"],
            "entities": ["Project Phoenix", "Amari Memory"],
            "source_label": "list-proof",
            "metadata": {"plan": "list-proof"}
        }),
    )
    .await;
    let project_fact_id = added_fact_id(&project);

    let listed = list(&fixture, json!({})).await;
    assert_eq!(
        listed["owner"],
        json!({"kind": "project", "project_id": project_id})
    );
    assert_eq!(listed["next_after_fact_id"], Value::Null);
    assert_eq!(listed["facts"].as_array().expect("facts").len(), 3);
    assert_identity_order(&listed);
    let facts = by_content(&listed);
    assert_project_fact(
        facts[LOW_TRUST_CONTENT],
        &project_id,
        "decision",
        200_000,
        json!(["alpha", "zeta"]),
        json!(["Train schedule"]),
        Some("list-proof-low"),
        json!({}),
    );
    assert_project_fact(
        facts[TOOL_CONTENT],
        &project_id,
        "tool",
        500_000,
        json!([]),
        json!(["Rebuild tool"]),
        None,
        json!({}),
    );
    assert_project_fact(
        facts[PROJECT_CONTENT],
        &project_id,
        "project",
        900_000,
        json!(["memory"]),
        json!(["Amari Memory", "Project Phoenix"]),
        Some("list-proof"),
        json!({"plan": "list-proof"}),
    );

    let decision = list(&fixture, json!({"category": "decision"})).await;
    assert_eq!(decision["next_after_fact_id"], Value::Null);
    let decision_facts = by_content(&decision);
    assert_eq!(
        decision_facts.keys().copied().collect::<Vec<_>>(),
        vec![LOW_TRUST_CONTENT]
    );
    assert_eq!(decision_facts[LOW_TRUST_CONTENT]["category"], "decision");
    assert_eq!(
        decision_facts[LOW_TRUST_CONTENT]["trust_score_millionths"],
        json!(200_000)
    );

    let trusted = list(&fixture, json!({"min_trust": 0.4})).await;
    assert_eq!(trusted["next_after_fact_id"], Value::Null);
    let trusted_facts = by_content(&trusted);
    // `by_content` orders by the stored text. Uppercase "Project" sorts before "the".
    assert_eq!(
        trusted_facts.keys().copied().collect::<Vec<_>>(),
        vec![PROJECT_CONTENT, TOOL_CONTENT]
    );
    assert_eq!(
        trusted_facts[PROJECT_CONTENT]["trust_score_millionths"],
        json!(900_000)
    );
    assert_eq!(
        trusted_facts[TOOL_CONTENT]["trust_score_millionths"],
        json!(500_000)
    );

    let unmatched = list(&fixture, json!({"category": "code_area"})).await;
    assert_eq!(unmatched["facts"], json!([]));
    assert_eq!(unmatched["next_after_fact_id"], Value::Null);

    let mut after = Value::Null;
    let mut pages: Vec<String> = Vec::new();
    let mut cursors = Vec::new();
    for _ in 0..3 {
        let mut arguments = json!({"limit": 1, "min_trust": 0.0});
        if !after.is_null() {
            arguments["after_fact_id"] = after.clone();
        }
        let page = list(&fixture, arguments).await;
        assert_eq!(page["facts"].as_array().expect("page facts").len(), 1);
        let fact_id = page["facts"][0]["fact"]["fact_id"]
            .as_str()
            .expect("page fact id")
            .to_owned();
        let content = page["facts"][0]["fact"]["content"]
            .as_str()
            .expect("page content")
            .to_owned();
        if pages.len() < 2 {
            assert_eq!(page["next_after_fact_id"], fact_id);
        } else {
            assert_eq!(page["next_after_fact_id"], Value::Null);
        }
        cursors.push(fact_id);
        pages.push(content);
        after = page["next_after_fact_id"].clone();
    }
    assert!(
        cursors[0] < cursors[1] && cursors[1] < cursors[2],
        "pages advance in fact_id order: {cursors:?}"
    );
    let mut paged = pages.clone();
    paged.sort_unstable();
    assert_eq!(
        paged,
        vec![PROJECT_CONTENT, LOW_TRUST_CONTENT, TOOL_CONTENT]
    );
    let exhausted = list(
        &fixture,
        json!({"limit": 1, "min_trust": 0.0, "after_fact_id": cursors[2]}),
    )
    .await;
    assert_eq!(exhausted["facts"], json!([]));
    assert_eq!(exhausted["next_after_fact_id"], Value::Null);

    let successor = add(
        &fixture,
        json!({
            "content": "List proof: the successor decision trains on Fridays",
            "category": "decision"
        }),
    )
    .await;
    let successor_id = added_fact_id(&successor);
    let low_id = listed["facts"]
        .as_array()
        .expect("facts")
        .iter()
        .find(|projection| projection["fact"]["content"] == LOW_TRUST_CONTENT)
        .expect("low-trust fact stays listed before supersession")["fact"]["fact_id"]
        .as_str()
        .expect("low-trust fact id")
        .to_owned();
    let superseded = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_supersede",
        json!({"fact_id": low_id, "superseded_by": successor_id}),
    )
    .await
    .expect("supersede");
    assert_eq!(superseded["outcome"], "superseded");
    let after_supersede = list(&fixture, json!({})).await;
    let remaining = by_content(&after_supersede);
    assert!(
        !remaining.contains_key(LOW_TRUST_CONTENT),
        "a superseded fact leaves the default list: {after_supersede}"
    );
    assert_eq!(
        remaining["List proof: the successor decision trains on Fridays"]["category"],
        "decision"
    );
    assert_eq!(remaining[PROJECT_CONTENT]["fact_id"], project_fact_id);

    let removed = invoke_production_tool(
        &fixture,
        "tracedecay_fact_store_remove",
        json!({"fact_id": project_fact_id}),
    )
    .await
    .expect("remove");
    assert_eq!(removed["outcome"], "removed");
    let after_remove = list(&fixture, json!({})).await;
    let after_remove_facts = by_content(&after_remove);
    assert!(
        !after_remove_facts.contains_key(PROJECT_CONTENT),
        "a removed fact leaves the default list: {after_remove}"
    );
    assert_eq!(after_remove_facts[TOOL_CONTENT]["category"], "tool");
    assert_eq!(
        after_remove_facts["List proof: the successor decision trains on Fridays"]["content"],
        "List proof: the successor decision trains on Fridays"
    );

    close_test_graph(fixture).await;
}

#[tokio::test]
async fn fact_store_list_user_scope_lists_only_profile_facts() {
    let fixture = setup_project().await;
    let project_id = project_id(&fixture).await;

    add(
        &fixture,
        json!({
            "content": PROJECT_CONTENT,
            "category": "project"
        }),
    )
    .await;
    add(
        &fixture,
        json!({
            "content": USER_CONTENT,
            "category": "user_pref",
            "memory_scope": "user",
            "trust": 0.8,
            "tags": ["prefs"]
        }),
    )
    .await;

    let project_list = list(&fixture, json!({})).await;
    assert_eq!(
        project_list["owner"],
        json!({"kind": "project", "project_id": project_id})
    );
    let project_facts = by_content(&project_list);
    assert_eq!(
        project_facts.keys().copied().collect::<Vec<_>>(),
        vec![PROJECT_CONTENT]
    );
    assert_eq!(project_facts[PROJECT_CONTENT]["category"], "project");

    let user_list = list(&fixture, json!({"memory_scope": "user"})).await;
    assert_eq!(user_list["owner"], json!({"kind": "profile"}));
    assert_eq!(user_list["next_after_fact_id"], Value::Null);
    let user_facts = by_content(&user_list);
    assert_eq!(
        user_facts.keys().copied().collect::<Vec<_>>(),
        vec![USER_CONTENT]
    );
    assert_eq!(user_facts[USER_CONTENT]["category"], "user_pref");
    assert_eq!(
        user_facts[USER_CONTENT]["trust_score_millionths"],
        json!(800_000)
    );
    assert_eq!(user_facts[USER_CONTENT]["tags"], json!(["prefs"]));
    assert_eq!(
        user_facts[USER_CONTENT]["owner"],
        json!({"kind": "profile"})
    );
    assert_eq!(user_facts[USER_CONTENT]["entities"], json!([]));

    close_test_graph(fixture).await;
}

fn assert_invalid_request_problem(response: &Value) {
    assert!(response["error"].is_null(), "{response}");
    assert_eq!(response["result"]["isError"], true, "{response}");
    let text = extract_real_server_text(&response["result"]);
    let envelope: Value = serde_json::from_str(text).expect("problem envelope");
    let problem = &envelope["problem"];
    assert_eq!(problem["kind"], "invalid_request");
    assert_eq!(problem["code"], "application.retained.invalid-request");
    assert_eq!(
        problem["message"],
        "The retained operation request is invalid."
    );
    assert_eq!(
        problem["diagnostic"],
        json!({
            "code": "application.retained.invalid-request",
            "message": "The retained operation request is invalid."
        })
    );
    assert_eq!(problem["retry"], "never");
    assert_eq!(problem["retryable"], false);
    assert_eq!(problem["revision"], json!(1));
    assert_eq!(problem["legal_actions"], json!(["correct_request"]));
    assert_eq!(
        &response["result"]["structuredContent"]["problem"]["kind"],
        "invalid_request"
    );
    assert_eq!(
        &response["result"]["structuredContent"]["problem"]["message"],
        "The retained operation request is invalid."
    );
}

#[tokio::test]
async fn fact_store_list_rejects_malformed_selectors_and_out_of_range_limits() {
    let fixture = setup_project().await;
    let server = fact_store_server(&fixture);

    let unknown_field = handle_real_server_tool_call_raw(
        server,
        "tracedecay_fact_store_list",
        json!({"query": "identity"}),
    )
    .await;
    assert_eq!(
        unknown_field["error"]["message"],
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_list: unknown field `query`"
    );
    assert_eq!(
        unknown_field["error"]["data"]["tool"],
        "tracedecay_fact_store_list"
    );

    let unknown_scope = handle_real_server_tool_call_raw(
        server,
        "tracedecay_fact_store_list",
        json!({"memory_scope": "session"}),
    )
    .await;
    assert_eq!(
        unknown_scope["error"]["message"],
        "tool execution failed: config error: invalid retained application request for tracedecay_fact_store_list: unknown variant `session`, expected `project` or `user`"
    );

    for arguments in [
        json!({"limit": 0}),
        json!({"limit": 201}),
        json!({"min_trust": 1.5}),
    ] {
        let refused =
            handle_real_server_tool_call_raw(server, "tracedecay_fact_store_list", arguments).await;
        assert_invalid_request_problem(&refused);
    }

    let still_empty = list(&fixture, json!({})).await;
    assert_eq!(still_empty["facts"], json!([]));
    assert_eq!(still_empty["next_after_fact_id"], Value::Null);

    close_test_graph(fixture).await;
}
