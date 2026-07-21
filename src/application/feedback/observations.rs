//! Plan-26 source-observation mapping for durable feedback-cycle telemetry.
//!
//! This module stops at the canonical, privacy-safe event envelope. Daemon
//! composition must enqueue that envelope through the existing authoritative
//! observation/analytics path; it must not write the analytics table directly.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{self, error::TrySendError};
use tracedecay_application::feedback::FeedbackObservationPort;
use tracedecay_domain::feedback::{
    FeedbackCycleObservationV1, FeedbackEvaluationInputV1, FeedbackSavedEvaluationV1,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

const OBSERVATION_ENVELOPE_DOMAIN: &str = "tracedecay.feedback.observation.plan26.v1";
const SAVED_EVALUATION_DOMAIN: &str = "tracedecay.feedback.saved-evaluation.plan26.v1";

/// Privacy-safe Plan-26 source event. It contains no source, path, diagnostic
/// message, overlay content, or transport-local delivery identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackObservationEnvelopeV1 {
    pub schema_version: u16,
    pub producer: String,
    pub privacy_class: String,
    pub idempotency_key: ManifestDigest,
    pub saved_evaluation_digest: ManifestDigest,
    pub observation: FeedbackCycleObservationV1,
}

impl FeedbackObservationEnvelopeV1 {
    pub fn validate(&self) -> Option<()> {
        if self.schema_version != 1
            || self.producer != "feedback_cycle"
            || self.privacy_class != "operational_no_content"
            || self.idempotency_key.validate().is_err()
            || self.saved_evaluation_digest.validate().is_err()
            || self.observation.validate().is_err()
        {
            return None;
        }
        let expected_key = canonical_sha256(&(
            OBSERVATION_ENVELOPE_DOMAIN,
            &self.saved_evaluation_digest,
            &self.observation,
        ))
        .ok()?;
        (expected_key == self.idempotency_key).then_some(())
    }

    /// Stable identity used by bounded ingress queues to converge retries and
    /// replay before an envelope reaches the durable observability authority.
    pub fn replay_identity(&self) -> Option<&str> {
        self.validate().map(|()| self.idempotency_key.as_str())
    }
}

/// Maps one validated durable saved-content observation into its canonical
/// Plan-26 source envelope. Overlay and mismatched observations fail closed.
pub fn feedback_observation_envelope(
    input: &FeedbackEvaluationInputV1,
    observation: FeedbackCycleObservationV1,
) -> Option<FeedbackObservationEnvelopeV1> {
    let saved = input.saved().ok()?;
    observation.validate().ok()?;
    if observation.cycle_id != input.request.cycle_id
        || observation.scope != input.request.scope
        || observation.policy_digest != input.request.policy_digest
        || observation.configuration_digest != input.request.configuration_digest
        || observation.observed_at != input.observed_at
    {
        return None;
    }
    observation_envelope(&saved, observation)
}

fn observation_envelope(
    saved: &FeedbackSavedEvaluationV1,
    observation: FeedbackCycleObservationV1,
) -> Option<FeedbackObservationEnvelopeV1> {
    let saved_evaluation_digest = canonical_sha256(&(SAVED_EVALUATION_DOMAIN, saved)).ok()?;
    let idempotency_key = canonical_sha256(&(
        OBSERVATION_ENVELOPE_DOMAIN,
        &saved_evaluation_digest,
        &observation,
    ))
    .ok()?;
    let envelope = FeedbackObservationEnvelopeV1 {
        schema_version: 1,
        producer: "feedback_cycle".to_owned(),
        privacy_class: "operational_no_content".to_owned(),
        idempotency_key,
        saved_evaluation_digest,
        observation,
    };
    envelope.validate()?;
    Some(envelope)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackObservationSinkOutcome {
    Enqueued,
    Duplicate,
    Dropped,
}

/// Bounded non-blocking daemon queue boundary. Implementations atomically use
/// `idempotency_key` to converge retries and replay; a plain append-only insert
/// is not conforming. `Dropped` is the explicit bounded-overflow outcome, not
/// permission to block or retry on the feedback path. Durable cursor/projection
/// commit and loss accounting remain daemon-owned.
pub trait Plan26FeedbackObservationQueue {
    fn enqueue_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome;

    /// Replays one previously accepted envelope through the exact same
    /// idempotency boundary. A duplicate outcome is successful convergence.
    fn replay_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        self.enqueue_feedback_observation(envelope)
    }
}

/// Daemon/store-owned durable observation ingress. The sink is responsible for
/// atomically retaining the idempotency key with the queued observation and
/// for preserving replay protection across process restart. This adapter has
/// no database handle, filesystem path, or retry worker of its own.
pub trait DurablePlan26FeedbackObservationSinkV1 {
    fn enqueue_durable_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome;

    fn replay_durable_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        self.enqueue_durable_feedback_observation(envelope)
    }
}

/// Concrete adapter from the durable daemon ingress to the application's
/// non-blocking queue boundary. Corrupt or privacy-invalid values are dropped
/// before the sink receives them, so a replay never turns bad input into state.
pub struct DurablePlan26FeedbackObservationQueueAdapterV1<S> {
    sink: S,
}

impl<S> DurablePlan26FeedbackObservationQueueAdapterV1<S> {
    pub fn new(sink: S) -> Self {
        Self { sink }
    }
}

impl<S> Plan26FeedbackObservationQueue for DurablePlan26FeedbackObservationQueueAdapterV1<S>
where
    S: DurablePlan26FeedbackObservationSinkV1,
{
    fn enqueue_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        if envelope.validate().is_none() {
            FeedbackObservationSinkOutcome::Dropped
        } else {
            self.sink.enqueue_durable_feedback_observation(envelope)
        }
    }

    fn replay_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        if envelope.validate().is_none() {
            FeedbackObservationSinkOutcome::Dropped
        } else {
            self.sink.replay_durable_feedback_observation(envelope)
        }
    }
}

/// Compatibility name for existing root-owned observation sinks.
pub use Plan26FeedbackObservationQueue as FeedbackObservationEventSink;

#[derive(Default)]
struct BoundedFeedbackObservationReplayWindow {
    replay_order: VecDeque<String>,
    replay_identities: BTreeSet<String>,
}

/// A bounded, process-local Plan-26 ingress queue.
///
/// The bounded channel never waits for capacity. A short replay-window
/// critical section serializes duplicate admission; contention is not overflow
/// and is never counted as observation loss. Replay identities remain active
/// for the most recently accepted `capacity` envelopes, including immediately
/// after dequeue. The daemon's durable observability authority remains
/// responsible for replay protection across restarts and beyond this window.
pub struct BoundedPlan26FeedbackObservationQueue {
    capacity: usize,
    sender: Option<mpsc::Sender<FeedbackObservationEnvelopeV1>>,
    receiver: Mutex<Option<mpsc::Receiver<FeedbackObservationEnvelopeV1>>>,
    replay_window: Mutex<BoundedFeedbackObservationReplayWindow>,
    dropped_count: AtomicU64,
}

impl BoundedPlan26FeedbackObservationQueue {
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = if capacity == 0 {
            (None, None)
        } else {
            let (sender, receiver) = mpsc::channel(capacity);
            (Some(sender), Some(receiver))
        };
        Self {
            capacity,
            sender,
            receiver: Mutex::new(receiver),
            replay_window: Mutex::new(BoundedFeedbackObservationReplayWindow::default()),
            dropped_count: AtomicU64::new(0),
        }
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn pending_len(&self) -> usize {
        self.sender.as_ref().map_or(0, |sender| {
            sender.max_capacity().saturating_sub(sender.capacity())
        })
    }

    /// Explicit bounded-overflow accounting for the daemon's Plan-26 metrics.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Removes the next accepted envelope for delivery to the durable
    /// observability authority. Its replay identity remains retained in the
    /// bounded window, so immediate delivery retries converge locally.
    pub fn take_next(&self) -> Option<FeedbackObservationEnvelopeV1> {
        self.receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()?
            .try_recv()
            .ok()
    }

    fn enqueue(&self, envelope: FeedbackObservationEnvelopeV1) -> FeedbackObservationSinkOutcome {
        let Some(identity) = envelope.replay_identity().map(str::to_owned) else {
            self.record_drop();
            return FeedbackObservationSinkOutcome::Dropped;
        };
        let Some(sender) = self.sender.as_ref() else {
            self.record_drop();
            return FeedbackObservationSinkOutcome::Dropped;
        };
        let mut replay_window = self
            .replay_window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if replay_window.replay_identities.contains(&identity) {
            return FeedbackObservationSinkOutcome::Duplicate;
        }

        replay_window.replay_identities.insert(identity.clone());
        match sender.try_send(envelope) {
            Ok(()) => {
                replay_window.replay_order.push_back(identity);
                while replay_window.replay_order.len() > self.capacity {
                    if let Some(expired) = replay_window.replay_order.pop_front() {
                        replay_window.replay_identities.remove(&expired);
                    }
                }
                FeedbackObservationSinkOutcome::Enqueued
            }
            Err(TrySendError::Full(_)) => {
                replay_window.replay_identities.remove(&identity);
                self.record_drop();
                FeedbackObservationSinkOutcome::Dropped
            }
            Err(TrySendError::Closed(_)) => {
                replay_window.replay_identities.remove(&identity);
                FeedbackObservationSinkOutcome::Dropped
            }
        }
    }

    fn record_drop(&self) {
        let _ = self
            .dropped_count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_add(1))
            });
    }
}

impl Plan26FeedbackObservationQueue for BoundedPlan26FeedbackObservationQueue {
    fn enqueue_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        self.enqueue(envelope)
    }
}

/// Adapts canonical Plan-26 envelopes to the application's one-way observation
/// port. Observation loss cannot alter feedback truth or trigger a retry cycle.
pub struct Plan26FeedbackObservationAdapter<S> {
    sink: S,
}

impl<S> Plan26FeedbackObservationAdapter<S> {
    pub fn new(sink: S) -> Self {
        Self { sink }
    }
}

impl<S> FeedbackObservationPort for Plan26FeedbackObservationAdapter<S>
where
    S: Plan26FeedbackObservationQueue,
{
    fn observe(&self, input: &FeedbackEvaluationInputV1, observation: FeedbackCycleObservationV1) {
        if let Some(envelope) = feedback_observation_envelope(input, observation) {
            let _ = self.sink.enqueue_feedback_observation(envelope);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::{BTreeSet, VecDeque};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use tracedecay_application::feedback::FeedbackObservationPort;
    use tracedecay_domain::feedback::{
        FeedbackActorContextV1, FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId,
        FeedbackCycleObservationV1, FeedbackCycleRequestV1, FeedbackEvaluationInputV1,
        FeedbackEvaluationStageV1, FeedbackObservationKindV1, FeedbackScopeV1, FeedbackTargetV1,
        FeedbackTriggerV1,
    };
    use tracedecay_domain::{
        CodeGenerationId, CommitId, FileOccurrenceId, HostInstanceId, ManifestDigest, ProjectId,
        RepositoryId, SessionId, SourceSpan, SymbolOccurrenceId, UtcMicros, WorktreeId,
    };

    use super::*;

    const SHA256_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA256_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn digest(value: &str) -> ManifestDigest {
        ManifestDigest::new(value).unwrap()
    }

    fn scope() -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: id::<ProjectId>("project.feedback.fixture"),
            repository_id: id::<RepositoryId>("repository.feedback.fixture"),
            worktree_id: id::<WorktreeId>("worktree.feedback.fixture"),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: id::<CommitId>("commit.feedback.fixture"),
        }
    }

    fn saved_input() -> FeedbackEvaluationInputV1 {
        let request = FeedbackCycleRequestV1::new(
            id::<FeedbackCycleId>("cycle.feedback.observation"),
            scope(),
            FeedbackContentIdentityV1::SavedContent {
                generation_digest: digest(SHA256_A),
                file_digest: digest(SHA256_B),
            },
            FeedbackTriggerV1::PostEditHook,
            digest(SHA256_A),
            digest(SHA256_B),
            FeedbackBudgetV1::bounded(100, 100, 1_000, 1_000),
        )
        .unwrap();
        FeedbackEvaluationInputV1 {
            request,
            target: FeedbackTargetV1 {
                file: id::<FileOccurrenceId>("file.feedback.observation"),
                span: Some(SourceSpan {
                    start_byte: 1,
                    end_byte: 2,
                }),
                symbol: Some(id::<SymbolOccurrenceId>("symbol.feedback.observation")),
                generation_id: Some(id::<CodeGenerationId>("generation.feedback.observation")),
            },
            actor: FeedbackActorContextV1::default(),
            observed_at: UtcMicros(2_000_000),
        }
    }

    fn overlay_input() -> FeedbackEvaluationInputV1 {
        let session_id = id::<SessionId>("session.feedback.observation");
        let client_id = id::<HostInstanceId>("client.feedback.observation");
        let request = FeedbackCycleRequestV1::new(
            id::<FeedbackCycleId>("cycle.feedback.overlay-observation"),
            scope(),
            FeedbackContentIdentityV1::EphemeralOverlay {
                session_id: session_id.clone(),
                owner_client_id: client_id.clone(),
                agent_id: None,
                document_version: 1,
                overlay_digest: digest(SHA256_A),
            },
            FeedbackTriggerV1::DocumentSave,
            digest(SHA256_A),
            digest(SHA256_B),
            FeedbackBudgetV1::bounded(100, 100, 1_000, 1_000),
        )
        .unwrap();
        FeedbackEvaluationInputV1 {
            request,
            target: FeedbackTargetV1 {
                file: id::<FileOccurrenceId>("file.feedback.overlay-observation"),
                span: None,
                symbol: None,
                generation_id: None,
            },
            actor: FeedbackActorContextV1 {
                session_id: Some(session_id),
                client_id: Some(client_id),
                agent_id: None,
                turn_id: None,
            },
            observed_at: UtcMicros(2_000_000),
        }
    }

    fn overlay_trigger(input: &FeedbackEvaluationInputV1) -> FeedbackCycleObservationV1 {
        FeedbackCycleObservationV1 {
            cycle_id: input.request.cycle_id.clone(),
            scope: input.request.scope.clone(),
            policy_digest: input.request.policy_digest.clone(),
            configuration_digest: input.request.configuration_digest.clone(),
            kind: FeedbackObservationKindV1::Trigger,
            stage: None,
            termination: None,
            dedupe_key: None,
            observed_at: input.observed_at,
            latency_micros: None,
            advisory_only: true,
        }
    }

    #[derive(Clone, Default)]
    struct RecordingSink(Rc<RefCell<Vec<FeedbackObservationEnvelopeV1>>>);

    impl FeedbackObservationEventSink for RecordingSink {
        fn enqueue_feedback_observation(
            &self,
            envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            if self
                .0
                .borrow()
                .iter()
                .any(|record| record.idempotency_key == envelope.idempotency_key)
            {
                return FeedbackObservationSinkOutcome::Duplicate;
            }
            self.0.borrow_mut().push(envelope);
            FeedbackObservationSinkOutcome::Enqueued
        }
    }

    #[derive(Clone, Default)]
    struct DroppingSink(Rc<Cell<usize>>);

    impl FeedbackObservationEventSink for DroppingSink {
        fn enqueue_feedback_observation(
            &self,
            _envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            self.0.set(self.0.get() + 1);
            FeedbackObservationSinkOutcome::Dropped
        }
    }

    #[derive(Clone, Default)]
    struct RestartSafeSink {
        replay_identities: Arc<Mutex<BTreeSet<String>>>,
        calls: Arc<AtomicUsize>,
    }

    impl DurablePlan26FeedbackObservationSinkV1 for RestartSafeSink {
        fn enqueue_durable_feedback_observation(
            &self,
            envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let identity = envelope.replay_identity().unwrap().to_owned();
            if self.replay_identities.lock().unwrap().insert(identity) {
                FeedbackObservationSinkOutcome::Enqueued
            } else {
                FeedbackObservationSinkOutcome::Duplicate
            }
        }

        fn replay_durable_feedback_observation(
            &self,
            envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            self.enqueue_durable_feedback_observation(envelope)
        }
    }

    #[test]
    fn durable_observation_mapping_is_replay_stable_and_sink_suppresses_overlays() {
        let input = saved_input();
        let observation = FeedbackCycleObservationV1::trigger(&input).unwrap();
        let first = feedback_observation_envelope(&input, observation.clone()).unwrap();
        let replay = feedback_observation_envelope(&input, observation.clone()).unwrap();
        assert_eq!(first, replay);
        assert_eq!(first.schema_version, 1);
        assert_eq!(first.producer, "feedback_cycle");
        assert_eq!(first.privacy_class, "operational_no_content");

        let sink = RecordingSink::default();
        let recorded = sink.0.clone();
        let adapter = Plan26FeedbackObservationAdapter::new(sink);
        adapter.observe(&input, observation.clone());
        adapter.observe(&input, observation);
        let overlay = overlay_input();
        adapter.observe(&overlay, overlay_trigger(&overlay));
        assert_eq!(recorded.borrow().as_slice(), &[first]);
    }

    #[test]
    fn bounded_queue_replay_converges_and_drop_never_retries() {
        let input = saved_input();
        let observation = FeedbackCycleObservationV1::trigger(&input).unwrap();
        let envelope = feedback_observation_envelope(&input, observation.clone()).unwrap();
        assert!(envelope.validate().is_some());

        let encoded = serde_json::to_string(&envelope).unwrap();
        let replay: FeedbackObservationEnvelopeV1 = serde_json::from_str(&encoded).unwrap();
        assert_eq!(replay, envelope);
        assert_eq!(replay.replay_identity(), envelope.replay_identity());

        let queue = BoundedPlan26FeedbackObservationQueue::new(1);
        assert_eq!(
            queue.enqueue_feedback_observation(envelope.clone()),
            FeedbackObservationSinkOutcome::Enqueued
        );
        assert_eq!(
            queue.replay_feedback_observation(replay),
            FeedbackObservationSinkOutcome::Duplicate
        );
        assert_eq!(queue.pending_len(), 1);

        let stage = feedback_observation_envelope(
            &input,
            FeedbackCycleObservationV1::stage(&input, FeedbackEvaluationStageV1::Admission)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            queue.enqueue_feedback_observation(stage.clone()),
            FeedbackObservationSinkOutcome::Dropped
        );
        assert_eq!(queue.dropped_count(), 1);
        assert_eq!(queue.take_next(), Some(envelope.clone()));
        assert_eq!(
            queue.replay_feedback_observation(envelope),
            FeedbackObservationSinkOutcome::Duplicate
        );
        assert_eq!(queue.take_next(), None);
        assert_eq!(
            queue.enqueue_feedback_observation(stage.clone()),
            FeedbackObservationSinkOutcome::Enqueued
        );
        assert_eq!(
            queue.replay_feedback_observation(stage),
            FeedbackObservationSinkOutcome::Duplicate
        );

        let dropping = DroppingSink::default();
        let dropped = dropping.0.clone();
        let adapter = Plan26FeedbackObservationAdapter::new(dropping);
        adapter.observe(&input, observation);
        assert_eq!(dropped.get(), 1);

        let contended = Arc::new(BoundedPlan26FeedbackObservationQueue::new(1));
        let guard = contended.replay_window.lock().unwrap();
        let started = Arc::new(Barrier::new(2));
        let worker_queue = Arc::clone(&contended);
        let worker_started = Arc::clone(&started);
        let contended_envelope = feedback_observation_envelope(
            &input,
            FeedbackCycleObservationV1::trigger(&input).unwrap(),
        )
        .unwrap();
        let worker = thread::spawn(move || {
            worker_started.wait();
            worker_queue.enqueue_feedback_observation(contended_envelope)
        });
        started.wait();
        thread::yield_now();
        assert_eq!(contended.dropped_count(), 0);
        drop(guard);
        assert_eq!(
            worker.join().unwrap(),
            FeedbackObservationSinkOutcome::Enqueued
        );
        assert_eq!(contended.dropped_count(), 0);
    }

    #[derive(Clone, Copy, Debug)]
    enum QueueAction {
        Enqueue(usize),
        Replay(usize),
        Take,
    }

    struct QueueModel {
        capacity: usize,
        pending: VecDeque<usize>,
        replay_order: VecDeque<usize>,
        replay_identities: BTreeSet<usize>,
        dropped_count: u64,
    }

    impl QueueModel {
        fn new(capacity: usize) -> Self {
            Self {
                capacity,
                pending: VecDeque::new(),
                replay_order: VecDeque::new(),
                replay_identities: BTreeSet::new(),
                dropped_count: 0,
            }
        }

        fn submit(&mut self, identity: usize) -> FeedbackObservationSinkOutcome {
            if self.replay_identities.contains(&identity) {
                return FeedbackObservationSinkOutcome::Duplicate;
            }
            if self.pending.len() >= self.capacity {
                self.dropped_count += 1;
                return FeedbackObservationSinkOutcome::Dropped;
            }

            self.pending.push_back(identity);
            self.replay_identities.insert(identity);
            self.replay_order.push_back(identity);
            while self.replay_order.len() > self.capacity {
                let expired = self.replay_order.pop_front().unwrap();
                self.replay_identities.remove(&expired);
            }
            FeedbackObservationSinkOutcome::Enqueued
        }
    }

    #[test]
    fn bounded_queue_matches_model_across_enqueue_replay_and_take_sequences() {
        let input = saved_input();
        let envelopes = [
            feedback_observation_envelope(
                &input,
                FeedbackCycleObservationV1::trigger(&input).unwrap(),
            )
            .unwrap(),
            feedback_observation_envelope(
                &input,
                FeedbackCycleObservationV1::stage(&input, FeedbackEvaluationStageV1::Admission)
                    .unwrap(),
            )
            .unwrap(),
            feedback_observation_envelope(
                &input,
                FeedbackCycleObservationV1::stage(&input, FeedbackEvaluationStageV1::Diagnostics)
                    .unwrap(),
            )
            .unwrap(),
        ];
        let actions = [
            QueueAction::Enqueue(0),
            QueueAction::Replay(0),
            QueueAction::Enqueue(1),
            QueueAction::Replay(1),
            QueueAction::Enqueue(2),
            QueueAction::Replay(2),
            QueueAction::Take,
        ];

        for first in actions {
            for second in actions {
                for third in actions {
                    for fourth in actions {
                        let sequence = [first, second, third, fourth];
                        let queue = BoundedPlan26FeedbackObservationQueue::new(2);
                        let mut model = QueueModel::new(2);

                        for action in sequence {
                            match action {
                                QueueAction::Enqueue(identity) => assert_eq!(
                                    queue.enqueue_feedback_observation(envelopes[identity].clone()),
                                    model.submit(identity),
                                    "sequence {sequence:?}"
                                ),
                                QueueAction::Replay(identity) => assert_eq!(
                                    queue.replay_feedback_observation(envelopes[identity].clone()),
                                    model.submit(identity),
                                    "sequence {sequence:?}"
                                ),
                                QueueAction::Take => assert_eq!(
                                    queue.take_next().map(|envelope| envelope.idempotency_key),
                                    model.pending.pop_front().map(|identity| envelopes[identity]
                                        .idempotency_key
                                        .clone()),
                                    "sequence {sequence:?}"
                                ),
                            }
                            assert_eq!(
                                queue.pending_len(),
                                model.pending.len(),
                                "sequence {sequence:?}"
                            );
                            assert_eq!(
                                queue.dropped_count(),
                                model.dropped_count,
                                "sequence {sequence:?}"
                            );
                        }
                    }
                }
            }
        }

        let zero_capacity = BoundedPlan26FeedbackObservationQueue::new(0);
        assert_eq!(
            zero_capacity.enqueue_feedback_observation(envelopes[0].clone()),
            FeedbackObservationSinkOutcome::Dropped
        );
        assert_eq!(zero_capacity.pending_len(), 0);
        assert_eq!(zero_capacity.dropped_count(), 1);
    }

    #[test]
    fn durable_sink_adapter_replays_across_restart_and_rejects_corruption() {
        let input = saved_input();
        let envelope = feedback_observation_envelope(
            &input,
            FeedbackCycleObservationV1::trigger(&input).unwrap(),
        )
        .unwrap();
        let sink = RestartSafeSink::default();
        let first_adapter = DurablePlan26FeedbackObservationQueueAdapterV1::new(sink.clone());
        assert_eq!(
            first_adapter.enqueue_feedback_observation(envelope.clone()),
            FeedbackObservationSinkOutcome::Enqueued
        );

        let restarted_adapter = DurablePlan26FeedbackObservationQueueAdapterV1::new(sink.clone());
        assert_eq!(
            restarted_adapter.replay_feedback_observation(envelope.clone()),
            FeedbackObservationSinkOutcome::Duplicate
        );
        assert_eq!(sink.calls.load(Ordering::SeqCst), 2);

        let mut corrupted = envelope;
        corrupted.schema_version = 0;
        assert_eq!(
            restarted_adapter.enqueue_feedback_observation(corrupted),
            FeedbackObservationSinkOutcome::Dropped
        );
        assert_eq!(
            sink.calls.load(Ordering::SeqCst),
            2,
            "corrupt envelopes must not reach a durable sink"
        );
    }
}
