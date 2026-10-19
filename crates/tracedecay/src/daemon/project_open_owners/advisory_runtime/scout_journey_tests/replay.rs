use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tracedecay_agent_hosts::agents::context_scout::address_registry::ContextScoutConfigurationPinV1;
use tracedecay_agent_hosts::agents::context_scout::intent_pass::ContextScoutIntentPassOutcomeV1;
use tracedecay_agent_hosts::agents::context_scout::{
    ContextScoutDurableStoreOutcomeV1, ContextScoutSuppressionV1,
};
use tracedecay_configuration::config::scope_control::daemon_owned_project_source_binding;
use tracedecay_contracts::context_scout::{
    ContextScoutDeliveryOutcomeV1, ContextScoutDeliveryReceiptV1, ContextScoutDurableQueueEntryV1,
    ContextScoutModelOutcomeV1,
};
use tracedecay_contracts::retrieval::{
    ContextScoutStoreStatusV1, HookRuntimeResultV1, HookRuntimeSurfaceRequestV1,
    HookV2AdmissionResultV1, HookV2AdmitRequestV1,
};
use tracedecay_daemon_service::context_scout_lifecycle::{
    AuthorityRegistrationV1, lookup_registered_context_scout_lifecycle,
    register_context_scout_lifecycle_authority, unregister_context_scout_lifecycle_authority,
};
use tracedecay_daemon_service::{
    BoundedHookOrchestratorV1, HookOrchestrationRequestV1, HookOrchestrationTriggerV1,
    HookOrchestrationWorkOutcomeV1, register_hook_orchestration_runtime,
    unregister_hook_orchestration_runtime,
};
use tracedecay_domain::configuration::{
    CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ConfigurationLayerIdV1, ConfigurationRevisionId,
    ConfigurationValueV1, ContextScoutConfigurationStateV1, ContextScoutSettingsV1,
    SOURCE_BINDINGS_SETTING_KEY,
};
use tracedecay_domain::{ProjectId, SessionId};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::configuration::GlobalDbConfigurationControlStore;
use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
use tracedecay_global_db::configuration::resolver::{ConfigurationLayerV1, resolve_configuration};
use tracedecay_hooks::{
    HookBoundaryV1, HookEventEnvelopeV2, HookEventV2, HookOrderingV1, HookReadyGuidanceV1,
    NativeContextScoutLifecycleV1,
};
use tracedecay_mcp::handlers::SessionAuthorities;
use tracedecay_mcp::handlers::hook_runtime::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_replayed_envelope_with_lifecycle, compute_hook_runtime,
    hook_v2_pending_work_envelopes,
};
use tracedecay_project::project::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_sessions::admission::HostAdmissionScope;

use super::{
    INTENT_SESSION, IntentJourneyFixture, RecordingDecisionEngine, completed_tool,
    context_scout_delivery_receipt_id, intent_settings, now_utc_micros, seed_intent_session,
    typed_id,
};
use crate::test_support::git::git;

mod configuration;
mod pending;

async fn registered_fixture() -> (
    IntentJourneyFixture,
    TraceDecay,
    Arc<HostAdmissionTestRuntimeV1>,
) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    let profile = temporary.path().join("profile");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "fixture"]);
    let commit = typed_id(&git(&root, &["rev-parse", "HEAD"]));
    let project = ProjectId::new(format!(
        "project.scout.replay.{}",
        temporary
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .trim_start_matches('.')
    ))
    .unwrap();
    let runtime = Arc::new(
        HostAdmissionTestRuntimeV1::project(&profile, &root, project.clone())
            .await
            .unwrap(),
    );
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    let registry = ConfigurationRegistry::core().unwrap();
    let revision: ConfigurationRevisionId = typed_id("configuration.scout.replay.initial");
    let resolution = resolve_configuration(
        &registry,
        &[ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project.clone(),
            },
            revision_id: revision.clone(),
            entries: BTreeMap::from([
                (
                    typed_id(SOURCE_BINDINGS_SETTING_KEY),
                    ConfigurationValueV1::SourceBindings(vec![
                        daemon_owned_project_source_binding(&project).unwrap(),
                    ]),
                ),
                (
                    typed_id(CONTEXT_SCOUT_SETTINGS_SETTING_KEY),
                    ConfigurationValueV1::ContextScoutSettings(ContextScoutSettingsV1 {
                        state: ContextScoutConfigurationStateV1::Active,
                        intent: Some(intent_settings()),
                        ..ContextScoutSettingsV1::disabled()
                    }),
                ),
            ]),
        }],
    )
    .unwrap();
    GlobalDbConfigurationControlStore::new_registered(&sessions)
        .initialize_canonical(&revision, &resolution, now_utc_micros())
        .await
        .unwrap();
    let graph = runtime
        .initialize_project_graph_for_test(
            &root,
            TraceDecayOpenOptions {
                profile_root: Some(profile.clone()),
                global_db_path: None,
            },
        )
        .await
        .unwrap();
    let owner = graph.context_scout_owner().unwrap();
    let configuration = ContextScoutConfigurationPinV1::from_current(
        &graph
            .configuration_runtime()
            .client()
            .current()
            .await
            .unwrap()
            .into_current_state(),
    )
    .unwrap();
    owner
        .install_configuration(configuration.clone(), None)
        .await
        .unwrap();
    let scope = tracedecay_code_index_runtime::resolved_scope_for_project(&root, &project).unwrap();
    let hook_runtime = crate::hook_runtime(ProfileRoot::new(profile));
    let worktree = tracedecay_agent_hosts::hooks::hook_worktree_id_for_layout(
        &hook_runtime,
        graph.hook_store_layout(),
    )
    .unwrap();
    let binding = tracedecay_hooks::published_hook_scope_binding(
        &graph.hook_store_layout().data_root,
        worktree,
        tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
        now_utc_micros(),
    )
    .unwrap();
    assert_eq!(
        register_context_scout_lifecycle_authority(
            binding.project_id,
            binding.worktree_id,
            scope.project_id.clone(),
            scope.worktree_id.clone(),
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );
    let native =
        NativeContextScoutLifecycleV1::new(INTENT_SESSION, "call.scout.prior", [69; 16]).unwrap();
    let prior = HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: native.event_id,
        producer: binding.host,
        protected_session_id: tracedecay_agent_hosts::hooks::protected_native_session_id(
            INTENT_SESSION,
        ),
        project_id: binding.project_id,
        repository_id: binding.repository_id,
        worktree_id: binding.worktree_id,
        worktree_epoch: binding.worktree_epoch,
        binding_token: binding.binding_token,
        ordering: HookOrderingV1::Unknown,
        observed_at: now_utc_micros(),
        event: completed_tool(),
    };
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let prior_producer =
        BoundedHookOrchestratorV1::new(1, move |request: HookOrchestrationRequestV1, _| {
            let ran = ran.clone();
            async move {
                assert_eq!(request.trigger, HookOrchestrationTriggerV1::IntentBoundary);
                assert!(request.lifecycle.is_some());
                ran.send(()).unwrap();
            }
        })
        .unwrap();
    assert!(register_hook_orchestration_runtime(
        binding.project_id,
        binding.worktree_id,
        &prior_producer
    ));
    assert!(matches!(
        admit_hook_v2_replayed_envelope_with_lifecycle(
            &graph,
            &prior,
            Some(native.session_id.clone()),
            Some(native),
            &sessions,
            &runtime.background_cpu(),
            now_utc_micros(),
        )
        .await,
        HookV2AdmissionOutcomeV1::Admitted { .. }
    ));
    tokio::time::timeout(Duration::from_secs(10), runs.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while prior_producer.active_tasks() != 0
            || !hook_v2_pending_work_envelopes(
                &graph.hook_store_layout().data_root,
                binding.host,
                now_utc_micros(),
            )
            .unwrap()
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    unregister_hook_orchestration_runtime(binding.project_id, binding.worktree_id, &prior_producer);
    assert!(prior_producer.shutdown().await);
    let lifecycle = lookup_registered_context_scout_lifecycle(
        binding.project_id,
        binding.worktree_id,
        &SessionId::new(INTENT_SESSION).unwrap(),
    )
    .await
    .unwrap();
    let fixture = IntentJourneyFixture::from_authorities(
        temporary,
        owner,
        configuration,
        scope,
        binding,
        lifecycle,
        commit,
    );
    (fixture, graph, runtime)
}

async fn mount_intent(
    fixture: &IntentJourneyFixture,
    graph: &TraceDecay,
    sessions: &RegisteredGlobalDb,
) -> (
    Arc<RecordingDecisionEngine>,
    Box<ContextScoutDurableQueueEntryV1>,
) {
    let project_key =
        tracedecay_global_db::RegisteredGlobalDb::canonical_project_key(graph.project_root());
    seed_intent_session(sessions, &project_key).await;
    let engine = RecordingDecisionEngine::new(0.9);
    let boundary = fixture.hook(71, completed_tool());
    let ContextScoutIntentPassOutcomeV1::Enqueued { entry, lifecycle } = fixture
        .pass(&boundary, sessions, &project_key, &engine)
        .await
    else {
        panic!("the registered session intent is enqueued");
    };
    // The queue commit survived, but no claim authority was mounted before
    // this retry. Recovery must keep that exact suggestion and its evidence.
    let recovered = fixture
        .pass(&boundary, sessions, &project_key, &engine)
        .await;
    assert_eq!(
        recovered,
        ContextScoutIntentPassOutcomeV1::Enqueued {
            entry: entry.clone(),
            lifecycle: lifecycle.clone(),
        }
    );
    assert_eq!(
        engine.requests(),
        1,
        "recovery does not re-judge committed work"
    );
    assert!(
        graph
            .mount_current_context_scout_claim_authority(
                Arc::clone(&fixture.registry),
                &boundary,
                1,
                fixture.pin.clone(),
                fixture.context.clone(),
                *lifecycle,
                entry.work.address,
                entry.work.input_watermark,
                now_utc_micros(),
            )
            .await
    );
    (engine, entry)
}

async fn intent_pass_overlapping_a_claim_is_suppressed(record_delivery: bool) {
    let (fixture, graph, runtime) = registered_fixture().await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    let (engine, entry) = mount_intent(&fixture, &graph, &sessions).await;
    let stop = fixture.hook(
        73,
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::TurnComplete,
        },
    );
    let project_key = RegisteredGlobalDb::canonical_project_key(graph.project_root());
    let (started, release) = engine.hold_next_request();
    let claim_during_evaluation = async {
        started.await.unwrap();
        let claimed_at = now_utc_micros();
        let (guidance, claim) = fixture
            .owner
            .claim_ready_guidance_exact(
                stop.envelope(),
                entry.work.address,
                entry.work.input_watermark,
                1,
                claimed_at,
            )
            .await
            .unwrap();
        assert_eq!(guidance.text, entry.envelope.candidate.suggestion_text);
        if record_delivery {
            let receipt = ContextScoutDeliveryReceiptV1 {
                receipt_id: context_scout_delivery_receipt_id(
                    stop.envelope().event_id,
                    guidance.guidance_id,
                ),
                envelope_id: guidance.guidance_id,
                delivered_at: now_utc_micros(),
                outcome: ContextScoutDeliveryOutcomeV1::Attempted,
            };
            assert_eq!(
                fixture.owner.record_delivery(&claim, &receipt).await,
                ContextScoutDurableStoreOutcomeV1::Stored
            );
        }
        release.send(()).unwrap();
    };
    let (outcome, ()) = tokio::join!(
        fixture.pass(&stop, &sessions, &project_key, &engine),
        claim_during_evaluation,
    );
    assert_eq!(
        outcome,
        ContextScoutIntentPassOutcomeV1::Suppressed {
            reason: ContextScoutSuppressionV1::Duplicate,
            model_outcome: ContextScoutModelOutcomeV1::Succeeded,
        },
        "the original suggestion is already claimed; delivered={record_delivery}"
    );
    assert_eq!(engine.requests(), 2);
    let recent = fixture
        .owner
        .recent_exact(entry.work.address, 8)
        .await
        .unwrap();
    if record_delivery {
        assert_eq!(recent.pending, Vec::new());
        assert_eq!(recent.deliveries.len(), 1);
        assert_eq!(recent.deliveries[0].entry, *entry);
    } else {
        assert_eq!(recent.pending, vec![*entry]);
        assert_eq!(recent.deliveries, Vec::new());
    }
    assert!(unregister_context_scout_lifecycle_authority(
        fixture.binding.project_id,
        fixture.binding.worktree_id,
        &sessions,
    ));
}

#[tokio::test]
async fn intent_pass_with_an_already_claimed_suggestion_is_suppressed() {
    Box::pin(intent_pass_overlapping_a_claim_is_suppressed(false)).await;
}

#[tokio::test]
async fn intent_pass_with_a_delivery_during_evaluation_is_suppressed() {
    Box::pin(intent_pass_overlapping_a_claim_is_suppressed(true)).await;
}

#[tokio::test]
async fn enqueued_intent_replay_mounts_and_delivers_once_without_reopening_producer_work() {
    let (fixture, graph, runtime) = registered_fixture().await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    let (engine, entry) = mount_intent(&fixture, &graph, &sessions).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let producer = BoundedHookOrchestratorV1::new(1, {
        let effects = Arc::clone(&effects);
        move |request: HookOrchestrationRequestV1, _| {
            let effects = Arc::clone(&effects);
            async move {
                if request.trigger == HookOrchestrationTriggerV1::IntentBoundary {
                    assert!(request.completion.is_none());
                    return;
                }
                assert_eq!(request.trigger, HookOrchestrationTriggerV1::Stop);
                assert!(request.lifecycle.is_some());
                effects.fetch_add(1, Ordering::SeqCst);
            }
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        fixture.binding.project_id,
        fixture.binding.worktree_id,
        &producer
    ));
    let stop = fixture.hook(
        72,
        HookEventV2::SessionBoundary {
            boundary: HookBoundaryV1::TurnComplete,
        },
    );
    let replay = admit_hook_v2_replayed_envelope_with_lifecycle(
        &graph,
        stop.envelope(),
        Some(typed_id(INTENT_SESSION)),
        None,
        &sessions,
        &runtime.background_cpu(),
        now_utc_micros(),
    )
    .await;
    assert!(
        matches!(replay, HookV2AdmissionOutcomeV1::Admitted { ready_guidance, .. } if ready_guidance.is_null())
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !hook_v2_pending_work_envelopes(
            &graph.hook_store_layout().data_root,
            fixture.binding.host,
            now_utc_micros(),
        )
        .unwrap()
        .is_empty()
            || producer.active_tasks() != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let live_request = || {
        HookRuntimeSurfaceRequestV1::HookV2Admit(HookV2AdmitRequestV1 {
            envelope: serde_json::to_value(stop.envelope()).unwrap(),
            native_session_id: Some(INTENT_SESSION.to_owned()),
            native_lifecycle: None,
        })
    };
    let authorities = || {
        SessionAuthorities::new(Some(&sessions), None)
            .with_background_cpu(Some(runtime.background_cpu()))
    };
    let live = compute_hook_runtime(&graph, live_request(), None, None, authorities())
        .await
        .unwrap();
    let HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::ExactDuplicate {
        context_scout_address: Some(address),
        ready_guidance: Some(guidance),
        ..
    }) = live
    else {
        panic!("completed replay must retain delivery authority: {live:?}");
    };
    assert_eq!(address, entry.work.address);
    let guidance: HookReadyGuidanceV1 = serde_json::from_value(guidance).unwrap();
    assert_eq!(guidance.event_id, stop.envelope().event_id);
    assert_eq!(guidance.text, entry.envelope.candidate.suggestion_text);
    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: context_scout_delivery_receipt_id(
            stop.envelope().event_id,
            guidance.guidance_id,
        ),
        envelope_id: guidance.guidance_id,
        delivered_at: now_utc_micros(),
        outcome: ContextScoutDeliveryOutcomeV1::Attempted,
    };
    assert!(matches!(
        compute_hook_runtime(
            &graph,
            HookRuntimeSurfaceRequestV1::HookV2DeliveryReceipt {
                receipt: serde_json::to_value(receipt).unwrap(),
            },
            None,
            None,
            authorities()
        )
        .await
        .unwrap(),
        HookRuntimeResultV1::HookV2DeliveryReceipt {
            status: ContextScoutStoreStatusV1::Stored
        }
    ));
    let duplicate = compute_hook_runtime(&graph, live_request(), None, None, authorities())
        .await
        .unwrap();
    assert!(matches!(
        duplicate,
        HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::ExactDuplicate {
            ready_guidance: None,
            ..
        })
    ));
    assert_eq!(producer.active_tasks(), 0);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(engine.requests(), 1);
    let recent = fixture.owner.recent_exact(address, 8).await.unwrap();
    assert_eq!(recent.pending, Vec::new());
    assert_eq!(recent.deliveries.len(), 1);
    unregister_hook_orchestration_runtime(
        fixture.binding.project_id,
        fixture.binding.worktree_id,
        &producer,
    );
    assert!(producer.shutdown().await);
    assert!(unregister_context_scout_lifecycle_authority(
        fixture.binding.project_id,
        fixture.binding.worktree_id,
        &sessions,
    ));
}

#[tokio::test]
async fn enqueued_intent_recovery_cannot_mount_past_durable_configuration_drift() {
    let (fixture, graph, runtime) = registered_fixture().await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    let project_key = RegisteredGlobalDb::canonical_project_key(graph.project_root());
    seed_intent_session(&sessions, &project_key).await;
    let engine = RecordingDecisionEngine::new(0.9);
    let boundary = fixture.hook(71, completed_tool());
    let original = fixture
        .pass(&boundary, &sessions, &project_key, &engine)
        .await;
    let next = configuration::advance_configuration(&sessions).await;
    let recovered = fixture
        .pass(&boundary, &sessions, &project_key, &engine)
        .await;
    assert_eq!(
        recovered, original,
        "recovery preserves the original evidence"
    );
    assert_eq!(
        super::intent_pass_work_outcome(&recovered),
        HookOrchestrationWorkOutcomeV1::RetryableFailure
    );
    let ContextScoutIntentPassOutcomeV1::Enqueued { entry, lifecycle } = recovered else {
        panic!("the queue commit remains pending before the owner observes configuration drift");
    };
    assert!(
        !graph
            .mount_current_context_scout_claim_authority(
                Arc::clone(&fixture.registry),
                &boundary,
                1,
                fixture.pin.clone(),
                fixture.context.clone(),
                *lifecycle,
                entry.work.address,
                entry.work.input_watermark,
                now_utc_micros(),
            )
            .await,
        "durable configuration must reject the stale pin before owner refresh"
    );
    fixture
        .owner
        .install_configuration(next, None)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .pass(&boundary, &sessions, &project_key, &engine)
            .await,
        ContextScoutIntentPassOutcomeV1::Unavailable
    );
    assert_eq!(
        engine.requests(),
        1,
        "stale recovery never re-judges the event"
    );
    assert!(
        graph
            .resolve_current_context_scout_session_claim_authority(
                &boundary,
                &fixture.lifecycle,
                now_utc_micros(),
            )
            .await
            .is_none()
    );
    assert!(unregister_context_scout_lifecycle_authority(
        fixture.binding.project_id,
        fixture.binding.worktree_id,
        &sessions
    ));
}
