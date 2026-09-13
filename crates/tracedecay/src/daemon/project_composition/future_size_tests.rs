//! Size probes for the project-open state machine.
//!
//! Async lowering stores every local that is live across an `.await` in one
//! generated enum, so an unrelated phase's temporaries inflate the whole
//! future. These probes read the compiled size without polling anything.

use std::future::Future;
use std::mem::size_of;

use super::*;

trait FutureSize<Args> {
    fn future_size(&self) -> usize;
}

macro_rules! impl_future_size {
    ($($arg:ident),*) => {
        impl<Fun, Fut, $($arg),*> FutureSize<($($arg,)*)> for Fun
        where
            Fun: FnOnce($($arg),*) -> Fut,
            Fut: Future,
        {
            fn future_size(&self) -> usize {
                size_of::<Fut>()
            }
        }
    };
}

impl_future_size!();
impl_future_size!(A);
impl_future_size!(A, B);
impl_future_size!(A, B, C);
impl_future_size!(A, B, C, D);
impl_future_size!(A, B, C, D, E);
impl_future_size!(A, B, C, D, E, F);
impl_future_size!(A, B, C, D, E, F, G);
impl_future_size!(A, B, C, D, E, F, G, H);
impl_future_size!(A, B, C, D, E, F, G, H, I);
impl_future_size!(A, B, C, D, E, F, G, H, I, J);

/// Size in bytes of the future an `async fn` returns, without constructing it.
fn future_size<Args, Fun: FutureSize<Args>>(function: Fun) -> usize {
    function.future_size()
}

fn phase_sizes() -> Vec<(&'static str, usize)> {
    vec![
        (
            "production_project_server",
            future_size(production_project_server),
        ),
        ("admit_route", future_size(ProjectOpenInputs::admit_route)),
        ("open_graph", future_size(ProjectOpenInputs::open_graph)),
        (
            "compose_core_server",
            future_size(ProjectOpenInputs::compose_core_server),
        ),
        (
            "bind_core_route",
            future_size(ProjectOpenInputs::bind_core_route),
        ),
        (
            "activate_core_route",
            future_size(ProjectOpenInputs::activate_core_route),
        ),
        (
            "admit_session_databases",
            future_size(ProjectOpenInputs::admit_session_databases),
        ),
        (
            "construct_full_server",
            future_size(ProjectOpenInputs::construct_full_server),
        ),
        (
            "mount_full_server_owners",
            future_size(ProjectOpenInputs::mount_full_server_owners),
        ),
        (
            "finish_full_server",
            future_size(ProjectOpenInputs::finish_full_server),
        ),
        (
            "settle_failed_full_upgrade",
            future_size(ProjectOpenInputs::settle_failed_full_upgrade),
        ),
    ]
}

fn awaited_sizes() -> Vec<(&'static str, usize)> {
    vec![
        (
            "release_one_idle_project_server_before_open",
            future_size(release_one_idle_project_server_before_open),
        ),
        (
            "ensure_registered_project_route",
            future_size(ensure_registered_project_route),
        ),
        (
            "open_project_for_handshake",
            future_size(open_project_for_handshake),
        ),
        (
            "TraceDecay::register_project_store_in_global_registry",
            future_size(crate::project::TraceDecay::register_project_store_in_global_registry),
        ),
        (
            "McpServer::new_with_context",
            future_size(crate::mcp::McpServer::new_with_context),
        ),
        (
            "McpServer::shutdown",
            future_size(crate::mcp::McpServer::shutdown),
        ),
        (
            "spawn_semantic_owner_registration",
            future_size(project_open_owners::spawn_semantic_owner_registration),
        ),
        (
            "StoreAdministration::registered_project_session_database",
            future_size(StoreAdministration::registered_project_session_database),
        ),
        (
            "ensure_project_delivery_settlement",
            future_size(project_delivery_mount::ensure_project_delivery_settlement),
        ),
        (
            "DaemonSessionSyncService::register_project",
            future_size(tracedecay_session_runtime::session_sync::DaemonSessionSyncService::register_project),
        ),
        (
            "register_project_open_production_owners",
            future_size(project_open_owners::register_project_open_production_owners),
        ),
        (
            "register_project_open_dependent_owners",
            future_size(project_open_owners::register_project_open_dependent_owners),
        ),
        (
            "schedule_project_server_retirement",
            future_size(schedule_project_server_retirement),
        ),
        (
            "retire_failed_project_open_owner",
            future_size(retire_failed_project_open_owner),
        ),
    ]
}

/// The composition entry holds only borrowed inputs and boxed phase futures.
/// Measured: 1,248 B in an ordinary build, 2,648 B under `--features hotpath`
/// (every measured async fn there embeds its body a second time). The boxed
/// `_inner` it replaced was 6,648 B / 21,200 B behind a 104 B / 424 B shell.
const COMPOSITION_ENTRY_CEILING: usize = 4 * 1024;

/// Every phase owns its temporaries and awaits at most one wide leaf at a
/// time. Measured maxima: 21,200 B (`mount_full_server_owners`, ordinary
/// build) and 175,424 B (the same phase under `--features hotpath`). The
/// next-widest phases are `construct_full_server` (7,208 B) and `open_graph`
/// (6,200 B); the semantic lifecycle registry read and the background-CPU
/// authority handle each cost their phase well under 128 B.
const PHASE_CEILING: usize = if cfg!(feature = "hotpath") {
    256 * 1024
} else {
    32 * 1024
};

#[test]
fn project_open_future_sizes() {
    let phases = phase_sizes();
    for (name, size) in phases.iter().chain(awaited_sizes().iter()) {
        eprintln!("{size:>10} B  {name}");
    }
    let (entry, entry_size) = phases[0];
    assert_eq!(entry, "production_project_server");
    assert!(
        entry_size <= COMPOSITION_ENTRY_CEILING,
        "{entry} future is {entry_size} B; ceiling {COMPOSITION_ENTRY_CEILING} B"
    );
    for (name, size) in &phases[1..] {
        assert!(
            *size <= PHASE_CEILING,
            "{name} future is {size} B; ceiling {PHASE_CEILING} B"
        );
    }
}

#[test]
fn shared_owner_futures_stay_below_large_future_threshold() {
    let owners = [
        ("source edit", future_size(tracedecay_daemon_service::project_owner_registration::ProjectSourceEditOwnerV1::execute)),
        ("production owner registration", future_size(crate::daemon::project_open_owners::register_project_open_production_owners)),
        ("automation admission", future_size(tracedecay_daemon_service::automation_effect::prepare)),
        ("invocation admission", future_size(tracedecay_daemon_service::DaemonInvocationService::invoke_with_project_admission)),
        ("Work dispatch", future_size(tracedecay_daemon_service::invocation::execute_work_application)),
        ("observation persistence", future_size(<tracedecay_global_db::GlobalDbObservationStore as tracedecay_store::ObservationStore>::persist_observation)),
        ("vector retirement", future_size(tracedecay_code_index_runtime::code_index_scheduler::semantic_vector_graph::retire_one_project_vector_generation)),
    ];
    for (name, size) in owners {
        eprintln!("{size:>10} B  {name}");
        assert!(size <= 16 * 1024, "{name} retains a {size} B future");
    }
}

mod measured_body {
    use std::mem::size_of_val;

    macro_rules! measured_chain {
    ($leaf:ident, $one:ident, $two:ident, $three:ident; $($options:tt)*) => {
        #[hotpath::measure($($options)*)]
        async fn $leaf() -> usize {
            let data = [1_u8; 1024];
            std::future::pending::<()>().await;
            std::hint::black_box(data).len()
        }
        #[hotpath::measure($($options)*)]
        async fn $one() -> usize { $leaf().await }
        #[hotpath::measure($($options)*)]
        async fn $two() -> usize { $one().await }
        #[hotpath::measure($($options)*)]
        async fn $three() -> usize { $two().await }
    };
}

    measured_chain!(timing_leaf, timing_one, timing_two, timing_three;);
    measured_chain!(logged_leaf, logged_one, logged_two, logged_three; log = true);
    measured_chain!(future_leaf, future_one, future_two, future_three; future = true);
    measured_chain!(both_leaf, both_one, both_two, both_three; future = true, log = true);

    #[test]
    fn nested_measurements_retain_one_body_frame_per_level() {
        let chains = [
            (size_of_val(&timing_leaf()), size_of_val(&timing_three())),
            (size_of_val(&logged_leaf()), size_of_val(&logged_three())),
            (size_of_val(&future_leaf()), size_of_val(&future_three())),
            (size_of_val(&both_leaf()), size_of_val(&both_three())),
        ];
        for (leaf, nested) in chains {
            // Three measurement guards may add metadata, but must not duplicate
            // the full child state at every generated async forwarding layer.
            assert!(nested <= leaf + 3 * 1024, "leaf={leaf}, nested={nested}");
        }
    }

    struct DropCount<'a>(&'a std::sync::atomic::AtomicUsize);
    impl Drop for DropCount<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[hotpath::measure(future = true, log = true)]
    async fn cancellable(drops: &std::sync::atomic::AtomicUsize) {
        let _drop = DropCount(drops);
        std::future::pending::<()>().await;
    }

    #[test]
    fn cancelling_a_borrowed_measured_body_drops_its_state_once() {
        use std::future::Future;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::{Context, Waker};

        let drops = AtomicUsize::new(0);
        let mut future = Box::pin(cancellable(&drops));
        assert!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(future);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
