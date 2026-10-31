use std::collections::BTreeSet;

use tracedecay_contracts::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestId,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_session_memory::context::{
    BranchId, ProfileId, RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId,
    SessionStoreId, session_application_grant_digest,
};
use tracedecay_session_memory::session::SessionRequestBinding;
use tracedecay_session_memory::session::lcm::{
    LcmAuthorityTarget, LcmCompressionEvidence, LcmHostProtocol,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;

async fn table_row_count(database: &RegisteredGlobalDb, table: &'static str) -> i64 {
    let snapshot = database.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn registered_authority() -> (
    tempfile::TempDir,
    tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
    DaemonLcmAuthority,
) {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
        directory.path(),
    )
    .await
    .unwrap();
    let authority = DaemonLcmAuthority::registered(runtime.profile_database_arc(), None);
    (directory, runtime, authority)
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
        LcmAuthorityOperation::Compact => target("cursor", Some("session.lcm-test")),
        LcmAuthorityOperation::Status => target("claude", None),
        LcmAuthorityOperation::Doctor => LcmAuthorityTarget::Store,
    }
}

fn preflight(provider: &str) -> LcmPreflightRequest {
    LcmPreflightRequest {
        provider: provider.to_owned(),
        session_id: "session.lcm-test".to_owned(),
        messages: Vec::new(),
        current_tokens: Some(1),
        overrides: tracedecay_lcm::LcmCompressionOverrides {
            threshold_tokens: Some(100),
            ..Default::default()
        },
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
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
async fn denied_command_is_rejected_before_registered_store_access() {
    let (_directory, _runtime, authority) = registered_authority().await;
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
}

#[tokio::test]
async fn mismatched_live_cancellation_identity_is_denied_before_store_effect() {
    let (_directory, _runtime, authority) = registered_authority().await;
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
}

#[tokio::test]
async fn request_target_not_bound_into_grant_is_denied_before_store_effect() {
    let (_directory, _runtime, authority) = registered_authority().await;
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
}

#[tokio::test]
async fn live_cancellation_stops_before_store_effect() {
    let (_directory, _runtime, authority) = registered_authority().await;
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
    assert_eq!(
        response.receipt.execution.termination,
        OperationTermination::Cancelled
    );
}

#[tokio::test]
async fn expired_deadline_stops_before_store_effect() {
    let (_directory, _runtime, authority) = registered_authority().await;
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
    assert_eq!(
        response.receipt.execution.termination,
        OperationTermination::TimedOut
    );
}

#[tokio::test]
async fn pressure_only_event_compacts_daemon_owned_without_caller_payload() {
    let (_directory, runtime, authority) = registered_authority().await;
    let database = runtime.profile_database_arc();
    let (context, binding, cancellation) = request_context(LcmAuthorityOperation::Compact, true);
    let mut pressure_preflight = preflight("cursor");
    pressure_preflight.messages = vec![serde_json::json!({
        "id": "message.caller-authored",
        "role": "user",
        "content": "caller-authored text must never reach the store"
    })];
    let command = LcmCompactionCommand {
        preflight: pressure_preflight,
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
            request: LcmAuthorityRequest::Compact(Box::new(command)),
        })
        .await;

    // A pressure signal triggers the daemon-owned compaction route; the
    // summarization source is the daemon's canonical store, never
    // caller-authored preflight text.
    assert_eq!(response.outcome, LcmAuthorityOutcome::Ready);
    assert!(matches!(
        response.payload,
        Some(LcmAuthorityPayload::Compaction(_))
    ));
    assert!(response.receipt.committed_state.is_some());
    assert_eq!(table_row_count(&database, "lcm_raw_messages").await, 0);
}

#[tokio::test]
async fn pressure_protocol_provider_mismatch_is_typed_before_ingest() {
    let (_directory, _runtime, authority) = registered_authority().await;
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
            request: LcmAuthorityRequest::Compact(Box::new(command)),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::HostProtocolUnavailable
        }
    );
    assert!(response.receipt.committed_state.is_none());
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
    let authority = DaemonLcmAuthority::registered(database.clone(), None);
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
            request: LcmAuthorityRequest::Compact(Box::new(LcmCompactionCommand {
                preflight: request,
                evidence: LcmCompressionEvidence::PressureOnly {
                    protocol: LcmHostProtocol::CursorPreCompact {
                        protocol_revision: "cursor.precompact.v1".to_owned(),
                        event_digest: canonical_sha256(&"read-only-pressure").unwrap(),
                    },
                },
            })),
        })
        .await;

    // The daemon-owned compaction route runs against its own canonical
    // store; for a session it never ingested there is nothing to compact
    // and, critically, the caller-authored preflight payload must not mint
    // durable session, raw-message, or summary state.
    assert_eq!(response.outcome, LcmAuthorityOutcome::Ready);
    for table in [
        "lcm_raw_messages",
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

    let task_database = database.clone();
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

    let authority = DaemonLcmAuthority::registered(database, None);
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
        mount_registered_lcm_authority(database.clone(), profile_identity, &shard, None).unwrap();
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
    assert_eq!(first.outcome, LcmAuthorityOutcome::Ready);
    assert_eq!(second.outcome, LcmAuthorityOutcome::Ready);
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
    assert!(mount_registered_lcm_authority(database, project_identity, &shard, None).is_none());
}

#[test]
fn pressure_conversion_keeps_every_override_and_drops_host_content() {
    let overrides = tracedecay_lcm::LcmCompressionOverrides {
        threshold_tokens: Some(11),
        max_assembly_tokens: Some(12),
        leaf_chunk_tokens: Some(13),
        max_source_messages: Some(14),
        summary_fan_in: Some(15),
        incremental_max_depth: Some(-1),
        fresh_tail_count: Some(0),
        dynamic_leaf_chunk_enabled: Some(true),
        dynamic_leaf_chunk_max: Some(18),
        context_length: Some(19),
        reserve_tokens_floor: Some(20),
    };
    let request = pressure_compression_request(LcmPreflightRequest {
        provider: "cursor".to_owned(),
        session_id: "session.lcm-test".to_owned(),
        messages: vec![serde_json::json!({
            "role": "user",
            "content": "host-authored text that must not be compacted",
        })],
        current_tokens: Some(42),
        overrides: overrides.clone(),
        ignore_session_patterns: vec!["ignored-*".to_owned()],
        stateless_session_patterns: vec!["stateless-*".to_owned()],
    });

    assert_eq!(
        request,
        LcmCompressionRequest {
            provider: "cursor".to_owned(),
            session_id: "session.lcm-test".to_owned(),
            messages: Vec::new(),
            current_tokens: Some(42),
            focus_topic: None,
            ignore_session_patterns: vec!["ignored-*".to_owned()],
            stateless_session_patterns: vec!["stateless-*".to_owned()],
            ignore_message_patterns: Vec::new(),
            expected_current_frontier_store_id: None,
            overrides,
            summarizer: LcmSummarizerMode::HermesAuxiliary,
        }
    );
}
