//! Runtime-specific choices for production project composition.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_global_db::RegisteredGlobalDb;

#[cfg(unix)]
use super::DaemonEngine;
#[cfg(any(not(unix), test, feature = "test-transport"))]
use super::portable_database_owner_reconciler;
use super::{DaemonHandshake, ProjectServerKey, StoreAdministration};

pub(super) async fn bind_verified_project_graph_runtime(
    database: Arc<crate::db::Database>,
    sessions: &RegisteredGlobalDb,
) -> crate::errors::Result<()> {
    let graph_proxy =
        database
            .memory_graph_runtime()
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "project memory graph runtime was not mounted before project sessions"
                    .to_owned(),
            })?;
    sessions
        .bind_project_graph_runtime(graph_proxy)
        .map_err(|_| crate::errors::TraceDecayError::Config {
            message: "project graph runtime was already mounted for project sessions".to_owned(),
        })
}

#[derive(Clone)]
pub(in crate::daemon) enum ProductionProjectCompositionRuntime {
    #[cfg(unix)]
    Unix(Box<DaemonEngine>),
    #[cfg(any(not(unix), test, feature = "test-transport"))]
    Portable {
        semantic_auto_download: bool,
        startup_catch_up: bool,
    },
}

impl ProductionProjectCompositionRuntime {
    pub(super) fn database_owner_reconciler(
        &self,
        _store_administration: &StoreAdministration,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        route_registered: Arc<AtomicBool>,
        handshake: DaemonHandshake,
    ) -> crate::mcp::DatabaseOwnerReconciler {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => engine.database_owner_reconciler(
                current_key,
                current_project_path,
                route_registered,
                handshake,
            ),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => portable_database_owner_reconciler(
                _store_administration.clone(),
                current_key,
                route_registered,
                handshake,
            ),
        }
    }

    pub(super) fn automation_scheduler_reconciler(
        &self,
        current_key: Arc<tokio::sync::Mutex<ProjectServerKey>>,
        current_project_path: Arc<tokio::sync::Mutex<PathBuf>>,
        handshake: DaemonHandshake,
    ) -> Option<crate::dashboard::AutomationSchedulerReconciler> {
        match self {
            #[cfg(unix)]
            Self::Unix(engine) => Some(engine.automation_scheduler_reconciler(
                current_key,
                current_project_path,
                handshake,
            )),
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable { .. } => None,
        }
    }

    pub(super) const fn semantic_auto_download(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                semantic_auto_download,
                ..
            } => *semantic_auto_download,
        }
    }

    pub(super) const fn startup_catch_up(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => true,
            #[cfg(any(not(unix), test, feature = "test-transport"))]
            Self::Portable {
                startup_catch_up, ..
            } => *startup_catch_up,
        }
    }
}
