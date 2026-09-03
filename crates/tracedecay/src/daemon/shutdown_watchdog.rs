//! Out-of-band enforcement of the daemon shutdown drain bound.
//!
//! Every deadline inside the graceful drain — the per-phase budgets, the
//! coordinator's overall `timeout_at`, the receipt grace — is a tokio timer.
//! Timers are driven by a parked runtime worker, so when every worker is
//! wedged in synchronous work (a graph replay holding a store mutex that
//! periodic tasks then block on, a saturated reader rendezvous, …) the drain
//! cannot enforce its own deadline: the root future never resumes, the
//! process outlives the supervisor's stop timeout, and the eventual SIGKILL
//! lands mid-WAL-write. This module enforces the same bound from a plain OS
//! thread that owes nothing to the runtime.
//!
//! Semantics: arming happens exactly when the daemon commits to exiting (the
//! accept loop has ended). From that instant the process has
//! [`shutdown_exit_bound`] of wall clock to finish the graceful drain and
//! exit on its own; a clean exit simply wins the race and the thread dies
//! with the process. A profiling build reserves the final bounded second for
//! best-effort report finalization. The watchdog then logs one typed receipt
//! and exits with [`DRAIN_BOUND_EXIT_CODE`] no later than the same absolute
//! bound. Work abandoned this way tears recoverably (crash-atomic checkpoints),
//! which is strictly better than the same tear at a supervisor-chosen SIGKILL
//! instant — and the exit code plus receipt name the cause instead of a bare
//! `status=9/KILL`.
//!
//! This is not a raised limit: the graceful drain keeps its own deadlines
//! and its receipt logging; the watchdog only guarantees the drain window is
//! real when the cooperative runtime cannot guarantee it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(feature = "hotpath")]
use std::sync::{Mutex, mpsc};

use super::log_daemon_event;
use tracedecay_runtime_core::DAEMON_SHUTDOWN_DEADLINE;

/// Wall clock past the graceful drain deadline reserved for receipt logging,
/// endpoint cleanup, profiling finalization, and bounded CLI runtime teardown.
const DRAIN_EXIT_RESERVE: Duration = Duration::from_secs(5);

/// Maximum share of the exit reserve offered to a best-effort Hotpath report.
///
/// The finalizer runs on its own OS thread because a wedged profiler must not
/// defeat the watchdog. Arming this one second before the absolute exit bound
/// keeps that work inside the existing reserve rather than extending it.
#[cfg(feature = "hotpath")]
const HOTPATH_FINALIZE_BOUND: Duration = Duration::from_secs(1);

/// Exit status for a forced drain-bound exit (`EX_SOFTWARE`), distinct from a
/// generic failure so the supervisor journal names the cause.
pub(crate) const DRAIN_BOUND_EXIT_CODE: i32 = 70;

/// Total wall clock the process may live after committing to shutdown.
///
/// Must stay comfortably below the supervisor stop timeout (systemd default
/// 90s) so a wedged drain exits with a typed receipt instead of a SIGKILL.
pub(crate) fn shutdown_exit_bound() -> Duration {
    DAEMON_SHUTDOWN_DEADLINE + DRAIN_EXIT_RESERVE
}

static ARMED: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "hotpath")]
type HotpathShutdownFinalizer = Box<dyn FnOnce() + Send + 'static>;

#[cfg(feature = "hotpath")]
static HOTPATH_SHUTDOWN_FINALIZER: Mutex<Option<HotpathShutdownFinalizer>> = Mutex::new(None);

/// Registers the process-owned profiler finalizer for the forced-exit path.
///
/// The CLI also owns the normal RAII drop. Both paths take the same guard from
/// a shared slot, so whichever wins finalizes exactly once.
#[cfg(feature = "hotpath")]
#[hotpath::measure(label = "daemon.shutdown_watchdog.install_hotpath_finalizer")]
pub fn install_hotpath_shutdown_finalizer(finalizer: impl FnOnce() + Send + 'static) -> bool {
    let mut slot = HOTPATH_SHUTDOWN_FINALIZER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if slot.is_some() {
        return false;
    }
    *slot = Some(Box::new(finalizer));
    true
}

#[cfg(feature = "hotpath")]
fn finalize_hotpath_report_within(bound: Duration) {
    let finalizer = HOTPATH_SHUTDOWN_FINALIZER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let Some(finalizer) = finalizer else {
        return;
    };
    let (completed_tx, completed_rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("tracedecay-hotpath-finalize".to_string())
        .spawn(move || {
            finalizer();
            let _ = completed_tx.send(());
        });
    if spawned.is_ok() {
        let _ = completed_rx.recv_timeout(bound);
    }
}

fn watchdog_fire_after() -> Duration {
    #[cfg(feature = "hotpath")]
    {
        return shutdown_exit_bound().saturating_sub(HOTPATH_FINALIZE_BOUND);
    }
    #[cfg(not(feature = "hotpath"))]
    shutdown_exit_bound()
}

fn drain_bound_exceeded(bound: Duration) -> ! {
    log_daemon_event(
        "daemon_shutdown",
        &[
            ("outcome", "drain_bound_exceeded".to_string()),
            ("bound_secs", bound.as_secs().to_string()),
            ("exit_code", DRAIN_BOUND_EXIT_CODE.to_string()),
        ],
    );
    #[cfg(feature = "hotpath")]
    finalize_hotpath_report_within(HOTPATH_FINALIZE_BOUND);
    std::process::exit(DRAIN_BOUND_EXIT_CODE);
}

/// Arms the drain-bound exit for this process. Idempotent: the unix and
/// loopback shutdown sequences each arm once, and only the first call spawns
/// the enforcement thread.
#[cfg_attr(
    feature = "hotpath",
    hotpath::measure(label = "daemon.shutdown_watchdog.arm")
)]
pub(super) fn arm_shutdown_exit_bound() {
    if ARMED.swap(true, Ordering::AcqRel) {
        return;
    }
    let bound = shutdown_exit_bound();
    arm_with_action(
        "tracedecay-shutdown-bound",
        watchdog_fire_after(),
        move || {
            drain_bound_exceeded(bound);
        },
    );
}

/// Spawns the enforcement thread. Split from the production arm so the
/// out-of-band property is directly falsifiable; production passes the
/// receipt-and-exit action above.
#[cfg_attr(
    feature = "hotpath",
    hotpath::measure(label = "daemon.shutdown_watchdog.spawn")
)]
fn arm_with_action(name: &str, bound: Duration, action: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            std::thread::sleep(bound);
            action();
        });
    if let Err(error) = spawned {
        // Losing enforcement is survivable (the graceful drain still runs);
        // losing it silently is not.
        log_daemon_event(
            "daemon_shutdown",
            &[
                ("outcome", "drain_bound_enforcement_unavailable".to_string()),
                ("error", error.to_string()),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "hotpath")]
    use std::process::Command;
    use std::sync::Arc;
    #[cfg(feature = "hotpath")]
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn armed_action_fires_after_the_bound_without_a_runtime() {
        let (fired_tx, fired_rx) = mpsc::channel();
        arm_with_action(
            "test-shutdown-bound",
            Duration::from_millis(50),
            move || {
                fired_tx.send(()).expect("watchdog observer alive");
            },
        );
        fired_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("armed action must fire after the bound");
    }

    /// The property the graceful drain cannot provide for itself: the bound
    /// fires even when every tokio worker is wedged in synchronous work and
    /// no timer can be driven.
    #[test]
    fn bound_fires_while_every_runtime_worker_is_wedged() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("wedge-probe runtime");
        let release = Arc::new(AtomicBool::new(false));
        for _ in 0..2 {
            let release = Arc::clone(&release);
            runtime.spawn(async move {
                // Synchronous spin on a worker thread: no await point, so the
                // worker can never park to drive the timer wheel.
                while !release.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            });
        }
        // Prove the wedge is real: a runtime timer no longer fires.
        let (timer_tx, timer_rx) = mpsc::channel();
        runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = timer_tx.send(());
        });
        assert!(
            timer_rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "wedge probe failed: runtime timers still fire"
        );

        let (fired_tx, fired_rx) = mpsc::channel();
        arm_with_action("test-wedged-bound", Duration::from_millis(50), move || {
            fired_tx.send(()).expect("watchdog observer alive");
        });
        let fired = fired_rx.recv_timeout(Duration::from_secs(5));
        release.store(true, Ordering::Release);
        fired.expect("drain bound must fire while the runtime is wedged");
        runtime.shutdown_timeout(Duration::from_secs(2));
    }

    #[test]
    fn exit_bound_stays_below_the_supervisor_stop_timeout() {
        assert!(
            shutdown_exit_bound() < Duration::from_secs(90),
            "drain bound must undercut the systemd default stop timeout"
        );
    }

    #[cfg(feature = "hotpath")]
    #[test]
    fn hotpath_finalization_stays_inside_the_existing_exit_bound() {
        assert_eq!(
            watchdog_fire_after() + HOTPATH_FINALIZE_BOUND,
            shutdown_exit_bound(),
            "profiling finalization must consume reserve, not extend process lifetime"
        );
    }

    #[cfg(feature = "hotpath")]
    #[test]
    fn watchdog_exit_child() {
        if std::env::var_os("TRACEDECAY_WATCHDOG_EXIT_CHILD").is_none() {
            return;
        }
        let guard = Arc::new(Mutex::new(Some(
            hotpath::HotpathGuardBuilder::new("watchdog-exit-test")
                .sections(vec![hotpath::Section::FunctionsTiming])
                .build(),
        )));
        let finalizer_guard = Arc::clone(&guard);
        assert!(
            install_hotpath_shutdown_finalizer(move || {
                let guard = finalizer_guard
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                drop(guard);
            }),
            "watchdog test installs one finalizer"
        );
        hotpath::measure_block!("watchdog.exit.test_measurement", {
            std::hint::black_box(1_u64);
        });
        arm_with_action(
            "test-watchdog-hotpath-exit",
            Duration::from_millis(10),
            move || drain_bound_exceeded(shutdown_exit_bound()),
        );
        loop {
            std::thread::park();
        }
    }

    #[cfg(feature = "hotpath")]
    #[test]
    fn watchdog_exit_flushes_a_readable_hotpath_report() {
        let temp = tempfile::tempdir().expect("temporary report directory");
        let report = temp.path().join("watchdog-hotpath.json");
        let output = Command::new(std::env::current_exe().expect("current test binary"))
            .arg("--exact")
            .arg("daemon::shutdown_watchdog::tests::watchdog_exit_child")
            .arg("--nocapture")
            .env("TRACEDECAY_WATCHDOG_EXIT_CHILD", "1")
            .env("HOTPATH_METRICS_SERVER_OFF", "true")
            .env("HOTPATH_OUTPUT_FORMAT", "json")
            .env("HOTPATH_OUTPUT_PATH", &report)
            .output()
            .expect("run watchdog exit child");

        assert_eq!(
            output.status.code(),
            Some(DRAIN_BOUND_EXIT_CODE),
            "{output:?}"
        );
        let bytes = std::fs::read(&report).expect("watchdog exit must leave a report");
        let parsed: serde_json::Value =
            serde_json::from_slice(&bytes).expect("watchdog report must be readable JSON");
        assert!(
            parsed.is_object(),
            "watchdog report must be a JSON object: {parsed:?}"
        );
    }
}
