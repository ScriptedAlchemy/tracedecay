use std::collections::BTreeSet;
use std::sync::Mutex;

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestId,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::context::{
    BranchId, ProfileId, RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId,
    SessionStoreId, session_application_grant_digest,
};
use tracedecay_usecases::session::SessionRequestBinding;
use tracedecay_usecases::session::lcm::{
    LcmAuthorityTarget, LcmCompressionEvidence, LcmHostProtocol,
};

use super::*;

#[derive(Default)]
struct FakeStore {
    calls: Mutex<Vec<LcmAuthorityOperation>>,
    status: Mutex<Option<LcmStatus>>,
}

impl FakeStore {
    fn calls(&self) -> Vec<LcmAuthorityOperation> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

impl LcmDaemonStore for FakeStore {
    fn ingest(&self, _request: LcmPreflightRequest) -> StoreFuture<'_, LcmPreflightResponse> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(LcmAuthorityOperation::Ingest);
        }
        Box::pin(async {
            Ok(LcmPreflightResponse {
                status: "ok".to_owned(),
                should_compress: false,
                reason: "below_threshold".to_owned(),
                replay_messages: Vec::new(),
            })
        })
    }

    fn status(&self, _query: LcmStatusQuery) -> StoreFuture<'_, LcmStatus> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(LcmAuthorityOperation::Status);
        }
        let status = self.status.lock().ok().and_then(|status| status.clone());
        Box::pin(async move { status.ok_or_else(|| LcmError::Db("unavailable".to_owned())) })
    }

    fn doctor(&self, _query: LcmDoctorQuery) -> StoreFuture<'_, serde_json::Value> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(LcmAuthorityOperation::Doctor);
        }
        Box::pin(async { Ok(serde_json::json!({"status": "healthy", "findings": []})) })
    }
}

async fn table_row_count(database: &RegisteredGlobalDb, table: &'static str) -> i64 {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn request_context(
    operation: LcmAuthorityOperation,
    allowed: bool,
) -> (RequestContext, SessionRequestBinding, CancellationToken) {
    request_context_for_target(
        operation,
        allowed,
        UtcMicros(i64::MAX - 1),
        default_target(operation),
    )
}

fn request_context_until(
    operation: LcmAuthorityOperation,
    allowed: bool,
    expires_at: UtcMicros,
) -> (RequestContext, SessionRequestBinding, CancellationToken) {
    request_context_for_target(operation, allowed, expires_at, default_target(operation))
}

fn request_context_for_target(
    operation: LcmAuthorityOperation,
    allowed: bool,
    expires_at: UtcMicros,
    target: LcmAuthorityTarget,
) -> (RequestContext, SessionRequestBinding, CancellationToken) {
    let actor = ActorId::new("actor.lcm-test").unwrap();
    let identity = ResolvedSessionIdentity::for_project(
        ProfileId::new("profile.lcm-test").unwrap(),
        ProjectId::new("project.lcm-test").unwrap(),
        SessionStoreId::new("store.lcm-test").unwrap(),
        SessionRootId::new("root.lcm-test").unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.lcm-test").unwrap(),
            WorktreeId::new("worktree.lcm-test").unwrap(),
            BranchId::new("branch.lcm-test").unwrap(),
        ),
    );
    let scope = identity.session_request_scope().unwrap();
    let (capability, use_case) = lcm_authority_operation_identity(operation).unwrap();
    let (capability_digest, policy_digest, configuration_digest) =
        mount::lcm_binding_digests(&identity, &capability, &target).unwrap();
    let request_id = RequestId::new("request.lcm-test").unwrap();
    let cancellation = CancellationToken::for_application_request(request_id.as_str());
    let budgets = RequestBudgets::new(100, 1_000_000, 10_000).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.lcm-test").unwrap(),
        1,
        session_application_grant_digest(
            capability_digest,
            policy_digest,
            configuration_digest,
            &cancellation,
            budgets,
        )
        .unwrap(),
        actor.clone(),
        UtcMicros(1),
        expires_at,
        scope.clone(),
        if allowed {
            BTreeSet::from([capability])
        } else {
            BTreeSet::from([CapabilityId::new("capability.other").unwrap()])
        },
        if allowed {
            BTreeSet::from([use_case])
        } else {
            BTreeSet::from([UseCaseId::new("use-case.other").unwrap()])
        },
        DisclosureClass::Sensitive,
    )
    .unwrap();
    let token_id = cancellation.application_token_id().unwrap();
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at).unwrap(),
        CancellationContext::active(token_id).unwrap(),
    )
    .unwrap();
    let binding = SessionRequestBinding::new(
        identity,
        capability_digest,
        policy_digest,
        configuration_digest,
        cancellation.clone(),
        budgets,
    );
    (context, binding, cancellation)
}

fn default_target(operation: LcmAuthorityOperation) -> LcmAuthorityTarget {
    match operation {
        LcmAuthorityOperation::Ingest => target("hermes", Some("session.lcm-test")),
        LcmAuthorityOperation::Compact => target("cursor", Some("session.lcm-test")),
        LcmAuthorityOperation::Status => target("claude", None),
        LcmAuthorityOperation::Doctor => LcmAuthorityTarget::Store,
    }
}

fn hermes_ingest(messages: Vec<serde_json::Value>) -> LcmTranscriptIngestCommand {
    let mut preflight = preflight("hermes");
    preflight.messages = messages;
    let event_digest = canonical_sha256(&(
        &preflight.provider,
        &preflight.session_id,
        &preflight.messages,
    ))
    .unwrap();
    LcmTranscriptIngestCommand {
        preflight,
        protocol_revision: "hermes.turn-completed.v1".to_owned(),
        event_digest,
    }
}

fn preflight(provider: &str) -> LcmPreflightRequest {
    LcmPreflightRequest {
        provider: provider.to_owned(),
        session_id: "session.lcm-test".to_owned(),
        messages: Vec::new(),
        current_tokens: Some(1),
        threshold_tokens: Some(100),
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
    }
}

fn target(provider: &str, session_id: Option<&str>) -> LcmAuthorityTarget {
    LcmAuthorityTarget::Provider {
        provider: provider.to_owned(),
        session_id: session_id.map(str::to_owned),
    }
}

fn status_request(provider: &str, session_id: Option<&str>) -> LcmAuthorityRequest {
    LcmAuthorityRequest::Status(LcmStatusQuery {
        provider: provider.to_owned(),
        session_id: session_id.map(str::to_owned),
        deep: false,
    })
}

#[tokio::test]
async fn denied_command_never_reaches_daemon_store() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Status, false);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("claude", None),
            cancellation,
            request: status_request("claude", None),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Denied);
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn authentic_hermes_turn_is_committed_through_daemon_store() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let command = hermes_ingest(vec![serde_json::json!({
        "id": "message.hermes.1",
        "role": "user",
        "content": "authentic callback content"
    })]);
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Ingest, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("hermes", Some("session.lcm-test")),
            cancellation,
            request: LcmAuthorityRequest::Ingest(command),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Ready);
    assert!(response.receipt.committed_state.is_some());
    assert!(matches!(
        response.payload,
        Some(LcmAuthorityPayload::Ingest(_))
    ));
    assert_eq!(store.calls(), vec![LcmAuthorityOperation::Ingest]);
}

#[tokio::test]
async fn altered_hermes_turn_is_rejected_before_store_effect() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let mut command = hermes_ingest(vec![serde_json::json!({
        "id": "message.hermes.1",
        "role": "user",
        "content": "original callback content"
    })]);
    command.preflight.messages[0]["content"] = serde_json::json!("altered");
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Ingest, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("hermes", Some("session.lcm-test")),
            cancellation,
            request: LcmAuthorityRequest::Ingest(command),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::HostProtocolUnavailable
        }
    );
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn mismatched_live_cancellation_identity_is_denied_before_store_effect() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, binding, _) = request_context(LcmAuthorityOperation::Status, true);
    let cancellation = CancellationToken::for_application_request("request.other");

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("claude", None),
            cancellation,
            request: status_request("claude", None),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Denied);
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn request_target_not_bound_into_grant_is_denied_before_store_effect() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Status, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("cursor", None),
            cancellation,
            request: status_request("cursor", None),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Denied);
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn unavailable_store_is_typed_and_never_fabricates_empty_success() {
    let authority = DaemonLcmAuthority::unavailable();
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Status, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("claude", None),
            cancellation,
            request: LcmAuthorityRequest::Status(LcmStatusQuery {
                provider: "claude".to_owned(),
                session_id: None,
                deep: false,
            }),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::StoreAuthorityUnavailable
        }
    );
    assert!(response.payload.is_none());
}

#[tokio::test]
async fn live_cancellation_stops_before_store_effect() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Status, true);
    cancellation.cancel();

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("claude", None),
            cancellation,
            request: status_request("claude", None),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Cancelled);
    assert!(store.calls().is_empty());
    assert_eq!(
        response.receipt.execution.termination,
        OperationTermination::Cancelled
    );
}

#[tokio::test]
async fn expired_deadline_stops_before_store_effect() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, binding, cancellation) =
        request_context_until(LcmAuthorityOperation::Status, true, UtcMicros(2));

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("claude", None),
            cancellation,
            request: status_request("claude", None),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::TimedOut);
    assert!(store.calls().is_empty());
    assert_eq!(
        response.receipt.execution.termination,
        OperationTermination::TimedOut
    );
}

#[tokio::test]
async fn pressure_only_event_is_read_only_and_reports_missing_native_payload() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Compact, true);
    let command = LcmCompactionCommand {
        preflight: preflight("cursor"),
        evidence: LcmCompressionEvidence::PressureOnly {
            protocol: LcmHostProtocol::CursorPreCompact {
                protocol_revision: "cursor.precompact.v1".to_owned(),
                event_digest: canonical_sha256(&"cursor-pressure-event").unwrap(),
            },
        },
    };

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("cursor", Some("session.lcm-test")),
            cancellation,
            request: LcmAuthorityRequest::Compact(command),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::HostPayloadUnavailable
        }
    );
    assert!(store.calls().is_empty());
    assert!(response.payload.is_none());
    assert!(response.receipt.committed_state.is_none());
}

#[tokio::test]
async fn pressure_protocol_provider_mismatch_is_typed_before_ingest() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Compact, true);
    let command = LcmCompactionCommand {
        preflight: preflight("cursor"),
        evidence: LcmCompressionEvidence::PressureOnly {
            protocol: LcmHostProtocol::CodexContextCompacted {
                protocol_revision: "codex.context-compacted.v1".to_owned(),
                event_digest: canonical_sha256(&"mismatched-pressure-event").unwrap(),
            },
        },
    };

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("cursor", Some("session.lcm-test")),
            cancellation,
            request: LcmAuthorityRequest::Compact(command),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::HostProtocolUnavailable
        }
    );
    assert!(store.calls().is_empty());
    assert!(response.receipt.committed_state.is_none());
}

#[tokio::test]
async fn registered_authority_restart_reads_committed_hermes_turn() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
        directory.path(),
    )
    .await
    .unwrap();
    let first = DaemonLcmAuthority::registered(runtime.profile_database_arc());
    let command = hermes_ingest(vec![serde_json::json!({
        "id": "message.restart.1",
        "role": "user",
        "content": "durable session content"
    })]);
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Ingest, true);

    let written = first
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("hermes", Some("session.lcm-test")),
            cancellation,
            request: LcmAuthorityRequest::Ingest(command),
        })
        .await;

    assert_eq!(written.outcome, LcmAuthorityOutcome::Ready);
    assert!(written.receipt.committed_state.is_some());
    drop(first);

    let remounted = runtime.remount_profile_database_for_test().await.unwrap();
    let restarted = DaemonLcmAuthority::registered(remounted);
    let (context, binding, cancellation) = request_context_for_target(
        LcmAuthorityOperation::Status,
        true,
        UtcMicros(i64::MAX - 1),
        target("hermes", Some("session.lcm-test")),
    );
    let read = restarted
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("hermes", Some("session.lcm-test")),
            cancellation,
            request: LcmAuthorityRequest::Status(LcmStatusQuery {
                provider: "hermes".to_owned(),
                session_id: Some("session.lcm-test".to_owned()),
                deep: false,
            }),
        })
        .await;

    assert_eq!(read.outcome, LcmAuthorityOutcome::Ready);
    let Some(LcmAuthorityPayload::Status(status)) = read.payload else {
        panic!("restarted authority must return typed LCM status");
    };
    assert_eq!(status.raw_message_count, 1);
}

#[tokio::test]
async fn unsupported_pressure_preflight_does_not_create_session_or_raw_messages() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
        directory.path(),
    )
    .await
    .unwrap();
    let database = runtime.profile_database_arc();
    let authority = DaemonLcmAuthority::registered(Arc::clone(&database));
    let mut request = preflight("cursor");
    request.messages = vec![serde_json::json!({
        "id": "message.must-not-persist",
        "role": "user",
        "content": "unsupported payload"
    })];
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Compact, true);
    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            binding,
            target: target("cursor", Some("session.lcm-test")),
            cancellation,
            request: LcmAuthorityRequest::Compact(LcmCompactionCommand {
                preflight: request,
                evidence: LcmCompressionEvidence::PressureOnly {
                    protocol: LcmHostProtocol::CursorPreCompact {
                        protocol_revision: "cursor.precompact.v1".to_owned(),
                        event_digest: canonical_sha256(&"read-only-pressure").unwrap(),
                    },
                },
            }),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::HostPayloadUnavailable
        }
    );
    assert!(response.receipt.committed_state.is_none());
    for table in [
        "lcm_raw_messages",
        "lcm_summary_nodes",
        "lcm_summary_sources",
        "lcm_lifecycle_state",
        "session_summary_nodes",
        "session_summary_sources",
    ] {
        assert_eq!(
            table_row_count(&database, table).await,
            0,
            "{table} was mutated by unsupported pressure evidence"
        );
    }
}

#[tokio::test]
async fn registered_doctor_reads_snapshot_while_writer_lane_is_held() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
        directory.path(),
    )
    .await
    .unwrap();
    let database = runtime.profile_database_arc();

    let committed = database.begin_write_transaction().await.unwrap();
    committed
        .execute(
            "INSERT INTO sessions (provider, session_id, project_key, project_path)
             VALUES ('cursor', 'committed-doctor-read', 'project', '/project')",
            (),
        )
        .await
        .unwrap();
    committed.commit().await.unwrap();

    let task_database = Arc::clone(&database);
    let (writer_ready_tx, writer_ready_rx) = tokio::sync::oneshot::channel();
    let writer = tokio::spawn(async move {
        let transaction = task_database.begin_write_transaction().await.unwrap();
        transaction
            .execute(
                "INSERT INTO sessions (provider, session_id, project_key, project_path)
                 VALUES ('cursor', 'uncommitted-doctor-read', 'project', '/project')",
                (),
            )
            .await
            .unwrap();
        writer_ready_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    writer_ready_rx.await.unwrap();

    let authority = DaemonLcmAuthority::registered(database);
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Doctor, true);
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        authority.execute(LcmAuthorityInvocation {
            context,
            binding,
            target: LcmAuthorityTarget::Store,
            cancellation,
            request: LcmAuthorityRequest::Doctor(LcmDoctorQuery),
        }),
    )
    .await
    .expect("daemon Doctor must not wait for writer authority");

    assert_eq!(response.outcome, LcmAuthorityOutcome::Ready);
    let Some(LcmAuthorityPayload::Doctor(report)) = response.payload else {
        panic!("daemon Doctor must return a temporal health report");
    };
    assert_eq!(report["status"], "complete");
    assert_eq!(report.get("reason"), None);

    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn mounted_authority_rejects_identity_that_does_not_own_registered_shard() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
        directory.path(),
    )
    .await
    .unwrap();
    let database = runtime.profile_database_arc();
    let shard = database.binding().shard_id.clone();
    let profile_id = ProfileId::new(shard.profile_id.as_str()).unwrap();
    let profile_identity = ResolvedSessionIdentity::for_profile(
        profile_id.clone(),
        SessionStoreId::new("store.profile.lcm-test").unwrap(),
        SessionRootId::new("root.profile.lcm-test").unwrap(),
    );
    let mounted =
        mount_registered_lcm_authority(database.clone(), profile_identity, &shard).unwrap();
    let first = mounted
        .execute(LcmAuthorityRequest::Status(LcmStatusQuery {
            provider: "claude".to_owned(),
            session_id: Some("session.mounted.first".to_owned()),
            deep: false,
        }))
        .await
        .unwrap();
    let second = mounted
        .execute(LcmAuthorityRequest::Status(LcmStatusQuery {
            provider: "claude".to_owned(),
            session_id: Some("session.mounted.second".to_owned()),
            deep: false,
        }))
        .await
        .unwrap();
    assert_ne!(first.receipt.grant_digest, second.receipt.grant_digest);

    let project_identity = ResolvedSessionIdentity::for_project(
        profile_id,
        ProjectId::new("project.wrong-owner").unwrap(),
        SessionStoreId::new("store.project.wrong-owner").unwrap(),
        SessionRootId::new("root.project.wrong-owner").unwrap(),
        ResolvedGitRoute::new(
            RepositoryId::new("repository.wrong-owner").unwrap(),
            WorktreeId::new("worktree.wrong-owner").unwrap(),
            BranchId::new("branch.wrong-owner").unwrap(),
        ),
    );
    assert!(mount_registered_lcm_authority(database, project_identity, &shard).is_none());
}
