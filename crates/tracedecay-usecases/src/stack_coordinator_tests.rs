#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use tracedecay_application::{
    CancellationContext, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, NativeIntegrationEvidenceRevisionsV1, NativeIntegrationPreflightRequestV1,
    NativeIntegrationSelectionBindingV1, NativeIntegrationStackResolutionRequestV1, RequestContext,
    RequestId, ResolvedScope,
};
use tracedecay_domain::{
    ActorId, BranchStackRevisionId, ManifestDigest, NativeIntegrationPreviewId, ProjectId, RefId,
    RepositoryId, StackDeliveryWatermarkId, StackSignalId, UtcMicros, WorktreeId,
    WorktreeInventoryEpoch, WorktreeInventorySnapshotId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::*;

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn actor(index: usize) -> ActorId {
    ActorId::new(format!("actor.stack.{index}")).unwrap()
}

fn signal(index: usize, kind: StackSignalKindV1) -> StackSignalV1 {
    StackSignalV1 {
        signal_id: StackSignalId::new(format!("signal.stack.{index}")).unwrap(),
        project_id: ProjectId::new("project.stack").unwrap(),
        repository_id: RepositoryId::new("repository.stack").unwrap(),
        stack_revision_id: BranchStackRevisionId::new("revision.stack").unwrap(),
        stack_revision_digest: digest('a'),
        kind,
        state_digest: digest(if index.is_multiple_of(2) { 'b' } else { 'c' }),
        github_stack_digest: None,
        observed_at: UtcMicros(index as i64),
        watermark_id: StackDeliveryWatermarkId::new("watermark.stack").unwrap(),
    }
}

#[derive(Default)]
struct MemoryStore {
    signals: Mutex<BTreeMap<StackSignalId, StackSignalV1>>,
    pending: Mutex<Vec<(StackPendingDeliveryV1, StackSignalV1)>>,
}

impl StackCoordinatorStore for MemoryStore {
    fn append_signal(
        &self,
        signal: StackSignalV1,
        recipients: Vec<ActorId>,
    ) -> Result<(), StackCoordinatorError> {
        let mut signals = self.signals.lock().unwrap();
        if let Some(existing) = signals.get(&signal.signal_id) {
            if existing != &signal {
                return Err(StackCoordinatorError::Invalid(
                    "signal identity conflict".to_owned(),
                ));
            }
            return Ok(());
        }
        signals.insert(signal.signal_id.clone(), signal.clone());
        self.pending
            .lock()
            .unwrap()
            .extend(recipients.into_iter().map(|recipient| {
                (
                    StackPendingDeliveryV1 {
                        recipient,
                        signal_id: signal.signal_id.clone(),
                    },
                    signal.clone(),
                )
            }));
        Ok(())
    }

    fn pending_deliveries(
        &self,
    ) -> Result<Vec<(StackPendingDeliveryV1, StackSignalV1)>, StackCoordinatorError> {
        Ok(self.pending.lock().unwrap().clone())
    }

    fn acknowledge(
        &self,
        watermark_id: &StackDeliveryWatermarkId,
        deliveries: &[StackPendingDeliveryV1],
    ) -> Result<(), StackCoordinatorError> {
        let acknowledged = deliveries.iter().cloned().collect::<BTreeSet<_>>();
        self.pending.lock().unwrap().retain(|(delivery, signal)| {
            signal.watermark_id != *watermark_id || !acknowledged.contains(delivery)
        });
        Ok(())
    }

    fn signal(
        &self,
        signal_id: &StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorError> {
        Ok(self.signals.lock().unwrap().get(signal_id).cloned())
    }
}

#[derive(Default)]
struct Authorization {
    denied: Mutex<BTreeSet<ActorId>>,
    unavailable: Mutex<BTreeSet<ActorId>>,
}

impl StackDeliveryAuthorizationPort for Authorization {
    fn authorize(
        &self,
        recipient: &ActorId,
        _signal: &StackSignalV1,
    ) -> StackDeliveryAuthorizationV1 {
        if self.unavailable.lock().unwrap().contains(recipient) {
            StackDeliveryAuthorizationV1::Unavailable
        } else if self.denied.lock().unwrap().contains(recipient) {
            StackDeliveryAuthorizationV1::Denied
        } else {
            StackDeliveryAuthorizationV1::Authorized
        }
    }
}

#[derive(Default)]
struct RecordingDelivery {
    batches: Mutex<Vec<StackDeliveryBatchV1>>,
    fail_next: Mutex<bool>,
}

impl StackDeliveryPort for RecordingDelivery {
    fn deliver(&self, batch: &StackDeliveryBatchV1) -> Result<(), StackCoordinatorError> {
        if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
            return Err(StackCoordinatorError::Unavailable);
        }
        self.batches.lock().unwrap().push(batch.clone());
        Ok(())
    }
}

struct PreflightOutcomes {
    outcomes: Mutex<VecDeque<NativeIntegrationPreflightOutcomeV1>>,
    calls: Mutex<usize>,
}

impl OptionalStackPreflightPort for PreflightOutcomes {
    fn preflight(
        &self,
        _request: &NativeIntegrationPreflightRequestV1,
        _cancellation: &CancellationSignal,
    ) -> Result<NativeIntegrationPreflightOutcomeV1, StackCoordinatorError> {
        *self.calls.lock().unwrap() += 1;
        Ok(self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(NativeIntegrationPreflightOutcomeV1::Unavailable))
    }
}

fn policy() -> StackCircuitPolicyV1 {
    StackCircuitPolicyV1 {
        revision: 1,
        policy_digest: digest('0'),
        failure_threshold: 2,
        open_micros: 100,
    }
    .seal()
    .unwrap()
}

fn coordinator(
    store: Arc<MemoryStore>,
    authorization: Arc<Authorization>,
    delivery: Arc<RecordingDelivery>,
    preflight: Arc<PreflightOutcomes>,
) -> StackCoordinator<MemoryStore, Authorization, RecordingDelivery, PreflightOutcomes> {
    StackCoordinator::new(store, authorization, delivery, preflight, policy()).unwrap()
}

#[test]
fn fanout_batches_are_bounded_ordered_and_never_drop_material_transitions() {
    let store = Arc::new(MemoryStore::default());
    let delivery = Arc::new(RecordingDelivery::default());
    let coordinator = coordinator(
        store.clone(),
        Arc::new(Authorization::default()),
        delivery.clone(),
        Arc::new(PreflightOutcomes {
            outcomes: Mutex::new(VecDeque::new()),
            calls: Mutex::new(0),
        }),
    );
    let recipients = (0..65).map(actor).collect::<Vec<_>>();
    for index in 0..129 {
        coordinator
            .enqueue(
                signal(index, StackSignalKindV1::IntegrationCommitted),
                recipients.clone(),
            )
            .unwrap();
    }

    assert_eq!(coordinator.drain_due(UtcMicros(0)).unwrap(), 65 * 129);
    assert!(store.pending.lock().unwrap().is_empty());
    let batches = delivery.batches.lock().unwrap();
    assert!(batches.len() > 1);
    assert!(batches.iter().all(|batch| {
        batch.recipients.len() <= MAX_BATCH_RECIPIENTS
            && batch.signals.len() <= MAX_BATCH_SIGNALS
            && batch.deliveries.len() <= MAX_BATCH_RECIPIENTS * MAX_BATCH_SIGNALS
    }));
    let delivered = batches
        .iter()
        .flat_map(|batch| batch.deliveries.iter().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(delivered.len(), 65 * 129);
}

#[test]
fn dedupe_never_suppresses_material_state_and_remote_failure_retries_after_restart() {
    let store = Arc::new(MemoryStore::default());
    let delivery = Arc::new(RecordingDelivery::default());
    let authorization = Arc::new(Authorization::default());
    let preflight = Arc::new(PreflightOutcomes {
        outcomes: Mutex::new(VecDeque::new()),
        calls: Mutex::new(0),
    });
    let first_coordinator = coordinator(
        store.clone(),
        authorization.clone(),
        delivery.clone(),
        preflight.clone(),
    );
    let mut duplicate = signal(1, StackSignalKindV1::DependencyReady);
    duplicate.state_digest = digest('d');
    let mut duplicate_two = signal(2, StackSignalKindV1::DependencyReady);
    duplicate_two.state_digest = digest('d');
    first_coordinator
        .enqueue(duplicate, vec![actor(1)])
        .unwrap();
    first_coordinator
        .enqueue(duplicate_two, vec![actor(1)])
        .unwrap();
    first_coordinator
        .enqueue(
            signal(3, StackSignalKindV1::IntegrationNeedsInspection),
            vec![actor(1)],
        )
        .unwrap();
    first_coordinator
        .enqueue(
            signal(4, StackSignalKindV1::IntegrationNeedsInspection),
            vec![actor(1)],
        )
        .unwrap();
    assert_eq!(store.signals.lock().unwrap().len(), 3);

    *delivery.fail_next.lock().unwrap() = true;
    assert_eq!(
        first_coordinator.drain_due(UtcMicros(2_000_000)),
        Err(StackCoordinatorError::Unavailable)
    );
    assert_eq!(store.pending.lock().unwrap().len(), 3);

    let restarted = coordinator(store.clone(), authorization, delivery.clone(), preflight);
    assert_eq!(restarted.drain_due(UtcMicros(2_000_000)).unwrap(), 3);
    assert!(store.pending.lock().unwrap().is_empty());
}

#[test]
fn authorization_is_rechecked_at_enqueue_delivery_and_expand() {
    let store = Arc::new(MemoryStore::default());
    let authorization = Arc::new(Authorization::default());
    authorization.denied.lock().unwrap().insert(actor(2));
    let delivery = Arc::new(RecordingDelivery::default());
    let coordinator = coordinator(
        store.clone(),
        authorization.clone(),
        delivery,
        Arc::new(PreflightOutcomes {
            outcomes: Mutex::new(VecDeque::new()),
            calls: Mutex::new(0),
        }),
    );
    let queued = signal(6, StackSignalKindV1::ActualConflict);
    coordinator
        .enqueue(queued.clone(), vec![actor(1), actor(2)])
        .unwrap();
    assert_eq!(store.pending.lock().unwrap().len(), 1);
    authorization.denied.lock().unwrap().insert(actor(1));
    assert!(
        coordinator
            .expand(&actor(1), &queued.signal_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(coordinator.drain_due(UtcMicros(0)).unwrap(), 0);
    assert!(store.pending.lock().unwrap().is_empty());
}

#[test]
fn unavailable_preflights_open_the_scoped_circuit_and_stale_half_open_does_not_close_it() {
    let store = Arc::new(MemoryStore::default());
    let preflight = Arc::new(PreflightOutcomes {
        outcomes: Mutex::new(VecDeque::from([
            NativeIntegrationPreflightOutcomeV1::Unavailable,
            NativeIntegrationPreflightOutcomeV1::Unavailable,
            NativeIntegrationPreflightOutcomeV1::Stale,
        ])),
        calls: Mutex::new(0),
    });
    let coordinator = coordinator(
        store,
        Arc::new(Authorization::default()),
        Arc::new(RecordingDelivery::default()),
        preflight.clone(),
    );
    let request = preflight_request();
    let cancellation = CancellationSignal::active("cancel.stack.preflight").unwrap();
    assert_eq!(
        coordinator
            .optional_preflight(&request, &cancellation, UtcMicros(10))
            .unwrap(),
        OptionalPreflightDispositionV1::Unavailable
    );
    assert_eq!(
        coordinator
            .optional_preflight(&request, &cancellation, UtcMicros(11))
            .unwrap(),
        OptionalPreflightDispositionV1::Unavailable
    );
    assert_eq!(
        coordinator
            .optional_preflight(&request, &cancellation, UtcMicros(12))
            .unwrap(),
        OptionalPreflightDispositionV1::SuppressedOpenCircuit
    );
    assert_eq!(
        coordinator
            .optional_preflight(&request, &cancellation, UtcMicros(111))
            .unwrap(),
        OptionalPreflightDispositionV1::Stale
    );
    assert_eq!(
        coordinator
            .optional_preflight(&request, &cancellation, UtcMicros(112))
            .unwrap(),
        OptionalPreflightDispositionV1::SuppressedOpenCircuit
    );
    assert_eq!(*preflight.calls.lock().unwrap(), 3);
}

fn preflight_request() -> NativeIntegrationPreflightRequestV1 {
    let project = ProjectId::new("project.stack").unwrap();
    let repository = RepositoryId::new("repository.stack").unwrap();
    let source = ResolvedScope::new(
        project.clone(),
        repository.clone(),
        WorktreeId::new("worktree.stack.source").unwrap(),
        Some(RefId::new("refs/heads/source").unwrap()),
    )
    .unwrap();
    let destination = ResolvedScope::new(
        project,
        repository,
        WorktreeId::new("worktree.stack.destination").unwrap(),
        Some(RefId::new("refs/heads/destination").unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.stack").unwrap(),
        1,
        digest('e'),
        actor(0),
        UtcMicros(1),
        UtcMicros(10_000),
        destination.clone(),
        BTreeSet::from([CapabilityId::new("capability.stack").unwrap()]),
        BTreeSet::from([UseCaseId::new("usecase.stack").unwrap()]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    NativeIntegrationPreflightRequestV1 {
        context: RequestContext::new(
            actor(0),
            destination.clone(),
            grant,
            RequestId::new("request.stack").unwrap(),
            Deadline::new(UtcMicros(9_000)).unwrap(),
            CancellationContext::active("cancel.stack").unwrap(),
        )
        .unwrap(),
        topology: NativeIntegrationStackResolutionRequestV1 {
            source,
            destination,
            authorized_scope_set_digest: digest('f'),
            inventory_snapshot_id: WorktreeInventorySnapshotId::new("inventory.stack").unwrap(),
            inventory_epoch: WorktreeInventoryEpoch::new(1).unwrap(),
            selection: NativeIntegrationSelectionBindingV1::IndependentBranch {
                proposal_digest: digest('1'),
            },
            grant_digest: digest('e'),
            policy_digest: digest('2'),
            observed_at: UtcMicros(2),
        },
        evidence: NativeIntegrationEvidenceRevisionsV1 {
            graph_revision_digest: digest('3'),
            test_revision_digest: digest('4'),
            schema_revision_digest: digest('5'),
            migration_revision_digest: digest('6'),
        },
        preview_id: NativeIntegrationPreviewId::new("preview.stack").unwrap(),
        preferred_mode: None,
        preview_expires_at: UtcMicros(8_000),
        observed_at: UtcMicros(2),
    }
}
