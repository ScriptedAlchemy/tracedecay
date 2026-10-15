//! One transport-neutral daemon shutdown sequence.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::lifecycle::{
    DAEMON_BACKGROUND_DRAIN_DEADLINE, DAEMON_CLIENT_DRAIN_DEADLINE,
    DAEMON_PROJECT_SERVER_DRAIN_DEADLINE, DAEMON_STORE_CLOSE_RESERVE, DAEMON_TASK_ABORT_DEADLINE,
    DaemonLifecycle, DaemonShutdownClaim,
};
use super::owners::{
    ShutdownOwner, ShutdownOwnerReceipt, ShutdownReceipt, ShutdownStatus,
    prepare_shutdown_owner_phases,
};
use tracedecay_runtime_core::logging::log_daemon_event;
use tracedecay_runtime_core::native_runtime::{self, TimeoutError};
use tracedecay_store_runtime::{ShutdownTaskOutcome, ShutdownTaskReceipt};

type ProjectServerShutdownFuture =
    Pin<Box<dyn Future<Output = ShutdownTaskReceipt> + Send + 'static>>;
/// The project-server drain is built as a *factory over its phase deadline*,
/// the same shape `ShutdownOwner::with_deadline` uses. Handing the drain the
/// global deadline instead let it spend every remaining second and starve the
/// store-close phase behind it.
type ProjectServerShutdown =
    Box<dyn FnOnce(Instant) -> ProjectServerShutdownFuture + Send + 'static>;

const SHUTDOWN_COORDINATOR_RECEIPT_GRACE: Duration = Duration::from_millis(100);

/// Maximum length of each serial shutdown phase.
#[derive(Clone, Copy)]
struct DaemonShutdownPhases {
    client_drain: Duration,
    background_drain: Duration,
    project_servers: Duration,
    store_close: Duration,
}

const DAEMON_SHUTDOWN_PHASES: DaemonShutdownPhases = DaemonShutdownPhases {
    client_drain: DAEMON_CLIENT_DRAIN_DEADLINE,
    background_drain: DAEMON_BACKGROUND_DRAIN_DEADLINE,
    project_servers: DAEMON_PROJECT_SERVER_DRAIN_DEADLINE,
    store_close: DAEMON_STORE_CLOSE_RESERVE,
};

/// Splits one shutdown deadline into per-phase budgets.
///
/// Each phase deadline is recomputed from the clock when that phase starts,
/// so unspent budget flows forward; each is additionally floored so it cannot
/// eat the reserve belonging to the phases behind it.
#[derive(Clone, Copy)]
struct DaemonShutdownBudget {
    overall: Instant,
    phases: DaemonShutdownPhases,
}

impl DaemonShutdownBudget {
    fn new(overall: Instant, phases: DaemonShutdownPhases) -> Self {
        Self { overall, phases }
    }

    /// Deadline for a phase starting at `now`, capped at `phase_max`, that
    /// leaves `downstream_reserve` untouched for the phases that follow it.
    ///
    /// Never returns a deadline before `now`: an overrunning predecessor
    /// degrades a later phase to an immediate, *named* timeout rather than
    /// deleting it from the sequence entirely.
    fn phase(self, now: Instant, phase_max: Duration, downstream_reserve: Duration) -> Instant {
        let reserved_floor = self
            .overall
            .checked_sub(downstream_reserve)
            .unwrap_or(self.overall);
        let capped = std::cmp::min(now + phase_max, self.overall);
        std::cmp::max(now, std::cmp::min(capped, reserved_floor))
    }

    fn client_drain(self, now: Instant) -> Instant {
        let phases = self.phases;
        self.phase(
            now,
            phases.client_drain,
            phases.background_drain + phases.project_servers + phases.store_close,
        )
    }

    fn background_drain(self, now: Instant) -> Instant {
        let phases = self.phases;
        self.phase(
            now,
            phases.background_drain,
            phases.project_servers + phases.store_close,
        )
    }

    fn project_servers(self, now: Instant) -> Instant {
        let phases = self.phases;
        self.phase(now, phases.project_servers, phases.store_close)
    }

    fn store_close(self, now: Instant) -> Instant {
        self.phase(now, self.phases.store_close, Duration::ZERO)
    }
}

/// `Elapsed` is the phase's own timeout; a timer that cannot start fails the
/// phase closed with its reason.
fn deadline_status(error: TimeoutError) -> ShutdownStatus {
    match error {
        TimeoutError::Elapsed => ShutdownStatus::TimedOut,
        TimeoutError::TimerUnavailable(_) => ShutdownStatus::Failed(error.to_string()),
    }
}

/// Operator receipts for one serial shutdown phase. Spans already measure
/// these windows (`daemon.shutdown.project_servers` / `store_close`; inner
/// work includes `mcp.server.shutdown` and `graph_db.runtime.close.engine`).
fn log_shutdown_phase(phase: &str, boundary: &str, started: Instant, deadline: Instant) {
    let now = Instant::now();
    log_daemon_event(
        "daemon_shutdown",
        &[
            ("outcome", format!("{phase}_{boundary}")),
            (
                "elapsed_ms",
                now.saturating_duration_since(started)
                    .as_millis()
                    .to_string(),
            ),
            (
                "deadline_remaining_ms",
                deadline
                    .saturating_duration_since(now)
                    .as_millis()
                    .to_string(),
            ),
        ],
    );
}

pub struct DaemonShutdownPlan {
    clients: Option<ShutdownOwner>,
    owner_phases: Vec<Vec<ShutdownOwner>>,
    terminal_owner_phases: Vec<Vec<ShutdownOwner>>,
    project_server_shutdown: ProjectServerShutdown,
    phases: DaemonShutdownPhases,
}

impl DaemonShutdownPlan {
    pub fn new<ProjectServers, ProjectServersFuture>(
        owner_phases: Vec<Vec<ShutdownOwner>>,
        project_server_shutdown: ProjectServers,
    ) -> Self
    where
        ProjectServers: FnOnce(Instant) -> ProjectServersFuture + Send + 'static,
        ProjectServersFuture: Future<Output = ShutdownTaskReceipt> + Send + 'static,
    {
        Self {
            clients: None,
            owner_phases,
            terminal_owner_phases: Vec::new(),
            project_server_shutdown: Box::new(move |deadline| {
                Box::pin(project_server_shutdown(deadline))
            }),
            phases: DAEMON_SHUTDOWN_PHASES,
        }
    }

    /// The accepted client connections, cancelled and joined once in-flight
    /// client work has drained or its drain window has closed. Its join
    /// status is the receipt's `clients` status.
    #[must_use]
    pub fn with_clients(mut self, clients: ShutdownOwner) -> Self {
        self.clients = Some(clients);
        self
    }

    #[must_use]
    pub fn with_terminal_owner_phases(
        mut self,
        terminal_owner_phases: Vec<Vec<ShutdownOwner>>,
    ) -> Self {
        self.terminal_owner_phases = terminal_owner_phases;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonShutdownReceipt {
    pub in_flight: ShutdownStatus,
    pub clients: ShutdownStatus,
    pub background: ShutdownReceipt,
    pub project_servers: ShutdownTaskReceipt,
}

impl DaemonShutdownReceipt {
    fn coordinator_failed(deadline: Instant, error: String) -> Self {
        Self {
            in_flight: ShutdownStatus::Failed(error.clone()),
            clients: ShutdownStatus::Failed(error.clone()),
            background: ShutdownReceipt::failed(deadline, "shutdown_coordinator", error.clone()),
            project_servers: ShutdownTaskReceipt::failed("shutdown_coordinator", error),
        }
    }

    fn coordinator_timed_out(deadline: Instant) -> Self {
        Self {
            in_flight: ShutdownStatus::TimedOut,
            clients: ShutdownStatus::TimedOut,
            background: ShutdownReceipt::timed_out(deadline, "shutdown_coordinator"),
            project_servers: ShutdownTaskReceipt::timed_out("shutdown_coordinator"),
        }
    }

    fn coordinator_deadline(deadline: Instant, error: &TimeoutError) -> Self {
        match error {
            TimeoutError::Elapsed => Self::coordinator_timed_out(deadline),
            TimeoutError::TimerUnavailable(_) => Self::coordinator_failed(
                deadline,
                format!("daemon shutdown deadline failed: {error}"),
            ),
        }
    }

    pub fn is_retryable(&self) -> bool {
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
pub struct DaemonShutdownFailures {
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

#[tracing::instrument(name = "daemon.shutdown.coordinate", level = "trace", skip_all)]
#[expect(
    clippy::too_many_lines,
    reason = "Shutdown coordination is one receipt-and-join of the running stop plan."
)]
pub async fn coordinate_daemon_shutdown<Prepare>(
    lifecycle: &DaemonLifecycle,
    shutdown_deadline: Instant,
    prepare: Prepare,
) -> Arc<DaemonShutdownReceipt>
where
    Prepare: Future<Output = DaemonShutdownPlan> + Send + 'static,
{
    if let Err(error) = native_runtime::timeout_at(
        shutdown_deadline,
        lifecycle.wait_for_finished_shutdown_coordinator(),
    )
    .await
    {
        return Arc::new(DaemonShutdownReceipt::coordinator_deadline(
            shutdown_deadline,
            &error,
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
            // The owner phases still reach Tokio timers and blocking tasks
            // (maintenance, project servers), so the runner stays on Tokio.
            let runner_runtime = native_runtime::tokio_handle("daemon-service.shutdown.runner");
            let coordinator_lifecycle = lifecycle.clone();
            let runner_lifecycle = lifecycle.clone();
            let coordinator_attempt = Arc::clone(&attempt);
            let mut coordinator_failures = failures.clone();
            let coordinator = runner_runtime.map(|runtime| async move {
                let mut runner = runtime.spawn(async move {
                    let prepare_started = Instant::now();
                    let plan = prepare.await;
                    log_shutdown_phase(
                        "plan_prepare",
                        "complete",
                        prepare_started,
                        shutdown_deadline,
                    );
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
                    fired = native_runtime::sleep_until(shutdown_deadline) => {
                        runner.abort();
                        let error = fired.err().map_or(TimeoutError::Elapsed, TimeoutError::TimerUnavailable);
                        (
                            DaemonShutdownReceipt::coordinator_deadline(shutdown_deadline, &error),
                            true,
                        )
                    }
                };
                coordinator_failures.record(&receipt);
                coordinator_failures.apply(&mut receipt);
                // Publishing the terminal receipt is the shutdown checkpoint:
                // once it lands, every concurrent waiter observes this
                // outcome instead of racing a duplicate shutdown attempt.
                {
                    let _span = tracing::trace_span!("daemon.shutdown.checkpoint").entered();
                    {
                        coordinator_lifecycle.finish_shutdown_attempt(
                            &coordinator_attempt,
                            Arc::new(receipt),
                            coordinator_failures,
                        );
                    }
                };
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
            });
            let spawned = coordinator
                .ok_or_else(|| "daemon shutdown runner found no Tokio runtime".to_owned())
                .and_then(|coordinator| {
                    lifecycle.spawn_shutdown_coordinator(&attempt, coordinator)
                });
            if let Err(error) = spawned {
                let receipt = Arc::new(DaemonShutdownReceipt::coordinator_failed(
                    shutdown_deadline,
                    error,
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
        fired = native_runtime::sleep_until(receipt_deadline) => {
            let error = fired.err().map_or(TimeoutError::Elapsed, TimeoutError::TimerUnavailable);
            Arc::new(DaemonShutdownReceipt::coordinator_deadline(
                shutdown_deadline,
                &error,
            ))
        }
    };
    let _ = native_runtime::timeout_at(
        receipt_deadline,
        lifecycle.wait_for_finished_shutdown_coordinator(),
    )
    .await;
    lifecycle.join_finished_shutdown_coordinator().await;
    receipt
}

#[tracing::instrument(name = "daemon.shutdown.run", level = "trace", skip_all)]
#[expect(
    clippy::too_many_lines,
    reason = "Daemon shutdown is one ordered client-drain then owner-phase list."
)]
async fn run_daemon_shutdown(
    lifecycle: DaemonLifecycle,
    plan: DaemonShutdownPlan,
    shutdown_deadline: Instant,
) -> DaemonShutdownReceipt {
    let budget = DaemonShutdownBudget::new(shutdown_deadline, plan.phases);
    let prepared = prepare_shutdown_owner_phases(plan.owner_phases);
    let mut background_shutdown = Box::pin(prepared.join(budget.background_drain(Instant::now())));
    let mut background_receipt = None;
    let client_drain_started = Instant::now();
    let client_drain_deadline = budget.client_drain(client_drain_started);
    let mut in_flight = pin!(native_runtime::timeout_at(
        client_drain_deadline,
        lifecycle.wait_for_idle()
    ));
    // Client drain: wait for in-flight client work to idle out cooperatively,
    // then cancel and join whatever remains. One span covers both the
    // cooperative wait and the forced cancel/join so its duration reads as a
    // single number against DAEMON_CLIENT_DRAIN_DEADLINE /
    // DAEMON_TASK_ABORT_DEADLINE instead of only surfacing as a bare timeout.
    log_shutdown_phase(
        "client_drain",
        "start",
        client_drain_started,
        client_drain_deadline,
    );
    let (in_flight, clients) = {
        use tracing::Instrument as _;
        async {
            let in_flight = loop {
                tokio::select! {
                    receipt = &mut background_shutdown, if background_receipt.is_none() => {
                        background_receipt = Some(receipt);
                    }
                    drained = &mut in_flight => {
                        break drained.map_or_else(deadline_status, |()| ShutdownStatus::Clean);
                    }
                }
            };

            let now = Instant::now();
            let client_join_deadline =
                std::cmp::min(now + DAEMON_TASK_ABORT_DEADLINE, budget.client_drain(now));
            let clients = match plan.clients {
                Some(clients) => prepare_shutdown_owner_phases(vec![vec![clients]])
                    .join(client_join_deadline)
                    .await
                    .owners
                    .pop()
                    .map_or(ShutdownStatus::Clean, |receipt| receipt.status),
                None => ShutdownStatus::Clean,
            };
            let clients =
                match native_runtime::timeout_at(client_join_deadline, lifecycle.wait_for_idle())
                    .await
                {
                    Ok(()) => clients,
                    Err(error) => deadline_status(error),
                };
            (in_flight, clients)
        }
        .instrument(tracing::trace_span!("daemon.shutdown.client_drain"))
        .await
    };
    log_shutdown_phase(
        "client_drain",
        "complete",
        client_drain_started,
        client_drain_deadline,
    );
    // Background-task drain: resolve the non-terminal ShutdownOwner phases
    // (maintenance, session sync, invocation, ...).
    // Often already resolved inside the client-drain select loop above; this
    // span only measures the residual wait when it was not.
    let background_drain_started = Instant::now();
    let mut background = {
        use tracing::Instrument as _;
        async {
            match background_receipt {
                Some(receipt) => receipt,
                None => background_shutdown.await,
            }
        }
        .instrument(tracing::trace_span!("daemon.shutdown.background_drain"))
        .await
    };
    log_shutdown_phase(
        "background_drain",
        "complete",
        background_drain_started,
        shutdown_deadline,
    );
    // Project servers hold session-database leases whose graph clients keep
    // the session relation graph owners leased. The terminal owner drains and
    // closes those graph runtimes, so it must run only after every server has
    // dropped its leases.
    let project_servers_started = Instant::now();
    let project_server_deadline = budget.project_servers(project_servers_started);
    log_shutdown_phase(
        "project_servers",
        "start",
        project_servers_started,
        project_server_deadline,
    );
    let project_servers = {
        use tracing::Instrument as _;
        async {
            // The drain gets its phase deadline, so it reaches its own bounded
            // join and reports one outcome *per named server*. The outer timer is
            // only the backstop for a drain that ignores its deadline; it fires
            // late enough that the named receipt normally wins the race.
            let drain = (plan.project_server_shutdown)(project_server_deadline);
            match native_runtime::timeout_at(
                project_server_deadline + DAEMON_TASK_ABORT_DEADLINE,
                drain,
            )
            .await
            {
                Ok(receipt) => receipt,
                Err(TimeoutError::Elapsed) => {
                    ShutdownTaskReceipt::timed_out("project_server_shutdown")
                }
                Err(error) => {
                    ShutdownTaskReceipt::failed("project_server_shutdown", error.to_string())
                }
            }
        }
        .instrument(tracing::trace_span!("daemon.shutdown.project_servers"))
        .await
    };
    log_shutdown_phase(
        "project_servers",
        "complete",
        project_servers_started,
        project_server_deadline,
    );
    // Store close: the terminal owner phase (memory_graph_reconciliation)
    // drains retained graph owners and closes their Grafeo runtimes. This is
    // the outer view of the close; daemon.branch_admin.close_graph_runtimes
    // and graph_db.registry.close_retained measure the work underneath it.
    let store_close_started = Instant::now();
    let store_close_deadline = budget.store_close(store_close_started);
    let terminal = if project_servers.timed_out_count() == 0 {
        log_shutdown_phase(
            "store_close",
            "start",
            store_close_started,
            store_close_deadline,
        );
        let receipt = {
            use tracing::Instrument as _;
            {
                prepare_shutdown_owner_phases(plan.terminal_owner_phases)
                    .join(store_close_deadline)
                    .instrument(tracing::trace_span!("daemon.shutdown.store_close"))
                    .await
            }
        };
        log_shutdown_phase(
            "store_close",
            "complete",
            store_close_started,
            store_close_deadline,
        );
        receipt
    } else {
        log_shutdown_phase(
            "store_close",
            "skipped",
            store_close_started,
            store_close_deadline,
        );
        ShutdownReceipt::timed_out(store_close_deadline, "memory_graph_reconciliation")
    };
    background.extend(terminal);
    DaemonShutdownReceipt {
        in_flight,
        clients,
        background,
        project_servers,
    }
}

#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "tests run on an ambient Tokio runtime"
)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE;

    /// Phase caps short enough for a stuck phase to time out in real time.
    const TEST_PHASES: DaemonShutdownPhases = DaemonShutdownPhases {
        client_drain: Duration::from_millis(50),
        background_drain: Duration::from_millis(200),
        project_servers: Duration::from_millis(300),
        store_close: Duration::from_millis(300),
    };

    /// A phase that never finishes used to spend the whole global deadline,
    /// so the phases behind it never ran: the project-server drain reported an
    /// instant timeout, store-close was skipped on the back of that timeout,
    /// and the coordinator's own deadline fired and replaced every receipt
    /// with an anonymous `shutdown_coordinator` entry. With per-phase budgets
    /// the stuck owner is bounded, is named in its own receipt, and the phases
    /// behind it still get their reserved budget.
    #[tokio::test]
    async fn a_stuck_background_owner_cannot_starve_project_servers_or_store_close() {
        let lifecycle = DaemonLifecycle::default();
        let overall = Instant::now() + Duration::from_secs(2);
        let observed_deadline = Arc::new(std::sync::Mutex::new(None));
        let project_server_deadline = Arc::clone(&observed_deadline);
        let store_closed = Arc::new(AtomicBool::new(false));
        let store_closed_by_owner = Arc::clone(&store_closed);

        let receipt = coordinate_daemon_shutdown(&lifecycle, overall, async move {
            let mut plan = DaemonShutdownPlan::new(
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
            )]]);
            plan.phases = TEST_PHASES;
            plan
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
            project_server_deadline + TEST_PHASES.store_close <= overall,
            "project-server phase must not encroach on the store-close reserve"
        );
        assert!(receipt.project_servers.is_clean());
        // Store close is the durability obligation: it must still run.
        assert!(
            store_closed.load(Ordering::Acquire),
            "store close must still run behind a stuck background owner"
        );
    }

    #[test]
    fn phase_budgets_flow_forward_but_reserve_the_phases_behind_them() {
        let start = Instant::now();
        let overall = start + DAEMON_SHUTDOWN_DEADLINE;
        let budget = DaemonShutdownBudget::new(overall, DAEMON_SHUTDOWN_PHASES);

        // A phase is capped at its own maximum, not the global deadline.
        assert_eq!(
            budget.background_drain(start),
            start + DAEMON_BACKGROUND_DRAIN_DEADLINE
        );
        // Unspent budget flows forward: recomputed from the clock, a phase
        // that starts later still gets its full cap while it fits.
        let now = start + Duration::from_secs(5);
        assert_eq!(
            budget.project_servers(now),
            now + DAEMON_PROJECT_SERVER_DRAIN_DEADLINE
        );
        // Late in the budget the reserve floor wins over the phase cap, so the
        // store-close phase keeps its slice instead of being consumed.
        let now = start + Duration::from_secs(25);
        assert_eq!(
            budget.project_servers(now) + DAEMON_STORE_CLOSE_RESERVE,
            overall
        );
        // Past the reserve boundary an overrun phase still gets a deadline it
        // can report a named timeout against, rather than being dropped from
        // the sequence, and store close still gets the rest.
        let now = start + Duration::from_secs(40);
        assert_eq!(budget.project_servers(now), now);
        assert_eq!(budget.store_close(now), overall);
    }

    #[tokio::test]
    async fn stuck_in_flight_work_times_out_at_the_client_drain_deadline() {
        let lifecycle = DaemonLifecycle::default();
        let _stuck = lifecycle.try_enter().expect("in-flight client work");

        let receipt = coordinate_daemon_shutdown(
            &lifecycle,
            Instant::now() + Duration::from_secs(2),
            async {
                let mut plan = DaemonShutdownPlan::new(Vec::new(), |_| async {
                    ShutdownTaskReceipt::default()
                });
                plan.phases = TEST_PHASES;
                plan
            },
        )
        .await;

        assert_eq!(receipt.in_flight, ShutdownStatus::TimedOut);
        assert_eq!(receipt.clients, ShutdownStatus::TimedOut);
        assert!(
            receipt.background.owners.is_empty(),
            "the client phase times out on its own deadline, not the coordinator's: {:?}",
            receipt.background
        );
        assert!(receipt.project_servers.is_clean());
    }

    #[tokio::test]
    async fn a_project_server_drain_that_ignores_its_deadline_hits_the_backstop() {
        let lifecycle = DaemonLifecycle::default();

        let receipt = coordinate_daemon_shutdown(
            &lifecycle,
            Instant::now() + Duration::from_secs(4),
            async {
                let mut plan = DaemonShutdownPlan::new(Vec::new(), |_| {
                    std::future::pending::<ShutdownTaskReceipt>()
                });
                plan.phases = TEST_PHASES;
                plan
            },
        )
        .await;

        assert_eq!(
            receipt.project_servers,
            ShutdownTaskReceipt::timed_out("project_server_shutdown")
        );
        assert!(matches!(
            receipt.background.owners.as_slice(),
            [owner] if owner.name == "memory_graph_reconciliation"
                && owner.status == ShutdownStatus::TimedOut
        ));
    }

    #[tokio::test]
    async fn post_reaper_phase_receipts_reach_stderr() {
        const CHILD_MODE: &str = "TRACEDECAY_SHUTDOWN_RECEIPT_TEST";
        if let Ok(mode) = std::env::var(CHILD_MODE) {
            let timed_out = mode == "timed_out";
            let closed = Arc::new(AtomicBool::new(false));
            let terminal_closed = Arc::clone(&closed);
            let lifecycle = DaemonLifecycle::default();
            let deadline = Instant::now() + Duration::from_secs(5);
            let receipt =
                coordinate_daemon_shutdown(&lifecycle, deadline, async move {
                    DaemonShutdownPlan::new(Vec::new(), move |_| async move {
                        if timed_out {
                            ShutdownTaskReceipt::timed_out("project_server")
                        } else {
                            ShutdownTaskReceipt::default()
                        }
                    })
                    .with_terminal_owner_phases(vec![vec![
                        ShutdownOwner::new("memory_graph_reconciliation", || {}, async move {
                            terminal_closed.store(true, Ordering::Release);
                        }),
                    ]])
                })
                .await;
            assert_eq!(closed.load(Ordering::Acquire), !timed_out);
            assert_eq!(receipt.background.unfinished().is_empty(), !timed_out);
            return;
        }

        for (mode, expected) in [
            (
                "complete",
                vec![
                    "project_servers_start",
                    "project_servers_complete",
                    "store_close_start",
                    "store_close_complete",
                ],
            ),
            (
                "timed_out",
                vec![
                    "project_servers_start",
                    "project_servers_complete",
                    "store_close_skipped",
                ],
            ),
        ] {
            // A child process exercises the production stderr sink, including formatting.
            // libtest names the test by its module path without the crate prefix.
            let test_name = format!(
                "{}::post_reaper_phase_receipts_reach_stderr",
                module_path!()
                    .split_once("::")
                    .map_or(module_path!(), |(_, rest)| rest)
            );
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test_name, "--nocapture"])
                .env(CHILD_MODE, mode)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stderr = String::from_utf8(output.stderr).unwrap();
            let phases: Vec<_> = stderr
                .lines()
                .filter(|line| {
                    line.starts_with("[tracedecay] event=daemon_shutdown ")
                        && (line.contains("outcome=project_servers_")
                            || line.contains("outcome=store_close_"))
                })
                .collect();
            let outcomes: Vec<_> = phases
                .iter()
                .map(|line| {
                    assert!(line.contains("elapsed_ms="), "{line}");
                    assert!(line.contains("deadline_remaining_ms="), "{line}");
                    line.split_whitespace()
                        .find_map(|field| field.strip_prefix("outcome="))
                        .unwrap()
                })
                .collect();
            assert_eq!(outcomes, expected, "{stderr}");
        }
    }

    #[tokio::test]
    async fn terminal_owner_waits_for_admitted_commit_to_drain() {
        let lifecycle = DaemonLifecycle::default();
        let activity = lifecycle.try_enter().expect("admitted commit activity");
        lifecycle.begin_draining();
        let terminal_cancelled = Arc::new(AtomicBool::new(false));
        let task_cancelled = Arc::clone(&terminal_cancelled);
        let deadline = Instant::now() + Duration::from_secs(5);
        let shutdown_lifecycle = lifecycle.clone();
        let shutdown = tokio::spawn(async move {
            coordinate_daemon_shutdown(&shutdown_lifecycle, deadline, async move {
                DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
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
        assert_eq!(receipt.background.unfinished().len(), 0);
    }

    #[tokio::test]
    async fn terminal_owner_waits_for_timed_out_project_server_lease_release() {
        let lifecycle = DaemonLifecycle::default();
        let terminal_started = Arc::new(AtomicBool::new(false));
        let terminal_started_by_owner = Arc::clone(&terminal_started);
        let deadline = Instant::now() + Duration::from_secs(1);
        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            DaemonShutdownPlan::new(Vec::new(), |_| async {
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
        // This owner is released by the test rather than resolving on its own,
        // so it is the one case here that needs a *real* background budget. A
        // deadline shorter than the reserves behind that phase floors its
        // deadline at `now` (`DaemonShutdownBudget::phase`), and the owner then
        // reports `TimedOut` whenever its first poll precedes the release by a
        // timer tick. That makes the receipt retryable, which legitimately
        // re-prepares shutdown and hides the ownership contract under test.
        let deadline = Instant::now() + DAEMON_SHUTDOWN_DEADLINE;

        let first_lifecycle = lifecycle.clone();
        let first_cancellations = Arc::clone(&cancellations);
        let first_server_shutdowns = Arc::clone(&server_shutdowns);
        let cancelled_signal = Arc::clone(&owner_cancelled);
        let owner_release = Arc::clone(&release_owner);
        let first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, deadline, async move {
                DaemonShutdownPlan::new(
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
        assert_eq!(receipt.background.unfinished().len(), 0);
        assert!(receipt.project_servers.is_clean());
    }

    #[tokio::test]
    async fn client_error_is_preserved_in_terminal_receipt() {
        let lifecycle = DaemonLifecycle::default();
        let deadline = Instant::now() + Duration::from_secs(5);

        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async move {
            DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
                .with_clients(ShutdownOwner::with_deadline_result(
                    "clients",
                    || {},
                    |_| async {
                        Err::<(), _>(TraceDecayError::Config {
                            message: "client failed during shutdown".to_owned(),
                        })
                    },
                ))
        })
        .await;

        assert_eq!(
            receipt.clients,
            ShutdownStatus::Failed("config error: client failed during shutdown".to_owned())
        );
        assert!(receipt.project_servers.is_clean());
    }

    /// The coordinator itself runs natively; the runner it owns still needs
    /// Tokio, so a caller without one gets a typed receipt, not a panic.
    #[test]
    fn a_shutdown_without_a_tokio_runtime_fails_with_a_typed_receipt() {
        const SITE: &str = "daemon-service.shutdown.runner";
        let lifecycle = DaemonLifecycle::default();
        let missing = native_runtime::tokio_runtime_missing_count(SITE);
        let receipt = native_runtime::block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_err());
            coordinate_daemon_shutdown(&lifecycle, Instant::now() + Duration::from_secs(5), async {
                DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
            })
            .await
        })
        .expect("native runtime");

        assert_eq!(
            receipt.in_flight,
            ShutdownStatus::Failed("daemon shutdown runner found no Tokio runtime".to_owned())
        );
        assert_eq!(
            native_runtime::tokio_runtime_missing_count(SITE) - missing,
            1
        );
    }

    #[tokio::test]
    async fn coordinator_panic_becomes_shared_terminal_failure() {
        let lifecycle = DaemonLifecycle::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let receipt = coordinate_daemon_shutdown(&lifecycle, deadline, async {
            DaemonShutdownPlan::new(Vec::new(), |_| async {
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
        assert_ne!(receipt.background.unfinished().len(), 0);
        assert!(!receipt.project_servers.is_clean());
    }

    #[tokio::test]
    async fn timed_out_shutdown_receipt_allows_one_non_overlapping_retry() {
        let lifecycle = DaemonLifecycle::default();
        let attempts = Arc::new(AtomicUsize::new(0));
        let cancellations = Arc::new(AtomicUsize::new(0));
        let first_attempts = Arc::clone(&attempts);
        let first_cancellations = Arc::clone(&cancellations);
        let first_deadline = Instant::now() + Duration::from_secs(1);
        let first = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                coordinate_daemon_shutdown(&lifecycle, first_deadline, async move {
                    first_attempts.fetch_add(1, Ordering::AcqRel);
                    DaemonShutdownPlan::new(
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
        let first = first.await.expect("timed-out shutdown attempt");
        assert!(matches!(
            first.background.owners.as_slice(),
            [owner] if owner.name == "uncooperative_owner"
                && owner.status == ShutdownStatus::TimedOut
        ));

        let retry_attempts = Arc::clone(&attempts);
        let retry_cancellations = Arc::clone(&cancellations);
        let retry_deadline = Instant::now() + Duration::from_secs(1);
        let retry = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async move {
            retry_attempts.fetch_add(1, Ordering::AcqRel);
            DaemonShutdownPlan::new(
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
        assert_eq!(retry.background.unfinished().len(), 0);
        assert!(retry.project_servers.is_clean());
    }

    #[tokio::test]
    async fn retry_preserves_a_typed_failure_from_an_earlier_timed_out_attempt() {
        let lifecycle = DaemonLifecycle::default();
        let first_deadline = Instant::now() + Duration::from_secs(1);
        let first = tokio::spawn({
            let lifecycle = lifecycle.clone();
            async move {
                coordinate_daemon_shutdown(&lifecycle, first_deadline, async {
                    DaemonShutdownPlan::new(
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
            Instant::now() + Duration::from_secs(1),
            async {
                DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
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
            Instant::now() + Duration::from_secs(1),
            async move {
                DaemonShutdownPlan::new(Vec::new(), move |_| async move { first_project_servers })
            },
        )
        .await;

        assert!(first.is_retryable());
        let retry = coordinate_daemon_shutdown(
            &lifecycle,
            Instant::now() + Duration::from_secs(1),
            async {
                DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
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

    #[tokio::test]
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
        let deadline = Instant::now() + Duration::from_millis(200);

        let first_lifecycle = lifecycle.clone();
        let first_lock = Arc::clone(&prepare_lock);
        let first_started = Arc::clone(&prepare_started);
        let first_dropped = Arc::clone(&dropped);
        let first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, deadline, async move {
                let _dropped = Dropped(first_dropped);
                first_started.notify_one();
                let _lock = first_lock.lock().await;
                DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
            })
            .await
        });
        prepare_started.notified().await;

        let receipt = tokio::time::timeout(Duration::from_secs(10), first)
            .await
            .expect("the shutdown deadline bounds a contended prepare")
            .expect("shutdown prepare timeout receipt");
        assert!(
            Instant::now() >= deadline,
            "the prepare is cut off at the shutdown deadline, not before it"
        );

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
        let retry_deadline = Instant::now() + Duration::from_secs(1);
        let retry = coordinate_daemon_shutdown(&lifecycle, retry_deadline, async {
            DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
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
        let deadline = Instant::now() + Duration::from_secs(1);
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
        let first_deadline = Instant::now() + Duration::from_millis(500);
        let first_lifecycle = lifecycle.clone();
        let first_prepares = Arc::clone(&prepares);
        let first_release = Arc::clone(&release);
        let mut first = tokio::spawn(async move {
            coordinate_daemon_shutdown(&first_lifecycle, first_deadline, async move {
                first_prepares.fetch_add(1, Ordering::AcqRel);
                DaemonShutdownPlan::new(Vec::new(), move |_| BlockingProjectServers {
                    entered: Some(entered_tx),
                    release: first_release,
                })
            })
            .await
        });
        entered_rx.await.expect("shutdown runner entered");

        let first_receipt = match tokio::time::timeout(Duration::from_secs(2), &mut first).await {
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
        let second_deadline = Instant::now() + Duration::from_millis(100);
        let second = coordinate_daemon_shutdown(&lifecycle, second_deadline, async move {
            second_prepares.fetch_add(1, Ordering::AcqRel);
            DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
        })
        .await;
        assert!(second.is_retryable());
        assert_eq!(second.in_flight, ShutdownStatus::TimedOut);
        assert_eq!(prepares.load(Ordering::Acquire), 1);

        release_runner(&release);
        tokio::time::timeout(
            Duration::from_secs(2),
            lifecycle.wait_for_finished_shutdown_coordinator(),
        )
        .await
        .expect("timed-out runner ownership released");
        lifecycle.join_finished_shutdown_coordinator().await;

        let retry_prepares = Arc::clone(&prepares);
        let retry = coordinate_daemon_shutdown(
            &lifecycle,
            Instant::now() + Duration::from_secs(1),
            async move {
                retry_prepares.fetch_add(1, Ordering::AcqRel);
                DaemonShutdownPlan::new(Vec::new(), |_| async { ShutdownTaskReceipt::default() })
            },
        )
        .await;

        assert!(!retry.is_retryable());
        assert_eq!(prepares.load(Ordering::Acquire), 2);
    }
}
