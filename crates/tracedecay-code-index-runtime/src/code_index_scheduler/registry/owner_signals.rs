//! Change signals for one project root, and the one seat wait built on them.

use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};

use std::time::Duration;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

use tracedecay_contracts::code_index_freshness::{
    CodeIndexConvergenceParkedV1, CodeIndexFreshnessReadFailureV1, CodeIndexReadinessTargetV1,
    CodeIndexReadinessV1, CodeIndexReadinessWaitReadV1,
};

use tracedecay_contracts::ResolvedScope;

use super::{
    CodeIndexCadenceTriggerV1, CodeIndexGenerationPublishedV1, CodeIndexOwnerActivityV1,
    CodeIndexReconcileAdmissionV1, CodeIndexSchedulerRegistryV1, unique_mounted_for_scope,
};
use crate::code_index_scheduler::reconcile::FreshnessProbeVerdictV1;
use crate::code_index_scheduler::{
    CodeIndexCadenceTelemetryV1, CodeIndexSchedulerErrorV1, CodeIndexWorktreeSchedulerV1,
    LatestCodeTextGenerationV1,
};

/// Why the source could not be proven current before waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeIndexFreshSweepRefusedV1 {
    /// The publication authority is parked until an operator reset.
    PublicationParked,
    /// The blocking sweep did not complete.
    SweepFailed,
}

/// Why a seat wait ended without the seat it waited for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexSeatParkV1 {
    /// The worker parked on a failure; the park names its cause and remedy.
    Convergence(CodeIndexConvergenceParkedV1),
    /// The worktree has no durable publication: a first index, with nothing
    /// retained to seat.
    Unpublished,
    /// The durable publication pointer could not be read.
    PublicationUnreadable,
    /// A generation is seated, but it does not serve the waiting read.
    SeatNotServable,
    /// The awaited readiness is unreachable for the named reason.
    Unreachable(String),
}

impl CodeIndexSeatParkV1 {
    /// The readiness-wait reason this park reports.
    fn readiness_reason(self) -> String {
        match self {
            Self::Convergence(_) => "code_index_convergence_parked".to_owned(),
            Self::Unpublished => "code_index_unpublished".to_owned(),
            Self::PublicationUnreadable => "code_index_publication_unreadable".to_owned(),
            Self::SeatNotServable => "code_index_seat_not_servable".to_owned(),
            Self::Unreachable(reason) => reason,
        }
    }
}

/// How one wait for a worktree's seat ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeIndexSeatWaitV1<T> {
    /// The awaited seat is in place.
    Seated(T),
    /// Waiting cannot install the seat.
    Parked(CodeIndexSeatParkV1),
    /// The registry closed, the worktree is shutting down, or the request
    /// was cancelled.
    Cancelled,
    /// The deadline passed while the seat was still being installed.
    Deadline,
}

/// The registry that owns these channels is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeIndexOwnerSignalsClosedV1;

/// Every signal the registry publishes for one root: registry-wide serving
/// seats and mounts, the root's sealed publications, the worktree's
/// serving-generation changes, its owner passes, pending wake and worker
/// phase, and cadence receipts.
///
/// Subscribe before the first read. `watch::Sender::subscribe()` marks the
/// current value seen, so a change between subscribe and [`Self::changed`]
/// still wakes the waiter, and a read that misses a transition re-runs on
/// the next one instead of on a timer. The per-worktree channels exist only
/// while the worktree is mounted: `changed` returns as soon as one is
/// (re)subscribed, so a caller re-reads before depending on it.
pub struct CodeIndexOwnerSignalsV1 {
    registry: CodeIndexSchedulerRegistryV1,
    path: PathBuf,
    seats: tokio::sync::watch::Receiver<u64>,
    root_mounted: tokio::sync::watch::Receiver<u64>,
    receipts: tokio::sync::watch::Receiver<CodeIndexCadenceTelemetryV1>,
    publications: tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1>,
    serving: Option<tokio::sync::watch::Receiver<()>>,
    activity: Option<CodeIndexOwnerActivityV1>,
}

impl CodeIndexOwnerSignalsV1 {
    pub async fn subscribe(registry: &CodeIndexSchedulerRegistryV1, path: &Path) -> Self {
        Self {
            registry: registry.clone(),
            path: canonical_existing_identity(path).unwrap_or_else(|_| path.to_path_buf()),
            seats: registry.subscribe_serving_seats(),
            root_mounted: registry.subscribe_root_mounted(),
            receipts: registry.subscribe_cadence_receipts(),
            publications: registry.subscribe_generation_publications(),
            serving: registry.subscribe_serving_generation_changes(path).await,
            activity: registry.subscribe_owner_activity(path).await,
        }
    }

    /// Resolves on a sealed publication of this root; a lagged receiver may
    /// have dropped one, so it resolves then too.
    async fn root_published(
        publications: &mut tokio::sync::broadcast::Receiver<CodeIndexGenerationPublishedV1>,
        path: &Path,
    ) -> Result<(), CodeIndexOwnerSignalsClosedV1> {
        loop {
            match publications.recv().await {
                Ok(publication) if publication.project_root == path => return Ok(()),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return Ok(()),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(CodeIndexOwnerSignalsClosedV1);
                }
            }
        }
    }

    /// Resolves once per quiescent burst of publications, so a waiter never
    /// re-reads, and contends with the worker, on every intra-pass update.
    pub async fn changed(&mut self) -> Result<(), CodeIndexOwnerSignalsClosedV1> {
        if self.serving.is_none() {
            self.serving = self
                .registry
                .subscribe_serving_generation_changes(&self.path)
                .await;
            if self.serving.is_some() {
                return Ok(());
            }
        }
        if self.activity.is_none() {
            self.activity = self.registry.subscribe_owner_activity(&self.path).await;
            if self.activity.is_some() {
                return Ok(());
            }
        }
        tokio::select! {
            changed = self.seats.changed() => changed.map_err(|_| CodeIndexOwnerSignalsClosedV1)?,
            changed = self.root_mounted.changed() => {
                changed.map_err(|_| CodeIndexOwnerSignalsClosedV1)?;
            }
            changed = self.receipts.changed() => {
                changed.map_err(|_| CodeIndexOwnerSignalsClosedV1)?;
            }
            published = Self::root_published(&mut self.publications, &self.path) => published?,
            changed = async {
                match self.serving.as_mut() {
                    Some(serving) => serving.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() {
                    self.serving = None;
                }
            }
            changed = async {
                match self.activity.as_mut() {
                    Some(activity) => activity.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                if changed.is_err() {
                    self.activity = None;
                }
            }
        }
        self.settle_burst().await;
        Ok(())
    }

    /// Whether the worktree's worker finished its last pass, graph tail
    /// included, and is back at a wait.
    fn owner_settled(&self) -> bool {
        self.activity
            .as_ref()
            .is_some_and(CodeIndexOwnerActivityV1::pass_finished)
    }

    /// Consume publications until a scheduler turn passes without one.
    async fn settle_burst(&mut self) {
        loop {
            self.seats.borrow_and_update();
            self.root_mounted.borrow_and_update();
            self.receipts.borrow_and_update();
            while self.publications.try_recv().is_ok() {}
            if let Some(serving) = self.serving.as_mut() {
                serving.borrow_and_update();
            }
            tokio::task::yield_now().await;
            let pending = [
                self.seats.has_changed(),
                self.root_mounted.has_changed(),
                self.receipts.has_changed(),
            ]
            .into_iter()
            .any(|changed| changed.unwrap_or(false))
                || !self.publications.is_empty()
                || self
                    .serving
                    .as_ref()
                    .is_some_and(|serving| serving.has_changed().unwrap_or(false))
                || self
                    .activity
                    .as_ref()
                    .is_some_and(CodeIndexOwnerActivityV1::has_changed);
            if !pending {
                return;
            }
            if let Some(activity) = self.activity.as_mut()
                && activity.has_changed()
                && activity.changed().await.is_err()
            {
                self.activity = None;
            }
        }
    }
}

impl CodeIndexSchedulerRegistryV1 {
    /// The one wait for a worktree's seat, whatever the seat is: a decoded
    /// generation, a current text owner, text serving under a query
    /// authority, or a readiness target.
    ///
    /// It subscribes to the root's signals before the first `probe`, so a
    /// seat installed between a probe and the wait still wakes it, and it
    /// re-probes only when the registry publishes a change. `probe` is told
    /// whether the worker finished its last pass, graph tail included, and
    /// answers `Some` once the wait is over and `None` while the seat is still
    /// being installed. Between probes the worker's own state ends the wait the same
    /// way for every reader: a worktree shutting down is `Cancelled`, and a
    /// parked worker installs no seat, so its park is the answer once the
    /// worker is back at a wait. A park the worker re-checks on every wake
    /// answers only after a pass since the wait began observed it again (each
    /// observation rewrites it). An unmounted root is waited through; the
    /// probe decides whether that ends the wait. Dropping the future cancels
    /// the wait.
    pub(crate) async fn wait_for_seat<T, E, Probe>(
        &self,
        project_root: &Path,
        deadline: tokio::time::Instant,
        mut probe: impl FnMut(bool) -> Probe,
    ) -> Result<CodeIndexSeatWaitV1<T>, E>
    where
        Probe: Future<Output = Result<Option<CodeIndexSeatWaitV1<T>>, E>>,
    {
        let mut signals = CodeIndexOwnerSignalsV1::subscribe(self, project_root).await;
        let park_at_start = self.convergence_park(project_root).await;
        loop {
            if let Some(ended) = probe(signals.owner_settled()).await? {
                return Ok(ended);
            }
            if let Some(ended) = self
                .seat_worker_end(project_root, &signals, park_at_start.as_ref())
                .await
            {
                return Ok(ended);
            }
            match tokio::time::timeout_at(deadline, signals.changed()).await {
                Err(_) => return Ok(CodeIndexSeatWaitV1::Deadline),
                Ok(Err(CodeIndexOwnerSignalsClosedV1)) => {
                    return Ok(CodeIndexSeatWaitV1::Cancelled);
                }
                Ok(Ok(())) => {}
            }
        }
    }

    /// The worker state that ends every seat wait; `None` while the worker
    /// may still install a seat.
    async fn seat_worker_end<T>(
        &self,
        project_root: &Path,
        signals: &CodeIndexOwnerSignalsV1,
        park_at_start: Option<&CodeIndexConvergenceParkedV1>,
    ) -> Option<CodeIndexSeatWaitV1<T>> {
        let canonical = canonical_existing_identity(project_root).ok()?;
        let (park, shutting_down) = {
            let mounted = self.mounted.lock().await;
            let worktree = mounted.get(&canonical)?;
            (
                worktree
                    .convergence_park
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
                worktree.shutting_down.load(Ordering::Acquire),
            )
        };
        if shutting_down {
            return Some(CodeIndexSeatWaitV1::Cancelled);
        }
        let parked = park?;
        (signals.owner_settled() && (!parked.retries_on_wake || park_at_start != Some(&parked)))
            .then(|| CodeIndexSeatWaitV1::Parked(CodeIndexSeatParkV1::Convergence(parked)))
    }

    /// Sweep the source witness now and post a wake for any proven change,
    /// so later freshness reads describe the source as of this call. The
    /// sweep reads only the freshness fence, never the scheduler mutex, so a
    /// pass in flight cannot delay it. An unmounted root has nothing to
    /// sweep; its mount reconciles. Without `sweep_source` only the
    /// publication park is checked.
    async fn request_fresh_now(
        &self,
        project_root: &Path,
        sweep_source: bool,
    ) -> Result<(), CodeIndexFreshSweepRefusedV1> {
        let Ok(canonical) = canonical_existing_identity(project_root) else {
            return Ok(());
        };
        let (source_freshness, shutting_down, hints, epoch, pending_wake, wake) = {
            let mounted = self.mounted.lock().await;
            let Some(worktree) = mounted.get(&canonical) else {
                return Ok(());
            };
            if Self::publication_authority_reset(worktree).is_some() {
                return Err(CodeIndexFreshSweepRefusedV1::PublicationParked);
            }
            if !sweep_source {
                return Ok(());
            }
            (
                worktree.source_freshness.clone(),
                Arc::clone(&worktree.shutting_down),
                Arc::clone(&worktree.hints),
                Arc::clone(&worktree.epoch),
                Arc::clone(&worktree.pending_wake),
                Arc::clone(&worktree.wake),
            )
        };
        tokio::task::spawn_blocking(move || {
            match source_freshness.ladder_verdict(&canonical, &shutting_down, None) {
                FreshnessProbeVerdictV1::Current => return,
                FreshnessProbeVerdictV1::Unverified => {}
                FreshnessProbeVerdictV1::Moved => {
                    CodeIndexWorktreeSchedulerV1::record_background_reconcile_hint(
                        &mut hints.lock().unwrap_or_else(PoisonError::into_inner),
                        &epoch,
                        true,
                    );
                }
            }
            Self::note_wake(
                &pending_wake,
                &wake,
                CodeIndexCadenceTriggerV1::QueryAdmission,
            );
        })
        .await
        .map_err(|_| CodeIndexFreshSweepRefusedV1::SweepFailed)
    }

    /// The retained text owner for `scope` once its source proof admits it.
    ///
    /// A settled worktree's proof lapses whenever Git metadata moves or a hook
    /// hint advances the source epoch; the read that notices posts the
    /// re-verification wake, and the worker renews the proof without a
    /// rebuild when the digests still match. Waiting on the registry's
    /// publications for that renewal keeps a read from answering unavailable
    /// in the window. Returns `None` when the scope has no mounted text owner,
    /// the wait ends without one, or `deadline` passes.
    pub(crate) async fn current_text_owner_for_scope(
        &self,
        scope: &ResolvedScope,
        deadline: tokio::time::Instant,
    ) -> Option<LatestCodeTextGenerationV1> {
        let root = {
            let mounted = self.mounted.lock().await;
            unique_mounted_for_scope(&mounted, scope)
                .unique()?
                .0
                .clone()
        };
        let Ok(waited) = self
            .wait_for_seat(&root, deadline, |_| async move {
                Ok::<_, Infallible>(
                    match self.retained_text_owner_freshness_for_scope(scope).await {
                        None => Some(CodeIndexSeatWaitV1::Parked(
                            CodeIndexSeatParkV1::Unpublished,
                        )),
                        Some((latest, true)) => Some(CodeIndexSeatWaitV1::Seated(latest)),
                        Some((_, false)) => None,
                    },
                )
            })
            .await;
        match waited {
            CodeIndexSeatWaitV1::Seated(latest) => Some(latest),
            CodeIndexSeatWaitV1::Parked(_)
            | CodeIndexSeatWaitV1::Cancelled
            | CodeIndexSeatWaitV1::Deadline => None,
        }
    }

    /// Wait until `project_root` reaches `target`, re-reading freshness only
    /// when the registry publishes a change, for at most `budget`.
    ///
    /// `fresh` means verified against the source as of the request: the wait
    /// first sweeps the source witness, which either refreshes the verified
    /// watermark or posts the wake for a proven change, so a reading taken
    /// after it cannot report an edit no hook announced as fresh. The sweep
    /// stats files against digests earlier sweeps proved and reads bytes
    /// only where a stat moved, without the scheduler mutex. `ready` and
    /// `graph_ready` accept a reading that already satisfies them, the answer
    /// a plain status read gives; a pending `ready` still sweeps first.
    /// `fresh` and `ready` are reached only once the worker has also finished
    /// the pass behind that reading: its graph tail seats the decoded
    /// generation and binds the source proof to that seat under a counted
    /// step, so a wait that ended before the tail was followed by reads
    /// reporting `verifying` for the generation it had just reported fresh.
    /// An unmounted root is waited through: a mount that lands inside the
    /// budget reconciles the source as of that mount. Dropping the future
    /// abandons the wait; a wake the sweep posted is ordinary demand.
    pub async fn wait_for_readiness(
        &self,
        project_root: &Path,
        target: CodeIndexReadinessTargetV1,
        budget: Duration,
    ) -> Result<CodeIndexReadinessWaitReadV1, CodeIndexFreshnessReadFailureV1> {
        let deadline = tokio::time::Instant::now() + budget;
        if target != CodeIndexReadinessTargetV1::Fresh
            && let Some(reading) = self
                .dashboard_freshness_read(project_root)
                .await?
                .filter(|freshness| freshness.readiness(target) == CodeIndexReadinessV1::Reached)
            && (target == CodeIndexReadinessTargetV1::GraphReady
                || self
                    .subscribe_owner_activity(project_root)
                    .await
                    .is_some_and(|activity| activity.pass_finished()))
        {
            return Ok(CodeIndexReadinessWaitReadV1::Reached {
                reading: Box::new(reading),
            });
        }
        // The caller's budget bounds the sweep, and an unproven source cannot
        // be reported as reached.
        let sweep_source = target != CodeIndexReadinessTargetV1::GraphReady;
        match tokio::time::timeout_at(deadline, self.request_fresh_now(project_root, sweep_source))
            .await
        {
            Ok(Err(CodeIndexFreshSweepRefusedV1::PublicationParked)) => {
                return Ok(CodeIndexReadinessWaitReadV1::Unreachable {
                    reason: "code_index_publication_parked".to_owned(),
                });
            }
            Ok(Err(CodeIndexFreshSweepRefusedV1::SweepFailed)) => {
                return Ok(CodeIndexReadinessWaitReadV1::Unreachable {
                    reason: "code_index_freshness_sweep_failed".to_owned(),
                });
            }
            Ok(Ok(())) => {}
            Err(_) => {
                return Ok(CodeIndexReadinessWaitReadV1::TimedOut {
                    last: self
                        .dashboard_freshness_read(project_root)
                        .await?
                        .map(Box::new),
                });
            }
        }
        let waited = self
            .wait_for_seat(project_root, deadline, |owner_settled| async move {
                let Some(freshness) = self.dashboard_freshness_read(project_root).await? else {
                    return Ok(None);
                };
                Ok(match freshness.readiness(target) {
                    CodeIndexReadinessV1::Reached
                        if owner_settled || target == CodeIndexReadinessTargetV1::GraphReady =>
                    {
                        Some(CodeIndexSeatWaitV1::Seated(freshness))
                    }
                    CodeIndexReadinessV1::Reached | CodeIndexReadinessV1::Pending => None,
                    CodeIndexReadinessV1::Unreachable { reason } => Some(
                        CodeIndexSeatWaitV1::Parked(CodeIndexSeatParkV1::Unreachable(reason)),
                    ),
                })
            })
            .await?;
        Ok(match waited {
            CodeIndexSeatWaitV1::Seated(reading) => CodeIndexReadinessWaitReadV1::Reached {
                reading: Box::new(reading),
            },
            CodeIndexSeatWaitV1::Parked(park) => CodeIndexReadinessWaitReadV1::Unreachable {
                reason: park.readiness_reason(),
            },
            CodeIndexSeatWaitV1::Cancelled => CodeIndexReadinessWaitReadV1::Unreachable {
                reason: "code_index_scheduler_registry_closed".to_owned(),
            },
            CodeIndexSeatWaitV1::Deadline => CodeIndexReadinessWaitReadV1::TimedOut {
                last: self
                    .dashboard_freshness_read(project_root)
                    .await?
                    .map(Box::new),
            },
        })
    }

    /// Wait, for at most `budget`, until the generation a restart retained
    /// for `project_root` serves exact and lexical search for `scope` again.
    ///
    /// A restarted worktree answers search only after its worker reopens the
    /// retained text owners and project open mounts the query authority, and
    /// a search that arrives first would otherwise see no authority at all.
    /// The wait follows the mount, then reads the durable publication pointer
    /// once: without one there is nothing retained to reopen, so a first
    /// index returns at once instead of waiting out its build. Neither input
    /// depends on the code graph, so the wait never covers graph-head
    /// recovery or a graph publication; search reports that lane warming.
    pub async fn wait_for_retained_text_serving(
        &self,
        project_root: &Path,
        scope: &ResolvedScope,
        budget: Duration,
    ) -> CodeIndexSeatWaitV1<()> {
        let published = AtomicBool::new(false);
        let reconcile_requested = AtomicBool::new(false);
        let (published, reconcile_requested) = (&published, &reconcile_requested);
        let Ok(waited) = self
            .wait_for_seat(
                project_root,
                tokio::time::Instant::now() + budget,
                |_| async move {
                    if !published.load(Ordering::Acquire) {
                        match self.has_active_publication(project_root).await {
                            Some(Ok(true)) => published.store(true, Ordering::Release),
                            Some(Ok(false)) => {
                                return Ok(Some(CodeIndexSeatWaitV1::Parked(
                                    CodeIndexSeatParkV1::Unpublished,
                                )));
                            }
                            Some(Err(_)) => {
                                return Ok(Some(CodeIndexSeatWaitV1::Parked(
                                    CodeIndexSeatParkV1::PublicationUnreadable,
                                )));
                            }
                            None => return Ok(None),
                        }
                    }
                    if self.text_serves_search(project_root, scope).await {
                        return Ok(Some(CodeIndexSeatWaitV1::Seated(())));
                    }
                    if !reconcile_requested.swap(true, Ordering::AcqRel) {
                        if let CodeIndexReconcileAdmissionV1::PublicationAuthorityCorrupt(parked) =
                            self.request_query_background_reconcile(scope).await
                        {
                            return Ok(Some(CodeIndexSeatWaitV1::Parked(
                                CodeIndexSeatParkV1::Convergence(parked),
                            )));
                        }
                    }
                    Ok::<_, Infallible>(None)
                },
            )
            .await;
        waited
    }

    /// Whether `scope`'s text owners serve exact and lexical search under a
    /// query authority, its own or one reused from a project peer.
    async fn text_serves_search(&self, project_root: &Path, scope: &ResolvedScope) -> bool {
        self.latest_text_serving_freshness_for_scope(scope)
            .await
            .is_some()
            && (self.query_authority_for_scope(scope).await.is_some()
                || matches!(
                    self.mount_query_authority_from_project_peer(project_root, scope)
                        .await,
                    Ok(true)
                ))
    }

    /// Whether the mounted worktree's durable publication names a sealed
    /// generation; `None` while `project_root` is not mounted.
    async fn has_active_publication(
        &self,
        project_root: &Path,
    ) -> Option<Result<bool, CodeIndexSchedulerErrorV1>> {
        let canonical = canonical_existing_identity(project_root).ok()?;
        let owner = {
            let mounted = self.mounted.lock().await;
            mounted.get(&canonical)?.historical_generation_owner.clone()
        };
        Some(
            tokio::task::spawn_blocking(move || owner.has_active_publication())
                .await
                .unwrap_or_else(|error| {
                    Err(CodeIndexSchedulerErrorV1::Identity(format!(
                        "publication pointer read task failed: {error}"
                    )))
                }),
        )
    }
}
