use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use tempfile::TempDir;
use tracedecay_application::{
    ApplicationResult, CancellationSignal, ConfigurationBatchRequestV1,
    ConfigurationDirectMutationRequestV1, ConfigurationSetRequestV1,
    ConfigurationWriteCredentialRequestV1, Deadline, PageRequest,
};
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationValueV1, CredentialKindV1, SettingKey, USER_UPLOAD_ENABLED_SETTING_KEY,
    UserProfileId,
};
use tracedecay_sdk::client::{Client, ClientError, ConnectionMode};
use tracedecay_sdk::operations::{ApplicationConfigurationSet, TypedOperation};
use tracedecay_tool_catalog::{
    EffectClass, IdempotencyContract, ReceiptContract, ReconciliationContract, TerminalState,
};

use super::journey_test_support::tool_payload;
use super::*;

const HTTP_AUTH_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn initialize_project(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("project source");
    std::fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("project source file");
    let status = std::process::Command::new("git")
        .current_dir(project)
        .args(["init", "--quiet"])
        .status()
        .expect("initialize Git project");
    assert!(status.success(), "Git project initialization failed");
}

async fn current_revision(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> ConfigurationRevisionId {
    harness
        .server(project)
        .expect("project server")
        .cg()
        .await
        .configuration_runtime()
        .client()
        .current()
        .await
        .expect("current configuration")
        .revision_id
}

async fn cli_configuration_set(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    request: ConfigurationSetRequestV1,
) -> ApplicationResult<Value> {
    let graph = harness.server(project).expect("project server").cg().await;
    let target = graph.configuration_runtime().configuration_target().clone();
    let scope =
        project_open_owners::resolved_scope_for_project(graph.project_root(), &target.project_id)
            .expect("project application scope");
    let project_root = graph.project_root().to_path_buf();
    drop(graph);
    let resources = harness
        .resources
        .as_ref()
        .expect("production composition resources");
    let executor = InProcessDaemonInvocationExecutor::new(
        resources.invocation.clone(),
        resources.store_administration.clone(),
        project_root,
        scope,
    );
    let operation = crate::application_surface::ApplicationSurfaceOperation::ConfigurationSet;
    let application_operation =
        tracedecay_application::configuration_surface_operation(operation.as_str())
            .expect("configuration operation contract")
            .expect("cataloged configuration operation");
    let catalog =
        crate::application_surface::application_surface_catalog_ref().expect("application catalog");
    let maximum_millis = catalog
        .capability(application_operation.capability_id())
        .expect("configuration capability")
        .deadline()
        .maximum_millis();
    assert_eq!(
        maximum_millis, 15_000,
        "CLI configuration effects use the catalog-owned 15 second deadline"
    );
    let observed_at = crate::daemon_client::invocation_now_micros();
    let deadline = Deadline::new(tracedecay_domain::UtcMicros(
        observed_at.0 + i64::try_from(maximum_millis).expect("deadline fits") * 1_000,
    ))
    .expect("configuration deadline");
    let request_id = crate::request_identity::mint_global_request_id(
        crate::request_identity::GlobalRequestSurface::Cli,
    )
    .expect("CLI request id");
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .expect("CLI cancellation");
    let dispatched =
        crate::application_surface::resolve_application_surface_dispatch_with_controls(
            tracedecay_tool_catalog::BindingSurface::Cli,
            operation,
            request_id,
            crate::application_surface::ApplicationSurfaceRequest::Configuration(
                tracedecay_application::ConfigurationWireRequestV1::Set(request),
            ),
            PageRequest::first(10).expect("CLI page"),
            Some(deadline),
            cancellation,
            crate::daemon_client::RequestedOutputFormat::Json,
        )
        .expect("CLI configuration dispatch");
    crate::application_surface::execute_application_surface(operation, dispatched, Some(&executor))
        .await
        .expect("CLI application invocation")
        .result
}

async fn current_profile_id(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> UserProfileId {
    harness
        .server(project)
        .expect("project server")
        .cg()
        .await
        .configuration_runtime()
        .registered_database()
        .binding()
        .shard_id
        .profile_id
        .clone()
}

async fn configuration_batch_via_surface(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
    surface: tracedecay_tool_catalog::BindingSurface,
    request: ConfigurationBatchRequestV1,
) -> ApplicationResult<Value> {
    let graph = harness.server(project).expect("project server").cg().await;
    let target = graph.configuration_runtime().configuration_target().clone();
    let scope =
        project_open_owners::resolved_scope_for_project(graph.project_root(), &target.project_id)
            .expect("project application scope");
    let project_root = graph.project_root().to_path_buf();
    drop(graph);
    let resources = harness
        .resources
        .as_ref()
        .expect("production composition resources");
    let executor = InProcessDaemonInvocationExecutor::new(
        resources.invocation.clone(),
        resources.store_administration.clone(),
        project_root,
        scope,
    );
    let operation = crate::application_surface::ApplicationSurfaceOperation::ConfigurationBatch;
    let application_operation =
        tracedecay_application::configuration_surface_operation(operation.as_str())
            .expect("configuration operation contract")
            .expect("cataloged configuration operation");
    let catalog =
        crate::application_surface::application_surface_catalog_ref().expect("application catalog");
    let maximum_millis = catalog
        .capability(application_operation.capability_id())
        .expect("configuration capability")
        .deadline()
        .maximum_millis();
    assert_eq!(
        maximum_millis, 15_000,
        "user configuration effects use the catalog-owned 15 second deadline"
    );
    let observed_at = crate::daemon_client::invocation_now_micros();
    let deadline = Deadline::new(tracedecay_domain::UtcMicros(
        observed_at.0 + i64::try_from(maximum_millis).expect("deadline fits") * 1_000,
    ))
    .expect("configuration deadline");
    let request_surface = match surface {
        tracedecay_tool_catalog::BindingSurface::Cli => {
            crate::request_identity::GlobalRequestSurface::Cli
        }
        tracedecay_tool_catalog::BindingSurface::Dashboard => {
            crate::request_identity::GlobalRequestSurface::DashboardSettings
        }
        other => panic!("unsupported configuration batch test surface: {other:?}"),
    };
    let request_id = crate::request_identity::mint_global_request_id(request_surface)
        .expect("surface request id");
    if surface == tracedecay_tool_catalog::BindingSurface::Dashboard {
        return crate::application_surface::resolve_dashboard_application_surface(
            operation,
            request_id,
            crate::application_surface::ApplicationSurfaceRequest::Configuration(
                tracedecay_application::ConfigurationWireRequestV1::Batch(request),
            ),
            crate::daemon_client::RequestedOutputFormat::Json,
            Some(&executor),
        )
        .await
        .expect("dashboard configuration batch invocation")
        .result;
    }
    let cancellation =
        CancellationSignal::active(format!("cancellation.surface.{}", request_id.as_str()))
            .expect("surface cancellation");
    let dispatched =
        crate::application_surface::resolve_application_surface_dispatch_with_controls(
            surface,
            operation,
            request_id,
            crate::application_surface::ApplicationSurfaceRequest::Configuration(
                tracedecay_application::ConfigurationWireRequestV1::Batch(request),
            ),
            PageRequest::first(10).expect("surface page"),
            Some(deadline),
            cancellation,
            crate::daemon_client::RequestedOutputFormat::Json,
        )
        .expect("configuration batch dispatch");
    crate::application_surface::execute_application_surface(operation, dispatched, Some(&executor))
        .await
        .expect("configuration batch application invocation")
        .result
}

async fn configuration_http_sdk(
    harness: &ProductionProjectCompositionHarnessV1,
    project: &Path,
) -> (
    crate::daemon::http_application::DaemonHttpApplicationService,
    Client,
) {
    let graph = harness.server(project).expect("project server").cg().await;
    let target = graph.configuration_runtime().configuration_target().clone();
    let scope =
        project_open_owners::resolved_scope_for_project(graph.project_root(), &target.project_id)
            .expect("project application scope");
    let project_root = graph.project_root().to_path_buf();
    drop(graph);
    let resources = harness
        .resources
        .as_ref()
        .expect("production composition resources");
    let executor = Arc::new(InProcessDaemonInvocationExecutor::new(
        resources.invocation.clone(),
        resources.store_administration.clone(),
        project_root,
        scope,
    ));
    let router = crate::application_surface::http_application_router_with_executor(
        executor,
        daemon_operation_event_authority(),
        target.project_id.clone(),
    )
    .expect("canonical HTTP application router");
    let registry = crate::daemon::http_application::DaemonHttpApplicationRegistry::default();
    registry
        .mount(target.project_id.as_str(), router)
        .await
        .expect("mount production configuration HTTP router");
    let service = crate::daemon::http_application::DaemonHttpApplicationService::bind(
        registry,
        HTTP_AUTH_TOKEN,
    )
    .await
    .expect("bind production configuration HTTP service");
    let endpoint = format!("http://{}", service.endpoint());
    let client = Client::builder(ConnectionMode::local(
        &endpoint,
        target.project_id.as_str(),
        HTTP_AUTH_TOKEN,
    ))
    .origin(service.origin())
    .build()
    .expect("generated SDK client");
    (service, client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_profile_configuration_batch_has_cli_dashboard_parity_after_restart() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let expected_revision = current_revision(&harness, &project).await;
    let profile_id = current_profile_id(&harness, &project).await;
    let request = ConfigurationBatchRequestV1 {
        mutations: vec![ConfigurationDirectMutationRequestV1::Set {
            layer: ConfigurationLayerIdV1::UserProfile {
                profile_id: profile_id.clone(),
            },
            key: SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY).expect("user setting key"),
            value: ConfigurationValueV1::Boolean(true),
        }],
        expected_revision: expected_revision.clone(),
        idempotency_key: ConfigurationIdempotencyKey::new(
            "configuration.idempotency.cli-dashboard-user-replay",
        )
        .expect("idempotency key"),
    };
    let first_effect = serde_json::to_value(
        configuration_batch_via_surface(
            &harness,
            &project,
            tracedecay_tool_catalog::BindingSurface::Cli,
            request.clone(),
        )
        .await
        .expect("first CLI user configuration effect"),
    )
    .expect("CLI application envelope");
    let committed_revision = current_revision(&harness, &project).await;
    assert_ne!(committed_revision, expected_revision);
    harness.shutdown().await;

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("restarted production composition");
    let replay = serde_json::to_value(
        configuration_batch_via_surface(
            &harness,
            &project,
            tracedecay_tool_catalog::BindingSurface::Dashboard,
            request.clone(),
        )
        .await
        .expect("dashboard replay of CLI user configuration effect"),
    )
    .expect("dashboard replay envelope");
    assert_eq!(
        replay, first_effect,
        "dashboard must replay the CLI operation's exact durable user configuration envelope"
    );
    assert_eq!(
        current_revision(&harness, &project).await,
        committed_revision,
        "cross-surface replay must not advance user configuration again"
    );

    let changed = ConfigurationBatchRequestV1 {
        mutations: vec![ConfigurationDirectMutationRequestV1::Set {
            layer: ConfigurationLayerIdV1::UserProfile { profile_id },
            key: SettingKey::new(USER_UPLOAD_ENABLED_SETTING_KEY).expect("user setting key"),
            value: ConfigurationValueV1::Boolean(false),
        }],
        ..request
    };
    let conflict = configuration_batch_via_surface(
        &harness,
        &project,
        tracedecay_tool_catalog::BindingSurface::Dashboard,
        changed,
    )
    .await
    .expect_err("same-key changed-input dashboard request must conflict");
    assert_eq!(conflict.problem.code, "configuration.conflict");
    assert_eq!(
        current_revision(&harness, &project).await,
        committed_revision,
        "user configuration idempotency conflict must not advance configuration"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configuration_set_has_cli_mcp_http_sdk_parity_and_replays_after_restart() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let expected_revision = current_revision(&harness, &project).await;
    let request = ConfigurationSetRequestV1 {
        layer: ConfigurationLayerIdV1::Default,
        key: SettingKey::new("mcp.tool_timings").expect("setting key"),
        value: ConfigurationValueV1::Boolean(true),
        expected_revision: expected_revision.clone(),
        idempotency_key: ConfigurationIdempotencyKey::new(
            "configuration.idempotency.restart-replay",
        )
        .expect("idempotency key"),
    };
    let first_effect = serde_json::to_value(
        cli_configuration_set(&harness, &project, request.clone())
            .await
            .expect("first CLI configuration effect"),
    )
    .expect("CLI application envelope");
    let effect = &first_effect["outcome"]["value"];
    assert_eq!(
        effect["execution"]["started_at"], effect["payload"]["created_at"],
        "effect execution must use the accepted durable commit time"
    );
    assert_eq!(
        effect["execution"]["effective_deadline"]["expires_at"],
        effect["payload"]["effective_deadline_at"],
        "effect execution must use the accepted durable deadline"
    );
    let committed_revision = current_revision(&harness, &project).await;
    assert_ne!(committed_revision, expected_revision);
    harness.shutdown().await;

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("restarted production composition");
    let replay = harness
        .call_tool(
            &project,
            "tracedecay_configuration_set",
            serde_json::to_value(&request).expect("replay request"),
        )
        .await
        .expect("replayed configuration effect");
    assert_eq!(
        tool_payload(&replay),
        first_effect,
        "MCP replay must return the CLI operation's exact durable effect envelope"
    );
    assert_eq!(
        current_revision(&harness, &project).await,
        committed_revision,
        "exact replay must not advance configuration again"
    );

    assert_eq!(ApplicationConfigurationSet::MAXIMUM_DEADLINE_MILLIS, 15_000);
    assert_eq!(
        ApplicationConfigurationSet::EFFECT,
        EffectClass::ConfigurationWrite
    );
    assert_eq!(
        ApplicationConfigurationSet::IDEMPOTENCY,
        IdempotencyContract::Required
    );
    const { assert!(!ApplicationConfigurationSet::CANCELLABLE) };
    assert!(ApplicationConfigurationSet::CANCELLATION_POINTS.is_empty());
    assert_eq!(
        ApplicationConfigurationSet::RECONCILIATION,
        ReconciliationContract::Required
    );
    assert_eq!(
        ApplicationConfigurationSet::RECEIPT,
        ReceiptContract::DurableEffect
    );
    assert_eq!(
        ApplicationConfigurationSet::TERMINAL_STATES,
        [
            TerminalState::Completed,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ]
    );

    let (http_service, sdk) = configuration_http_sdk(&harness, &project).await;
    let sdk_request = serde_json::from_value::<
        <ApplicationConfigurationSet as TypedOperation>::Request,
    >(serde_json::to_value(&request).expect("SDK replay request"))
    .expect("generated SDK replay request");
    let replay_sdk = sdk.clone();
    let sdk_replay = tokio::task::spawn_blocking(move || {
        replay_sdk.execute::<ApplicationConfigurationSet>(&sdk_request)
    })
    .await
    .expect("generated SDK replay task")
    .expect("generated SDK replay");
    assert_eq!(
        sdk_replay.envelope["outcome"]["value"], first_effect["outcome"]["value"],
        "real HTTP generated-SDK replay must preserve the exact original effect receipt"
    );
    assert_eq!(
        serde_json::to_value(&sdk_replay.result).expect("generated SDK result"),
        first_effect["outcome"]["value"]["payload"],
        "the generated response type must decode the original durable mutation receipt"
    );
    assert_eq!(
        current_revision(&harness, &project).await,
        committed_revision,
        "real HTTP generated-SDK replay must not advance configuration again"
    );

    let changed = ConfigurationSetRequestV1 {
        value: ConfigurationValueV1::Boolean(false),
        ..request.clone()
    };
    let changed_sdk_request = serde_json::from_value::<
        <ApplicationConfigurationSet as TypedOperation>::Request,
    >(serde_json::to_value(&changed).expect("changed SDK request"))
    .expect("generated changed SDK request");
    let sdk_conflict = tokio::task::spawn_blocking(move || {
        sdk.execute::<ApplicationConfigurationSet>(&changed_sdk_request)
    })
    .await
    .expect("generated SDK conflict task")
    .expect_err("same-key changed-input SDK request must conflict");
    let ClientError::Problem(sdk_problem) = sdk_conflict else {
        panic!("generated SDK must preserve the canonical conflict problem: {sdk_conflict}");
    };
    assert_eq!(sdk_problem.code, "configuration.conflict");
    assert_eq!(
        current_revision(&harness, &project).await,
        committed_revision,
        "generated SDK idempotency conflict must not advance configuration"
    );
    http_service
        .shutdown()
        .await
        .expect("shutdown configuration HTTP service");

    let conflict = cli_configuration_set(&harness, &project, changed)
        .await
        .expect_err("same-key changed-input CLI request must conflict");
    assert_eq!(
        conflict.problem.code, "configuration.conflict",
        "the CLI adapter must preserve the canonical application conflict"
    );
    assert_eq!(
        current_revision(&harness, &project).await,
        committed_revision,
        "idempotency conflict must not advance configuration"
    );
    harness.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_effect_uses_the_durable_request_operation_digest() {
    let isolation = TempDir::new().expect("journey isolation");
    let project = isolation.path().join("project");
    initialize_project(&project);

    let harness = ProductionProjectCompositionHarnessV1::open(isolation.path(), [project.clone()])
        .await
        .expect("production composition");
    let request = ConfigurationWriteCredentialRequestV1 {
        expected_reference_id: None,
        kind: CredentialKindV1::ApiToken,
        write_handle: "credential-write-handle.production-journey".to_owned(),
        expected_revision: current_revision(&harness, &project).await,
        idempotency_key: ConfigurationIdempotencyKey::new(
            "configuration.idempotency.credential-digest",
        )
        .expect("idempotency key"),
    };
    let response = harness
        .call_tool(
            &project,
            "tracedecay_configuration_write_credential",
            serde_json::to_value(&request).expect("credential request"),
        )
        .await
        .expect("credential effect");
    let envelope = tool_payload(&response);
    let effect = &envelope["outcome"]["value"];
    let operation_digest = &effect["payload"]["operation_digest"];
    assert_eq!(
        &effect["receipt"]["input_digest"], operation_digest,
        "the effect must carry the canonical digest accepted by the credential store"
    );
    assert_ne!(
        operation_digest, &effect["payload"]["reference_digest"],
        "the durable request digest must not be replaced by the metadata digest"
    );
    assert_eq!(
        effect["execution"]["effective_deadline"]["expires_at"],
        effect["payload"]["effective_deadline_at"],
        "effect execution must replay the accepted credential deadline"
    );
    harness.shutdown().await;
}
