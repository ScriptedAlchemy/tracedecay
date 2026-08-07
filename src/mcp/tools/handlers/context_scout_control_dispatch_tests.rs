use std::fs;
use std::sync::Mutex;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_test_support::*;
use super::*;
use crate::config::lock_user_data_dir_test_env;
use crate::tracedecay::TraceDecay;

#[derive(Default)]
struct RecordingUnavailableExecutor {
    invocations: Mutex<Vec<(String, Value)>>,
}

impl tracedecay_application::ApplicationInvocationExecutor for RecordingUnavailableExecutor {
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
        if let (Some(binding), Some(payload)) = (
            invocation.request().binding(),
            invocation.request().surface_payload(),
        ) {
            self.invocations
                .lock()
                .unwrap()
                .push((binding.operation().as_str().to_owned(), payload.clone()));
        }
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for RecordingUnavailableExecutor {
    fn invoke_controlled(
        &self,
        _request: crate::daemon_contract::DaemonInvocationRequest,
        _deadline: tracedecay_application::Deadline,
        _cancellation: tracedecay_application::CancellationSignal,
        _policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        Box::pin(async { Err(crate::daemon_client::DaemonInvocationError::Unavailable) })
    }

    fn observe_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: crate::application::feedback::observations::FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn context_scout_pause_and_resume_preserve_caller_idempotency_keys() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("context-scout-control-dispatch");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-context-scout-control-dispatch",
    )
    .await
    .unwrap();
    let executor = RecordingUnavailableExecutor::default();
    let address = json!({
        "profile_id": vec![1; 16],
        "provider_id": vec![2; 16],
        "protected_session_id": vec![3; 32],
        "thread_id": vec![4; 16],
        "turn_id": vec![5; 16],
        "agent_id": vec![6; 16],
        "logical_message_id": vec![7; 16],
        "project_id": vec![8; 16]
    });
    let controls = [
        (
            "tracedecay_context_scout_pause",
            "context_scout_pause",
            "configuration.idempotency.mcp-context-scout-pause",
        ),
        (
            "tracedecay_context_scout_resume",
            "context_scout_resume",
            "configuration.idempotency.mcp-context-scout-resume",
        ),
    ];

    for (tool_name, _, idempotency_key) in controls {
        let cancellation = tracedecay_application::CancellationSignal::active(format!(
            "cancellation.{}",
            tool_name.strip_prefix("tracedecay_").unwrap()
        ))
        .unwrap();
        let result = handle_tool_call_with_registry_and_implicit_project(
            &cg,
            tool_name,
            json!({
                "address": address.clone(),
                "expected_revision": "revision.mcp-context-scout-control-dispatch",
                "idempotency_key": idempotency_key
            }),
            None,
            None,
            ToolCallRegistryOptions {
                application_invocation_executor: Some(&executor),
                application_cancellation: Some(cancellation.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("Context Scout control must reach its canonical executor");

        assert_eq!(result.semantic_error(), Some(true));
        assert_eq!(
            result.failure_message(),
            Some("application surface unavailable")
        );
        assert!(
            cancellation.commit_started(),
            "{tool_name} must claim authoritative effect settlement before invocation"
        );
    }

    let invocations = executor.invocations.lock().unwrap();
    assert_eq!(invocations.len(), controls.len());
    for ((actual_operation, request), (_, expected_operation, idempotency_key)) in
        invocations.iter().zip(controls)
    {
        assert_eq!(actual_operation, expected_operation);
        assert_eq!(request["idempotency_key"], idempotency_key);
    }
    drop(invocations);
    cg.close();
}
