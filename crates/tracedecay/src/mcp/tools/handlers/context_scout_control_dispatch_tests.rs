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
    invocations: Mutex<
        Vec<(
            ApplicationSurfaceOperation,
            Value,
            tracedecay_daemon_protocol::InvocationCancellationPolicy,
        )>,
    >,
}

impl tracedecay_application::ApplicationInvocationExecutor for RecordingUnavailableExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for RecordingUnavailableExecutor {
    fn invoke_controlled(
        &self,
        request: tracedecay_daemon_protocol::DaemonInvocationRequest,
        _deadline: tracedecay_application::Deadline,
        _cancellation: tracedecay_application::CancellationSignal,
        policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            tracedecay_daemon_protocol::DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        if let tracedecay_daemon_protocol::DaemonInvocationPayload::ContextScout {
            surface_operation,
            request,
            ..
        } = request.payload
        {
            self.invocations.lock().unwrap().push((
                surface_operation,
                serde_json::to_value(request).unwrap(),
                policy,
            ));
        }
        Box::pin(async { Err(tracedecay_daemon_protocol::DaemonInvocationError::Unavailable) })
    }

    fn observe_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: tracedecay_application::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        tracedecay_domain::errors::Result<()>,
    > {
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
            ApplicationSurfaceOperation::ContextScoutPause,
            "configuration.idempotency.mcp-context-scout-pause",
        ),
        (
            "tracedecay_context_scout_resume",
            ApplicationSurfaceOperation::ContextScoutResume,
            "configuration.idempotency.mcp-context-scout-resume",
        ),
    ];

    for (tool_name, _, idempotency_key) in controls {
        let cancellation = tracedecay_application::CancellationSignal::active(format!(
            "cancellation.{}",
            tool_name.strip_prefix("tracedecay_").unwrap()
        ))
        .unwrap();
        let result = handle_tool_call_with_registry_options(
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
            !cancellation.commit_started(),
            "{tool_name} pre-admission executor refusal must not claim effect settlement"
        );
    }

    let invocations = executor.invocations.lock().unwrap();
    assert_eq!(invocations.len(), controls.len());
    for ((actual_operation, request, policy), (_, expected_operation, idempotency_key)) in
        invocations.iter().zip(controls)
    {
        assert_eq!(actual_operation, &expected_operation);
        assert_eq!(request["request"]["idempotency_key"], idempotency_key);
        assert_eq!(
            policy,
            &tracedecay_daemon_protocol::InvocationCancellationPolicy::AuthoritativeEffect
        );
    }
    drop(invocations);
    cg.close();
}
