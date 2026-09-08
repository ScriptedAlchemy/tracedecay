//! Daemon-global admission and fairness for semantic projection work.
//!
//! Per-project semantic runtimes submit immutable source-generation batches
//! through [`SemanticProjectionSchedulingPortV1`]. The daemon drains one batch
//! per exact worktree in round-robin order. Interactive queries use the
//! zero-waiter semantic session pool rather than this projection scheduler.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(feature = "hotpath")]
use std::time::Instant;

use thiserror::Error;
use tokio::sync::Notify;
use tracedecay_domain::{CodeGenerationId, WorktreeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SemanticProjectionSchedulerLimitsV1 {
    pub max_queued_batches: usize,
    pub max_queued_bytes: u64,
    pub max_session_memory_bytes: u64,
    pub max_publications: usize,
}

impl Default for SemanticProjectionSchedulerLimitsV1 {
    fn default() -> Self {
        Self {
            max_queued_batches: 64,
            max_queued_bytes: 16 * 1024 * 1024 * 1024,
            max_session_memory_bytes: 16 * 1024 * 1024 * 1024,
            max_publications: 1,
        }
    }
}

impl SemanticProjectionSchedulerLimitsV1 {
    fn validate(self) -> Result<Self, SemanticProjectionSchedulerConfigErrorV1> {
        if self.max_queued_batches == 0 {
            return Err(SemanticProjectionSchedulerConfigErrorV1::ZeroQueuedBatches);
        }
        if self.max_queued_bytes == 0 {
            return Err(SemanticProjectionSchedulerConfigErrorV1::ZeroQueuedBytes);
        }
        if self.max_session_memory_bytes == 0 {
            return Err(SemanticProjectionSchedulerConfigErrorV1::ZeroSessionMemory);
        }
        if self.max_publications == 0 {
            return Err(SemanticProjectionSchedulerConfigErrorV1::ZeroPublications);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SemanticProjectionSchedulerConfigErrorV1 {
    #[error("semantic projection queue must admit at least one batch")]
    ZeroQueuedBatches,
    #[error("semantic projection queue byte capacity must be non-zero")]
    ZeroQueuedBytes,
    #[error("semantic session-memory capacity must be non-zero")]
    ZeroSessionMemory,
    #[error("semantic publication concurrency must be non-zero")]
    ZeroPublications,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticProjectionBatchV1 {
    worktree_id: WorktreeId,
    source_generation: CodeGenerationId,
    queued_bytes: u64,
    session_memory_bytes: u64,
}

impl SemanticProjectionBatchV1 {
    pub fn new(
        worktree_id: WorktreeId,
        source_generation: CodeGenerationId,
        queued_bytes: u64,
        session_memory_bytes: u64,
    ) -> Self {
        Self {
            worktree_id,
            source_generation,
            queued_bytes,
            session_memory_bytes,
        }
    }

    pub fn worktree_id(&self) -> &WorktreeId {
        &self.worktree_id
    }

    pub fn source_generation(&self) -> &CodeGenerationId {
        &self.source_generation
    }

    pub fn queued_bytes(&self) -> u64 {
        self.queued_bytes
    }

    pub fn session_memory_bytes(&self) -> u64 {
        self.session_memory_bytes
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticProjectionEnqueueOutcomeV1 {
    pub ticket: u64,
    pub coalesced_batches: usize,
    pub coalesced_bytes: u64,
    pub cancelled_running_batches: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticProjectionCancellationOutcomeV1 {
    pub removed_queued_batches: usize,
    pub removed_queued_bytes: u64,
    pub cancelled_running_batches: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticProjectionSchedulerStatsV1 {
    pub queued_batches: usize,
    pub queued_bytes: u64,
    pub running_batches: usize,
    pub reserved_session_memory_bytes: u64,
    pub active_publications: usize,
}

/// Completion authority for one exact worktree's projection retirement.
///
/// Queue removal and active cancellation linearize before this receipt is
/// returned. Waiting observes only that worktree; unrelated projections never
/// enter its completion boundary.
pub struct SemanticProjectionWorktreeRetirementReceiptV1 {
    scheduler: DaemonGlobalSemanticProjectionSchedulerV1,
    worktree_id: WorktreeId,
    outcome: SemanticProjectionCancellationOutcomeV1,
}

impl SemanticProjectionWorktreeRetirementReceiptV1 {
    #[must_use]
    pub fn outcome(&self) -> SemanticProjectionCancellationOutcomeV1 {
        self.outcome
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        let state = self
            .scheduler
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        !state
            .active
            .values()
            .any(|active| active.worktree_id == self.worktree_id)
    }

    pub async fn wait_until(self, deadline: tokio::time::Instant) -> bool {
        loop {
            let idle = self.scheduler.worktree_idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self.is_idle() {
                return true;
            }
            if tokio::time::timeout_at(deadline, idle.as_mut())
                .await
                .is_err()
            {
                return false;
            }
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SemanticProjectionScheduleErrorV1 {
    #[error(
        "semantic projection queue byte capacity exhausted: requested {requested}, available {available}"
    )]
    QueueBytesCapacity { requested: u64, available: u64 },
    #[error(
        "semantic projection queue batch capacity exhausted: requested {requested}, available {available}"
    )]
    QueueBatchCapacity { requested: usize, available: usize },
    #[error("semantic session-memory reservation {requested} exceeds the global maximum {maximum}")]
    SessionMemoryReservationTooLarge { requested: u64, maximum: u64 },
    #[error("semantic publication capacity exhausted: active {active}, maximum {maximum}")]
    PublicationCapacity { active: usize, maximum: usize },
    #[error("semantic projection batch has already claimed its publication slot")]
    PublicationAlreadyClaimed,
    #[error("semantic projection batch was cancelled before publication")]
    Cancelled,
}

pub trait SemanticProjectionSchedulingPortV1: Send + Sync {
    fn enqueue(
        &self,
        batch: SemanticProjectionBatchV1,
    ) -> Result<SemanticProjectionEnqueueOutcomeV1, SemanticProjectionScheduleErrorV1>;

    fn try_dispatch(&self) -> Option<SemanticProjectionLeaseV1>;

    fn enqueue_work(
        &self,
        batch: SemanticProjectionBatchV1,
        dispatch: SemanticProjectionDispatchV1,
    ) -> Result<SemanticProjectionEnqueueOutcomeV1, SemanticProjectionScheduleErrorV1>;

    fn cancel_generation(
        &self,
        worktree_id: &WorktreeId,
        source_generation: &CodeGenerationId,
    ) -> SemanticProjectionCancellationOutcomeV1;

    fn stats(&self) -> SemanticProjectionSchedulerStatsV1;
}

pub type SemanticProjectionDispatchV1 = Box<dyn FnOnce(SemanticProjectionLeaseV1) + Send + 'static>;

struct QueuedProjectionBatchV1 {
    ticket: u64,
    batch: SemanticProjectionBatchV1,
    dispatch: Option<SemanticProjectionDispatchV1>,
    #[cfg(feature = "hotpath")]
    enqueued_at: Instant,
}

struct ActiveProjectionBatchV1 {
    worktree_id: WorktreeId,
    source_generation: CodeGenerationId,
    session_memory_bytes: u64,
    cancellation: Arc<AtomicBool>,
    publication_claimed: bool,
}

#[derive(Default)]
struct SemanticProjectionSchedulerStateV1 {
    next_ticket: u64,
    queues: BTreeMap<WorktreeId, VecDeque<QueuedProjectionBatchV1>>,
    ready_worktrees: VecDeque<WorktreeId>,
    latest_generations: BTreeMap<WorktreeId, CodeGenerationId>,
    active: BTreeMap<u64, ActiveProjectionBatchV1>,
    queued_batches: usize,
    queued_bytes: u64,
    reserved_session_memory_bytes: u64,
    active_publications: usize,
    draining_work: bool,
    work_drain_requested: bool,
}

impl SemanticProjectionSchedulerStateV1 {
    fn observe_hotpath(&self) {
        crate::hotpath_observe::semantic_queue(
            self.queued_batches,
            self.queued_bytes,
            self.reserved_session_memory_bytes,
            self.active_publications,
        );
    }
}

#[derive(Clone)]
pub struct DaemonGlobalSemanticProjectionSchedulerV1 {
    limits: SemanticProjectionSchedulerLimitsV1,
    state: Arc<Mutex<SemanticProjectionSchedulerStateV1>>,
    worktree_idle: Arc<Notify>,
}

impl Default for DaemonGlobalSemanticProjectionSchedulerV1 {
    fn default() -> Self {
        let limits = SemanticProjectionSchedulerLimitsV1::default();
        match Self::new(limits) {
            Ok(scheduler) => scheduler,
            Err(_) => unreachable!("default semantic projection scheduler limits are valid"),
        }
    }
}

impl DaemonGlobalSemanticProjectionSchedulerV1 {
    pub fn new(
        limits: SemanticProjectionSchedulerLimitsV1,
    ) -> Result<Self, SemanticProjectionSchedulerConfigErrorV1> {
        Ok(Self {
            limits: limits.validate()?,
            state: Arc::new(Mutex::new(SemanticProjectionSchedulerStateV1::default())),
            worktree_idle: Arc::new(Notify::new()),
        })
    }

    pub fn limits(&self) -> SemanticProjectionSchedulerLimitsV1 {
        self.limits
    }

    /// Fence all queued and active semantic projection work for one worktree.
    ///
    /// The caller must first stop and join the worktree's code-index producer,
    /// making this state-lock transition a closed inventory. Queued dispatch
    /// closures are dropped immediately; active leases are cancelled and the
    /// returned receipt waits for only those exact leases to release.
    pub fn begin_worktree_retirement(
        &self,
        worktree_id: &WorktreeId,
    ) -> SemanticProjectionWorktreeRetirementReceiptV1 {
        let (retired_queue, outcome) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let retired_queue = state.queues.remove(worktree_id).unwrap_or_default();
            let removed_queued_batches = retired_queue.len();
            let removed_queued_bytes = retired_queue
                .iter()
                .map(|queued| queued.batch.queued_bytes)
                .sum();
            state.queued_batches = state.queued_batches.saturating_sub(removed_queued_batches);
            state.queued_bytes = state.queued_bytes.saturating_sub(removed_queued_bytes);
            state
                .ready_worktrees
                .retain(|queued_worktree| queued_worktree != worktree_id);
            state.latest_generations.remove(worktree_id);
            let cancelled_running_batches = state
                .active
                .values()
                .filter(|active| {
                    &active.worktree_id == worktree_id
                        && !active.cancellation.swap(true, Ordering::AcqRel)
                })
                .count();
            let outcome = SemanticProjectionCancellationOutcomeV1 {
                removed_queued_batches,
                removed_queued_bytes,
                cancelled_running_batches,
            };
            crate::hotpath_observe::semantic_cancelled(
                removed_queued_batches.saturating_add(cancelled_running_batches),
            );
            state.observe_hotpath();
            (retired_queue, outcome)
        };
        drop(retired_queue);
        SemanticProjectionWorktreeRetirementReceiptV1 {
            scheduler: self.clone(),
            worktree_id: worktree_id.clone(),
            outcome,
        }
    }

    #[hotpath::measure(label = "usecases.semantic.enqueue")]
    pub fn enqueue(
        &self,
        batch: SemanticProjectionBatchV1,
    ) -> Result<SemanticProjectionEnqueueOutcomeV1, SemanticProjectionScheduleErrorV1> {
        self.enqueue_inner(batch, None)
    }

    #[hotpath::measure(label = "usecases.semantic.enqueue_work")]
    pub fn enqueue_work(
        &self,
        batch: SemanticProjectionBatchV1,
        dispatch: SemanticProjectionDispatchV1,
    ) -> Result<SemanticProjectionEnqueueOutcomeV1, SemanticProjectionScheduleErrorV1> {
        let outcome = self.enqueue_inner(batch, Some(dispatch))?;
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.work_drain_requested = true;
        }
        self.drain_ready_work();
        Ok(outcome)
    }

    fn enqueue_inner(
        &self,
        batch: SemanticProjectionBatchV1,
        dispatch: Option<SemanticProjectionDispatchV1>,
    ) -> Result<SemanticProjectionEnqueueOutcomeV1, SemanticProjectionScheduleErrorV1> {
        if batch.session_memory_bytes > self.limits.max_session_memory_bytes {
            return Err(
                SemanticProjectionScheduleErrorV1::SessionMemoryReservationTooLarge {
                    requested: batch.session_memory_bytes,
                    maximum: self.limits.max_session_memory_bytes,
                },
            );
        }

        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let supersedes = state
            .latest_generations
            .get(&batch.worktree_id)
            .is_some_and(|generation| generation != &batch.source_generation);
        let (coalesced_batches, coalesced_bytes) = if supersedes {
            state
                .queues
                .get(&batch.worktree_id)
                .map(|queue| {
                    (
                        queue.len(),
                        queue.iter().map(|queued| queued.batch.queued_bytes).sum(),
                    )
                })
                .unwrap_or_default()
        } else {
            (0, 0)
        };
        let projected_batches = state
            .queued_batches
            .saturating_sub(coalesced_batches)
            .saturating_add(1);
        if projected_batches > self.limits.max_queued_batches {
            return Err(SemanticProjectionScheduleErrorV1::QueueBatchCapacity {
                requested: 1,
                available: self
                    .limits
                    .max_queued_batches
                    .saturating_sub(state.queued_batches.saturating_sub(coalesced_batches)),
            });
        }
        let retained_bytes = state.queued_bytes.saturating_sub(coalesced_bytes);
        let projected_bytes = retained_bytes.checked_add(batch.queued_bytes).ok_or(
            SemanticProjectionScheduleErrorV1::QueueBytesCapacity {
                requested: batch.queued_bytes,
                available: self.limits.max_queued_bytes.saturating_sub(retained_bytes),
            },
        )?;
        if projected_bytes > self.limits.max_queued_bytes {
            return Err(SemanticProjectionScheduleErrorV1::QueueBytesCapacity {
                requested: batch.queued_bytes,
                available: self.limits.max_queued_bytes.saturating_sub(retained_bytes),
            });
        }

        let cancelled_running_batches = if supersedes {
            state
                .active
                .values()
                .filter(|active| {
                    active.worktree_id == batch.worktree_id
                        && active.source_generation != batch.source_generation
                })
                .map(|active| {
                    active.cancellation.store(true, Ordering::Release);
                    1
                })
                .sum()
        } else {
            0
        };
        if supersedes {
            state.queues.remove(&batch.worktree_id);
            state.queued_batches = state.queued_batches.saturating_sub(coalesced_batches);
            state.queued_bytes = retained_bytes;
        }

        state.next_ticket = state.next_ticket.wrapping_add(1);
        let ticket = state.next_ticket;
        let queue_was_empty = !state.queues.contains_key(&batch.worktree_id);
        state
            .queues
            .entry(batch.worktree_id.clone())
            .or_default()
            .push_back(QueuedProjectionBatchV1 {
                ticket,
                batch: batch.clone(),
                dispatch,
                #[cfg(feature = "hotpath")]
                enqueued_at: Instant::now(),
            });
        if queue_was_empty
            && !state
                .ready_worktrees
                .iter()
                .any(|worktree| worktree == &batch.worktree_id)
        {
            state.ready_worktrees.push_back(batch.worktree_id.clone());
        }
        state
            .latest_generations
            .insert(batch.worktree_id.clone(), batch.source_generation.clone());
        state.queued_batches += 1;
        state.queued_bytes = projected_bytes;
        state.observe_hotpath();

        Ok(SemanticProjectionEnqueueOutcomeV1 {
            ticket,
            coalesced_batches,
            coalesced_bytes,
            cancelled_running_batches,
        })
    }

    #[hotpath::measure(label = "usecases.semantic.dispatch")]
    pub fn try_dispatch(&self) -> Option<SemanticProjectionLeaseV1> {
        self.try_dispatch_matching(false).map(|(lease, _)| lease)
    }

    fn try_dispatch_matching(
        &self,
        scheduled_work: bool,
    ) -> Option<(
        SemanticProjectionLeaseV1,
        Option<SemanticProjectionDispatchV1>,
    )> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let worktree_count = state.ready_worktrees.len();
        for _ in 0..worktree_count {
            let worktree_id = state.ready_worktrees.pop_front()?;
            if state
                .active
                .values()
                .any(|active| active.worktree_id == worktree_id)
            {
                state.ready_worktrees.push_back(worktree_id);
                continue;
            }
            let required_memory = state
                .queues
                .get(&worktree_id)
                .and_then(VecDeque::front)
                .map(|queued| queued.batch.session_memory_bytes);
            let Some(required_memory) = required_memory else {
                state.queues.remove(&worktree_id);
                continue;
            };
            let dispatch_matches = state
                .queues
                .get(&worktree_id)
                .and_then(VecDeque::front)
                .is_some_and(|queued| queued.dispatch.is_some() == scheduled_work);
            if !dispatch_matches {
                state.ready_worktrees.push_back(worktree_id);
                continue;
            }
            let available_memory = self
                .limits
                .max_session_memory_bytes
                .saturating_sub(state.reserved_session_memory_bytes);
            if required_memory > available_memory {
                state.ready_worktrees.push_back(worktree_id);
                continue;
            }

            let Some((queued, has_more)) = state.queues.get_mut(&worktree_id).and_then(|queue| {
                let queued = queue.pop_front()?;
                Some((queued, !queue.is_empty()))
            }) else {
                state.queues.remove(&worktree_id);
                continue;
            };
            if has_more {
                state.ready_worktrees.push_back(worktree_id.clone());
            } else {
                state.queues.remove(&worktree_id);
            }
            state.queued_batches = state.queued_batches.saturating_sub(1);
            state.queued_bytes = state.queued_bytes.saturating_sub(queued.batch.queued_bytes);
            state.reserved_session_memory_bytes = state
                .reserved_session_memory_bytes
                .saturating_add(required_memory);
            let cancellation = Arc::new(AtomicBool::new(false));
            state.active.insert(
                queued.ticket,
                ActiveProjectionBatchV1 {
                    worktree_id,
                    source_generation: queued.batch.source_generation.clone(),
                    session_memory_bytes: required_memory,
                    cancellation: Arc::clone(&cancellation),
                    publication_claimed: false,
                },
            );
            #[cfg(feature = "hotpath")]
            {
                let wait_ns =
                    u64::try_from(queued.enqueued_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
                crate::hotpath_observe::semantic_queue_wait_ns(wait_ns);
            }
            state.observe_hotpath();
            let lease = SemanticProjectionLeaseV1 {
                ticket: queued.ticket,
                batch: queued.batch,
                cancellation,
                scheduler: self.clone(),
            };
            return Some((lease, queued.dispatch));
        }
        None
    }

    #[hotpath::measure(label = "usecases.semantic.cancel")]
    pub fn cancel_generation(
        &self,
        worktree_id: &WorktreeId,
        source_generation: &CodeGenerationId,
    ) -> SemanticProjectionCancellationOutcomeV1 {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut removed_queued_batches = 0;
        let mut removed_queued_bytes = 0_u64;
        let queue_is_empty = if let Some(queue) = state.queues.get_mut(worktree_id) {
            queue.retain(|queued| {
                if &queued.batch.source_generation == source_generation {
                    removed_queued_batches += 1;
                    removed_queued_bytes =
                        removed_queued_bytes.saturating_add(queued.batch.queued_bytes);
                    false
                } else {
                    true
                }
            });
            queue.is_empty()
        } else {
            false
        };
        if queue_is_empty {
            state.queues.remove(worktree_id);
            state
                .ready_worktrees
                .retain(|queued_worktree| queued_worktree != worktree_id);
        }
        state.queued_batches = state.queued_batches.saturating_sub(removed_queued_batches);
        state.queued_bytes = state.queued_bytes.saturating_sub(removed_queued_bytes);
        let cancelled_running_batches = state
            .active
            .values()
            .filter(|active| {
                &active.worktree_id == worktree_id
                    && &active.source_generation == source_generation
                    && !active.cancellation.swap(true, Ordering::AcqRel)
            })
            .count();
        if !state.queues.contains_key(worktree_id)
            && !state
                .active
                .values()
                .any(|active| &active.worktree_id == worktree_id)
            && state.latest_generations.get(worktree_id) == Some(source_generation)
        {
            state.latest_generations.remove(worktree_id);
        }
        crate::hotpath_observe::semantic_cancelled(
            removed_queued_batches.saturating_add(cancelled_running_batches),
        );
        state.observe_hotpath();
        SemanticProjectionCancellationOutcomeV1 {
            removed_queued_batches,
            removed_queued_bytes,
            cancelled_running_batches,
        }
    }

    pub fn stats(&self) -> SemanticProjectionSchedulerStatsV1 {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.observe_hotpath();
        SemanticProjectionSchedulerStatsV1 {
            queued_batches: state.queued_batches,
            queued_bytes: state.queued_bytes,
            running_batches: state.active.len(),
            reserved_session_memory_bytes: state.reserved_session_memory_bytes,
            active_publications: state.active_publications,
        }
    }

    fn release_projection(&self, ticket: u64) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(active) = state.active.remove(&ticket) {
                let worktree_id = active.worktree_id.clone();
                state.reserved_session_memory_bytes = state
                    .reserved_session_memory_bytes
                    .saturating_sub(active.session_memory_bytes);
                if !state.queues.contains_key(&worktree_id)
                    && !state
                        .active
                        .values()
                        .any(|running| running.worktree_id == worktree_id)
                {
                    state.latest_generations.remove(&worktree_id);
                }
            }
            state.work_drain_requested = true;
            state.observe_hotpath();
        }
        self.worktree_idle.notify_waiters();
        self.drain_ready_work();
    }

    fn release_publication(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.active_publications = state.active_publications.saturating_sub(1);
        state.observe_hotpath();
    }

    fn drain_ready_work(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.draining_work {
                state.work_drain_requested = true;
                return;
            }
            state.draining_work = true;
            state.work_drain_requested = false;
        }

        loop {
            while let Some((lease, Some(dispatch))) = self.try_dispatch_matching(true) {
                dispatch(lease);
            }
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.work_drain_requested {
                state.work_drain_requested = false;
                drop(state);
                continue;
            }
            state.draining_work = false;
            break;
        }
    }
}

impl SemanticProjectionSchedulingPortV1 for DaemonGlobalSemanticProjectionSchedulerV1 {
    fn enqueue(
        &self,
        batch: SemanticProjectionBatchV1,
    ) -> Result<SemanticProjectionEnqueueOutcomeV1, SemanticProjectionScheduleErrorV1> {
        Self::enqueue(self, batch)
    }

    fn try_dispatch(&self) -> Option<SemanticProjectionLeaseV1> {
        Self::try_dispatch(self)
    }

    fn enqueue_work(
        &self,
        batch: SemanticProjectionBatchV1,
        dispatch: SemanticProjectionDispatchV1,
    ) -> Result<SemanticProjectionEnqueueOutcomeV1, SemanticProjectionScheduleErrorV1> {
        Self::enqueue_work(self, batch, dispatch)
    }

    fn cancel_generation(
        &self,
        worktree_id: &WorktreeId,
        source_generation: &CodeGenerationId,
    ) -> SemanticProjectionCancellationOutcomeV1 {
        Self::cancel_generation(self, worktree_id, source_generation)
    }

    fn stats(&self) -> SemanticProjectionSchedulerStatsV1 {
        Self::stats(self)
    }
}

pub struct SemanticProjectionLeaseV1 {
    ticket: u64,
    batch: SemanticProjectionBatchV1,
    cancellation: Arc<AtomicBool>,
    scheduler: DaemonGlobalSemanticProjectionSchedulerV1,
}

impl SemanticProjectionLeaseV1 {
    pub fn ticket(&self) -> u64 {
        self.ticket
    }

    pub fn batch(&self) -> &SemanticProjectionBatchV1 {
        &self.batch
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }

    #[hotpath::measure(label = "usecases.semantic.begin_publication")]
    pub fn try_begin_publication(
        &self,
    ) -> Result<SemanticProjectionPublicationLeaseV1, SemanticProjectionScheduleErrorV1> {
        if self.is_cancelled() {
            crate::hotpath_observe::semantic_cancelled(1);
            return Err(SemanticProjectionScheduleErrorV1::Cancelled);
        }
        let mut state = self
            .scheduler
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let active_publications = state.active_publications;
        let active = state
            .active
            .get_mut(&self.ticket)
            .ok_or(SemanticProjectionScheduleErrorV1::Cancelled)?;
        if active.cancellation.load(Ordering::Acquire) {
            crate::hotpath_observe::semantic_cancelled(1);
            return Err(SemanticProjectionScheduleErrorV1::Cancelled);
        }
        if active.publication_claimed {
            return Err(SemanticProjectionScheduleErrorV1::PublicationAlreadyClaimed);
        }
        if active_publications >= self.scheduler.limits.max_publications {
            return Err(SemanticProjectionScheduleErrorV1::PublicationCapacity {
                active: active_publications,
                maximum: self.scheduler.limits.max_publications,
            });
        }
        active.publication_claimed = true;
        state.active_publications += 1;
        state.observe_hotpath();
        Ok(SemanticProjectionPublicationLeaseV1 {
            cancellation: Arc::clone(&self.cancellation),
            scheduler: self.scheduler.clone(),
        })
    }
}

impl Drop for SemanticProjectionLeaseV1 {
    fn drop(&mut self) {
        self.scheduler.release_projection(self.ticket);
    }
}

pub struct SemanticProjectionPublicationLeaseV1 {
    cancellation: Arc<AtomicBool>,
    scheduler: DaemonGlobalSemanticProjectionSchedulerV1,
}

impl SemanticProjectionPublicationLeaseV1 {
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
}

impl Drop for SemanticProjectionPublicationLeaseV1 {
    fn drop(&mut self) {
        self.scheduler.release_publication();
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{CodeGenerationId, WorktreeId};

    use super::*;

    fn worktree(value: &str) -> WorktreeId {
        WorktreeId::new(format!("worktree.{value}")).expect("worktree id")
    }

    fn generation(value: &str) -> CodeGenerationId {
        CodeGenerationId::new(format!("code-generation.{value}")).expect("generation id")
    }

    fn batch(
        worktree_value: &str,
        generation_value: &str,
        queued_bytes: u64,
        session_memory_bytes: u64,
    ) -> SemanticProjectionBatchV1 {
        SemanticProjectionBatchV1::new(
            worktree(worktree_value),
            generation(generation_value),
            queued_bytes,
            session_memory_bytes,
        )
    }

    fn scheduler() -> DaemonGlobalSemanticProjectionSchedulerV1 {
        DaemonGlobalSemanticProjectionSchedulerV1::new(SemanticProjectionSchedulerLimitsV1 {
            max_queued_batches: 8,
            max_queued_bytes: 800,
            max_session_memory_bytes: 100,
            max_publications: 1,
        })
        .expect("valid limits")
    }

    #[test]
    fn dispatch_is_deterministically_round_robin_by_exact_worktree() {
        let scheduler = scheduler();
        scheduler.enqueue(batch("a", "one", 10, 1)).unwrap();
        scheduler.enqueue(batch("a", "one", 10, 1)).unwrap();
        scheduler.enqueue(batch("b", "one", 10, 1)).unwrap();
        scheduler.enqueue(batch("c", "one", 10, 1)).unwrap();
        scheduler.enqueue(batch("b", "one", 10, 1)).unwrap();
        scheduler.enqueue(batch("c", "one", 10, 1)).unwrap();

        let order = (0..6)
            .map(|_| {
                let lease = scheduler.try_dispatch().expect("ready batch");
                lease.batch().worktree_id().as_str().to_owned()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            order,
            vec![
                "worktree.a",
                "worktree.b",
                "worktree.c",
                "worktree.a",
                "worktree.b",
                "worktree.c",
            ]
        );
    }

    #[test]
    fn newer_source_generation_coalesces_queued_and_cancels_running_work() {
        let scheduler = scheduler();
        scheduler.enqueue(batch("a", "old", 30, 20)).unwrap();
        scheduler.enqueue(batch("a", "old", 40, 20)).unwrap();
        let running = scheduler.try_dispatch().expect("old batch");

        let outcome = scheduler
            .enqueue(batch("a", "new", 50, 20))
            .expect("replacement fits after coalescing");

        assert_eq!(outcome.coalesced_batches, 1);
        assert_eq!(outcome.coalesced_bytes, 40);
        assert_eq!(outcome.cancelled_running_batches, 1);
        assert!(running.is_cancelled());
        assert!(scheduler.try_dispatch().is_none());
        drop(running);
        let replacement = scheduler.try_dispatch().expect("replacement");
        assert_eq!(replacement.batch().source_generation(), &generation("new"));
    }

    #[test]
    fn queue_and_session_memory_bounds_are_typed_and_recover_on_release() {
        let scheduler =
            DaemonGlobalSemanticProjectionSchedulerV1::new(SemanticProjectionSchedulerLimitsV1 {
                max_queued_batches: 1,
                max_queued_bytes: 50,
                max_session_memory_bytes: 50,
                max_publications: 1,
            })
            .unwrap();

        assert_eq!(
            scheduler.enqueue(batch("a", "one", 51, 1)),
            Err(SemanticProjectionScheduleErrorV1::QueueBytesCapacity {
                requested: 51,
                available: 50,
            })
        );
        assert_eq!(
            scheduler.enqueue(batch("a", "one", 1, 51)),
            Err(
                SemanticProjectionScheduleErrorV1::SessionMemoryReservationTooLarge {
                    requested: 51,
                    maximum: 50,
                }
            )
        );

        scheduler.enqueue(batch("a", "one", 20, 40)).unwrap();
        assert_eq!(
            scheduler.enqueue(batch("b", "one", 20, 10)),
            Err(SemanticProjectionScheduleErrorV1::QueueBatchCapacity {
                requested: 1,
                available: 0,
            })
        );
        let first = scheduler.try_dispatch().expect("first batch");
        scheduler.enqueue(batch("b", "one", 20, 20)).unwrap();
        assert!(scheduler.try_dispatch().is_none());
        drop(first);
        assert!(scheduler.try_dispatch().is_some());
    }

    #[test]
    fn publication_capacity_is_global_and_released_by_lease_drop() {
        let scheduler = scheduler();
        scheduler.enqueue(batch("a", "one", 10, 10)).unwrap();
        scheduler.enqueue(batch("b", "one", 10, 10)).unwrap();
        let first = scheduler.try_dispatch().unwrap();
        let second = scheduler.try_dispatch().unwrap();
        let publication = first.try_begin_publication().expect("first publication");

        assert!(matches!(
            second.try_begin_publication(),
            Err(SemanticProjectionScheduleErrorV1::PublicationCapacity {
                active: 1,
                maximum: 1,
            })
        ));
        drop(publication);
        assert!(second.try_begin_publication().is_ok());
    }

    #[test]
    fn cancellation_removes_only_the_exact_worktree_generation() {
        let scheduler = scheduler();
        scheduler.enqueue(batch("a", "old", 10, 1)).unwrap();
        scheduler.enqueue(batch("a", "old", 20, 1)).unwrap();
        scheduler.enqueue(batch("b", "old", 30, 1)).unwrap();

        let outcome = scheduler.cancel_generation(&worktree("a"), &generation("old"));

        assert_eq!(outcome.removed_queued_batches, 2);
        assert_eq!(outcome.removed_queued_bytes, 30);
        assert_eq!(outcome.cancelled_running_batches, 0);
        let remaining = scheduler.try_dispatch().expect("other worktree remains");
        assert_eq!(remaining.batch().worktree_id(), &worktree("b"));
        assert!(scheduler.try_dispatch().is_none());
    }

    #[tokio::test]
    async fn worktree_retirement_releases_queued_resources_and_preserves_siblings() {
        let scheduler = scheduler();
        let active = Arc::new(Mutex::new(None));
        let active_dispatch = Arc::clone(&active);
        scheduler
            .enqueue_work(
                batch("a", "old", 1, 100),
                Box::new(move |lease| {
                    *active_dispatch
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(lease);
                }),
            )
            .unwrap();
        let target_resource = Arc::new(());
        let target_resource_weak = Arc::downgrade(&target_resource);
        scheduler
            .enqueue_work(
                batch("a", "latest", 10, 10),
                Box::new(move |_lease| drop(target_resource)),
            )
            .unwrap();
        let unrelated_ran = Arc::new(AtomicBool::new(false));
        let unrelated_dispatch = Arc::clone(&unrelated_ran);
        scheduler
            .enqueue_work(
                batch("b", "other", 10, 10),
                Box::new(move |_lease| {
                    unrelated_dispatch.store(true, Ordering::Release);
                }),
            )
            .unwrap();

        let receipt = scheduler.begin_worktree_retirement(&worktree("a"));
        assert_eq!(receipt.outcome().removed_queued_batches, 1);
        assert_eq!(receipt.outcome().cancelled_running_batches, 0);
        assert!(!receipt.is_idle());
        assert!(target_resource_weak.upgrade().is_none());
        assert!(!unrelated_ran.load(Ordering::Acquire));
        assert!(
            active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .is_some_and(SemanticProjectionLeaseV1::is_cancelled)
        );

        active.lock().unwrap_or_else(PoisonError::into_inner).take();
        assert!(
            receipt
                .wait_until(tokio::time::Instant::now() + std::time::Duration::from_secs(1))
                .await
        );
        assert!(unrelated_ran.load(Ordering::Acquire));
    }

    #[test]
    fn concrete_scheduler_is_callable_through_the_shared_port() {
        let scheduler = scheduler();
        let port: &dyn SemanticProjectionSchedulingPortV1 = &scheduler;

        port.enqueue(batch("a", "one", 10, 1)).unwrap();
        assert_eq!(port.stats().queued_batches, 1);
        assert!(port.try_dispatch().is_some());
    }

    #[test]
    fn scheduled_work_self_drains_in_worktree_fair_order_after_capacity_returns() {
        let scheduler = scheduler();
        scheduler.enqueue(batch("blocker", "one", 1, 100)).unwrap();
        let blocker = scheduler.try_dispatch().expect("capacity blocker");
        let order = Arc::new(Mutex::new(Vec::new()));
        for (worktree, label) in [("a", "a1"), ("a", "a2"), ("b", "b1")] {
            let order = Arc::clone(&order);
            scheduler
                .enqueue_work(
                    batch(worktree, "one", 10, 10),
                    Box::new(move |_lease| {
                        order
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(label);
                    }),
                )
                .unwrap();
        }

        assert!(order.lock().unwrap().is_empty());
        drop(blocker);
        assert_eq!(
            *order.lock().unwrap_or_else(PoisonError::into_inner),
            vec!["a1", "b1", "a2"]
        );
    }
}
