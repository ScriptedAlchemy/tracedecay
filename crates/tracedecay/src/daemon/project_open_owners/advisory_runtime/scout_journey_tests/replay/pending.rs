use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracedecay_contracts::HookOrchestrationAdmissionV1;
use tracedecay_contracts::context_scout::{
    ContextScoutDeliveryOutcomeV1, ContextScoutDeliveryReceiptV1,
};
use tracedecay_contracts::retrieval::{
    ContextScoutStoreStatusV1, HookRuntimeResultV1, HookRuntimeSurfaceRequestV1,
    HookV2AdmissionResultV1, HookV2AdmitRequestV1,
};
use tracedecay_daemon_service::context_scout_lifecycle::unregister_context_scout_lifecycle_authority;
use tracedecay_daemon_service::{
    BoundedHookOrchestratorV1, HookOrchestrationRequestV1, HookOrchestrationSettlementV1,
    HookOrchestrationTriggerV1, register_hook_orchestration_runtime,
    unregister_hook_orchestration_runtime,
};
use tracedecay_hooks::{HookBoundaryV1, HookEventV2, HookReadyGuidanceV1};
use tracedecay_mcp::handlers::SessionAuthorities;
use tracedecay_mcp::handlers::hook_runtime::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_replayed_envelope_with_lifecycle, compute_hook_runtime,
    hook_v2_pending_work_envelopes,
};
use tracedecay_sessions::admission::HostAdmissionScope;

use super::super::{INTENT_SESSION, context_scout_delivery_receipt_id, now_utc_micros, typed_id};
use super::{mount_intent, registered_fixture};

#[tokio::test]
async fn pending_stop_replay_claims_its_mounted_intent_before_feedback_settles() {
    pending_stop_delivers_once(false, None).await;
}

#[tokio::test]
async fn pending_stop_retry_reuses_its_retained_claim_before_feedback_settles() {
    pending_stop_delivers_once(true, None).await;
}

#[tokio::test]
async fn pending_stop_rejects_a_retained_claim_after_configuration_advances() {
    pending_stop_delivers_once(true, Some(false)).await;
}

#[tokio::test]
async fn completed_stop_rejects_a_retained_claim_after_configuration_advances() {
    pending_stop_delivers_once(true, Some(true)).await;
}

async fn pending_stop_delivers_once(
    first_response_available: bool,
    advance_after_completion: Option<bool>,
) {
    let (fixture, graph, runtime) = registered_fixture().await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    let (engine, entry) = mount_intent(&fixture, &graph, &sessions).await;
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(None));
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered)));
    let (release, released) = tokio::sync::oneshot::channel();
    let mut release = Some(release);
    let released = Arc::new(tokio::sync::Mutex::new(Some(released)));
    let producer = BoundedHookOrchestratorV1::new(1, {
        let effects = Arc::clone(&effects);
        let captured = Arc::clone(&captured);
        move |request: HookOrchestrationRequestV1, _| {
            let effects = Arc::clone(&effects);
            let captured = Arc::clone(&captured);
            let entered = Arc::clone(&entered);
            let released = Arc::clone(&released);
            async move {
                if request.trigger == HookOrchestrationTriggerV1::IntentBoundary {
                    assert!(request.completion.is_none());
                    return;
                }
                assert_eq!(request.trigger, HookOrchestrationTriggerV1::Stop);
                effects.fetch_add(1, Ordering::SeqCst);
                *captured.lock().unwrap() = Some(request);
                entered.lock().unwrap().take().unwrap().send(()).unwrap();
                let released = released.lock().await.take().unwrap();
                released.await.unwrap();
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
    let request = || {
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
    let first_guidance = if first_response_available {
        let first = compute_hook_runtime(&graph, request(), None, None, authorities())
            .await
            .unwrap();
        let HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::Accepted {
            ready_guidance: Some(guidance),
            ..
        }) = first
        else {
            panic!("the first live admission must claim the mounted intent: {first:?}");
        };
        Some(guidance)
    } else {
        assert!(matches!(admit_hook_v2_replayed_envelope_with_lifecycle(
            &graph, stop.envelope(), Some(typed_id(INTENT_SESSION)), None,
            &sessions, &runtime.background_cpu(), now_utc_micros(),
        ).await, HookV2AdmissionOutcomeV1::Admitted { ready_guidance, .. } if ready_guidance.is_null()));
        None
    };
    tokio::time::timeout(Duration::from_secs(10), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let pending = || {
        hook_v2_pending_work_envelopes(
            &graph.hook_store_layout().data_root,
            fixture.binding.host,
            now_utc_micros(),
        )
        .unwrap()
    };
    assert_eq!(pending(), vec![stop.envelope().clone()]);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let (terminal, terminal_rx) = tokio::sync::oneshot::channel();
    let terminal = Mutex::new(Some(terminal));
    let mut terminal_rx = Some(terminal_rx);
    let settlements = Arc::new(AtomicUsize::new(0));
    let mut observer = captured.lock().unwrap().clone().unwrap();
    observer.completion = Some(Arc::new({
        let settlements = Arc::clone(&settlements);
        move |settlement| {
            settlements.fetch_add(1, Ordering::SeqCst);
            terminal
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(settlement)
                .unwrap();
        }
    }));
    assert_eq!(
        producer.admit(observer),
        HookOrchestrationAdmissionV1::Enqueued
    );
    if let Some(completed) = advance_after_completion {
        if completed {
            release.take().unwrap().send(()).unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(10), terminal_rx.take().unwrap())
                    .await
                    .unwrap()
                    .unwrap(),
                HookOrchestrationSettlementV1::Completed
            );
            wait_until_settled(&pending, &producer).await;
        }
        let next = super::configuration::advance_configuration(&sessions).await;
        assert_ne!(
            next.control().configuration_revision,
            entry.envelope.configuration_revision
        );
        // The durable pin must reject the old claim even before the owner has
        // observed the committed configuration change.
        for owner_updated in [false, true] {
            if owner_updated {
                fixture
                    .owner
                    .install_configuration(next.clone(), None)
                    .await
                    .unwrap();
            }
            let retry = compute_hook_runtime(&graph, request(), None, None, authorities())
                .await
                .unwrap();
            match retry {
                HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::Accepted {
                    ready_guidance: None,
                    ..
                }) => assert!(!completed),
                HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::ExactDuplicate {
                    ready_guidance: None,
                    ..
                }) => assert!(completed),
                other => panic!("a retained claim cannot cross configuration revisions: {other:?}"),
            }
            assert_eq!(effects.load(Ordering::SeqCst), 1);
            assert_eq!(pending().is_empty(), completed);
        }
        if !completed {
            release.take().unwrap().send(()).unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(10), terminal_rx.take().unwrap())
                    .await
                    .unwrap()
                    .unwrap(),
                HookOrchestrationSettlementV1::Completed
            );
            wait_until_settled(&pending, &producer).await;
        }
        assert_eq!(settlements.load(Ordering::SeqCst), 1);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
        assert_eq!(engine.requests(), 1);
        let recent = fixture
            .owner
            .recent_exact(entry.work.address, 8)
            .await
            .unwrap();
        assert!(recent.deliveries.is_empty());
        unregister_hook_orchestration_runtime(
            fixture.binding.project_id,
            fixture.binding.worktree_id,
            &producer,
        );
        assert!(producer.shutdown().await);
        assert!(unregister_context_scout_lifecycle_authority(
            fixture.binding.project_id,
            fixture.binding.worktree_id,
            &sessions
        ));
        return;
    }
    let retry = compute_hook_runtime(&graph, request(), None, None, authorities())
        .await
        .unwrap();
    let HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::Accepted {
        context_scout_address: Some(address),
        ready_guidance: Some(guidance),
        ..
    }) = retry
    else {
        panic!("pending feedback must not suppress live retry delivery: {retry:?}");
    };
    if let Some(first_guidance) = first_guidance {
        assert_eq!(
            guidance, first_guidance,
            "the lost response's exact claim is reused"
        );
    }
    let guidance: HookReadyGuidanceV1 = serde_json::from_value(guidance).unwrap();
    assert_eq!(address, entry.work.address);
    assert_eq!(guidance.event_id, stop.envelope().event_id);
    assert_eq!(guidance.text, entry.envelope.candidate.suggestion_text);
    assert_eq!(pending(), vec![stop.envelope().clone()]);
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
    let duplicate = compute_hook_runtime(&graph, request(), None, None, authorities())
        .await
        .unwrap();
    assert!(matches!(
        duplicate,
        HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::Accepted {
            ready_guidance: None,
            ..
        })
    ));
    assert_eq!(pending(), vec![stop.envelope().clone()]);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(engine.requests(), 1);
    release.take().unwrap().send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), terminal_rx.take().unwrap())
            .await
            .unwrap()
            .unwrap(),
        HookOrchestrationSettlementV1::Completed
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !pending().is_empty() || producer.active_tasks() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(settlements.load(Ordering::SeqCst), 1);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let completed_duplicate = compute_hook_runtime(&graph, request(), None, None, authorities())
        .await
        .unwrap();
    assert!(matches!(
        completed_duplicate,
        HookRuntimeResultV1::HookV2Admit(HookV2AdmissionResultV1::ExactDuplicate {
            ready_guidance: None,
            ..
        })
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(engine.requests(), 1);
    let recent = fixture.owner.recent_exact(address, 8).await.unwrap();
    assert!(recent.pending.is_empty());
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
        &sessions
    ));
}

async fn wait_until_settled(
    pending: &impl Fn() -> Vec<tracedecay_hooks::HookEventEnvelopeV2>,
    producer: &BoundedHookOrchestratorV1,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !pending().is_empty() || producer.active_tasks() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
