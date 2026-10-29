use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_domain::{ObservationSourceRangeV1, ProviderId};
use tracedecay_hooks::NativeContextScoutLifecycleV1;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_sessions::admission::HostAdmissionScope;

use super::*;
use crate::handlers::hook_runtime::context_scout::{
    NativeLifecycleAdmissionV1, admit_native_context_scout_lifecycle,
};

struct LifecycleFixture {
    _temporary: tempfile::TempDir,
    graph: TraceDecay,
    runtime: Arc<HostAdmissionTestRuntimeV1>,
    envelope: tracedecay_hooks::HookEventEnvelopeV2,
    lifecycle: NativeContextScoutLifecycleV1,
    sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
}

impl LifecycleFixture {
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
}

impl Drop for LifecycleFixture {
    fn drop(&mut self) {
        self.restart();
        assert!(tracedecay_daemon_service::context_scout_lifecycle::unregister_context_scout_lifecycle_authority(self.envelope.project_id, self.envelope.worktree_id, &self.sessions));
    }
}

#[tokio::test]
async fn unavailable_claude_policy_preserves_owed_work_without_lifecycle_or_producer_effects() {
    let fixture =
        LifecycleFixture::open_for_host(tracedecay_domain::NativeHostIdentityV1::ClaudeCode).await;
    fixture
        .commit_lifecycle("call.prior-real-session", 99)
        .await;
    let canonical = daemon_mint_hook_v2_envelope(&fixture.envelope);
    let mut native_lifecycle = fixture.lifecycle.clone();
    native_lifecycle.event_id = canonical.event_id;
    let staged = stage_hook_v2_admission(
        &fixture.graph.hook_store_layout().data_root,
        &canonical,
        Some(&fixture.envelope),
        Some(&native_lifecycle),
        Some(99),
        hook_now(),
    )
    .unwrap();
    staged.commit.wait().unwrap();
    let effects = Arc::new(AtomicUsize::new(0));
    let producer = fixture.producer(Arc::clone(&effects));
    // The registered configuration reader refuses an ambiguous revision head.
    // Existing immutable revisions remain untouched by this fault injection.
    let database = fixture.graph.configuration_runtime().registered_database();
    let transaction = database.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "INSERT INTO configuration_revisions
                 (revision_id, parent_revision_id, snapshot_id, effective_behavior_digest,
                  resolution_provenance_digest, actor_id, operation_kind, created_at)
                 VALUES ('configuration.fault.ambiguous', NULL, 'unavailable', 'unavailable',
                         'unavailable', 'actor.fixture', 'fault-injection', 0)",
                (),
            )
            .await
            .unwrap(),
        1,
    );
    transaction.commit().await.unwrap();
    assert!(
        fixture
            .graph
            .configuration_runtime()
            .client()
            .current()
            .await
            .is_err()
    );
    assert_eq!(
        hook_v2_native_lifecycle_policy(&fixture.graph, fixture.envelope.producer).await,
        HookV2NativeLifecyclePolicyV1::Unavailable,
    );
    assert!(matches!(
        fixture.admit().await,
        HookV2AdmissionOutcomeV1::Unavailable
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.pending(), std::slice::from_ref(&fixture.envelope));
    let staged = stage_hook_v2_admission(
        &fixture.graph.hook_store_layout().data_root,
        &canonical,
        None,
        None,
        None,
        hook_now(),
    )
    .unwrap();
    assert_eq!(staged.receipt.order, 99);
    assert_eq!(staged.receipt.work_settlement, None);
    staged.commit.wait().unwrap();
    assert_eq!(
        super::super::super::context_scout::native_lifecycle_order_floor(
            &fixture.sessions,
            ProviderId::new(fixture.envelope.producer.hook_key()).unwrap(),
            fixture.lifecycle.session_id.clone(),
        )
        .await,
        Some(99),
        "unavailable policy creates no lifecycle or cursor advance",
    );
    fixture.restart();
    assert_eq!(fixture.pending(), std::slice::from_ref(&fixture.envelope));
    assert!(producer.shutdown().await);
}
