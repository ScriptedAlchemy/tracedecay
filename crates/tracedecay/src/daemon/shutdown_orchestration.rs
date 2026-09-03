//! One transport-neutral daemon shutdown sequence.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::task::JoinSet;

use super::shutdown_coordination::{
    DrainingGauge, ShutdownOwner, ShutdownOwnerReceipt, ShutdownReceipt, ShutdownStatus,
    prepare_shutdown_owner_phases,
};
use super::store_shutdown::{ShutdownTaskOutcome, ShutdownTaskReceipt};
use super::{
    DAEMON_BACKGROUND_DRAIN_DEADLINE, DAEMON_CLIENT_DRAIN_DEADLINE,
    DAEMON_PROJECT_SERVER_DRAIN_DEADLINE, DAEMON_STORE_CLOSE_RESERVE, DAEMON_TASK_ABORT_DEADLINE,
    DaemonLifecycle, core_lifecycle::DaemonShutdownClaim,
};
use tracedecay_domain::errors::Result;

type ProjectServerShutdownFuture =
    Pin<Box<dyn Future<Output = ShutdownTaskReceipt> + Send + 'static>>;
/// The project-server drain is built as a *factory over its phase deadline*,
/// the same shape `ShutdownOwner::with_deadline` uses. Handing the drain the
/// global deadline instead let it spend every remaining second and starve the
/// store-close phase behind it.
type ProjectServerShutdown =
    Box<dyn FnOnce(tokio::time::Instant) -> ProjectServerShutdownFuture + Send + 'static>;

const SHUTDOWN_COORDINATOR_RECEIPT_GRACE: tokio::time::Duration =
    tokio::time::Duration::from_millis(100);

/// Splits one shutdown deadline into per-phase budgets.
///
/// Each phase deadline is recomputed from the clock when that phase starts,
/// so unspent budget flows forward; each is additionally floored so it cannot
/// eat the reserve belonging to the phases behind it.
#[derive(Clone, Copy)]
struct DaemonShutdownBudget {
    overall: tokio::time::Instant,
}

impl DaemonShutdownBudget {
    fn new(overall: tokio::time::Instant) -> Self {
        Self { overall }
    }

    /// Deadline for a phase capped at `phase_max` that leaves
    /// `downstream_reserve` untouched for the phases that follow it.
    ///
    /// Never returns a deadline before `now`: an overrunning predecessor
    /// degrades a later phase to an immediate, *named* timeout rather than
    /// deleting it from the sequence entirely.
    fn phase(
        self,
        phase_max: tokio::time::Duration,
        downstream_reserve: tokio::time::Duration,
    ) -> tokio::time::Instant {
        let now = tokio::time::Instant::now();
        let reserved_floor = self
            .overall
            .checked_sub(downstream_reserve)
            .unwrap_or(self.overall);
        let capped = std::cmp::min(now + phase_max, self.overall);
        std::cmp::max(now, std::cmp::min(capped, reserved_floor))
    }

    fn client_drain(self) -> tokio::time::Instant {
        self.phase(
            DAEMON_CLIENT_DRAIN_DEADLINE,
            DAEMON_BACKGROUND_DRAIN_DEADLINE
                + DAEMON_PROJECT_SERVER_DRAIN_DEADLINE
                + DAEMON_STORE_CLOSE_RESERVE,
        )
    }

    fn background_drain(self) -> tokio::time::Instant {
        self.phase(
            DAEMON_BACKGROUND_DRAIN_DEADLINE,
            DAEMON_PROJECT_SERVER_DRAIN_DEADLINE + DAEMON_STORE_CLOSE_RESERVE,
        )
    }

    fn project_servers(self) -> tokio::time::Instant {
        self.phase(
            DAEMON_PROJECT_SERVER_DRAIN_DEADLINE,
            DAEMON_STORE_CLOSE_RESERVE,
        )
    }

    fn store_close(self) -> tokio::time::Instant {
        self.phase(DAEMON_STORE_CLOSE_RESERVE, tokio::time::Duration::ZERO)
    }
}

pub(super) struct DaemonShutdownPlan {
    clients: JoinSet<Result<()>>,
    owner_phases: Vec<Vec<ShutdownOwner>>,
    terminal_owner_phases: Vec<Vec<ShutdownOwner>>,
    project_server_shutdown: ProjectServerShutdown,
}

impl DaemonShutdownPlan {
    pub(super) fn new<ProjectServers, ProjectServersFuture>(
        clients: JoinSet<Result<()>>,
        owner_phases: Vec<Vec<ShutdownOwner>>,
        project_server_shutdown: ProjectServers,
    ) -> Self
    where
        ProjectServers: FnOnce(tokio::time::Instant) -> ProjectServersFuture + Send + 'static,
        ProjectServersFuture: Future<Output = ShutdownTaskReceipt> + Send + 'static,
    {
        Self {
            clients,
            owner_phases,
            terminal_owner_phases: Vec::new(),
            project_server_shutdown: Box::new(move |deadline| {
                Box::pin(project_server_shutdown(deadline))
            }),
        }
    }

    pub(super) fn with_terminal_owner_phases(
        mut self,
        terminal_owner_phases: Vec<Vec<ShutdownOwner>>,
    ) -> Self {
        self.terminal_owner_phases = terminal_owner_phases;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DaemonShutdownReceipt {
    pub(super) in_flight: ShutdownStatus,
    pub(super) clients: ShutdownStatus,
    pub(super) background: ShutdownReceipt,
    pub(super) project_servers: ShutdownTaskReceipt,
}

impl DaemonShutdownReceipt {
    fn coordinator_failed(deadline: tokio::time::Instant, error: String) -> Self {
        // Coordinator-level failures never reach the per-phase counters, so
        // they are recorded here or the waste is invisible to profiling.
        hotpath::gauge!("daemon.shutdown.coordinator.failed_total").inc(1_u64);
        Self {
            in_flight: ShutdownStatus::Failed(error.clone()),
            clients: ShutdownStatus::Failed(error.clone()),
            background: ShutdownReceipt::failed(deadline, "shutdown_coordinator", error.clone()),
            project_servers: ShutdownTaskReceipt::failed("shutdown_coordinator", error),
        }
    }

    fn coordinator_timed_out(deadline: tokio::time::Instant) -> Self {
        hotpath::gauge!("daemon.shutdown.coordinator.timed_out_total").inc(1_u64);
        Self {
            in_flight: ShutdownStatus::TimedOut,
            clients: ShutdownStatus::TimedOut,
            background: ShutdownReceipt::timed_out(deadline, "shutdown_coordinator"),
            project_servers: ShutdownTaskReceipt::timed_out("shutdown_coordinator"),
        }
    }

    pub(super) fn is_retryable(&self) -> bool {
        matches!(self.in_flight, ShutdownStatus::TimedOut)
            || matches!(self.clients, ShutdownStatus::TimedOut)
            || self
                .background
                .owners
                .iter()
                .any(|owner| owner.status == ShutdownStatus::TimedOut)
            || self.project_servers.timed_out_count() > 0
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct DaemonShutdownFailures {
    in_flight: Vec<String>,
    clients: Vec<String>,
    background: Vec<ShutdownOwnerReceipt>,
    project_servers: Vec<ShutdownTaskOutcome>,
}

impl DaemonShutdownFailures {
    fn record(&mut self, receipt: &DaemonShutdownReceipt) {
        record_status_failure(&mut self.in_flight, &receipt.in_flight);
        record_status_failure(&mut self.clients, &receipt.clients);
        for owner in &receipt.background.owners {
            if matches!(owner.status, ShutdownStatus::Failed(_)) && !self.background.contains(owner)
            {
                self.background.push(owner.clone());
            }
        }
        for outcome in &receipt.project_servers.outcomes {
            if matches!(outcome.status, ShutdownStatus::Failed(_))
                && !self.project_servers.contains(outcome)
            {
                self.project_servers.push(outcome.clone());
            }
        }
    }

    fn apply(&self, receipt: &mut DaemonShutdownReceipt) {
        retain_status_failures(&mut receipt.in_flight, &self.in_flight);
        retain_status_failures(&mut receipt.clients, &self.clients);
        receipt.background.retain_failures_from(&self.background);
        receipt
            .project_servers
            .retain_failures_from(&self.project_servers);
    }
}

fn record_status_failure(failures: &mut Vec<String>, status: &ShutdownStatus) {
    if let ShutdownStatus::Failed(error) = status
        && !failures.contains(error)
    {
        failures.push(error.clone());
    }
}

fn retain_status_failures(status: &mut ShutdownStatus, failures: &[String]) {
    if matches!(status, ShutdownStatus::TimedOut) || failures.is_empty() {
        return;
    }
    let mut errors = failures.to_vec();
    if let ShutdownStatus::Failed(error) = status
        && !errors.contains(error)
    {
        errors.push(error.clone());
    }
    *status = ShutdownStatus::Failed(errors.join("; retry failed: "));
}

#[hotpath::measure(label = "daemon.shutdown.coordinate", future = true)]
pub(super) async fn coordinate_daemon_shutdown<Prepare>(
    lifecycle: &DaemonLifecycle,
    shutdown_deadline: tokio::time::Instant,
    prepare: Prepare,
) -> Arc<DaemonShutdownReceipt>
where
    Prepare: Future<Output = DaemonShutdownPlan> + Send + 'static,
{
    if tokio::time::timeout_at(
        shutdown_deadline,
        lifecycle.wait_for_finished_shutdown_coordinator(),
    )
    .await
    .is_err()
    {
        return Arc::new(DaemonShutdownReceipt::coordinator_timed_out(
            shutdown_deadline,
        ));
    }
    lifecycle.join_finished_shutdown_coordinator().await;
    lifecycle.begin_draining();
    let attempt = match lifecycle.claim_shutdown_coordination() {
        DaemonShutdownClaim::Terminal(receipt) => {
            drop(prepare);
            return receipt;
        }
        DaemonShutdownClaim::Wait(attempt) => {
            drop(prepare);
            attempt
        }
        DaemonShutdownClaim::Run { attempt, failures } => {
            let coordinator_lifecycle = lifecycle.clone();
            let runner_lifecycle = lifecycle.clone();
            let coordinator_attempt = Arc::clone(&attempt);
            let mut coordinator_failures = failures.clone();
            let coordinator = async move {
                let mut runner = tokio::spawn(async move {
                    let plan = prepare.await;
                    run_daemon_shutdown(runner_lifecycle, plan, shutdown_deadline).await
                });
                let (mut receipt, runner_needs_join) = tokio::select! {
                    biased;
                    result = &mut runner => {
                        let receipt = match result {
                            Ok(receipt) => receipt,
                            Err(error) => DaemonShutdownReceipt::coordinator_failed(
                                shutdown_deadline,
                                format!("daemon shutdown runner failed: {error}"),
                            ),
                        };
                        (receipt, false)
                    }
                    () = tokio::time::sleep_until(shutdown_deadline) => {
                        runner.abort();
                        (
                            DaemonShutdownReceipt::coordinator_timed_out(shutdown_deadline),
                            true,
                        )
                    }
                };
                coordinator_failures.record(&receipt);
                coordinator_failures.apply(&mut receipt);
                // Publishing the terminal receipt is the shutdown checkpoint:
                // once it lands, every concurrent waiter observes this
                // outcome instead of racing a duplicate shutdown attempt.
                hotpath::measure_block!("daemon.shutdown.checkpoint", {
                    coordinator_lifecycle.finish_shutdown_attempt(
                        &coordinator_attempt,
                        Arc::new(receipt),
                        coordinator_failures,
                    );
                });
                // A timed-out runner may be inside synchronous third-party or
                // filesystem work that cannot observe abort immediately. Keep
                // the coordinator task owned until that runner actually exits;
                // callers already have a typed timeout receipt, and retries
                // remain fenced by wait_for_finished_shutdown_coordinator.
                if runner_needs_join
                    && let Err(error) = runner.await
                    && !error.is_cancelled()
                {
                    tracing::error!(%error, "timed-out daemon shutdown runner failed while joining");
                }
            };
            if !lifecycle.spawn_shutdown_coordinator(&attempt, coordinator) {
                let receipt = Arc::new(DaemonShutdownReceipt::coordinator_failed(
                    shutdown_deadline,
                    "daemon shutdown coordinator ownership was lost".to_owned(),
                ));
                let mut failures = failures;
                failures.record(&receipt);
                lifecycle.finish_shutdown_attempt(&attempt, receipt, failures);
            }
            attempt
        }
    };

    let receipt_deadline = shutdown_deadline + SHUTDOWN_COORDINATOR_RECEIPT_GRACE;
    let receipt = tokio::select! {
        biased;
        result = attempt.wait_for_receipt() => match result {
            Ok(receipt) => receipt,
            Err(error) => Arc::new(DaemonShutdownReceipt::coordinator_failed(
                shutdown_deadline,
                error,
            )),
        },
        () = tokio::time::sleep_until(receipt_deadline) => {
            Arc::new(DaemonShutdownReceipt::coordinator_timed_out(
                shutdown_deadline,
            ))
        }
    };
    let _ = tokio::time::timeout_at(
        receipt_deadline,
        lifecycle.wait_for_finished_shutdown_coordinator(),
    )
    .await;
    lifecycle.join_finished_shutdown_coordinator().await;
    receipt
}

#[hotpath::measure(label = "daemon.shutdown.run", future = true)]
async fn run_daemon_shutdown(
    lifecycle: DaemonLifecycle,
    mut plan: DaemonShutdownPlan,
    shutdown_deadline: tokio::time::Instant,
) -> DaemonShutdownReceipt {
    let budget = DaemonShutdownBudget::new(shutdown_deadline);
    let prepared = prepare_shutdown_owner_phases(plan.owner_phases);
    let mut background_shutdown = Box::pin(prepared.join(budget.background_drain()));
    let mut background_receipt = None;
    let client_drain_deadline = budget.client_drain();
    let in_flight = tokio::time::timeout_at(client_drain_deadline, lifecycle.wait_for_idle());
    tokio::pin!(in_flight);
    // Client drain: wait for in-flight client work to idle out cooperatively,
    // then abort and join whatever remains. One span covers both the
    // cooperative wait and the forced abort/join so its duration reads as a
    // single number against DAEMON_CLIENT_DRAIN_DEADLINE /
    // DAEMON_TASK_ABORT_DEADLINE instead of only surfacing as a bare timeout.
    let (in_flight, clients) = hotpath::measure_block!("daemon.shutdown.client_drain", {
        let _draining = DrainingGauge::arm("daemon.shutdown.draining.clients");
        let in_flight = loop {
            tokio::select! {
                receipt = &mut background_shutdown, if background_receipt.is_none() => {
                    background_receipt = Some(receipt);
                }
                drained = &mut in_flight => {
                    break match drained {
                        Ok(()) => ShutdownStatus::Clean,
                        Err(_) => ShutdownStatus::TimedOut,
                    };
                }
            }
        };

        plan.clients.abort_all();
        let client_join_deadline = std::cmp::min(
            tokio::time::Instant::now() + DAEMON_TASK_ABORT_DEADLINE,
            budget.client_drain(),
        );
        let clients = join_aborted_clients_until(&mut plan.clients, client_join_deadline).await;
        let clients = if tokio::time::timeout_at(client_join_deadline, lifecycle.wait_for_idle())
            .await
            .is_err()
        {
            ShutdownStatus::TimedOut
        } else {
            clients
        };
        (in_flight, clients)
    });
    // Forced vs graceful: graceful means in-flight client work idled out
    // cooperatively before the drain deadline; forced means the deadline
    // expired and the abort/join path did the draining.
    if in_flight.is_clean() {
        hotpath::gauge!("daemon.shutdown.client_drain.graceful_total").inc(1_u64);
    } else {
        hotpath::gauge!("daemon.shutdown.client_drain.forced_total").inc(1_u64);
    }
    // Background-task drain: resolve the non-terminal ShutdownOwner phases
    // (semantic artifact GC, maintenance, session sync, invocation, ...).
    // Often already resolved inside the client-drain select loop above; this
    // span only measures the residual wait when it was not.
    let mut background = hotpath::measure_block!("daemon.shutdown.background_drain", {
        let _draining = DrainingGauge::arm("daemon.shutdown.draining.background");
        match background_receipt {
            Some(receipt) => receipt,
            None => background_shutdown.await,
        }
    });
    // Project servers hold session-database leases whose graph clients keep
    // the session relation graph owners leased. The terminal owner drains and
    // closes those graph runtimes, so it must run only after every server has
    // dropped its leases.
    let project_server_deadline = budget.project_servers();
    let project_servers = hotpath::measure_block!("daemon.shutdown.project_servers", {
        let _draining = DrainingGauge::arm("daemon.shutdown.draining.project_servers");
        // The drain gets its phase deadline, so it reaches its own bounded
        // join and reports one outcome *per named server*. The outer sleep is
        // only the backstop for a drain that ignores its deadline; it fires
        // late enough that the named receipt normally wins the race.
        let mut drain = (plan.project_server_shutdown)(project_server_deadline);
        tokio::select! {
            biased;
            receipt = &mut drain => receipt,
            () = tokio::time::sleep_until(
                project_server_deadline + DAEMON_TASK_ABORT_DEADLINE,
            ) => ShutdownTaskReceipt::timed_out("project_server_shutdown"),
        }
    });
    // Store close: the terminal owner phase (memory_graph_reconciliation)
    // drains retained graph owners and closes their Grafeo runtimes. This is
    // the outer view of the close; daemon.branch_admin.close_graph_runtimes
    // and graph_db.registry.close_retained measure the work underneath it.
    let store_close_deadline = budget.store_close();
    let terminal = hotpath::measure_block!("daemon.shutdown.store_close", {
        let _draining = DrainingGauge::arm("daemon.shutdown.draining.store_close");
        if project_servers.timed_out_count() == 0 {
            prepare_shutdown_owner_phases(plan.terminal_owner_phases)
                .join(store_close_deadline)
                .await
        } else {
            ShutdownReceipt::timed_out(store_close_deadline, "memory_graph_reconciliation")
        }
    });
    background.extend(terminal);
    let receipt = DaemonShutdownReceipt {
        in_flight,
        clients,
        background,
        project_servers,
    };
    // Graceful means every lane drained cooperatively inside its budget;
    // anything else — a timed-out owner, a forced client abort, a failed or
    // timed-out project server — makes this attempt a forced shutdown.
    if receipt.in_flight.is_clean()
        && receipt.clients.is_clean()
        && receipt.background.unfinished().is_empty()
        && receipt.project_servers.is_clean()
    {
        hotpath::gauge!("daemon.shutdown.outcome.graceful_total").inc(1_u64);
    } else {
        hotpath::gauge!("daemon.shutdown.outcome.forced_total").inc(1_u64);
    }
    receipt
}

#[hotpath::measure(label = "daemon.shutdown.join_aborted_clients", future = true)]
async fn join_aborted_clients_until(
    clients: &mut JoinSet<Result<()>>,
    deadline: tokio::time::Instant,
) -> ShutdownStatus {
    match tokio::time::timeout_at(deadline, async {
        let mut failures = Vec::new();
        while let Some(completed) = clients.join_next().await {
            match completed {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error.to_string()),
                Err(error) if error.is_cancelled() => {}
                Err(error) => failures.push(error.to_string()),
            }
        }
        failures
    })
    .await
    {
        Err(_) => ShutdownStatus::TimedOut,
        Ok(failures) if failures.is_empty() => ShutdownStatus::Clean,
        Ok(failures) => ShutdownStatus::Failed(failures.join("; ")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE;

    /// A phase that never finishes used to spend the whole global deadline,
    /// so the phases behind it never ran: the project-server drain reported an
    /// instant timeout, store-close was skipped on the back of that timeout,
    /// and the coordinator's own deadline fired and replaced every receipt
    /// with an anonymous `shutdown_coordinator` entry. With per-phase budgets
    /// the stuck owner is bounded, is named in its own receipt, and the phases
    /// behind it still get their reserved budget.
    #[tokio::test(start_paused = true)]
    async fn a_stuck_background_owner_cannot_starve_project_servers_or_store_close() {
        let lifecycle = DaemonLifecycle::default();
        let overall = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE;
        let observed_deadline = Arc::new(std::sync::Mutex::new(None));
        let project_server_deadline = Arc::clone(&observed_deadline);
        let store_closed = Arc::new(AtomicBool::new(false));
        let store_closed_by_owner = Arc::clone(&store_closed);

        let receipt = coordinate_daemon_shutdown(&lifecycle, overall, async move {
            DaemonShutdownPlan::new(
                JoinSet::new(),
                vec![vec![ShutdownOwner::new(
                    "stuck_owner",
                    || {},
                    std::future::pending(),
                )]],
                move |deadline| async move {
                    *project_server_deadline
                        .lock()
                        .expect("project server deadline") = Some(deadline);
                    ShutdownTaskReceipt::default()
                },
            )
            .with_terminal_owner_phases(vec![vec![ShutdownOwner::new(
                "memory_graph_reconciliation",
                move || store_closed_by_owner.store(true, Ordering::Release),
                async {},
            )]])
        })
        .await;

        // The stuck owner is bounded by the background budget and keeps its
        // own identity instead of degrading to `shutdown_coordinator`.
        assert_eq!(receipt.background.unfinished(), &["stuck_owner"]);
        assert!(matches!(
            receipt.background.owners.as_slice(),
            [stuck, terminal]
                if stuck.name == "stuck_owner"
                    && stuck.status == ShutdownStatus::TimedOut
                    && terminal.name == "memory_graph_reconciliation"
                    && terminal.status == ShutdownStatus::Clean
        ));
        // The project-server drain ran, and its deadline left the store-close
        // reserve intact.
        let project_server_deadline = observed_deadline
            .lock()
            .expect("project server deadline")
            .expect("project server drain ran");
        assert!(
            project_server_deadline <= overall - DAEMON_STORE_CLOSE_RESERVE,
            "project-server phase must not encroach on the store-close reserve"
        );
        assert!(receipt.project_servers.is_clean());
        // Store close is the durability obligation: it must still run.
        assert!(
            store_closed.load(Ordering::Acquire),
            "store close must still run behind a stuck background owner"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn phase_budgets_flow_forward_but_reserve_the_phases_behind_them() {
        let overall = tokio::time::Instant::now() + DAEMON_SHUTDOWN_DEADLINE;
        let budget = DaemonShutdownBudget::new(overall);

        // A phase is capped at its own maximum, not the global deadline.
        assert_eq!(
            budget.background_drain(),
            tokio::time::Instant::now() + DAEMON_BACKGROUND_DRAIN_DEADLINE
        );
        // Unspent budget flows forward: recomputed from the clock, a phase
        // that starts later still gets its full cap while it fits.
        tokio::time::advance(tokio::time::Duration::from_secs(5)).await;
        assert_eq!(
            budget.project_servers(),
            tokio::time::Instant::now() + DAEMON_PROJECT_SERVER_DRAIN_DEADLINE
        );
        // Late in the budget the reserve floor wins over the phase cap, so the
        // store-close phase keeps its slice instead of being consumed.
        tokio::time::advance(tokio::time::Duration::from_secs(20)).await;
        assert_eq!(
            budget.project_servers(),
            overall - DAEMON_STORE_CLOSE_RESERVE
        );
        // Past the reserve boundary an overrun phase still gets a deadline it
        // can report a named timeout against, rather than being dropped from
        // the sequence — and store close still gets the rest.
        tokio::time::advance(tokio::time::Duration::from_secs(15)).await;
        assert_eq!(budget.project_servers(), tokio::time::Instant::now());
        assert_eq!(budget.store_close(), overall);
    }

    #[tokio::test]
    async fn terminal_owner_waits_for_admitted_commit_to_drain() {
        let lifecycle = DaemonLifecycle::default();
        let activity = lifecycle.try_enter().expect("admitted commit activity");
        lifecycle.begin_draining();
        let terminal_cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&terminal_cancelled);
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        let shutdown_lifecycle = lifecycle.clone();
        let shutdown = tokio::spawn(async move {
            coordinate_daemon_shutdown(&shutdown_lifecycle, deadline, async move {
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                    ShutdownTaskReceipt::default()
                })
                .with_terminal_owner_phases(vec![vec![ShutdownOwner::new(
                    "memory_graph_reconciliation",
                    move || {
                        task_cancelled.store(true, Ordering::Release);
                    },
                    async {},
                )]])
            })
            .await
        });

        tokio::task::yield_now().await;
        assert!(!terminal_cancelled.load(Ordering::Acquire));
        drop(activity);
        let receipt = shutdown.await.expect("terminal shutdown receipt");
        assert!(terminal_cancelled.load(Ordering::Acquire));
        assert!(receipt.background.unfinished().is_empty());
    }

    #[tokio::test]
    async fn terminal_owner_waits_for_timed_out_project_server_lease_release() {
        let lifecycle = DaemonLifecycle::default();
        let terminal_started = Arc::new(AtomicBool::new(false));
        let terminal_started_by_owner = Arc::clone(&terminal_started);
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                ShutdownTaskReceipt::timed_out("project_server[0]")
            })
            .with_terminal_owner_phases(vec![vec![ShutdownOwner::new(
                "memory_graph_reconciliation",
                move || terminal_started_by_owner.store(true, Ordering::Release),
                async {},
            )]])
        })
        .await;

        assert!(!terminal_started.load(Ordering::Acquire));
        assert!(matches!(
            receipt.background.owners.as_slice(),
            [owner]
                if owner.name == "memory_graph_reconciliation"
                    && owner.status == ShutdownStatus::TimedOut
        ));
        assert_eq!(receipt.project_servers.timed_out_count(), 1);
    }

    #[tokio::test]
    async fn cancelled_first_waiter_does_not_duplicate_shutdown_ownership() {
        let lifecycle = DaemonLifecycle::default();
        let cancellations = Arc::new(AtomicUsize::new(0));
        let server_shutdowns = Arc::new(AtomicUsize::new(0));
        let duplicate_prepares = Arc::new(AtomicUsize::new(0));
        let owner_cancelled = Arc::new(tokio::sync::Notify::new());
        let release_owner = Arc::new(tokio::sync::Notify::new());
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

        let first_lifecycle = lifecycle.clone();
        let first_cancellations = Arc::clone(&cancellations);
        let first_server_shutdowns = Arc::clone(&server_shutdowns);
        let cancelled_signal = Arc::clone(&owner_cancelled);
        let owner_release = Arc::clone(&release_owner);
        let first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, deadline, async move {
                DaemonShutdownPlan::new(
                    JoinSet::new(),
                    vec![vec![ShutdownOwner::new(
                        "owner",
                        move || {
                            first_cancellations.fetch_add(1, Ordering::AcqRel);
                            cancelled_signal.notify_one();
                        },
                        async move { owner_release.notified().await },
                    )]],
                    move |_| async move {
                        first_server_shutdowns.fetch_add(1, Ordering::AcqRel);
                        ShutdownTaskReceipt::default()
                    },
                )
            })
            .await
        });
        owner_cancelled.notified().await;
        first.abort();
        assert!(
            first
                .await
                .expect_err("first waiter cancelled")
                .is_cancelled()
        );

        let second_lifecycle = lifecycle.clone();
        let second_duplicate_prepares = Arc::clone(&duplicate_prepares);
        let second = tokio::spawn(async move {
            coordinate_daemon_shutdown(&second_lifecycle, deadline, async move {
                second_duplicate_prepares.fetch_add(1, Ordering::AcqRel);
                panic!("duplicate shutdown prepare future was polled");
            })
            .await
        });
        release_owner.notify_one();
        let receipt = second.await.expect("second shutdown waiter");
        let duplicate_prepares_after_terminal = Arc::clone(&duplicate_prepares);
        let subsequent = coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            duplicate_prepares_after_terminal.fetch_add(1, Ordering::AcqRel);
            panic!("subsequent shutdown prepare future was polled");
        })
        .await;

        assert!(Arc::ptr_eq(&receipt, &subsequent));
        assert_eq!(cancellations.load(Ordering::Acquire), 1);
        assert_eq!(server_shutdowns.load(Ordering::Acquire), 1);
        assert_eq!(duplicate_prepares.load(Ordering::Acquire), 0);
        assert_eq!(receipt.in_flight, ShutdownStatus::Clean);
        assert_eq!(receipt.clients, ShutdownStatus::Clean);
        assert!(receipt.background.unfinished().is_empty());
        assert!(receipt.project_servers.is_clean());
    }

    #[tokio::test]
    async fn client_error_is_preserved_in_terminal_receipt() {
        let lifecycle = DaemonLifecycle::default();
        let mut clients = JoinSet::new();
        clients.spawn(async {
            Err(TraceDecayError::Config {
                message: "client failed during shutdown".to_owned(),
            })
        });
        tokio::task::yield_now().await;
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            DaemonShutdownPlan::new(clients, Vec::new(), |_| async {
                ShutdownTaskReceipt::default()
            })
        })
        .await;

        assert_eq!(
            receipt.clients,
            ShutdownStatus::Failed("config error: client failed during shutdown".to_owned())
        );
        assert!(receipt.project_servers.is_clean());
    }

    #[tokio::test]
    async fn coordinator_panic_becomes_shared_terminal_failure() {
        let lifecycle = DaemonLifecycle::default();
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async {
            DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                panic!("server shutdown panic");
            })
        })
        .await;
        let subsequent = coordinate_daemon_shutdown(&lifecycle, deadline, async {
            panic!("duplicate prepare");
        })
        .await;

        assert!(Arc::ptr_eq(&receipt, &subsequent));
        for status in [&receipt.in_flight, &receipt.clients] {
            assert!(
                matches!(status, ShutdownStatus::Failed(error) if error.contains("server shutdown panic"))
            );
        }
        assert!(!receipt.background.unfinished().is_empty());
        assert!(!receipt.project_servers.is_clean());
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_shutdown_receipt_allows_one_non_overlapping_retry() {
        let lifecycle = DaemonLifecycle::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let first_attempts = Arc::clone(&attempts);
        let first_cancellations = Arc::clone(&cancellations);
        let first_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let first = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                coordinate_daemon_shutdown(&lifecycle, first_deadline, async move {
                    first_attempts.fetch_add(1, Ordering::AcqRel);
                    DaemonShutdownPlan::new(
                        JoinSet::new(),
                        vec![vec![ShutdownOwner::new(
                            "uncooperative_owner",
                            move || {
                                first_cancellations.fetch_add(1, Ordering::AcqRel);
                            },
                            std::future::pending(),
                        )]],
                        |_| async { ShutdownTaskReceipt::default() },
                    )
                })
                .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(tokio::time::Duration::from_secs(1)).await;
        let first = first.await.expect("timed-out shutdown attempt");
        assert_eq!(first.background.owners[0].status, ShutdownStatus::TimedOut);

        let retry_attempts = Arc::clone(&attempts);
        let retry_cancellations = Arc::clone(&cancellations);
        let retry_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let retry = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async move {
            retry_attempts.fetch_add(1, Ordering::AcqRel);
            DaemonShutdownPlan::new(
                JoinSet::new(),
                vec![vec![ShutdownOwner::new(
                    "cooperative_owner",
                    move || {
                        retry_cancellations.fetch_add(1, Ordering::AcqRel);
                    },
                    async {},
                )]],
                |_| async { ShutdownTaskReceipt::default() },
            )
        })
        .await;
        let duplicate_attempts = Arc::clone(&attempts);
        let duplicate = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async move {
            duplicate_attempts.fetch_add(1, Ordering::AcqRel);
            panic!("terminal receipt must not prepare a duplicate shutdown");
        })
        .await;

        assert!(!Arc::ptr_eq(&first, &retry));
        assert!(Arc::ptr_eq(&retry, &duplicate));
        assert_eq!(attempts.load(Ordering::Acquire), 2);
        assert_eq!(cancellations.load(Ordering::Acquire), 2);
        assert!(retry.background.unfinished().is_empty());
        assert!(retry.project_servers.is_clean());
    }

    #[tokio::test(start_paused = true)]
    async fn retry_preserves_a_typed_failure_from_an_earlier_timed_out_attempt() {
        let lifecycle = DaemonLifecycle::default();
        let first_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let first = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                coordinate_daemon_shutdown(&lifecycle, first_deadline, async {
                    DaemonShutdownPlan::new(
                        JoinSet::new(),
                        vec![vec![
                            ShutdownOwner::with_deadline_result(
                                "failed_owner",
                                || {},
                                |_| async { Err::<(), _>("typed shutdown failure") },
                            ),
                            ShutdownOwner::new("timed_out_owner", || {}, std::future::pending()),
                        ]],
                        |_| async { ShutdownTaskReceipt::default() },
                    )
                })
                .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(tokio::time::Duration::from_secs(1)).await;
        let first = first.await.expect("first shutdown attempt");

        assert!(first.is_retryable());
        assert!(matches!(
            first.background.owners.as_slice(),
            [failed, timed_out]
                if failed.name == "failed_owner"
                    && failed.status
                        == ShutdownStatus::Failed("typed shutdown failure".to_owned())
                    && timed_out.name == "timed_out_owner"
                    && timed_out.status == ShutdownStatus::TimedOut
        ));

        let retry = coordinate_daemon_shutdown(
            &lifecycle,
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(1),
            async {
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                    ShutdownTaskReceipt::default()
                })
            },
        )
        .await;

        assert!(!retry.is_retryable());
        assert!(matches!(
            retry.background.owners.as_slice(),
            [failed]
                if failed.name == "failed_owner"
                    && failed.status
                        == ShutdownStatus::Failed("typed shutdown failure".to_owned())
        ));
        assert_eq!(retry.background.unfinished(), &["failed_owner"]);
    }

    #[tokio::test]
    async fn mixed_project_server_failure_and_timeout_retries_without_losing_failure() {
        let lifecycle = DaemonLifecycle::default();
        let mut first_project_servers =
            ShutdownTaskReceipt::failed("failed_server", "typed server failure");
        first_project_servers.extend(ShutdownTaskReceipt::timed_out("timed_out_server"));
        let first = coordinate_daemon_shutdown(
            &lifecycle,
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(1),
            async move {
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), move |_| async move {
                    first_project_servers
                })
            },
        )
        .await;

        assert!(first.is_retryable());
        let retry = coordinate_daemon_shutdown(
            &lifecycle,
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(1),
            async {
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                    ShutdownTaskReceipt::default()
                })
            },
        )
        .await;

        assert!(!retry.is_retryable());
        assert_eq!(
            retry.project_servers.status(),
            ShutdownStatus::Failed("failed_server: typed server failure".to_owned())
        );
        assert!(matches!(
            retry.project_servers.outcomes.as_slice(),
            [failed]
                if failed.owner == "failed_server"
                    && failed.status
                        == ShutdownStatus::Failed("typed server failure".to_owned())
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn contended_prepare_lock_times_out_with_retryable_receipt() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);

        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let lifecycle = DaemonLifecycle::default();
        let prepare_lock = Arc::new(tokio::sync::Mutex::new(()));
        let held_lock = prepare_lock.lock().await;
        let prepare_started = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);

        let first_lifecycle = lifecycle.clone();
        let first_lock = Arc::clone(&prepare_lock);
        let first_started = Arc::clone(&prepare_started);
        let first_dropped = Arc::clone(&dropped);
        let first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, deadline, async move {
                let _dropped = Dropped(first_dropped);
                first_started.notify_one();
                let _lock = first_lock.lock().await;
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                    ShutdownTaskReceipt::default()
                })
            })
            .await
        });
        prepare_started.notified().await;

        tokio::time::advance(tokio::time::Duration::from_secs(1)).await;
        let receipt = first.await.expect("shutdown prepare timeout receipt");

        assert!(receipt.is_retryable());
        assert_eq!(receipt.in_flight, ShutdownStatus::TimedOut);
        assert_eq!(receipt.clients, ShutdownStatus::TimedOut);
        assert!(matches!(
            receipt.background.owners.as_slice(),
            [owner] if owner.name == "shutdown_coordinator"
                && owner.status == ShutdownStatus::TimedOut
        ));
        assert_eq!(receipt.project_servers.status(), ShutdownStatus::TimedOut,);
        assert!(dropped.load(Ordering::Acquire));

        drop(held_lock);
        let retry_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let retry = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async {
            DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                ShutdownTaskReceipt::default()
            })
        })
        .await;

        assert!(!retry.is_retryable());
    }

    #[tokio::test]
    async fn prepare_panic_becomes_typed_coordinator_failure() {
        struct PanickingPrepare;

        impl Future for PanickingPrepare {
            type Output = DaemonShutdownPlan;

            fn poll(
                self: std::pin::Pin<&mut Self>,
                _context: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                panic!("shutdown prepare panic probe");
            }
        }

        let lifecycle = DaemonLifecycle::default();
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
        let shutdown_lifecycle = lifecycle.clone();
        let receipt = tokio::spawn(async move {
            coordinate_daemon_shutdown(&shutdown_lifecycle, deadline, PanickingPrepare).await
        })
        .await
        .expect("typed coordinator receipt");

        assert!(matches!(
            &receipt.in_flight,
            ShutdownStatus::Failed(error)
                if error.contains("daemon shutdown runner failed")
                    && error.contains("shutdown prepare panic probe")
        ));
        assert_eq!(receipt.clients, receipt.in_flight);
        assert!(matches!(
            receipt.background.owners.as_slice(),
            [owner]
                if owner.name == "shutdown_coordinator"
                    && owner.status == receipt.in_flight
        ));
        assert!(matches!(
            receipt.project_servers.status(),
            ShutdownStatus::Failed(error)
                if error.contains("shutdown_coordinator")
                    && error.contains("shutdown prepare panic probe")
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn timed_out_blocking_runner_stays_owned_until_it_exits() {
        struct BlockingProjectServers {
            entered: Option<tokio::sync::oneshot::Sender<()>>,
            release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        }

        impl Future for BlockingProjectServers {
            type Output = ShutdownTaskReceipt;

            fn poll(
                mut self: Pin<&mut Self>,
                _context: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Self::Output> {
                if let Some(entered) = self.entered.take() {
                    let _ = entered.send(());
                }
                let (released, changed) = &*self.release;
                let mut released = released
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                while !*released {
                    released = changed
                        .wait(released)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                std::task::Poll::Ready(ShutdownTaskReceipt::default())
            }
        }

        fn release_runner(release: &Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>) {
            let (released, changed) = &**release;
            *released
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            changed.notify_all();
        }

        let lifecycle = DaemonLifecycle::default();
        let prepares = Arc::new(AtomicUsize::new(0));
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let first_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
        let first_lifecycle = lifecycle.clone();
        let first_prepares = Arc::clone(&prepares);
        let first_release = Arc::clone(&release);
        let mut first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, first_deadline, async move {
                first_prepares.fetch_add(1, Ordering::AcqRel);
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), move |_| {
                    BlockingProjectServers {
                        entered: Some(entered_tx),
                        release: first_release,
                    }
                })
            })
            .await
        });
        entered_rx.await.expect("shutdown runner entered");

        let first_receipt =
            match tokio::time::timeout(tokio::time::Duration::from_secs(2), &mut first).await {
                Ok(joined) => joined.expect("first shutdown coordinator"),
                Err(_) => {
                    release_runner(&release);
                    let _ = first.await;
                    panic!("timed-out shutdown did not publish before runner release");
                }
            };
        assert!(first_receipt.is_retryable());
        assert_eq!(first_receipt.in_flight, ShutdownStatus::TimedOut);

        let second_prepares = Arc::clone(&prepares);
        let second_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(100);
        let second = coordinate_daemon_shutdown(&lifecycle, second_deadline, async move {
            second_prepares.fetch_add(1, Ordering::AcqRel);
            DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                ShutdownTaskReceipt::default()
            })
        })
        .await;
        assert!(second.is_retryable());
        assert_eq!(second.in_flight, ShutdownStatus::TimedOut);
        assert_eq!(prepares.load(Ordering::Acquire), 1);

        release_runner(&release);
        tokio::time::timeout(
            tokio::time::Duration::from_secs(2),
            lifecycle.wait_for_finished_shutdown_coordinator(),
        )
        .await
        .expect("timed-out runner ownership released");
        lifecycle.join_finished_shutdown_coordinator().await;

        let retry_prepares = Arc::clone(&prepares);
        let retry = coordinate_daemon_shutdown(
            &lifecycle,
            tokio::time::Instant::now() + tokio::time::Duration::from_secs(1),
            async move {
                retry_prepares.fetch_add(1, Ordering::AcqRel);
                DaemonShutdownPlan::new(JoinSet::new(), Vec::new(), |_| async {
                    ShutdownTaskReceipt::default()
                })
            },
        )
        .await;

        assert!(!retry.is_retryable());
        assert_eq!(prepares.load(Ordering::Acquire), 2);
    }
}
