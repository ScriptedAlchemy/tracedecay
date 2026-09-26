//! Host-facing behavior of `tracedecay_configuration_protected_preview`.
//!
//! The tool is a dry-run: the answer is a redacted plan bound to the revision
//! the caller supplied, and a wrong revision or an invalid change is a typed
//! problem rather than a committed setting. Callers observe that through MCP
//! `tools/call`, which is the path this journey drives.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay_contracts::{
    ConfigurationProtectedApplyRequestV1, ConfigurationProtectedPreviewRequestV1,
};
use tracedecay_domain::configuration::{
    AccessRuleId, AuthorityRef, ConfigurationIdempotencyKey, ConfigurationRevisionId,
    ProtectedChange, ProtectedChangePlan, RuleEffect, ScopeAccessRule, ScopeAccessSubjectV1,
    ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::{CapabilityId, LocatorDigest, ManifestDigest};

use super::journey_test_support::{git, tool_answer};
use super::*;

const ACCESS_RULE_ID: &str = "access-rule.preview-cursor-deny";
const DENIED_CAPABILITY: &str = "capability.work.generate_proposal";
const ABSENT_BINDING_ID: &str = "source-binding.preview-absent";
const STALE_REVISION: &str = "configuration.revision.protected-preview-not-current";

fn initialize_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("project source");
    std::fs::write(project.join("src/lib.rs"), "pub fn preview_probe() {}\n")
        .expect("project source file");
    git(project, &["init", "--quiet"]);
}

fn preview_arguments(change: &ProtectedChange, revision: &ConfigurationRevisionId) -> Value {
    let mut arguments = serde_json::to_value(ConfigurationProtectedPreviewRequestV1 {
        change: change.clone(),
        expected_revision: revision.clone(),
    })
    .expect("protected preview arguments");
    arguments["format"] = json!("json");
    arguments
}

fn deny_cursor_work(project_id: tracedecay_domain::ProjectId) -> ProtectedChange {
    ProtectedChange::UpsertAccessRule(
        ScopeAccessRule::new(
            AccessRuleId::new(ACCESS_RULE_ID).expect("access rule identity"),
            ScopeAccessSubjectV1 {
                actor: None,
                operation: None,
                source_kind: Some(SourceKindV1::Cursor),
            },
            AuthorityRef::Project(project_id),
            BTreeSet::from([
                CapabilityId::new(DENIED_CAPABILITY).expect("generate proposal capability")
            ]),
            RuleEffect::Deny,
            None,
        )
        .expect("deny-only work rule"),
    )
}

async fn call_preview(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    arguments: Value,
) -> (bool, Value) {
    let response = harness
        .call_tool(
            project,
            "tracedecay_configuration_protected_preview",
            arguments,
        )
        .await
        .expect("protected preview tools/call");
    tool_answer(&response)
}

fn assert_redacted_plan(
    payload: &Value,
    revision: &str,
    setting_key: &str,
    operation: &str,
    before_digest: &str,
    after_digest: &str,
    hidden: &[&str],
) {
    assert_eq!(payload["outcome"]["outcome"], "preview");
    assert_eq!(
        payload["outcome"]["value"]["effect_class"],
        "configuration_write"
    );
    let plan = &payload["outcome"]["value"]["payload"];
    assert_eq!(plan["base_revision_id"], revision);
    assert_eq!(
        plan["redacted_changes"],
        json!([{
            "setting_key": setting_key,
            "operation": operation,
            "before_digest": before_digest,
            "after_digest": after_digest,
        }])
    );
    assert_eq!(plan["operation_digest"], after_digest);
    assert_eq!(payload["outcome"]["value"]["preview_digest"], after_digest);
    assert_eq!(
        payload["outcome"]["value"]["preview_id"], plan["plan_id"],
        "the preview id the host applies is the plan id"
    );
    let plan_id = plan["plan_id"].as_str().expect("plan id");
    assert!(
        plan_id.starts_with("configuration.plan.v1."),
        "plan id {plan_id} is not a configuration plan"
    );
    let created_at = plan["created_at"].as_i64().expect("plan created_at");
    let expires_at = plan["expires_at"].as_i64().expect("plan expires_at");
    assert_eq!(
        expires_at - created_at,
        300_000_000,
        "a protected preview stays valid for five minutes"
    );
    let rendered = serde_json::to_string(payload).expect("preview json");
    for secret in hidden {
        assert!(
            !rendered.contains(secret),
            "preview leaked {secret}: {rendered}"
        );
    }
}

fn assert_problem(
    payload: &Value,
    kind: &str,
    code: &str,
    message: &str,
    retry: &str,
    legal_actions: Value,
) {
    assert_eq!(payload["problem"]["kind"], kind, "{payload}");
    assert_eq!(payload["problem"]["code"], code, "{payload}");
    assert_eq!(payload["problem"]["message"], message, "{payload}");
    assert_eq!(payload["problem"]["diagnostic"]["code"], code, "{payload}");
    assert_eq!(
        payload["problem"]["diagnostic"]["message"], message,
        "{payload}"
    );
    assert_eq!(payload["problem"]["retry"], retry, "{payload}");
    assert_eq!(
        payload["problem"]["legal_actions"], legal_actions,
        "{payload}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_preview_redacts_the_change_and_refuses_stale_or_invalid_input() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let graph = harness.server(&project).expect("project server").cg().await;
    let project_id = graph
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let current = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("current configuration");
    let revision = current.revision_id().clone();
    let before_digest: ManifestDigest = current.snapshot().effective_behavior_digest.clone();
    drop(graph);

    let access_rule = deny_cursor_work(project_id.clone());
    let access_digest = access_rule
        .compute_digest()
        .expect("access rule digest")
        .as_str()
        .to_owned();
    let (refused, accepted) = call_preview(
        &harness,
        &project,
        preview_arguments(&access_rule, &revision),
    )
    .await;
    assert!(!refused, "access-rule preview was refused: {accepted}");
    assert_redacted_plan(
        &accepted,
        revision.as_str(),
        "scope.access_rules.v1",
        "access_rule_upsert",
        before_digest.as_str(),
        &access_digest,
        &[ACCESS_RULE_ID, DENIED_CAPABILITY],
    );

    // Apply refuses to unbind a binding the snapshot does not hold, so the
    // preview must refuse with apply's typed reason instead of issuing a plan
    // apply would reject.
    let unbind = ProtectedChange::UnbindSource {
        binding_id: SourceBindingId::new(ABSENT_BINDING_ID).expect("binding identity"),
    };
    let (refused, unbound) =
        call_preview(&harness, &project, preview_arguments(&unbind, &revision)).await;
    assert!(
        refused,
        "an absent-binding unbind must be refused: {unbound}"
    );
    assert_problem(
        &unbound,
        "stale",
        "configuration.stale",
        "The configuration preview is stale",
        "after_revalidate",
        json!(["refresh"]),
    );

    let mut stale = preview_arguments(&access_rule, &revision);
    stale["expected_revision"] = json!(STALE_REVISION);
    let (refused, conflict) = call_preview(&harness, &project, stale).await;
    assert!(refused, "a stale revision must be a tool error: {conflict}");
    assert_problem(
        &conflict,
        "conflict",
        "configuration.conflict",
        "The configuration request conflicts with current state",
        "after_revalidate",
        json!(["refresh"]),
    );

    let mut invalid = preview_arguments(&access_rule, &revision);
    invalid["change"]["value"]["capabilities"] = json!([]);
    let (refused, rejected) = call_preview(&harness, &project, invalid).await;
    assert!(
        refused,
        "an empty capability set must be a tool error: {rejected}"
    );
    assert_problem(
        &rejected,
        "invalid_request",
        "configuration.invalid_request",
        "The configuration request is invalid: access rule capabilities must not be empty",
        "never",
        json!([]),
    );

    let graph = harness.server(&project).expect("project server").cg().await;
    let unchanged = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("configuration after previews")
        .revision_id()
        .clone();
    drop(graph);
    assert_eq!(
        unchanged.as_str(),
        revision.as_str(),
        "protected preview must not commit a revision"
    );

    harness.shutdown().await;
}

/// A GitHub source binding whose id sorts before the project-open binding
/// previews and applies to the same outcome: both place it in canonical
/// binding order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_bind_source_preview_and_apply_share_one_outcome() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let graph = harness.server(&project).expect("project server").cg().await;
    let project_id = graph
        .configuration_runtime()
        .configuration_target()
        .project_id
        .clone();
    let revision = graph
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("current configuration")
        .revision_id()
        .clone();
    drop(graph);

    let bind = ProtectedChange::BindSource(
        ScopeSourceBinding::new(
            SourceBindingId::new("binding.github.rust-lang-log").expect("binding identity"),
            SourceKindV1::GitHub,
            LocatorDigest::new(format!("sha256:{}", "d".repeat(64))).expect("locator digest"),
            AuthorityRef::Project(project_id),
        )
        .expect("GitHub source binding"),
    );
    let (refused, preview) =
        call_preview(&harness, &project, preview_arguments(&bind, &revision)).await;
    assert!(!refused, "bind preview was refused: {preview}");
    let plan: ProtectedChangePlan =
        serde_json::from_value(preview["outcome"]["value"]["payload"].clone())
            .expect("protected change plan");
    let mut apply = serde_json::to_value(ConfigurationProtectedApplyRequestV1 {
        plan_id: plan.plan_id,
        expected_base_revision_id: revision,
        operation_digest: plan.operation_digest,
        idempotency_key: ConfigurationIdempotencyKey::new(
            "configuration.bind-github-before-daemon",
        )
        .expect("idempotency key"),
    })
    .expect("protected apply arguments");
    apply["format"] = json!("json");
    let response = harness
        .call_tool(&project, "tracedecay_configuration_protected_apply", apply)
        .await
        .expect("protected apply tools/call");
    let (refused, applied) = tool_answer(&response);
    assert!(
        !refused,
        "apply refused the plan its preview issued: {applied}"
    );
    assert_eq!(applied["outcome"]["outcome"], "effect", "{applied}");

    harness.shutdown().await;
}
