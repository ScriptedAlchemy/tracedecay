use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tracedecay_domain::{ObservationSourceRangeV1, ProviderId};
use tracedecay_hooks::{HookAdmissionWorkSettlementV1, NativeContextScoutLifecycleV1};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;

use super::*;
use crate::handlers::hook_runtime::context_scout::{
    NativeLifecycleAdmissionV1, admit_native_context_scout_lifecycle,
};

struct LegacyLifecycleFixture {
    _temporary: tempfile::TempDir,
    graph: TraceDecay,
    runtime: Arc<HostAdmissionTestRuntimeV1>,
    envelope: tracedecay_hooks::HookEventEnvelopeV2,
    lifecycle: NativeContextScoutLifecycleV1,
    sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
}

impl LegacyLifecycleFixture {
    async fn open() -> Self {
        Self::open_for_host(tracedecay_domain::NativeHostIdentityV1::KimiCode).await
    }

    async fn open_for_host(host: tracedecay_domain::NativeHostIdentityV1) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let profile = temporary.path().join("profile");
        let root = temporary.path().join("project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn saved() {}\n").unwrap();
        let (graph, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &profile,
            &root,
            "project.legacy-hook-continuity",
        )
        .await
        .unwrap();
        let hook_runtime = tracedecay_project::runtime_ports::hook_runtime(
            tracedecay_runtime_core::config::ProfileRoot::new(profile),
        )
        .unwrap();
        let worktree = tracedecay_agent_hosts::hooks::hook_worktree_id_for_layout(
            &hook_runtime,
            graph.hook_store_layout(),
        )
        .unwrap();
        let now = hook_now();
        let binding = tracedecay_hooks::published_hook_scope_binding(
            &graph.hook_store_layout().data_root,
            worktree,
            host,
            now,
        )
        .unwrap();
        let lifecycle = NativeContextScoutLifecycleV1::new(
            "session.legacy-hook",
            "call.legacy-saved",
            [61; 16],
        )
        .unwrap();
        let envelope = tracedecay_hooks::HookEventEnvelopeV2 {
            schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
            event_id: lifecycle.event_id,
            producer: host,
            protected_session_id: tracedecay_agent_hosts::hooks::protected_native_session_id(
                lifecycle.session_id.as_str(),
            ),
            project_id: binding.project_id,
            repository_id: binding.repository_id,
            worktree_id: binding.worktree_id,
            worktree_epoch: binding.worktree_epoch,
            binding_token: binding.binding_token,
            observed_at: now,
            ordering: tracedecay_hooks::HookOrderingV1::Unknown,
            event: tracedecay_hooks::HookEventV2::SavedEdit {
                file_id: [62; 16],
                changed_range_count: 1,
            },
        };
        let project = graph
            .store_layout()
            .identity
            .project_id
            .as_ref()
            .unwrap()
            .clone()
            .try_into()
            .unwrap();
        let scope = (hook_runtime.scope_resolver)(&root, &project).unwrap();
        let sessions = runtime
            .registered_database_arc(HostAdmissionScope::Project)
            .unwrap();
        assert_eq!(tracedecay_daemon_service::context_scout_lifecycle::register_context_scout_lifecycle_authority(
            envelope.project_id, envelope.worktree_id, scope.project_id, scope.worktree_id, &sessions,
        ), tracedecay_daemon_service::context_scout_lifecycle::AuthorityRegistrationV1::Registered);
        Self {
            _temporary: temporary,
            graph,
            runtime,
            envelope,
            lifecycle,
            sessions,
        }
    }

    async fn commit_lifecycle(&self, call: &str, start: u64) {
        let lifecycle =
            NativeContextScoutLifecycleV1::new(self.lifecycle.session_id.as_str(), call, [60; 16])
                .unwrap();
        assert_eq!(
            admit_native_context_scout_lifecycle(
                &self.sessions,
                Some(&self.runtime.background_cpu()),
                ProviderId::new(self.envelope.producer.hook_key()).unwrap(),
                &lifecycle,
                ObservationSourceRangeV1::new(start, start + 1).unwrap(),
            )
            .await,
            NativeLifecycleAdmissionV1::Ready
        );
    }

    async fn commit_conflicting_lifecycle(&self) {
        use tracedecay_domain::{
            CanonicalBoundaryKindV1, CanonicalObservationEnvelopeV1,
            CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
            CanonicalObservationRelationsV1, ObservationId, ObservationIdentityMaterialV1,
            ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
            ObservationSourceIdentityV1, RetentionClass,
        };
        use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
        use tracedecay_sessions::observation::{
            CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
        };
        use tracedecay_store::StoreShardScopeV1;

        let binding = self.sessions.binding();
        let StoreShardScopeV1::ProjectSessions { project_id } = &binding.shard_id.scope else {
            panic!("project fixture");
        };
        let provider = ProviderId::new("kimi").unwrap();
        let range = ObservationSourceRangeV1::new(99, 100).unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let source = ObservationSourceIdentityV1::for_provider(
            provider.clone(),
            self.lifecycle.session_id.clone(),
        )
        .unwrap();
        let agent = ObservationId::new(self.lifecycle.session_id.as_str().to_owned()).unwrap();
        let parsed = tracedecay_privacy::parse_normalized_observation_record_v1(
            &serde_json::to_vec(&self.lifecycle).unwrap(),
            range,
            ObservationOrderingDomainV1::DaemonSequence,
            |_| {
                CanonicalObservationEnvelopeV1::new(
                    provider,
                    "hook_tool_after",
                    self.lifecycle.call_id.clone(),
                    CanonicalObservationRelationsV1::new(self.lifecycle.session_id.clone())
                        .with_thread_id(agent.clone())
                        .with_agent_id(agent)
                        .with_turn_id(self.lifecycle.call_id.clone())
                        .with_message_id(ObservationId::new("call.conflicting-lifecycle").unwrap()),
                    vec![CanonicalObservationFactV1::Boundary {
                        boundary_kind: CanonicalBoundaryKindV1::TurnEnd,
                    }],
                    CanonicalObservationEvidenceV1::new(
                        ObservationOrderingDomainV1::DaemonSequence,
                        range,
                    ),
                )
                .map_err(|_| tracedecay_privacy::ObservationRecordParseErrorV1::NormalizationFailed)
            },
        )
        .unwrap();
        let identity = ObservationIdentityMaterialV1::for_native_record(
            source,
            scope,
            ObservationSourceGenerationV1::new(1).unwrap(),
            range,
            ObservationOrderingDomainV1::DaemonSequence,
            self.lifecycle.call_id.clone(),
        )
        .unwrap();
        let facade = HostAdmissionFacade::new(
            HostAdmissionAuthorities::for_project(
                binding.shard_id.brain_id.clone(),
                binding.shard_id.profile_id.clone(),
                project_id.clone(),
                &self.sessions,
            )
            .with_background_cpu(self.runtime.background_cpu()),
        );
        let outcome = facade
            .capture_observation(
                CaptureObservationRequest::new(
                    parsed,
                    identity,
                    None,
                    RetentionClass::new("transcript.hook-lifecycle.v1").unwrap(),
                    ObservationCancellation::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            CaptureObservationOutcome::Persisted { .. }
                | CaptureObservationOutcome::AcceptedForReplay { .. }
        ));
    }

    fn write_shipped_ledger(&self, retained: bool, owed: bool) {
        let root = hook_v2_admission_ledger_root(
            &self.graph.hook_store_layout().data_root,
            self.envelope.producer,
        );
        drop(
            tracedecay_hooks::HookAdmissionLedgerV1::open(
                &root,
                self.envelope.producer,
                tracedecay_hooks::HookAdmissionLedgerLimitsV1::stock(),
                hook_now(),
            )
            .unwrap(),
        );
        let mut bytes = b"TDL2".to_vec();
        bytes.extend_from_slice(&2u16.to_le_bytes());
        if retained {
            let canonical = daemon_mint_hook_v2_envelope(&self.envelope);
            let mut body = canonical.event_id.to_vec();
            body.extend_from_slice(
                &tracedecay_hooks::admission_ledger::hook_admission_digest(&canonical).unwrap(),
            );
            body.extend_from_slice(&hook_now().0.to_le_bytes());
            if owed {
                body.extend_from_slice(
                    &tracedecay_domain::canonical_json_bytes(&self.envelope).unwrap(),
                );
            }
            append_shipped_frame(&mut bytes, 1, &body);
            // An expired admission forces the real legacy-open rewrite, which
            // must preserve reconstructed provenance across the next restart.
            let mut expired = body[..56].to_vec();
            expired[..16].copy_from_slice(&[59; 16]);
            expired[48..56].copy_from_slice(
                &(hook_now().0 - tracedecay_hooks::MAX_SPOOL_AGE_MICROS - 1).to_le_bytes(),
            );
            append_shipped_frame(&mut bytes, 1, &expired);
        }
        std::fs::write(root.join("admissions.v2.log"), bytes).unwrap();
    }

    async fn admit(&self) -> HookV2AdmissionOutcomeV1 {
        admit_hook_v2_replayed_envelope_with_lifecycle(
            &self.graph,
            &self.envelope,
            Some(self.lifecycle.session_id.clone()),
            Some(self.lifecycle.clone()),
            &self.sessions,
            &self.runtime.background_cpu(),
            hook_now(),
        )
        .await
    }

    fn pending(&self) -> Vec<tracedecay_hooks::HookEventEnvelopeV2> {
        hook_v2_pending_work_envelopes(
            &self.graph.hook_store_layout().data_root,
            self.envelope.producer,
            hook_now(),
        )
        .unwrap()
    }

    fn restart(&self) {
        forget_hook_v2_admission_ledger_for_test(
            &self.graph.hook_store_layout().data_root,
            self.envelope.producer,
        );
    }

    fn producer(
        &self,
        effects: Arc<AtomicUsize>,
    ) -> Arc<tracedecay_daemon_service::BoundedHookOrchestratorV1> {
        let producer = tracedecay_daemon_service::BoundedHookOrchestratorV1::new(
            1,
            move |request: tracedecay_daemon_service::HookOrchestrationRequestV1, _| {
                let effects = Arc::clone(&effects);
                async move {
                    if request.trigger
                        == tracedecay_daemon_service::HookOrchestrationTriggerV1::IntentBoundary
                    {
                        assert!(
                            matches!(
                                request.hook.envelope().event,
                                tracedecay_hooks::HookEventV2::SessionBoundary {
                                    boundary: tracedecay_hooks::HookBoundaryV1::End
                                        | tracedecay_hooks::HookBoundaryV1::TurnComplete,
                                }
                            ),
                            "only the real Stop intent companion is outside the producer count"
                        );
                        assert!(request.completion.is_none());
                        return;
                    }
                    assert!(matches!(
                        request.trigger,
                        tracedecay_daemon_service::HookOrchestrationTriggerV1::SavedEdit
                            | tracedecay_daemon_service::HookOrchestrationTriggerV1::Stop
                    ));
                    assert!(
                        request.lifecycle.is_some(),
                        "owed edit requires its exact lifecycle"
                    );
                    effects.fetch_add(1, Ordering::SeqCst);
                }
            },
        )
        .unwrap();
        assert!(
            tracedecay_daemon_service::register_hook_orchestration_runtime(
                self.envelope.project_id,
                self.envelope.worktree_id,
                &producer
            )
        );
        producer
    }

    async fn settled(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.pending().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("producer completion settles the durable event");
    }
}

impl Drop for LegacyLifecycleFixture {
    fn drop(&mut self) {
        self.restart();
        assert!(tracedecay_daemon_service::context_scout_lifecycle::unregister_context_scout_lifecycle_authority(self.envelope.project_id, self.envelope.worktree_id, &self.sessions));
    }
}

fn append_shipped_frame(bytes: &mut Vec<u8>, kind: u8, body: &[u8]) {
    let start = bytes.len();
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(body);
    let checksum = tracedecay_domain::framed_log::checksum(&bytes[start..]);
    bytes.extend_from_slice(&checksum[..8]);
}

#[tokio::test]
async fn fresh_saved_edit_runs_after_shipped_compacted_or_header_only_ledger() {
    for retained in [false, true] {
        let mut fixture = LegacyLifecycleFixture::open().await;
        fixture.commit_lifecycle("call.legacy-previous", 99).await;
        fixture.write_shipped_ledger(retained, false);
        // A compacted unrelated admission is not this fresh native event.
        fixture.envelope.event_id = [63; 16];
        fixture.lifecycle.event_id = fixture.envelope.event_id;
        let effects = Arc::new(AtomicUsize::new(0));
        let producer = fixture.producer(Arc::clone(&effects));
        assert!(matches!(
            fixture.admit().await,
            HookV2AdmissionOutcomeV1::Admitted { .. }
        ));
        fixture.settled().await;
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        fixture.restart();
        assert!(matches!(
            fixture.admit().await,
            HookV2AdmissionOutcomeV1::ExactDuplicate { .. }
        ));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert!(producer.shutdown().await);
    }
}

#[tokio::test]
async fn retained_legacy_receipt_keeps_producer_work_owed_until_execution() {
    let fixture = LegacyLifecycleFixture::open().await;
    fixture
        .commit_lifecycle(fixture.lifecycle.call_id.as_str(), 99)
        .await;
    fixture.write_shipped_ledger(true, true);
    assert_eq!(fixture.pending(), [fixture.envelope.clone()]);
    fixture.restart();
    let effects = Arc::new(AtomicUsize::new(0));
    let producer = fixture.producer(Arc::clone(&effects));
    assert!(matches!(
        fixture.admit().await,
        HookV2AdmissionOutcomeV1::Admitted { .. }
    ));
    fixture.settled().await;
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let canonical = daemon_mint_hook_v2_envelope(&fixture.envelope);
    let receipt = stage_hook_v2_admission(
        &fixture.graph.hook_store_layout().data_root,
        &canonical,
        None,
        None,
        None,
        hook_now(),
    )
    .unwrap();
    assert!(receipt.receipt.order_reconstructed);
    assert_eq!(receipt.receipt.order, 0);
    assert_eq!(
        receipt.receipt.work_settlement,
        Some(HookAdmissionWorkSettlementV1::Completed)
    );
    receipt.commit.wait().unwrap();
    assert!(producer.shutdown().await);
}

#[tokio::test]
async fn missing_or_conflicting_legacy_receipt_stays_owed_through_rewrite_and_restart() {
    for conflicting in [false, true] {
        let fixture = LegacyLifecycleFixture::open().await;
        if conflicting {
            fixture.commit_conflicting_lifecycle().await;
        } else {
            fixture.commit_lifecycle("call.other-committed", 99).await;
        }
        fixture.write_shipped_ledger(true, true);
        for _ in 0..2 {
            assert!(matches!(
                fixture.admit().await,
                HookV2AdmissionOutcomeV1::Backpressured
            ));
            assert_eq!(fixture.pending(), [fixture.envelope.clone()]);
            fixture.restart();
        }
    }
}

#[tokio::test]
async fn a_later_canonical_receipt_truthfully_supersedes_retained_legacy_work() {
    let fixture = LegacyLifecycleFixture::open().await;
    fixture
        .commit_lifecycle(fixture.lifecycle.call_id.as_str(), 99)
        .await;
    fixture.commit_lifecycle("call.actual-newer", 100).await;
    fixture.write_shipped_ledger(true, true);
    let effects = Arc::new(AtomicUsize::new(0));
    let producer = fixture.producer(Arc::clone(&effects));
    assert!(matches!(
        fixture.admit().await,
        HookV2AdmissionOutcomeV1::ExactDuplicate { .. }
    ));
    assert!(fixture.pending().is_empty());
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    let canonical = daemon_mint_hook_v2_envelope(&fixture.envelope);
    let receipt = stage_hook_v2_admission(
        &fixture.graph.hook_store_layout().data_root,
        &canonical,
        None,
        None,
        None,
        hook_now(),
    )
    .unwrap();
    assert_eq!(
        receipt.receipt.work_settlement,
        Some(HookAdmissionWorkSettlementV1::Superseded)
    );
    receipt.commit.wait().unwrap();
    assert!(producer.shutdown().await);
}

#[tokio::test]
async fn disabled_claude_stop_follows_the_prior_session_cursor_without_minting_lifecycle() {
    let mut fixture =
        LegacyLifecycleFixture::open_for_host(tracedecay_domain::NativeHostIdentityV1::ClaudeCode)
            .await;
    assert!(!hook_v2_native_lifecycle_enabled(&fixture.graph, fixture.envelope.producer).await);
    fixture
        .commit_lifecycle("call.prior-real-session", 99)
        .await;
    fixture.write_shipped_ledger(false, false);
    fixture.envelope.event = tracedecay_hooks::HookEventV2::SessionBoundary {
        boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
    };
    let effects = Arc::new(AtomicUsize::new(0));
    let producer = fixture.producer(Arc::clone(&effects));
    for duplicate in [false, true] {
        let outcome = admit_hook_v2_replayed_envelope_with_lifecycle(
            &fixture.graph,
            &fixture.envelope,
            Some(fixture.lifecycle.session_id.clone()),
            None,
            &fixture.sessions,
            &fixture.runtime.background_cpu(),
            hook_now(),
        )
        .await;
        if duplicate {
            assert!(matches!(
                outcome,
                HookV2AdmissionOutcomeV1::ExactDuplicate { .. }
            ));
        } else {
            assert!(matches!(outcome, HookV2AdmissionOutcomeV1::Admitted { .. }));
        }
        fixture.settled().await;
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        fixture.restart();
    }
    let canonical = daemon_mint_hook_v2_envelope(&fixture.envelope);
    let receipt = stage_hook_v2_admission(
        &fixture.graph.hook_store_layout().data_root,
        &canonical,
        None,
        None,
        None,
        hook_now(),
    )
    .unwrap();
    assert_eq!(receipt.receipt.order, 99);
    assert_eq!(
        receipt.receipt.work_settlement,
        Some(HookAdmissionWorkSettlementV1::Completed)
    );
    assert!(
        receipt.native_lifecycle.is_none(),
        "Stop cannot attach a tool lifecycle"
    );
    receipt.commit.wait().unwrap();
    assert_eq!(
        super::super::super::context_scout::native_lifecycle_order_floor(
            &fixture.sessions,
            ProviderId::new(fixture.envelope.producer.hook_key()).unwrap(),
            fixture.lifecycle.session_id.clone(),
        )
        .await,
        Some(99),
        "disabled policy must not mint another lifecycle or advance the source cursor",
    );
    assert!(producer.shutdown().await);
}
