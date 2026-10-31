use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tracedecay_contracts::HookOrchestrationAdmissionV1;
use tracedecay_daemon_service::{
    BoundedHookOrchestratorV1, HookOrchestrationRequestV1, HookOrchestrationTriggerV1,
    HookOrchestrationWorkOutcomeV1, register_hook_orchestration_runtime,
    unregister_hook_orchestration_runtime,
};
use tracedecay_hooks::{HookAdmissionDecisionV1, HookAdmissionWorkSettlementV1};

use super::super::{
    HookV2AdmissionOutcomeV1, admit_hook_v2_replayed_envelope_with_lifecycle,
    hook_v2_pending_work_envelopes, stage_hook_v2_admission,
};
use crate::handlers::hook_runtime::envelope::{
    daemon_mint_hook_v2_envelope, hook_now, hook_v2_requires_producer_work,
};

mod fixture;

#[tokio::test]
async fn committed_claude_tool_boundary_replays_its_intent_once_after_dispatch_cutoff() {
    prove_dispatch_cutoff(false).await;
}

#[tokio::test]
async fn retryable_tool_intent_retains_debt_until_recovery_then_settles_once() {
    prove_dispatch_cutoff(true).await;
}

async fn prove_dispatch_cutoff(refuse_first: bool) {
    let fixture = fixture::open().await;
    let envelope = &fixture.envelope;
    let effects = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let (ran, mut runs) = tokio::sync::mpsc::unbounded_channel();
    let producer = BoundedHookOrchestratorV1::new(1, {
        let effects = Arc::clone(&effects);
        let attempts = Arc::clone(&attempts);
        let expected_event = daemon_mint_hook_v2_envelope(envelope);
        let expected_session = fixture.lifecycle.session_id.clone();
        move |request: HookOrchestrationRequestV1, _| {
            let effects = Arc::clone(&effects);
            let attempts = Arc::clone(&attempts);
            let ran = ran.clone();
            let expected_event = expected_event.clone();
            let expected_session = expected_session.clone();
            async move {
                assert_eq!(request.trigger, HookOrchestrationTriggerV1::IntentBoundary);
                assert_eq!(request.hook.envelope(), &expected_event);
                let lifecycle = request.lifecycle.unwrap();
                assert_eq!(lifecycle.session_id, expected_session);
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 && refuse_first {
                    ran.send(false).unwrap();
                    return HookOrchestrationWorkOutcomeV1::RetryableFailure;
                }
                effects.fetch_add(1, Ordering::SeqCst);
                ran.send(true).unwrap();
                HookOrchestrationWorkOutcomeV1::Completed
            }
        }
    })
    .unwrap();
    assert!(register_hook_orchestration_runtime(
        envelope.project_id,
        envelope.worktree_id,
        &producer
    ));
    let canonical = daemon_mint_hook_v2_envelope(envelope);
    let mut lifecycle = fixture.lifecycle.clone();
    lifecycle.event_id = canonical.event_id;
    let staged = stage_hook_v2_admission(
        &fixture.graph.hook_store_layout().data_root,
        &canonical,
        hook_v2_requires_producer_work(envelope).then_some(envelope),
        Some(&lifecycle),
        Some(0),
        hook_now(),
    )
    .unwrap();
    assert_eq!(staged.receipt.decision, HookAdmissionDecisionV1::Admitted);
    staged.commit.wait().unwrap();
    assert_eq!(
        effects.load(Ordering::SeqCst),
        0,
        "cutoff precedes dispatch"
    );
    let replay = || {
        admit_hook_v2_replayed_envelope_with_lifecycle(
            &fixture.graph,
            envelope,
            Some(fixture.lifecycle.session_id.clone()),
            Some(fixture.lifecycle.clone()),
            &fixture.sessions,
            &fixture.background_cpu,
            hook_now(),
        )
    };
    assert!(matches!(
        replay().await,
        HookV2AdmissionOutcomeV1::Admitted {
            orchestration: HookOrchestrationAdmissionV1::Enqueued,
            ..
        } | HookV2AdmissionOutcomeV1::ExactDuplicate { .. }
    ));
    let pending = || {
        hook_v2_pending_work_envelopes(
            &fixture.graph.hook_store_layout().data_root,
            envelope.producer,
            hook_now(),
        )
        .unwrap()
    };
    // Settlement removes debt after the effect, so observe debt first.
    assert!(
        !pending().is_empty() || effects.load(Ordering::SeqCst) == 1,
        "a committed Tool boundary cannot lose both its intent execution and durable debt"
    );
    if refuse_first {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), runs.recv())
                .await
                .unwrap(),
            Some(false)
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while producer.active_tasks() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(pending(), vec![envelope.clone()]);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert!(matches!(
            replay().await,
            HookV2AdmissionOutcomeV1::Admitted { .. }
                | HookV2AdmissionOutcomeV1::ExactDuplicate { .. }
        ));
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), runs.recv())
            .await
            .unwrap(),
        Some(true)
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while producer.active_tasks() != 0 || !pending().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    let settled = stage_hook_v2_admission(
        &fixture.graph.hook_store_layout().data_root,
        &canonical,
        None,
        None,
        None,
        hook_now(),
    )
    .unwrap();
    assert_eq!(
        settled.receipt.decision,
        HookAdmissionDecisionV1::ExactDuplicate
    );
    assert_eq!(
        settled.receipt.work_settlement,
        Some(HookAdmissionWorkSettlementV1::Completed)
    );
    settled.commit.wait().unwrap();
    assert!(matches!(
        replay().await,
        HookV2AdmissionOutcomeV1::ExactDuplicate { .. }
    ));
    assert_eq!(producer.active_tasks(), 0);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        if refuse_first { 2 } else { 1 }
    );
    assert!(runs.try_recv().is_err());
    unregister_hook_orchestration_runtime(envelope.project_id, envelope.worktree_id, &producer);
    assert!(producer.shutdown().await);
    assert!(tracedecay_daemon_service::context_scout_lifecycle::unregister_context_scout_lifecycle_authority(envelope.project_id, envelope.worktree_id, &fixture.sessions));
}
