//! Profile-targeted retained requests travel as typed daemon invocations to
//! the composition root's profile owner, with no project on the connection.

use super::*;
use tracedecay_contracts::retained_surfaces::{RetainedSurfaceOperation, RetainedSurfaceResultV1};
use tracedecay_contracts::{ApplicationOutcome, ApplicationProblemKind, CancellationContext};
use tracedecay_daemon_service::{DaemonInvocationOutcome, DaemonInvocationRequest};

fn profile_request(
    request_id: &str,
    operation: RetainedSurfaceOperation,
    body: serde_json::Value,
) -> DaemonInvocationRequest {
    let request = tracedecay_daemon_protocol::decode_retained_request(operation, body)
        .expect("canonical retained request");
    DaemonInvocationRequest::profile_retained_application(
        request_id,
        request,
        tracedecay_contracts::now_micros(),
        tracedecay_contracts::Deadline::new(tracedecay_domain::UtcMicros(
            tracedecay_contracts::now_micros().0 + 30_000_000,
        ))
        .expect("deadline"),
        CancellationContext::active(format!("cancel.{request_id}")).expect("cancellation"),
    )
}

async fn invoke(
    engine: &DaemonEngine,
    request: DaemonInvocationRequest,
) -> DaemonInvocationOutcome {
    engine
        .invocation
        .invoke_for_project(&engine.store_administration, None, request, None)
        .await
        .outcome
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_target_writes_and_reads_the_profile_memory_without_a_project() {
    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join(".tracedecay");
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "profile retained");
    let engine = test_daemon_engine_for_profile(&profile_root);
    prewarm_test_profile_runtime(&engine.store_administration).await;

    let added = invoke(
        &engine,
        profile_request(
            "request.profile-retained.add",
            RetainedSurfaceOperation::FactStoreAdd,
            serde_json::json!({
                "content": "The operator prefers terse answers",
                "category": "user_pref",
                "memory_scope": "user"
            }),
        ),
    )
    .await;
    let DaemonInvocationOutcome::RetainedApplication {
        outcome: ApplicationOutcome::Effect(effect),
        ..
    } = added
    else {
        panic!("a profile fact add must settle a typed effect: {added:?}");
    };
    assert!(
        matches!(
            effect.payload,
            Some(RetainedSurfaceResultV1::FactStoreAdd(_))
        ),
        "{effect:?}"
    );

    let status = invoke(
        &engine,
        profile_request(
            "request.profile-retained.status",
            RetainedSurfaceOperation::MemoryStatus,
            serde_json::json!({ "memory_scope": "user" }),
        ),
    )
    .await;
    let DaemonInvocationOutcome::RetainedApplication {
        outcome: ApplicationOutcome::Evidence(packet),
        ..
    } = status
    else {
        panic!("profile memory status must settle typed evidence: {status:?}");
    };
    let Some(RetainedSurfaceResultV1::MemoryStatus(status)) = packet.payload else {
        panic!("memory status must carry its typed result");
    };
    let memory = serde_json::to_value(&status.memory).expect("memory status JSON");
    assert_eq!(memory["owner"]["kind"], "profile", "{memory}");
    assert_eq!(memory["fact_count"], 1, "{memory}");

    let selected = invoke(
        &engine,
        profile_request(
            "request.profile-retained.selected",
            RetainedSurfaceOperation::MemoryStatus,
            serde_json::json!({
                "memory_scope": "user",
                "project_selector": { "project_id": "project.foreign" }
            }),
        ),
    )
    .await;
    let DaemonInvocationOutcome::RetainedApplicationProblem { problem, .. } = selected else {
        panic!("a project selector must not reach the profile memory: {selected:?}");
    };
    assert_eq!(
        problem.kind(),
        ApplicationProblemKind::NotFoundOrNotAuthorized
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn projectless_mcp_memory_calls_answer_from_the_profile_owner() {
    let home = TempDir::new().expect("isolated home");
    let profile_root = home.path().join(".tracedecay");
    let _database_scope = enter_test_daemon_database_scope(&profile_root, "projectless memory");
    let engine = test_daemon_engine_for_profile(&profile_root);
    prewarm_test_profile_runtime(&engine.store_administration).await;
    let identity = test_client_identity_for(profile_root);
    let call = |name: &str, arguments: serde_json::Value| {
        let params = serde_json::json!({ "name": name, "arguments": arguments });
        let identity = &identity;
        let engine = &engine;
        async move {
            let response = super::super::projectless_tools_call_response(
                serde_json::json!(1),
                Some(&params),
                identity,
                &engine.store_administration,
            )
            .await;
            let result = response
                .result
                .unwrap_or_else(|| panic!("projectless call failed: {:?}", response.error));
            serde_json::from_str::<serde_json::Value>(
                result["content"][0]["text"].as_str().expect("tool text"),
            )
            .expect("tool JSON")
        }
    };

    let added = call(
        "tracedecay_fact_store_add",
        serde_json::json!({
            "content": "Projectless profile memory beacon",
            "category": "user_pref",
            "memory_scope": "user",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(added["outcome"]["outcome"], "effect", "{added}");
    let status = call(
        "tracedecay_memory_status",
        serde_json::json!({ "memory_scope": "user", "format": "json" }),
    )
    .await;
    assert_eq!(
        status["outcome"]["value"]["payload"]["memory"]["fact_count"], 1,
        "{status}"
    );
    assert_eq!(status["scope"], added["scope"], "{status}");
}
