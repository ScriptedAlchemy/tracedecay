//! The project-info, registry, and runtime reads, and the admin sync, admin
//! project, and admin CLI actions the CLI requests, decode their arguments against a typed request over the
//! production MCP `tools/call` path: an argument outside the request contract
//! is refused instead of being silently ignored, and a valid call answers the
//! typed result through its owner.

#![cfg(feature = "test-transport")]

use serde_json::{Value, json};
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

use crate::support::{ProductionCompositionFixture, production_composition_fixture};

async fn call_json(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    mut arguments: Value,
) -> Value {
    arguments["format"] = json!("json");
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name} returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result"));
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool_name} returned no text: {result}"));
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool_name} JSON: {error}: {text}"));
    if payload["truncated"] != true {
        return payload;
    }
    // The full body behind a truncated envelope, read back through
    // `tracedecay_retrieve` the way a host recovers it.
    let mut body = String::new();
    let mut offset = 0;
    loop {
        let page = fixture
            .harness
            .call_tool(
                &fixture.project_root,
                "tracedecay_retrieve",
                json!({ "handle": payload["handle"], "format": "json", "offset": offset }),
            )
            .await
            .expect("retrieve invocation");
        let page: Value = serde_json::from_str(
            page.result.expect("retrieve result")["content"][0]["text"]
                .as_str()
                .expect("retrieve text"),
        )
        .expect("retrieve page JSON");
        body.push_str(page["content"].as_str().expect("retrieved content"));
        match page["next_offset"].as_u64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    serde_json::from_str(&body).unwrap_or_else(|error| panic!("{tool_name} body JSON: {error}"))
}

/// The owner's refusal record for one call: its kind, code, and message.
async fn refusal(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.error.is_none(),
        "{tool_name} must refuse through its owner's problem record: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result"));
    assert_eq!(result["isError"], true, "{tool_name} must refuse: {result}");
    let problem = &result["problem"];
    json!({
        "kind": problem["kind"],
        "code": problem["code"],
        "message": problem["message"],
    })
}

fn invalid(tool_name: &str, detail: &str) -> Value {
    json!({
        "kind": "invalid_request",
        "code": "application.surface.invalid_request",
        "message": format!("invalid arguments for {tool_name}: {detail}"),
    })
}

#[tokio::test]
async fn info_and_runtime_reads_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture().await;
    let root = canonical_existing_identity(&fixture.project_root)
        .expect("canonical project root")
        .display()
        .to_string();
    let project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("registered project id");
    let registry_path = canonical_existing_identity(fixture.harness.profile_root())
        .expect("canonical profile root")
        .join("global.db")
        .display()
        .to_string();

    let admission = call_json(
        &fixture,
        "tracedecay_status",
        json!({"admission_only": true}),
    )
    .await;
    assert_eq!(
        (&admission["project_admitted"], &admission["project_root"]),
        (&json!(true), &json!(root))
    );
    assert_eq!(admission["server"]["errors"], 0, "{admission}");
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_status",
            json!({"include_diagnostics": true})
        )
        .await,
        invalid(
            "tracedecay_status",
            "unknown field `include_diagnostics`, expected one of `admission_only`, `include_branch_diagnostics`, `include_storage_health`, `include_session_ingest`, `include_staleness`, `wait_for`"
        )
    );

    let active = call_json(&fixture, "tracedecay_active_project", json!({})).await;
    assert_eq!(
        (
            &active["project_id"],
            &active["project_root"],
            &active["resolution_source"],
            &active["storage"]["class"],
            &active["storage"]["mode"],
        ),
        (
            &json!(project_id),
            &json!(root),
            &json!("active_project"),
            &json!("code_project"),
            &json!("profile_sharded"),
        )
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_active_project",
            json!({"project": "other"})
        )
        .await,
        invalid(
            "tracedecay_active_project",
            "unknown field `project`, there are no fields"
        )
    );

    assert_eq!(
        call_json(&fixture, "tracedecay_remote_status", json!({})).await,
        json!({"kind": "unconfigured"})
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_remote_status",
            json!({"kind": "observed"})
        )
        .await,
        invalid(
            "tracedecay_remote_status",
            "unknown field `kind`, there are no fields"
        )
    );

    let runtime = call_json(
        &fixture,
        "tracedecay_runtime",
        json!({"doctor_report": true}),
    )
    .await;
    let doctor = &runtime["doctor_report"];
    assert_eq!(doctor["kind"], "observed", "{doctor}");
    assert_eq!(
        doctor
            .as_object()
            .map(|report| report.keys().cloned().collect::<Vec<_>>()),
        Some(
            [
                "kind",
                "language_servers",
                "report",
                "schema_convergences",
                "table_growth_evidence"
            ]
            .map(str::to_owned)
            .to_vec()
        ),
        "{doctor}"
    );
    assert_eq!(
        doctor["report"]
            .as_object()
            .map(|report| report.keys().cloned().collect::<Vec<_>>()),
        Some(["coverage", "entries"].map(str::to_owned).to_vec()),
        "{doctor}"
    );
    assert_eq!(
        refusal(&fixture, "tracedecay_runtime", json!({"doctor": true})).await,
        invalid(
            "tracedecay_runtime",
            "unknown field `doctor`, expected one of `authority_audit`, `session_temporal_health`, `doctor_report`, `session_ingest_health`, `startup_health`"
        )
    );

    let listing = call_json(&fixture, "tracedecay_project_list", json!({"limit": 5})).await;
    assert_eq!(
        (
            &listing["status"],
            &listing["title"],
            &listing["registry_path"],
            &listing["limit"],
            &listing["truncated"],
            &listing["summary"],
        ),
        (
            &json!("ok"),
            &json!("registered projects"),
            &json!(registry_path),
            &json!(5),
            &json!(false),
            &json!({"project_count": 1, "repo_count": 1, "truncated": false}),
        )
    );
    assert_eq!(
        (
            &listing["projects"][0]["project_id"],
            &listing["projects"][0]["is_active"],
        ),
        (&json!(project_id), &json!(true)),
        "{listing}"
    );
    assert_eq!(
        refusal(&fixture, "tracedecay_project_list", json!({"limt": 5})).await,
        invalid(
            "tracedecay_project_list",
            "unknown field `limt`, expected `limit`"
        )
    );
    assert_eq!(
        refusal(&fixture, "tracedecay_project_list", json!({"limit": 2.5})).await,
        invalid(
            "tracedecay_project_list",
            "invalid type: floating point `2.5`, expected usize"
        )
    );

    assert_eq!(
        refusal(&fixture, "tracedecay_project_search", json!({"query": 12})).await,
        invalid(
            "tracedecay_project_search",
            "invalid type: integer `12`, expected a string"
        )
    );

    let context = call_json(&fixture, "tracedecay_project_context", json!({})).await;
    assert_eq!(
        (
            &context["status"],
            &context["is_active"],
            &context["registry_path"],
            &context["project"]["project_id"],
        ),
        (
            &json!("ok"),
            &json!(true),
            &json!(registry_path),
            &json!(project_id),
        ),
        "{context}"
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_project_context",
            json!({"project_selector": {"project": "other"}})
        )
        .await,
        invalid(
            "tracedecay_project_context",
            "unknown field `project`, expected `project_id`"
        )
    );

    fixture.harness.shutdown().await;
}

/// `tracedecay_admin_sync` is the owner's side effect the CLI's `init` and
/// `sync` request by name: it answers the code-index scheduler's typed
/// admission and refuses any argument, since it always reconciles the served
/// project.
#[tokio::test]
async fn admin_sync_answers_the_scheduler_admission_and_refuses_arguments() {
    let fixture = production_composition_fixture().await;
    let root = canonical_existing_identity(&fixture.project_root)
        .expect("canonical project root")
        .display()
        .to_string();

    assert_eq!(
        call_json(&fixture, "tracedecay_admin_sync", json!({})).await,
        json!({
            "reconcile_scope": "authoritative_project",
            "status": "queued",
            "project_root": root,
        })
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_admin_sync",
            json!({"project_root": "/elsewhere"})
        )
        .await,
        invalid(
            "tracedecay_admin_sync",
            "unknown field `project_root`, there are no fields"
        )
    );

    fixture.harness.shutdown().await;
}

/// `tracedecay_admin_project` is the owner's bookkeeping the CLI requests by
/// name: each action answers its typed result through the project's owner,
/// a profile reconcile reaches the daemon's profile owner from a project
/// connection, and an argument outside the action's request is refused
/// instead of being ignored or normalized.
#[tokio::test]
async fn admin_project_answers_through_its_owners_and_refuses_arguments_outside_its_action() {
    let fixture = production_composition_fixture().await;
    let project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("registered project id");
    let store_root = canonical_existing_identity(fixture.harness.profile_root())
        .expect("canonical profile root")
        .join("projects")
        .join(&project_id);
    let admin = |arguments: Value| call_json(&fixture, "tracedecay_admin_project", arguments);

    assert_eq!(
        admin(json!({"action": "counter_reset"})).await,
        json!({"reset": true})
    );
    assert_eq!(
        admin(json!({"action": "counter_get"})).await,
        json!({"counter": 0})
    );
    assert_eq!(
        admin(json!({"action": "automatic_fact_receipt_list", "state": "applied", "limit": 5}))
            .await,
        json!({
            "availability": {"state": "available"},
            "count": 0,
            "receipts": [],
            "next_after_apply_id": null,
        })
    );
    // The fixture's project runs no automation scheduler.
    assert_eq!(
        admin(json!({"action": "automation_reconcile", "scope": "project"})).await,
        json!({"scope": "project", "outcome": "owner_unavailable"})
    );
    assert_eq!(
        admin(json!({"action": "automation_reconcile", "scope": "profile"})).await,
        json!({
            "scope": "profile",
            "cached_owners": 1,
            "outcomes": [{
                "project_id": project_id,
                "store_root": store_root.display().to_string(),
                "graph_db_path": store_root.join("tracedecay.db").display().to_string(),
                "scope_prefix": null,
                "outcome": "owner_unavailable",
            }],
            "uncached_projects": "deferred_until_project_startup",
        })
    );

    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_admin_project",
            json!({"action": "counter_get", "project": "/elsewhere"})
        )
        .await,
        invalid(
            "tracedecay_admin_project",
            "unknown field `project`, there are no fields"
        )
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_admin_project",
            json!({"action": "automatic_fact_receipt_list", "state": " applied ", "limit": 5})
        )
        .await,
        invalid(
            "tracedecay_admin_project",
            "unknown variant ` applied `, expected `applied` or `quarantined`"
        )
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_admin_project",
            json!({"action": "automatic_fact_receipt_view", "id": "automatic-fact.missing"})
        )
        .await,
        json!({
            "kind": "invalid_request",
            "code": "application.surface.invalid_request",
            "message": "automatic fact receipt not found",
        })
    );

    fixture.harness.shutdown().await;
}

/// `tracedecay_admin_cli` is the owner's profile maintenance the CLI's
/// registry, storage, cost, and session commands request by name. Each action
/// answers its established body through the owner, and an argument outside
/// the named action is refused instead of being silently ignored.
#[tokio::test]
async fn admin_cli_actions_answer_through_the_owner_and_refuse_arguments_outside_the_action() {
    let fixture = production_composition_fixture().await;
    let root = canonical_existing_identity(&fixture.project_root)
        .expect("canonical project root")
        .display()
        .to_string();
    let project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("registered project id");

    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_admin_cli",
            json!({"action": "registry_empty"})
        )
        .await,
        json!({"empty": false})
    );
    // The ledger total only grows: each update records the larger total.
    let first = call_json(
        &fixture,
        "tracedecay_admin_cli",
        json!({"action": "registry_update", "tokens": 42}),
    )
    .await;
    assert_eq!(first["current"], 42, "{first}");
    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_admin_cli",
            json!({"action": "registry_update", "tokens": 50}),
        )
        .await,
        json!({"previous": 42, "current": 50})
    );
    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_admin_cli",
            json!({"action": "registry_project_tokens", "project_args": [root]}),
        )
        .await,
        json!({"projects": [{"project": root, "tokens": 50}]})
    );
    let context = call_json(
        &fixture,
        "tracedecay_admin_cli",
        json!({"action": "registry_context"}),
    )
    .await;
    assert_eq!(
        (
            &context["status"],
            &context["project"]["project_id"],
            &context["project"]["canonical_root"],
        ),
        (&json!("ok"), &json!(project_id), &json!(root)),
        "{context}"
    );

    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_admin_cli",
            json!({"action": "registry_empty", "project_root": "/elsewhere"})
        )
        .await,
        invalid(
            "tracedecay_admin_cli",
            "unknown field `project_root`, there are no fields"
        )
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_admin_cli",
            json!({"action": "registry_update", "tokens": 99, "project_arg": "/elsewhere"})
        )
        .await,
        invalid(
            "tracedecay_admin_cli",
            "unknown field `project_arg`, expected `tokens`"
        )
    );
    // The refused write left the recorded total untouched.
    assert_eq!(
        call_json(
            &fixture,
            "tracedecay_admin_cli",
            json!({"action": "registry_project_tokens", "project_args": [root]}),
        )
        .await,
        json!({"projects": [{"project": root, "tokens": 50}]})
    );

    fixture.harness.shutdown().await;
}
