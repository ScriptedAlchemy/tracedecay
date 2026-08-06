use std::sync::Arc;

use tracedecay_domain::{ObservationScopeV1, ProjectId};

use super::*;
use crate::application::host_admission::HostAdmissionScope;
use crate::mcp::server::message_search_cutover_tests::{
    MESSAGE_SEARCH_PROJECT_ID, seed_temporal_message, server_with_project_refresh_wake,
};

const PROVIDER: &str = "cursor";
const MESSAGE_ID: &str = "message.stored-lcm";
const CONTENT: &str = "already projected canonical content";

async fn unavailable_refresh_service() -> (
    Arc<dyn SessionRetrievalServicePort>,
    crate::sessions::lcm::LcmRawMessage,
    Arc<crate::mcp::server::McpServer>,
) {
    let (server, _project, _pin) =
        server_with_project_refresh_wake(Some(SessionTemporalRefreshWake::unavailable())).await;
    let runtime = server
        .host_admission_test_runtime_for_test()
        .expect("retained host-admission runtime");
    seed_temporal_message(
        runtime,
        HostAdmissionScope::Project,
        MESSAGE_SEARCH_PROJECT_ID,
        ObservationScopeV1::Project {
            project_id: ProjectId::new(MESSAGE_SEARCH_PROJECT_ID).expect("project identity"),
        },
        1,
        MESSAGE_SEARCH_ROOT_SESSION_ID,
        PROVIDER,
        MESSAGE_ID,
        CONTENT,
    )
    .await;
    runtime
        .lcm_ingest_raw_message_for_test(
            HostAdmissionScope::Project,
            &crate::sessions::SessionMessageRecord {
                provider: PROVIDER.to_string(),
                message_id: MESSAGE_ID.to_string(),
                session_id: MESSAGE_SEARCH_ROOT_SESSION_ID.to_string(),
                role: "assistant".to_string(),
                timestamp: Some(1),
                ordinal: 1,
                text: CONTENT.to_string(),
                kind: Some("message".to_string()),
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            },
        )
        .await
        .expect("canonical LCM raw message");
    let raw = runtime
        .project_lcm_raw_message_for_test(PROVIDER, MESSAGE_ID)
        .await
        .expect("registered project LCM read")
        .expect("canonical LCM raw message");
    let service = Arc::clone(
        server
            .project_session_retrieval_service
            .as_ref()
            .expect("project retrieval service"),
    );
    (service, raw, server)
}

#[tokio::test]
async fn describe_serves_projected_session_when_refresh_worker_is_unavailable() {
    let (service, _raw, server) = unavailable_refresh_service().await;

    let outcome = service
        .describe_lcm(LcmDescribeServiceCommand::new(
            PROVIDER,
            SessionId::new(MESSAGE_SEARCH_ROOT_SESSION_ID).expect("session identity"),
            LcmDescribeTarget::Session,
            RetrievalGrainV1::Session,
            SessionRetrievalStoreScope::Project,
        ))
        .await;

    let LcmDescribeServiceOutcome::Complete {
        description,
        retrieval: LcmRetrievalOutcome::Complete { .. },
        ..
    } = outcome
    else {
        panic!("already-projected describe must remain available: {outcome:?}");
    };
    assert_eq!(description.raw_message_count, 1);
    assert_eq!(description.provider, PROVIDER);
    assert_eq!(description.session_id, MESSAGE_SEARCH_ROOT_SESSION_ID);
    server.shutdown().await;
}

#[tokio::test]
async fn expand_hydrates_projected_content_when_refresh_worker_is_unavailable() {
    let (service, raw, server) = unavailable_refresh_service().await;

    let outcome = service
        .expand_lcm(LcmExpandServiceCommand::new(
            PROVIDER,
            SessionId::new(MESSAGE_SEARCH_ROOT_SESSION_ID).expect("session identity"),
            LcmExpandTarget::RawMessage {
                store_id: raw.store_id,
            },
            RetrievalGrainV1::Occurrence,
            LcmContentSlice {
                offset: 0,
                limit: 1_024,
            },
            0,
            None,
            None,
            SessionRetrievalStoreScope::Project,
        ))
        .await;

    let LcmExpandServiceOutcome::Complete {
        expansion,
        retrieval: LcmRetrievalOutcome::Complete { .. },
        ..
    } = outcome
    else {
        panic!("already-projected expand must remain available: {outcome:?}");
    };
    assert_eq!(expansion.content, CONTENT);
    server.shutdown().await;
}
