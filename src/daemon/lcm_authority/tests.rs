use std::collections::BTreeSet;
use std::sync::Mutex;

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;

struct ExactCanonicalSummaryAdmission {
    anchors: Vec<RetrievalAnchorId>,
    source_messages: Vec<tracedecay_sessions::runtime::lcm::LcmSummarySourceMessage>,
}

impl LcmCanonicalSummaryAdmissionPort for ExactCanonicalSummaryAdmission {
    fn admit<'a>(
        &'a self,
        _context: &'a RequestContext,
        _cancellation: &'a CancellationToken,
        command: &'a LcmCompactionCommand,
        summary_request: &'a LcmSummaryRequest,
    ) -> CanonicalSummaryFuture<'a> {
        Box::pin(async move {
            let LcmCompressionEvidence::ClaudeNativeSummary {
                summary_text,
                source_anchors,
                source_content_digest,
                ..
            } = &command.evidence
            else {
                return CanonicalSummaryAdmission::Unavailable(
                    LcmAuthorityUnavailableReason::HostProtocolUnavailable,
                );
            };
            let Ok(canonical_digest) = canonical_sha256(&self.source_messages) else {
                return CanonicalSummaryAdmission::Unavailable(
                    LcmAuthorityUnavailableReason::CanonicalSourceUnavailable,
                );
            };
            if source_anchors != &self.anchors
                || source_content_digest != &canonical_digest
                || summary_request.source_messages != self.source_messages
            {
                return CanonicalSummaryAdmission::Unavailable(
                    LcmAuthorityUnavailableReason::CanonicalSourceUnavailable,
                );
            }
            let Ok(source_state) = canonical_sha256(&(
                command.evidence.protocol().event_digest(),
                source_anchors,
                source_content_digest,
                &summary_request.source_range,
            )) else {
                return CanonicalSummaryAdmission::Unavailable(
                    LcmAuthorityUnavailableReason::CanonicalSourceUnavailable,
                );
            };
            CanonicalSummaryAdmission::Admitted {
                summary_text: summary_text.clone(),
                source_state,
            }
        })
    }
}

#[derive(Default)]
struct FakeStore {
    calls: Mutex<Vec<LcmAuthorityOperation>>,
    provided_summaries: Mutex<Vec<String>>,
    status: Mutex<Option<LcmStatus>>,
}

impl FakeStore {
    fn calls(&self) -> Vec<LcmAuthorityOperation> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    fn provided_summaries(&self) -> Vec<String> {
        self.provided_summaries
            .lock()
            .map(|summaries| summaries.clone())
            .unwrap_or_default()
    }
}

impl LcmDaemonStore for FakeStore {
    fn preflight(&self, _request: LcmPreflightRequest) -> StoreFuture<'_, LcmPreflightResponse> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(LcmAuthorityOperation::Preflight);
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

    fn compact(&self, request: LcmCompressionRequest) -> StoreFuture<'_, LcmCompressionResponse> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(LcmAuthorityOperation::Compact);
        }
        if let LcmSummarizerMode::Provided { summary_text, .. } = &request.summarizer
            && let Ok(mut summaries) = self.provided_summaries.lock()
        {
            summaries.push(summary_text.clone());
        }
        Box::pin(async move {
            let summary_request = matches!(&request.summarizer, LcmSummarizerMode::HermesAuxiliary)
                .then(|| LcmSummaryRequest {
                    provider: request.provider.clone(),
                    session_id: request.session_id.clone(),
                    focus_topic: request.focus_topic.clone(),
                    prompt: "Summarize the exact canonical source range".to_owned(),
                    source_range: tracedecay_sessions::runtime::lcm::LcmSummarySourceRange {
                        from_store_id: 11,
                        to_store_id: 12,
                    },
                    source_messages: vec![
                        tracedecay_sessions::runtime::lcm::LcmSummarySourceMessage {
                            store_id: 11,
                            role: "user".to_owned(),
                            content: "canonical visible source".to_owned(),
                        },
                        tracedecay_sessions::runtime::lcm::LcmSummarySourceMessage {
                            store_id: 12,
                            role: "assistant".to_owned(),
                            content: "[REDACTED]".to_owned(),
                        },
                    ],
                    extraction_request: None,
                });
            Ok(LcmCompressionResponse {
                status: "ok".to_owned(),
                reason: match request.summarizer {
                    LcmSummarizerMode::Provided { .. } => "native_summary",
                    LcmSummarizerMode::HermesAuxiliary => "needs_summary",
                    LcmSummarizerMode::Noop | LcmSummarizerMode::Fake { .. } => "no_summary",
                }
                .to_owned(),
                summary_nodes_created: 0,
                summary_nodes: Vec::new(),
                replay_messages: Vec::new(),
                replay_token_estimate: 0,
                replay_over_budget: false,
                compression_attempts: 0,
                fallback_used: false,
                context_recovery_hint: None,
                retry_status: None,
                frontier: tracedecay_sessions::runtime::lcm::LcmLifecycleState {
                    provider: request.provider,
                    conversation_id: request.session_id.clone(),
                    current_session_id: request.session_id,
                    current_frontier_store_id: Some(3),
                    last_finalized_session_id: None,
                    last_finalized_frontier_store_id: None,
                    maintenance_debt: Vec::new(),
                },
                summary_request,
            })
        })
    }

    fn session_boundary(
        &self,
        _request: LcmSessionBoundaryRequest,
    ) -> StoreFuture<'_, LcmSessionBoundaryResponse> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(LcmAuthorityOperation::SessionBoundary);
        }
        Box::pin(async {
            Ok(LcmSessionBoundaryResponse {
                status: "ok".to_owned(),
                recorded: true,
                reason: "recorded".to_owned(),
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
}

fn request_context(
    operation: LcmAuthorityOperation,
    allowed: bool,
) -> (RequestContext, CancellationToken) {
    request_context_until(operation, allowed, UtcMicros(i64::MAX - 1))
}

fn request_context_until(
    operation: LcmAuthorityOperation,
    allowed: bool,
    expires_at: UtcMicros,
) -> (RequestContext, CancellationToken) {
    let actor = ActorId::new("actor.lcm-test").unwrap();
    let scope = ResolvedScope::new(
        ProjectId::new("project.lcm-test").unwrap(),
        RepositoryId::new("repository.lcm-test").unwrap(),
        WorktreeId::new("worktree.lcm-test").unwrap(),
        None,
    )
    .unwrap();
    let (capability, use_case) = lcm_authority_operation_identity(operation).unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.lcm-test").unwrap(),
        1,
        canonical_sha256(&"lcm-test-grant").unwrap(),
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
    let request_id = RequestId::new("request.lcm-test").unwrap();
    let cancellation = CancellationToken::for_application_request(request_id.as_str());
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
    (context, cancellation)
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

fn canonical_source_messages() -> Vec<tracedecay_sessions::runtime::lcm::LcmSummarySourceMessage> {
    vec![
        tracedecay_sessions::runtime::lcm::LcmSummarySourceMessage {
            store_id: 11,
            role: "user".to_owned(),
            content: "canonical visible source".to_owned(),
        },
        tracedecay_sessions::runtime::lcm::LcmSummarySourceMessage {
            store_id: 12,
            role: "assistant".to_owned(),
            content: "[REDACTED]".to_owned(),
        },
    ]
}

fn claude_native_command(source_content_digest: ManifestDigest) -> LcmCompactionCommand {
    LcmCompactionCommand {
        preflight: preflight("claude"),
        focus_topic: None,
        expected_current_frontier_store_id: None,
        evidence: LcmCompressionEvidence::ClaudeNativeSummary {
            protocol: LcmHostProtocol::ClaudeCodePreCompact {
                protocol_revision: "claude.precompact.v1".to_owned(),
                event_digest: canonical_sha256(&"claude-event").unwrap(),
            },
            summary_text: "exact native Claude summary".to_owned(),
            source_anchors: vec![
                RetrievalAnchorId::new("anchor.source.11").unwrap(),
                RetrievalAnchorId::new("anchor.source.12").unwrap(),
            ],
            source_content_digest,
        },
    }
}

#[tokio::test]
async fn denied_command_never_reaches_daemon_store() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, cancellation) = request_context(LcmAuthorityOperation::Preflight, false);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Preflight(preflight("claude")),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Denied);
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn mismatched_live_cancellation_identity_is_denied_before_store_effect() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, _) = request_context(LcmAuthorityOperation::Preflight, true);
    let cancellation = CancellationToken::for_application_request("request.other");

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Preflight(preflight("claude")),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Denied);
    assert!(store.calls().is_empty());
}

#[tokio::test]
async fn unavailable_store_is_typed_and_never_fabricates_empty_success() {
    let authority = DaemonLcmAuthority::unavailable();
    let (context, cancellation) = request_context(LcmAuthorityOperation::Status, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
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
    let (context, cancellation) = request_context(LcmAuthorityOperation::Preflight, true);
    cancellation.cancel();

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Preflight(preflight("claude")),
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
    let (context, cancellation) =
        request_context_until(LcmAuthorityOperation::Preflight, true, UtcMicros(2));

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Preflight(preflight("claude")),
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
async fn non_claude_summary_content_is_unrepresentable_as_native_admission() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, cancellation) = request_context(LcmAuthorityOperation::Compact, true);
    let protocol = LcmHostProtocol::CursorPreCompact {
        protocol_revision: "cursor.precompact.v1".to_owned(),
        event_digest: canonical_sha256(&"cursor-event").unwrap(),
    };
    let command = LcmCompactionCommand {
        preflight: preflight("cursor"),
        focus_topic: None,
        expected_current_frontier_store_id: None,
        evidence: LcmCompressionEvidence::ClaudeNativeSummary {
            protocol,
            summary_text: "untrusted cursor summary".to_owned(),
            source_anchors: vec![
                tracedecay_domain::RetrievalAnchorId::new("anchor.cursor").unwrap(),
            ],
            source_content_digest: canonical_sha256(&"source").unwrap(),
        },
    };

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
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
}

#[tokio::test]
async fn pressure_only_event_ingests_then_reports_missing_native_payload() {
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, cancellation) = request_context(LcmAuthorityOperation::Compact, true);
    let command = LcmCompactionCommand {
        preflight: preflight("cursor"),
        focus_topic: None,
        expected_current_frontier_store_id: None,
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
    assert_eq!(store.calls(), vec![LcmAuthorityOperation::Preflight]);
    assert!(response.payload.is_none());
    assert!(response.receipt.committed_state.is_some());
}

#[tokio::test]
async fn native_summary_commit_requires_exact_canonical_hydration_and_redaction_state() {
    let source_messages = canonical_source_messages();
    let anchors = vec![
        RetrievalAnchorId::new("anchor.source.11").unwrap(),
        RetrievalAnchorId::new("anchor.source.12").unwrap(),
    ];
    let canonical_digest = canonical_sha256(&source_messages).unwrap();
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone()).with_canonical_summary_admission(
        Arc::new(ExactCanonicalSummaryAdmission {
            anchors,
            source_messages,
        }),
    );
    let (context, cancellation) = request_context(LcmAuthorityOperation::Compact, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Compact(claude_native_command(canonical_digest)),
        })
        .await;

    assert_eq!(response.outcome, LcmAuthorityOutcome::Ready);
    assert!(response.receipt.committed_state.is_some());
    assert_eq!(
        store.calls(),
        vec![
            LcmAuthorityOperation::Compact,
            LcmAuthorityOperation::Compact
        ]
    );
    assert_eq!(
        store.provided_summaries(),
        vec!["exact native Claude summary".to_owned()]
    );
}

#[tokio::test]
async fn missing_summary_relation_authority_never_commits_native_summary() {
    let source_messages = canonical_source_messages();
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone());
    let (context, cancellation) = request_context(LcmAuthorityOperation::Compact, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Compact(claude_native_command(
                canonical_sha256(&source_messages).unwrap(),
            )),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::SummaryRelationAuthorityUnavailable
        }
    );
    assert_eq!(store.calls(), vec![LcmAuthorityOperation::Compact]);
    assert!(store.provided_summaries().is_empty());
    assert!(response.receipt.committed_state.is_some());
}

#[tokio::test]
async fn canonical_source_digest_mismatch_never_commits_native_summary() {
    let source_messages = canonical_source_messages();
    let anchors = vec![
        RetrievalAnchorId::new("anchor.source.11").unwrap(),
        RetrievalAnchorId::new("anchor.source.12").unwrap(),
    ];
    let store = Arc::new(FakeStore::default());
    let authority = DaemonLcmAuthority::with_store(store.clone()).with_canonical_summary_admission(
        Arc::new(ExactCanonicalSummaryAdmission {
            anchors,
            source_messages,
        }),
    );
    let (context, cancellation) = request_context(LcmAuthorityOperation::Compact, true);

    let response = authority
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Compact(claude_native_command(
                canonical_sha256(&"wrong source").unwrap(),
            )),
        })
        .await;

    assert_eq!(
        response.outcome,
        LcmAuthorityOutcome::Unavailable {
            reason: LcmAuthorityUnavailableReason::CanonicalSourceUnavailable
        }
    );
    assert_eq!(store.calls(), vec![LcmAuthorityOperation::Compact]);
    assert!(store.provided_summaries().is_empty());
    assert!(response.receipt.committed_state.is_some());
}

#[tokio::test]
async fn registered_authority_restart_reads_committed_preflight_state() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::profile(
        directory.path(),
    )
    .await
    .unwrap();
    let first = DaemonLcmAuthority::registered(runtime.profile_database_arc());
    let mut request = preflight("claude");
    request.messages = vec![serde_json::json!({
        "id": "message.restart.1",
        "role": "user",
        "content": "durable session content"
    })];
    let (context, cancellation) = request_context(LcmAuthorityOperation::Preflight, true);

    let written = first
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Preflight(request),
        })
        .await;

    assert_eq!(written.outcome, LcmAuthorityOutcome::Ready);
    assert!(written.receipt.committed_state.is_some());
    drop(first);

    let remounted = runtime.remount_profile_database_for_test().await.unwrap();
    let restarted = DaemonLcmAuthority::registered(remounted);
    let (context, cancellation) = request_context(LcmAuthorityOperation::Status, true);
    let read = restarted
        .execute(LcmAuthorityInvocation {
            context,
            cancellation,
            request: LcmAuthorityRequest::Status(LcmStatusQuery {
                provider: "claude".to_owned(),
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
