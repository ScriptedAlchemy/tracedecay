use std::collections::BTreeSet;

use tempfile::TempDir;
use tracedecay_contracts::{
    ApplicationOutcome, ConfigurationProtectedApplyRequestV1,
    ConfigurationProtectedPreviewRequestV1,
};
use tracedecay_domain::configuration::{
    AccessRuleId, AuthorityRef, ConfigurationIdempotencyKey, ProtectedChange, RuleEffect,
    ScopeAccessRule, ScopeAccessSubjectV1, SourceKindV1,
};

use super::journey_test_support::{git, tool_payload};
use super::*;

fn json_arguments(request: impl serde::Serialize) -> serde_json::Value {
    let mut arguments = serde_json::to_value(request).expect("tool request");
    arguments["format"] = serde_json::json!("json");
    arguments
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn denying_work_generation_preserves_restart_index_and_search() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn read_only_search_probe() -> bool { true }\n",
    )
    .expect("project source file");
    git(&project, &["init", "--quiet"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "fixture", "--quiet"]);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("initial production composition");
    let graph = harness.server(&project).expect("project server").cg().await;
    let target = graph.configuration_runtime().configuration_target().clone();
    let revision = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("current configuration")
        .revision_id()
        .clone();
    drop(graph);
    let change = ProtectedChange::UpsertAccessRule(
        ScopeAccessRule::new(
            AccessRuleId::new("access-rule.deny-work-generation").expect("access rule identity"),
            ScopeAccessSubjectV1 {
                actor: None,
                operation: None,
                source_kind: Some(SourceKindV1::Cursor),
            },
            AuthorityRef::Project(target.project_id),
            BTreeSet::from([tracedecay_domain::CapabilityId::new(
                "capability.work.generate_proposal",
            )
            .expect("generate proposal capability")]),
            RuleEffect::Deny,
            None,
        )
        .expect("deny-only Work rule"),
    );
    let preview = harness
        .call_tool(
            &project,
            "tracedecay_configuration_protected_preview",
            json_arguments(ConfigurationProtectedPreviewRequestV1 {
                change,
                expected_revision: revision.clone(),
            }),
        )
        .await
        .expect("protected preview response");
    let preview = tool_payload(&preview);
    let plan: tracedecay_domain::configuration::ProtectedChangePlan =
        serde_json::from_value(preview["outcome"]["value"]["payload"].clone())
            .expect("protected change plan");
    let apply = harness
        .call_tool(
            &project,
            "tracedecay_configuration_protected_apply",
            json_arguments(ConfigurationProtectedApplyRequestV1 {
                plan_id: plan.plan_id,
                expected_base_revision_id: revision,
                operation_digest: plan.operation_digest,
                idempotency_key: ConfigurationIdempotencyKey::new(
                    "configuration.deny-work-generation",
                )
                .expect("configuration idempotency key"),
            }),
        )
        .await
        .expect("protected apply response");
    assert!(
        matches!(
            serde_json::from_value::<tracedecay_contracts::ApplicationEnvelope<serde_json::Value>>(
                tool_payload(&apply)
            )
            .expect("configuration apply envelope")
            .outcome,
            ApplicationOutcome::Effect(_)
        ),
        "supported protected configuration must commit the deny-only rule"
    );
    harness.shutdown().await;

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("read-only production composition must restart");
    let response = harness
        .call_tool(
            &project,
            "tracedecay_search",
            serde_json::json!({
                "query": "read_only_search_probe",
                "limit": 10,
                "format": "json"
            }),
        )
        .await
        .expect("search response");
    let payload = tool_payload(&response);
    assert!(
        payload["results"]
            .as_array()
            .expect("search results")
            .iter()
            .any(|result| result["display"]["name"] == "read_only_search_probe"),
        "read-only project open must retain exact indexed search: {payload}"
    );

    harness.shutdown().await;
}
