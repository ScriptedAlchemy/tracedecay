//! The automation-run, managed-skill, Hermes-inventory and analytics reads
//! decode their arguments against a typed request over the production MCP
//! `tools/call` path: an argument outside the request contract is refused
//! instead of being silently defaulted or ignored, and a valid call still
//! answers from the real ledger, profile skill store and Hermes install.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_automation_runtime::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, SkillInstallTarget,
    create_managed_skill,
};

use crate::support::{ProductionCompositionFixture, production_composition_fixture};

async fn call_json(
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
        "{tool_name} returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result"));
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool_name} returned no text block: {result}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{tool_name} JSON: {error}\n{text}"))
}

async fn refusal(
    fixture: &ProductionCompositionFixture,
    tool_name: &str,
    arguments: Value,
) -> String {
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} production invocation failed: {error}"));
    assert!(
        response.result.is_none(),
        "{tool_name} must refuse the request, answered {:?}",
        response.result
    );
    response
        .error
        .unwrap_or_else(|| panic!("{tool_name} must refuse the request"))
        .message
}

/// Runs the memory curator once through `tracedecay_fact_store_curate` and
/// returns its run id. An empty store settles the run as a skipped
/// `nothing_to_review` terminal in the project's automation ledger.
async fn curate_once(fixture: &ProductionCompositionFixture) -> String {
    for _attempt in 0..20 {
        let envelope = call_json(
            fixture,
            "tracedecay_fact_store_curate",
            json!({"fact_review_limit": 7, "min_confidence_millionths": 500_000}),
        )
        .await;
        let run = &envelope["outcome"]["value"]["payload"];
        if run["terminal"]["reason"] == "scheduler_lock_active" {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        assert_eq!(run["terminal"]["reason"], "nothing_to_review", "{envelope}");
        return run["run_id"].as_str().expect("curate run id").to_owned();
    }
    panic!("the curator lock never released");
}

/// The terminal ledger row for `run_id`, read from the file the run wrote.
fn terminal_ledger_row(dashboard_root: &Path, run_id: &str) -> Value {
    fs::read_to_string(dashboard_root.join("automation_runs.jsonl"))
        .expect("automation run ledger")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("ledger row JSON"))
        .rfind(|row| row["run_id"] == run_id)
        .unwrap_or_else(|| panic!("ledger must record run {run_id}"))
}

#[tokio::test]
async fn automation_run_reads_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture().await;
    let dashboard_root = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production automation server")
        .cg()
        .await
        .store_layout()
        .dashboard_root
        .clone();
    let run_id = curate_once(&fixture).await;
    let row = terminal_ledger_row(&dashboard_root, &run_id);

    let listed = call_json(
        &fixture,
        "tracedecay_automation_run_list",
        json!({"format": "json"}),
    )
    .await;
    let application_runs: Vec<&Value> = listed["runs"]
        .as_array()
        .expect("runs")
        .iter()
        .filter(|run| run["trigger"] == "application")
        .collect();
    assert_eq!(
        application_runs,
        vec![&json!({
            "run_id": run_id,
            "task": "memory_curator",
            "task_key": "memory_curator",
            "trigger": "application",
            "backend": row["backend"],
            "model": null,
            "status": "skipped",
            "reviewed_count": 0,
            "accepted_count": 0,
            "rejected_count": 0,
            "skipped_count": 1,
            "error": "nothing_to_review",
            "started_at": row["started_at"],
            "completed_at": row["completed_at"],
            "artifact_kinds": [],
        })]
    );
    assert_eq!(listed["scope"], "active_project");
    assert_eq!(listed["limit"], 50);
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_automation_run_list",
            json!({"limit": "5"})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_automation_run_list: invalid type: string \"5\", expected u32"
    );

    let viewed = call_json(
        &fixture,
        "tracedecay_automation_run_view",
        json!({"run_id": run_id, "format": "json"}),
    )
    .await;
    assert_eq!(viewed["run"], row);
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_automation_run_view",
            json!({"run_id": run_id, "verbose": true})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_automation_run_view: unknown field `verbose`, expected `run_id`"
    );

    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_automation_run_artifact_view",
            json!({"run_id": run_id, "kind": "trace"})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_automation_run_artifact_view: unknown variant `trace`, expected one of `traces`, `feedback`, `generated_evals`, `validation_gate`, `optimizer_diagnosis`, `codex_handoff`"
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_automation_run_artifact_view",
            json!({"run_id": run_id, "kind": "traces"})
        )
        .await,
        format!(
            "tool execution failed: config error: automation run artifact not found: {run_id}/traces"
        )
    );

    let analytics = call_json(
        &fixture,
        "tracedecay_analytics",
        json!({"section": "automation", "format": "json"}),
    )
    .await;
    assert_eq!(
        analytics["automation"]["by_job"],
        json!([{"job": "memory_curator", "succeeded": 0, "failed": 0, "skipped": 1, "other": 0}])
    );
    assert_eq!(
        refusal(&fixture, "tracedecay_analytics", json!({"windowdays": 7})).await,
        "tool execution failed: config error: invalid arguments for tracedecay_analytics: unknown field `windowdays`, expected one of `scope`, `window_days`, `section`"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn skill_reads_refuse_arguments_outside_their_typed_request() {
    let fixture = production_composition_fixture().await;
    create_managed_skill(
        fixture.harness.profile_root(),
        ManagedSkillDraft {
            id: "keeper".to_owned(),
            title: "Keeper".to_owned(),
            summary: "Keeps the ledger honest.".to_owned(),
            routing_description: "Use when the ledger needs keeping.".to_owned(),
            category: "maintenance".to_owned(),
            targets: vec![SkillInstallTarget::Codex],
            body_markdown: "Keep the ledger.".to_owned(),
            support_files: Vec::new(),
            provenance: ManagedSkillProvenance {
                source: ManagedSkillSource::User,
                actor: "typed-request-proof".to_owned(),
                run_id: None,
            },
        },
    )
    .await
    .expect("seed managed skill");

    let listed = call_json(&fixture, "tracedecay_skill_list", json!({"format": "json"})).await;
    let ids: Vec<&Value> = listed["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .map(|skill| &skill["metadata"]["id"])
        .collect();
    assert_eq!(ids, vec![&json!("keeper")]);
    assert_eq!(listed["count"], 1);
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_skill_list",
            json!({"include_bodies": true})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_skill_list: unknown field `include_bodies`, expected `state` or `include_body`"
    );

    let viewed = call_json(
        &fixture,
        "tracedecay_skill_view",
        json!({"id": "keeper", "format": "json"}),
    )
    .await;
    assert_eq!(viewed["skill"]["metadata"]["id"], "keeper");
    assert_eq!(viewed["skill"]["body_markdown"], "Keep the ledger.");
    assert_eq!(viewed["usage_summary"]["view_count"], 1);
    assert_eq!(viewed["support_files_included"], false);
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_skill_view",
            json!({"id": "keeper", "include_support": true})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_skill_view: unknown field `include_support`, expected `id` or `include_support_files`"
    );

    let home = ProductionProjectCompositionHarnessV1::transcript_source_home(
        fixture.harness.isolation_root(),
    )
    .expect("composition home");
    let skill_dir = home.join(".hermes/skills/keeper");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: keeper\ndescription: Keeps the ledger\n---\n\nKeep it.\n",
    )
    .unwrap();
    let bridge = call_json(
        &fixture,
        "tracedecay_hermes_skill_bridge",
        json!({"format": "json"}),
    )
    .await;
    assert_eq!(bridge["bridge"]["skill_count"], 1);
    assert_eq!(bridge["bridge"]["skills"][0]["name"], "keeper");
    assert_eq!(
        bridge["bridge"]["skills"][0]["description"],
        "Keeps the ledger"
    );
    assert_eq!(
        refusal(
            &fixture,
            "tracedecay_hermes_skill_bridge",
            json!({"include_skill_body": true})
        )
        .await,
        "tool execution failed: config error: invalid arguments for tracedecay_hermes_skill_bridge: unknown field `include_skill_body`, expected `include_skill_bodies` or `include_pending_payloads`"
    );

    fixture.shutdown().await;
}
