//! Serialized producer fixtures shared with the dashboard behavior tests.

use axum::Json;
use axum::body::to_bytes;
use axum::response::IntoResponse;
use serde_json::{Value, json};
use tracedecay_api::configuration::{
    configuration_application_problem_error, configuration_authority_unavailable_error,
    configuration_revision_conflict_error, parse_project_settings_patch,
};
use tracedecay_api::read_model::{
    DashboardCoverageV1, DashboardEnvelopeV1, DashboardLegalActionKindV1,
    DashboardLegalActionRefV1, DashboardScopeV1,
};
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblem, ApplicationProblemEnvelope, AuthorityReceipt,
    CapabilityGrantId, Deadline, DisclosureClass, EffectId, EffectReceipt, EffectResult,
    EffectTermination, IdempotencyKey, OperationBudgetUsage, OperationReceipt, PolicyDecisionRef,
    ReconciliationState, RequestId, ResolvedScope, ResultContractRef,
};
use tracedecay_domain::configuration::{CodeIndexWorkerSelectionV1, ConfigurationRevisionId};
use tracedecay_domain::{
    ActorId, ComponentVersion, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{EffectClass, SchemaId, UseCaseId};

use super::pinned_defaults;
use crate::settings_api::{
    EnvironmentSettingsPayloadV1, PrAutoTrackPayloadV1, ProjectSettingsPatchResponseV1,
    ProjectSettingsPayloadV1, SettingsPayloadV1, StorageSettingsPayloadV1, UserSettingsPayloadV1,
    VersionSettingsPayloadV1, automation_settings_payload, project_editable_settings,
};

fn settings(revision: &str, max_file_size: u64) -> DashboardEnvelopeV1<SettingsPayloadV1> {
    let pinned = pinned_defaults(&ConfigurationRevisionId::new(revision).unwrap());
    let mut config = project_editable_settings(&pinned);
    config.max_file_size = max_file_size;
    let payload = SettingsPayloadV1 {
        project: ProjectSettingsPayloadV1 {
            configuration_snapshot_id: pinned.snapshot().snapshot_id.as_str().to_owned(),
            configuration_revision_id: revision.to_owned(),
            config,
            pr_autotrack: PrAutoTrackPayloadV1 { tracked: vec![] },
        },
        user: UserSettingsPayloadV1 {
            configuration_snapshot_id: "user-snapshot-7".to_owned(),
            configuration_revision_id: "user-rev-7".to_owned(),
            code_index_worker_configuration_snapshot_id: "workers-snapshot-3".to_owned(),
            code_index_worker_configuration_revision_id: "workers-rev-3".to_owned(),
            upload_enabled: false,
            code_index_workers: CodeIndexWorkerSelectionV1::Automatic {},
            code_index_worker_status: None,
            watcher_debounce: "15s".to_owned(),
            extraction_timeout_secs: 30,
            installed_agents: vec![],
        },
        automation: automation_settings_payload(&pinned),
        environment: EnvironmentSettingsPayloadV1 {
            global_accounting_mode: "disabled".to_owned(),
            global_accounting_enabled: false,
            pricing_offline: true,
            variables: vec![],
        },
        storage: StorageSettingsPayloadV1 {
            project_id: Some("project.settings.scout".to_owned()),
            project_root: "/project".to_owned(),
            storage_mode: "project_local".to_owned(),
            store_root: "/project/.tracedecay".to_owned(),
            dashboard_root: "/project".to_owned(),
            graph_db: "/project/.tracedecay/graph.db".to_owned(),
            memory_db: "/project/.tracedecay/memory.db".to_owned(),
            lcm_db: "/project/.tracedecay/lcm.db".to_owned(),
            lcm_scope: "project".to_owned(),
            savings_db: "/project/.tracedecay/savings.db".to_owned(),
        },
        version: VersionSettingsPayloadV1 {
            version: "0.1.0".to_owned(),
            channel: "stable".to_owned(),
            cached_latest_version: None,
        },
        resync_recommended: Some(max_file_size != 1_048_576),
        restart_recommended: None,
    };
    let mut envelope = DashboardEnvelopeV1::ready(
        DashboardScopeV1 {
            project_id: Some("project.settings.scout".to_owned()),
            storage_mode: "project_local".to_owned(),
            store_root: "/project/.tracedecay".to_owned(),
        },
        DashboardCoverageV1::complete(1, "configuration_control_plane"),
        payload,
    )
    .with_legal_actions(vec![
        DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::RequestApply,
            "configuration_batch",
        ),
        DashboardLegalActionRefV1::new(DashboardLegalActionKindV1::Refresh, "configuration_list"),
    ]);
    envelope.time.observation_time_micros = 100;
    envelope.freshness.observed_at_micros = Some(100);
    envelope
}

fn digest(character: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", character.to_string().repeat(64))).unwrap()
}

fn effect() -> ApplicationOutcome<Value> {
    let scope = ResolvedScope::new(
        ProjectId::new("project.settings.scout").unwrap(),
        RepositoryId::new("repository.settings").unwrap(),
        WorktreeId::new("worktree.settings").unwrap(),
        None,
    )
    .unwrap();
    let idempotency_key = IdempotencyKey::new("settings-idempotency-fixture").unwrap();
    let authority = AuthorityReceipt {
        grant_id: CapabilityGrantId::new("grant.settings").unwrap(),
        grant_revision: 1,
        grant_digest: digest('a'),
        authorized_scope_digest: scope.scope_digest.clone(),
        disclosure: DisclosureClass::Metadata,
        policy: PolicyDecisionRef::new(
            "policy.settings",
            1,
            digest('b'),
            ComponentVersion::new("policy.settings.v1").unwrap(),
        )
        .unwrap(),
        revalidated_at: UtcMicros(10),
    };
    authority.validate_for(&scope).unwrap();
    ApplicationOutcome::Effect(
        EffectResult::new(
            EffectId::new("effect.configuration.settings").unwrap(),
            EffectClass::Administrative,
            idempotency_key.clone(),
            authority,
            digest('c'),
            OperationReceipt::completed(
                UtcMicros(10),
                UtcMicros(20),
                Deadline::new(UtcMicros(1_000)).unwrap(),
                OperationBudgetUsage::default(),
            )
            .unwrap(),
            ReconciliationState::Reconciled,
            EffectReceipt {
                operation: UseCaseId::new("use-case.configuration.batch").unwrap(),
                request_id: RequestId::new("request.configuration.settings-fixture").unwrap(),
                actor: ActorId::new("actor.settings").unwrap(),
                scope,
                effect_class: EffectClass::Administrative,
                idempotency_key,
                input_digest: digest('a'),
                expected_state: digest('c'),
                policy_digest: digest('b'),
                configuration_digest: digest('d'),
                catalog_digest: digest('e'),
                privacy_digest: digest('f'),
                outcome: EffectTermination::Completed,
                committed_state: Some(digest('1')),
                external_proof: None,
            },
            None,
        )
        .unwrap(),
    )
}

async fn http_fixture(response: impl IntoResponse) -> Value {
    let response = response.into_response();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    json!({ "status": status, "body": body })
}

#[tokio::test]
async fn project_patch_serialization_preserves_settlement_and_refusals() {
    let outcome = effect();
    let expected_receipt = match &outcome {
        ApplicationOutcome::Effect(effect) => serde_json::to_value(&effect.receipt).unwrap(),
        _ => unreachable!("fixture constructs an effect"),
    };
    let changed = http_fixture(Json(ProjectSettingsPatchResponseV1 {
        application_outcome: Some(outcome),
        current: settings("rev-43", 2_097_152),
    }))
    .await;
    assert_eq!(changed["status"], 200);
    assert_eq!(changed["body"]["application_outcome"]["outcome"], "effect");
    assert_eq!(
        changed["body"]["application_outcome"]["value"]["receipt"],
        expected_receipt
    );
    assert_eq!(
        changed["body"]["current"]["payload"]["project"]["configuration_revision_id"],
        "rev-43"
    );
    let unchanged = http_fixture(Json(ProjectSettingsPatchResponseV1 {
        application_outcome: None,
        current: settings("rev-42", 1_048_576),
    }))
    .await;
    assert!(
        unchanged["body"]
            .as_object()
            .unwrap()
            .contains_key("application_outcome")
    );
    assert!(unchanged["body"]["application_outcome"].is_null());

    let idempotency_conflict = ApplicationProblemEnvelope::new(
        ResultContractRef::new(
            SchemaId::new("schema.configuration.batch.result").unwrap(),
            1,
        )
        .unwrap(),
        RequestId::new("request.configuration.settings-fixture").unwrap(),
        ApplicationProblem::conflict(
            "configuration.conflict",
            "The idempotency key was already used for a different settings change.",
        ),
    )
    .unwrap();
    let expected_problem = serde_json::to_value(&idempotency_conflict).unwrap();
    let idempotency = http_fixture(configuration_application_problem_error(
        idempotency_conflict,
    ))
    .await;
    assert_eq!(idempotency["status"], 409);
    assert_eq!(idempotency["body"], expected_problem);

    let validation = parse_project_settings_patch(json!({
        "expected_revision_id": "rev-42",
        "idempotency_key": "settings-idempotency-fixture",
        "unknown_setting": true,
    }))
    .expect_err("unknown fields must be refused");
    let cancelled = ApplicationProblemEnvelope::new(
        ResultContractRef::new(
            SchemaId::new("schema.configuration.batch.result").unwrap(),
            1,
        )
        .unwrap(),
        RequestId::new("request.configuration.settings-fixture").unwrap(),
        ApplicationProblem::cancelled_before_admission(),
    )
    .unwrap();
    let fixtures = json!({
        "read": http_fixture(Json(settings("rev-42", 1_048_576))).await,
        "changed": changed,
        "unchanged": unchanged,
        "revision_conflict": http_fixture(configuration_revision_conflict_error(
            "settings changed after this edit began; refresh and retry", "rev-42", "rev-43"
        )).await,
        "validation": http_fixture(validation).await,
        "unavailable": http_fixture(configuration_authority_unavailable_error()).await,
        "idempotency_conflict": idempotency,
        "cancelled": http_fixture(configuration_application_problem_error(cancelled)).await,
    });
    // Bazel preserves this producer output for the frontend's shared fixture.
    if let Some(output) = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR") {
        std::fs::write(
            std::path::PathBuf::from(output).join("settings-patch.json"),
            format!("{}\n", serde_json::to_string_pretty(&fixtures).unwrap()),
        )
        .unwrap();
    }
}
