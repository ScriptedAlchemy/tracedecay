//! Project session authorities of a daemon-hosted dashboard.
//!
//! A dashboard can be started while its project is still opening: the daemon
//! publishes a core server before the project's session store is admitted, and
//! a dashboard composed from that server has no session authority yet. The
//! composition states that as [`DashboardSessionMountV1::Opening`] with a
//! receiver the daemon sends one terminal resolution to when the project's
//! publication finishes; requests only read it.

use std::sync::Arc;

use tokio::sync::watch;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;

use crate::lcm_api::DashboardLcmReadPortV1;
use crate::loom_api::DashboardGitCorrelationReadPortV1;

/// The session authorities one project's dashboard reads through.
#[derive(Clone)]
pub struct DashboardSessionAuthoritiesV1 {
    pub project_sessions: RegisteredGlobalDbLeaseV1,
    pub lcm_read_authority: Option<Arc<dyn DashboardLcmReadPortV1>>,
    pub git_correlation_read_authority: Option<Arc<dyn DashboardGitCorrelationReadPortV1>>,
}

/// The daemon's answer about a project that was opening.
#[derive(Clone)]
pub enum DashboardSessionResolutionV1 {
    /// The project's publication has not finished.
    Opening,
    Ready(DashboardSessionAuthoritiesV1),
    /// The publication finished without session authorities, or the project
    /// no longer has a serving owner.
    Unavailable,
}

/// How a dashboard state is composed with its project's session store.
#[derive(Clone)]
pub enum DashboardSessionMountV1 {
    Ready(DashboardSessionAuthoritiesV1),
    Opening(watch::Receiver<DashboardSessionResolutionV1>),
    Unavailable,
}

/// The session authority state a dashboard reports to its readers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DashboardSessionAuthorityStateV1 {
    Opening,
    Ready,
    Unavailable,
}

impl DashboardSessionAuthorityStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Ready => "ready",
            Self::Unavailable => "unavailable",
        }
    }
}
