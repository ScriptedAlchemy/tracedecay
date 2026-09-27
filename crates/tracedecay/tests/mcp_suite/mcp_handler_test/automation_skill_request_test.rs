//! The automation-run, managed-skill, Hermes-inventory and analytics reads
//! decode their arguments against a typed request over the production MCP
//! `tools/call` path: an argument outside the request contract is refused
//! instead of being silently defaulted or ignored, and a valid call still
//! answers from the real ledger, profile skill store and Hermes install.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::PathBuf;
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

/// A curate receipt returns at admission; the run is readable through
/// `tracedecay_automation_run_view` once it settles.
async fn settled_run(fixture: &ProductionCompositionFixture, run_id: &str) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let response = fixture
            .harness
            .call_tool(
                &fixture.project_root,
                "tracedecay_automation_run_view",
                json!({"run_id": run_id, "format": "json"}),
            )
            .await
            .unwrap_or_else(|error| panic!("automation run view invocation failed: {error}"));
        let settled = response.error.is_none()
            && response
                .result
                .as_ref()
                .is_some_and(|result| result["isError"] != json!(true));
        if settled {
            return call_json(
                fixture,
                "tracedecay_automation_run_view",
                json!({"run_id": run_id, "format": "json"}),
            )
            .await["run"]
                .clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run {run_id} never settled"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

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
        "{tool_name} returned a production MCP error: {:?}",
        response.error.as_ref().map(|error| &error.message)
    );
    let result = response
        .result
        .unwrap_or_else(|| panic!("{tool_name} returned no production MCP result"));
    crate::support::refusal_problem(&result).clone()
}

fn assert_invalid_request(problem: &Value, message: &str) {
    assert_eq!(problem["kind"], "invalid_request");
    assert_eq!(problem["code"], "application.surface.invalid_request");
    assert_eq!(problem["message"], message);
}

/// One project whose automation ledger holds exactly one application run:
/// the memory curator, run through `tracedecay_fact_store_curate` on an empty
/// store, which settles as a skipped `nothing_to_review` terminal.
struct CuratedProject {
    fixture: ProductionCompositionFixture,
    run_id: String,
    /// The run's terminal row, read from the ledger file the run wrote.
    terminal_row: Value,
}

async fn curated_project() -> CuratedProject {
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
    for _attempt in 0..20 {
        let envelope = call_json(
            &fixture,
            "tracedecay_fact_store_curate",
            json!({
                "fact_review_limit": 7,
                "min_confidence_millionths": 500_000,
                "format": "json",
            }),
        )
        .await;
        let receipt = &envelope["outcome"]["value"]["payload"];
        assert_eq!(receipt["state"], "started", "{envelope}");
        let run_id = receipt["run_id"]
            .as_str()
            .expect("curate run id")
            .to_owned();
        let settled = settled_run(&fixture, &run_id).await;
        if settled["error"] == "scheduler_lock_active" {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }
        assert_eq!(settled["error"], "nothing_to_review", "{settled}");
        let terminal_row = fs::read_to_string(dashboard_root.join("automation_runs.jsonl"))
            .expect("automation run ledger")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str::<Value>(line).expect("ledger row JSON"))
            .rfind(|row| row["run_id"] == run_id)
            .unwrap_or_else(|| panic!("ledger must record run {run_id}"));
        return CuratedProject {
            fixture,
            run_id,
            terminal_row,
        };
    }
    panic!("the curator lock never released");
}

async fn fixture_with_keeper_skill() -> ProductionCompositionFixture {
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
    fixture
}

#[tokio::test]
async fn automation_run_list_refuses_a_mistyped_limit() {
    let CuratedProject {
        fixture,
        run_id,
        terminal_row,
    } = curated_project().await;

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
            "backend": terminal_row["backend"],
            "model": null,
            "status": "skipped",
            "reviewed_count": 0,
            "accepted_count": 0,
            "rejected_count": 0,
            "skipped_count": 1,
            "error": "nothing_to_review",
            "started_at": terminal_row["started_at"],
            "completed_at": terminal_row["completed_at"],
            "artifact_kinds": [],
        })]
    );
    assert_eq!(listed["scope"], "active_project");
    assert_eq!(listed["limit"], 50);
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_automation_run_list",
            json!({"limit": "5"}),
        )
        .await,
        "invalid arguments for tracedecay_automation_run_list: invalid type: string \"5\", expected u32",
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn automation_run_view_refuses_an_unknown_field() {
    let CuratedProject {
        fixture,
        run_id,
        terminal_row,
    } = curated_project().await;

    let viewed = call_json(
        &fixture,
        "tracedecay_automation_run_view",
        json!({"run_id": run_id, "format": "json"}),
    )
    .await;
    assert_eq!(viewed["run"], terminal_row);
    assert_eq!(viewed["run"]["status"], "skipped");
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_automation_run_view",
            json!({"run_id": run_id, "verbose": true}),
        )
        .await,
        "invalid arguments for tracedecay_automation_run_view: unknown field `verbose`, expected `run_id`",
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn automation_run_artifact_view_refuses_an_unknown_kind() {
    let CuratedProject {
        fixture, run_id, ..
    } = curated_project().await;

    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_automation_run_artifact_view",
            json!({"run_id": run_id, "kind": "trace"}),
        )
        .await,
        "invalid arguments for tracedecay_automation_run_artifact_view: unknown variant `trace`, expected one of `traces`, `feedback`, `generated_evals`, `validation_gate`, `optimizer_diagnosis`, `codex_handoff`",
    );
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_automation_run_artifact_view",
            json!({"run_id": run_id, "kind": "traces"}),
        )
        .await,
        &format!("automation run artifact not found: {run_id}/traces"),
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn analytics_refuses_an_unknown_field() {
    let CuratedProject { fixture, .. } = curated_project().await;

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
    assert_invalid_request(
        &refusal(&fixture, "tracedecay_analytics", json!({"windowdays": 7})).await,
        "invalid arguments for tracedecay_analytics: unknown field `windowdays`, expected one of `scope`, `window_days`, `section`",
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn skill_list_refuses_an_unknown_field() {
    let fixture = fixture_with_keeper_skill().await;

    let listed = call_json(&fixture, "tracedecay_skill_list", json!({"format": "json"})).await;
    let ids: Vec<&Value> = listed["skills"]
        .as_array()
        .expect("skills")
        .iter()
        .map(|skill| &skill["metadata"]["id"])
        .collect();
    assert_eq!(ids, vec![&json!("keeper")]);
    assert_eq!(listed["count"], 1);
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_skill_list",
            json!({"include_bodies": true}),
        )
        .await,
        "invalid arguments for tracedecay_skill_list: unknown field `include_bodies`, expected `state` or `include_body`",
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn skill_view_refuses_an_unknown_field() {
    let fixture = fixture_with_keeper_skill().await;

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
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_skill_view",
            json!({"id": "keeper", "include_support": true}),
        )
        .await,
        "invalid arguments for tracedecay_skill_view: unknown field `include_support`, expected `id` or `include_support_files`",
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn hermes_skill_bridge_refuses_an_unknown_field() {
    let fixture = production_composition_fixture().await;
    let home: PathBuf = ProductionProjectCompositionHarnessV1::transcript_source_home(
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
    assert_invalid_request(
        &refusal(
            &fixture,
            "tracedecay_hermes_skill_bridge",
            json!({"include_skill_body": true}),
        )
        .await,
        "invalid arguments for tracedecay_hermes_skill_bridge: unknown field `include_skill_body`, expected `include_skill_bodies` or `include_pending_payloads`",
    );

    fixture.shutdown().await;
}
