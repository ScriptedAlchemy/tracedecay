//! Daemon process lifecycle: accepting/draining state, the transport-neutral
//! shutdown sequence over its owner phases, and the exit-bound watchdog.
//!
//! The composition root drives these from its engine; the MCP crate adapts
//! [`DaemonLifecycle`] to its connection-lifecycle port.

pub mod lifecycle;
pub mod orchestration;
pub mod owners;
pub mod watchdog;

pub use lifecycle::{
    DAEMON_BACKGROUND_DRAIN_DEADLINE, DAEMON_CLIENT_DRAIN_DEADLINE,
    DAEMON_PROJECT_SERVER_DRAIN_DEADLINE, DAEMON_STORE_CLOSE_RESERVE, DAEMON_TASK_ABORT_DEADLINE,
    DaemonActivity, DaemonLifecycle, DaemonShutdownClaim,
};
pub use orchestration::{
    DaemonShutdownFailures, DaemonShutdownPlan, DaemonShutdownReceipt, coordinate_daemon_shutdown,
};
pub use owners::{
    DrainingGauge, PreparedShutdownOwners, ShutdownOwner, ShutdownOwnerReceipt, ShutdownReceipt,
    ShutdownStatus, prepare_shutdown_owner_phases,
};
#[cfg(any(test, feature = "test-helpers"))]
pub use owners::{join_shutdown_owner_phases, join_shutdown_owners};
#[cfg(feature = "hotpath")]
pub use watchdog::install_hotpath_shutdown_finalizer;
pub use watchdog::{DRAIN_BOUND_EXIT_CODE, arm_shutdown_exit_bound, shutdown_exit_bound};
