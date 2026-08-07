use std::fs;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_test_support::*;
use super::*;
use crate::config::lock_user_data_dir_test_env;
use crate::tracedecay::TraceDecay;

#[derive(Default)]
struct UnavailableEffectExecutor {
    invocations: AtomicUsize,
    application_surface_invocations: Mutex<Vec<(String, Value)>>,
    configuration_invocations: Mutex<
        Vec<(
            crate::application_surface::ApplicationSurfaceOperation,
            Value,
            crate::daemon_client::InvocationCancellationPolicy,
        )>,
    >,
}

impl tracedecay_application::ApplicationInvocationExecutor for UnavailableEffectExecutor {
    fn invoke(
        &self,
        invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        if let (Some(binding), Some(payload)) = (
            invocation.request().binding(),
            invocation.request().surface_payload(),
        ) {
            self.application_surface_invocations
                .lock()
                .unwrap()
                .push((binding.operation().as_str().to_owned(), payload.clone()));
        }
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for UnavailableEffectExecutor {
    fn invoke_controlled(
        &self,
        request: crate::daemon_contract::DaemonInvocationRequest,
        _deadline: tracedecay_application::Deadline,
        _cancellation: tracedecay_application::CancellationSignal,
        policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        if let crate::daemon_contract::DaemonInvocationPayload::Configuration {
            surface_operation,
            request,
            ..
        } = request.payload
        {
            self.configuration_invocations.lock().unwrap().push((
                surface_operation,
                serde_json::to_value(request).unwrap(),
                policy,
            ));
        }
        Box::pin(async { Err(crate::daemon_client::DaemonInvocationError::Unavailable) })
    }

    fn observe_plan26_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: crate::application::feedback::observations::Plan26FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn available_configuration_effect_reaches_canonical_executor() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("unavailable-application-effect");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-unavailable-application-effect",
    )
    .await
    .unwrap();
    let executor = UnavailableEffectExecutor::default();
    let request = serde_json::to_value(tracedecay_application::ConfigurationSetRequestV1 {
        layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Default,
        key: tracedecay_domain::configuration::SettingKey::new("mcp.tool_timings").unwrap(),
        value: tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true),
        expected_revision: tracedecay_domain::configuration::ConfigurationRevisionId::new(
            "revision.mcp-unavailable-application-effect",
        )
        .unwrap(),
        idempotency_key: tracedecay_domain::configuration::ConfigurationIdempotencyKey::new(
            "configuration.idempotency.mcp-unavailable-application-effect",
        )
        .unwrap(),
    })
    .unwrap();
    let cancellation =
        tracedecay_application::CancellationSignal::active("configuration-set-canonical-effect")
            .unwrap();

    let result = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_configuration_set",
        request,
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_cancellation: Some(cancellation.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("verified configuration effect must reach its canonical executor");

    assert_eq!(result.semantic_error(), Some(true));
    assert_eq!(
        result.failure_message(),
        Some("application surface unavailable")
    );
    assert_eq!(executor.invocations.load(Ordering::SeqCst), 1);
    assert!(
        cancellation.commit_started(),
        "canonical effect admission must win settlement before invoking the daemon"
    );
    cg.close();
}

#[tokio::test]
async fn every_other_configuration_effect_reaches_the_authoritative_daemon_executor() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("configuration-effect-dispatch");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-configuration-effect-dispatch",
    )
    .await
    .unwrap();
    let executor = UnavailableEffectExecutor::default();
    let revision = "revision.mcp-configuration-effect-dispatch";
    let digest = format!("sha256:{}", "a".repeat(64));
    let effects = [
        (
            "tracedecay_configuration_unset",
            ApplicationSurfaceOperation::ConfigurationUnset,
            "configuration.idempotency.mcp-unset",
            json!({
                "layer": {"kind": "default"},
                "key": "mcp.tool_timings",
                "expected_revision": revision,
                "idempotency_key": "configuration.idempotency.mcp-unset"
            }),
        ),
        (
            "tracedecay_configuration_batch",
            ApplicationSurfaceOperation::ConfigurationBatch,
            "configuration.idempotency.mcp-batch",
            json!({
                "mutations": [{
                    "operation": "set",
                    "layer": {"kind": "default"},
                    "key": "mcp.tool_timings",
                    "value": {"kind": "boolean", "value": true}
                }],
                "expected_revision": revision,
                "idempotency_key": "configuration.idempotency.mcp-batch"
            }),
        ),
        (
            "tracedecay_configuration_write_credential",
            ApplicationSurfaceOperation::ConfigurationWriteCredential,
            "configuration.idempotency.mcp-credential",
            json!({
                "expected_reference_id": null,
                "kind": "api_token",
                "write_handle": "credential-write-handle.mcp",
                "expected_revision": revision,
                "idempotency_key": "configuration.idempotency.mcp-credential"
            }),
        ),
        (
            "tracedecay_configuration_protected_apply",
            ApplicationSurfaceOperation::ConfigurationProtectedApply,
            "configuration.idempotency.mcp-protected-apply",
            json!({
                "plan_id": "change-plan.mcp-protected-apply",
                "expected_base_revision_id": revision,
                "operation_digest": digest,
                "idempotency_key": "configuration.idempotency.mcp-protected-apply"
            }),
        ),
        (
            "tracedecay_configuration_rollback_apply",
            ApplicationSurfaceOperation::ConfigurationRollbackApply,
            "configuration.idempotency.mcp-rollback-apply",
            json!({
                "plan_id": "change-plan.mcp-rollback-apply",
                "expected_base_revision_id": revision,
                "operation_digest": digest,
                "idempotency_key": "configuration.idempotency.mcp-rollback-apply"
            }),
        ),
    ];

    for (tool_name, _, _, request) in &effects {
        let cancellation = tracedecay_application::CancellationSignal::active(format!(
            "cancellation.{}",
            tool_name.strip_prefix("tracedecay_").unwrap()
        ))
        .unwrap();
        let result = handle_tool_call_with_registry_and_implicit_project(
            &cg,
            tool_name,
            request.clone(),
            None,
            None,
            ToolCallRegistryOptions {
                application_invocation_executor: Some(&executor),
                application_cancellation: Some(cancellation.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("every catalog-advertised configuration effect must reach its daemon handler");

        assert_eq!(result.semantic_error(), Some(true));
        assert_eq!(
            result.failure_message(),
            Some("application surface unavailable")
        );
        assert!(
            cancellation.commit_started(),
            "{tool_name} must claim authoritative effect settlement before daemon invocation"
        );
    }

    let invocations = executor.configuration_invocations.lock().unwrap();
    assert_eq!(invocations.len(), effects.len());
    for ((actual_operation, request, policy), (_, expected_operation, idempotency_key, _)) in
        invocations.iter().zip(&effects)
    {
        assert_eq!(actual_operation, expected_operation);
        assert_eq!(
            policy,
            &crate::daemon_client::InvocationCancellationPolicy::AuthoritativeEffect
        );
        assert_eq!(request["request"]["idempotency_key"], *idempotency_key);
    }
    drop(invocations);
    cg.close();
}

#[tokio::test]
async fn every_configuration_read_and_preview_reaches_its_canonical_daemon_handler() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("configuration-read-dispatch");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-configuration-read-dispatch",
    )
    .await
    .unwrap();
    let executor = UnavailableEffectExecutor::default();
    let revision = "revision.mcp-configuration-read-dispatch";
    let reads = [
        (
            "tracedecay_configuration_list",
            ApplicationSurfaceOperation::ConfigurationList,
            json!({}),
        ),
        (
            "tracedecay_configuration_explain",
            ApplicationSurfaceOperation::ConfigurationExplain,
            json!({"key": "mcp.tool_timings"}),
        ),
        (
            "tracedecay_configuration_get",
            ApplicationSurfaceOperation::ConfigurationGet,
            json!({"key": "mcp.tool_timings"}),
        ),
        (
            "tracedecay_configuration_observed_state",
            ApplicationSurfaceOperation::ConfigurationObservedState,
            json!({}),
        ),
        (
            "tracedecay_configuration_protected_preview",
            ApplicationSurfaceOperation::ConfigurationProtectedPreview,
            json!({
                "change": {
                    "kind": "unbind_source",
                    "value": {"binding_id": "source-binding.mcp-protected-preview"}
                },
                "expected_revision": revision
            }),
        ),
        (
            "tracedecay_configuration_rollback_preview",
            ApplicationSurfaceOperation::ConfigurationRollbackPreview,
            json!({
                "target_revision_id": revision,
                "mode": "all_or_nothing"
            }),
        ),
        (
            "tracedecay_configuration_audit",
            ApplicationSurfaceOperation::ConfigurationAudit,
            json!({"after_event_id": null, "limit": 10}),
        ),
    ];

    for (tool_name, _, request) in &reads {
        let result = handle_tool_call_with_registry_and_implicit_project(
            &cg,
            tool_name,
            request.clone(),
            None,
            None,
            ToolCallRegistryOptions {
                application_invocation_executor: Some(&executor),
                ..Default::default()
            },
        )
        .await
        .expect("every catalog-advertised configuration read must reach its daemon handler");
        assert_eq!(result.semantic_error(), Some(true));
        assert_eq!(
            result.failure_message(),
            Some("application surface unavailable")
        );
    }

    let application_invocations = executor.application_surface_invocations.lock().unwrap();
    assert_eq!(application_invocations.len(), 1);
    assert_eq!(application_invocations[0].0, "configuration_get");
    assert_eq!(
        application_invocations[0].1,
        json!({"key": "mcp.tool_timings"})
    );
    drop(application_invocations);

    let invocations = executor.configuration_invocations.lock().unwrap();
    assert_eq!(invocations.len(), reads.len() - 1);
    let expected_operations = reads.iter().filter_map(|(_, operation, _)| {
        (*operation != ApplicationSurfaceOperation::ConfigurationGet).then_some(*operation)
    });
    for ((actual_operation, request, policy), expected_operation) in
        invocations.iter().zip(expected_operations)
    {
        assert_eq!(actual_operation, &expected_operation);
        assert_eq!(
            policy,
            &crate::daemon_client::InvocationCancellationPolicy::ReadOnly
        );
        assert!(request["request"].is_object());
        assert!(request["request"].get("idempotency_key").is_none());
    }
    drop(invocations);
    cg.close();
}
