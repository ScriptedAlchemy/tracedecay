//! Daemon-owned registry assembly for profile and project session shards.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
#[cfg(feature = "hotpath")]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracedecay_automation_runtime::ports::project_runtime::{ProfileRuntime, RuntimeFuture};
use tracedecay_domain::BrainNodeId;
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_store::{
    AdmissionConfigV1, ProjectId, StoreIncarnationV1, StoreShardIdV1, StoreShardScopeV1,
};

use tracedecay_daemon_identity::profile_identity::LocalProfileIdentityAuthorityV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::{RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1};
use tracedecay_graph_db::{GraphDbOwnerAttachmentV1, GraphDbRetirementCommit};
use tracedecay_runtime_core::db::MemoryGraphReconciliationRetirementTerminalV1;
use tracedecay_runtime_core::db::{
    Database, DatabaseAccessMode, DatabaseAuthority, DatabaseOwnerV1,
    DatabaseOwnerWeakLeaseIssuerV1, MemoryGraphReconciliationTaskOwnerV1,
};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalGraphStoreOwnerRetirementTargetV1, StoreRuntimeRetirementCommit,
};
use tracedecay_runtime_core::store_runtime::registry::{
    DestructiveMaintenanceReservation, DestructiveMaintenanceTarget,
    LifecycleShardRuntimePublisher, ProfileAuthorityPin, ProfileAuthorityPinResult,
    StoreRuntimeClientLease, StoreRuntimeKey, StoreRuntimeOpenRequest, StoreRuntimeOpenResult,
    StoreRuntimeRegistry, StoreRuntimeRegistryFailure, StoreRuntimeResolver,
};
use tracedecay_runtime_core::store_runtime::resolver::{
    LocalProfileStoreAuthorityV1, LocalProjectEnrollmentAuthorityV1, LocalStoreLocatorResolutionV1,
    LocalStoreRuntimeResolverV1,
};
use tracedecay_session_temporal_store::relations::SessionRelationScope;

mod code_graph;
mod code_graph_manifest;
mod code_reads;
pub mod maintenance;
mod memory_graph_reconciliation_tasks;
mod mounts;
mod profile_memory;
mod project_store_runtime;
mod remote_recovery;
mod retained_hook_tasks;
mod terminal_tasks;

use maintenance::RegisteredSchemaConvergenceMaintenance;
#[cfg(any(test, feature = "test-helpers"))]
use mounts::SessionGraphPublicationTestGateState;
use retained_hook_tasks::RetainedHookTasks;

#[cfg(any(test, feature = "test-helpers"))]
pub use mounts::SessionGraphPublicationTestGate;

pub use profile_memory::open_user_memory_db;

/// RAII hold for a root-owned remote-recovery writer admission.
///
/// The concrete guard type stays in the composition root; this crate only
/// needs the admission to remain live for the recovery effect.
pub struct RemoteRecoveryAdmission {
    _hold: Box<dyn Send + Sync>,
}

impl RemoteRecoveryAdmission {
    pub fn hold<T: Send + Sync + 'static>(value: T) -> Self {
        Self {
            _hold: Box::new(value),
        }
    }
}

/// RAII hold for a root-owned remote-recovery project quiescence fence.
pub struct RemoteRecoveryQuiescence {
    _hold: Box<dyn Send + Sync>,
}

impl RemoteRecoveryQuiescence {
    pub fn hold<T: Send + Sync + 'static>(value: T) -> Self {
        Self {
            _hold: Box::new(value),
        }
    }
}

/// Operations the composition root installs after opening a registry.
///
/// The concrete `RemoteRecoveryProjectLifecycleV1` stays in root because it
/// holds daemon invocation, project-open, and retirement state. This is the
/// existing install seam, not a new recovery port.
pub trait RemoteRecoveryProjectLifecycle: Send + Sync {
    fn authorize_project_recovery<'a>(
        &'a self,
        project_id: &'a ProjectId,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteRecoveryAdmission>> + Send + 'a>>;

    fn quiesce<'a>(
        &'a self,
        project_id: &'a ProjectId,
        database: &'a RegisteredGlobalDbLeaseV1,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteRecoveryQuiescence>> + Send + 'a>>;
}

/// Sanity ceiling on concurrently mounted project runtime owners, not a bound
/// on how many projects a profile may enrol.
///
/// This was 8, which is where enrolment actually stopped: a profile with four
/// projects was refused with `graph capacity budget exhausted (limit 8)`, and
/// once the budget was exhausted even a read-only `projects list` failed. Two
/// graph owners per mounted project plus two profile-wide ones is exactly 8,
/// so three projects filled it. Nothing about 8 was derived.
///
/// Resident memory is the real constraint and it is currently ungoverned for
/// these owners: capacity eviction cannot reclaim a mounted project, because
/// it holds its owner attachment for the life of the mount and so is never a
/// candidate. A ceiling this high therefore lets residency grow with the
/// number of projects actually touched. That is the deliberate trade —
/// refusing the fourth project was the worse failure — and the real fix is
/// idle-project hibernation, which is follow-up work.
///
/// This is the *only* declared project-population ceiling; every other ceiling
/// that must admit the same projects is derived from it below.
const MAX_RETAINED_PROJECT_RUNTIME_OWNERS: usize = 4_096;

/// Graph owners that exist once per profile rather than once per project: the
/// profile memory graph and the profile session-relation graph. Both live for
/// the daemon's lifetime, so they permanently hold slots no project can use.
const PROFILE_WIDE_GRAPH_DB_OWNERS: usize = 2;

const PROJECT_GRAPH_OWNER_ADMISSION_DEMAND: usize = 3;

/// Graph registry slot ceiling, derived so the project ceiling above is
/// actually reachable. Written as arithmetic rather than a literal because the
/// profile-wide owners take their slots first: a hand-picked number is short by
/// exactly that many, and the last project then fails inside the graph registry
/// before ever reaching [`MAX_RETAINED_PROJECT_RUNTIME_OWNERS`].
pub const MAX_RETAINED_GRAPH_DB_OWNERS: usize = PROFILE_WIDE_GRAPH_DB_OWNERS
    + PROJECT_GRAPH_OWNER_ADMISSION_DEMAND * MAX_RETAINED_PROJECT_RUNTIME_OWNERS;

/// Remote Brain node ceiling, taken from the credential registry rather than
/// declared independently. Owner admission refuses before anything is
/// published; the credential authority refuses after the runtime owner exists
/// and the node's `remote.db` is provisioned. Whenever this ceiling is the
/// looser of the two the refusal lands on the later check and leaves a
/// provisioned database behind, and startup remounts every discovered
/// `remote.db`, turning the residue into a hard failure on the next start.
const MAX_RETAINED_REMOTE_NODE_OWNERS: usize = crate::MAX_REGISTERED_REMOTE_NODES;

#[cfg(feature = "hotpath")]
static SESSION_STORE_MOUNTS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// RAII observation of one full (non-reuse) session-store mount attempt.
///
/// Entering counts the attempt; dropping restores the in-flight gauge on
/// every exit path, so failed, denied, or cancelled mounts cannot leak it.
#[cfg(feature = "hotpath")]
pub(crate) struct StoreMountObservationV1;

#[cfg(feature = "hotpath")]
impl StoreMountObservationV1 {
    pub fn enter() -> Self {
        let in_flight = SESSION_STORE_MOUNTS_IN_FLIGHT
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        hotpath::gauge!("daemon.session_registry.mount.attempts_total").inc(1_u64);
        hotpath::gauge!("daemon.session_registry.mount.in_flight").set(in_flight);
        Self
    }
}

#[cfg(feature = "hotpath")]
impl Drop for StoreMountObservationV1 {
    fn drop(&mut self) {
        let _ = SESSION_STORE_MOUNTS_IN_FLIGHT.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |in_flight| in_flight.checked_sub(1),
        );
        hotpath::gauge!("daemon.session_registry.mount.in_flight")
            .set(SESSION_STORE_MOUNTS_IN_FLIGHT.load(Ordering::Relaxed));
    }
}

struct SessionGraphOwnerV1 {
    graph: GraphDbOwnerAttachmentV1,
    store_target: CanonicalGraphStoreOwnerRetirementTargetV1,
}

enum SessionGraphAttachmentStateV1 {
    Warming,
    Attached {
        owner: Option<Box<SessionGraphOwnerV1>>,
    },
    Detached {
        error: String,
    },
}

struct RegisteredSessionOwnerV1 {
    database: RegisteredGlobalDbOwnerV1,
    relation_graph: Arc<StdMutex<SessionGraphAttachmentStateV1>>,
    graph_settled: Arc<tokio::sync::Notify>,
    graph_open_task_key: String,
}

impl RegisteredSessionOwnerV1 {
    fn with_attached_graph(
        database: RegisteredGlobalDbOwnerV1,
        relation_graph: SessionGraphOwnerV1,
        graph_open_task_key: String,
    ) -> Self {
        Self {
            database,
            relation_graph: Arc::new(StdMutex::new(SessionGraphAttachmentStateV1::Attached {
                owner: Some(Box::new(relation_graph)),
            })),
            graph_settled: Arc::new(tokio::sync::Notify::new()),
            graph_open_task_key,
        }
    }

    fn issue_lease(&self, scope: SessionRelationScope) -> Result<RegisteredGlobalDbLeaseV1> {
        let database = self.database.issue_lease().map_err(|error| {
            session_registry_error(
                "issue registered session database client",
                format!("{error:?}"),
            )
        })?;
        let state = self
            .relation_graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let SessionGraphAttachmentStateV1::Attached { owner: Some(owner) } = &*state else {
            return Ok(database);
        };
        Self::bind_relation_graph(&database, owner, scope)?;
        Ok(database)
    }

    fn bind_relation_graph(
        database: &RegisteredGlobalDbLeaseV1,
        owner: &SessionGraphOwnerV1,
        scope: SessionRelationScope,
    ) -> Result<()> {
        let graph = owner.graph.issue_lease().map_err(|error| {
            session_registry_error(
                "issue registered session relation graph client",
                error.to_string(),
            )
        })?;
        database
            .bind_session_relation_graph(
                scope,
                graph,
                owner.graph.binding().clone(),
                owner.graph.verified_locator().clone(),
            )
            .map_err(|_| {
                session_registry_error(
                    "bind issued registered session relation graph",
                    "issued graph client did not match the exact registered session owner"
                        .to_owned(),
                )
            })?;
        Ok(())
    }

    fn graph_unavailable_reason(&self) -> String {
        let state = self
            .relation_graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            SessionGraphAttachmentStateV1::Warming => {
                "Project session relation graph is warming".to_owned()
            }
            SessionGraphAttachmentStateV1::Detached { error } => {
                format!("Project session relation graph is unavailable: {error}")
            }
            SessionGraphAttachmentStateV1::Attached { owner: None } => {
                "Project session relation graph is reserved for retirement".to_owned()
            }
            SessionGraphAttachmentStateV1::Attached { owner: Some(_) } => {
                "Project session relation graph attachment changed during retirement admission"
                    .to_owned()
            }
        }
    }

    fn into_retirement(self) -> std::result::Result<ProjectSessionRetirementOwnerV1, Self> {
        let relation_graph = {
            let mut state = self
                .relation_graph
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match &mut *state {
                SessionGraphAttachmentStateV1::Attached { owner } => owner.take(),
                SessionGraphAttachmentStateV1::Warming
                | SessionGraphAttachmentStateV1::Detached { .. } => None,
            }
        };
        let Some(relation_graph) = relation_graph else {
            return Err(self);
        };
        self.database.detach_session_relation_graph();
        let SessionGraphOwnerV1 {
            graph,
            store_target,
        } = *relation_graph;
        Ok(ProjectSessionRetirementOwnerV1 {
            database: self.database,
            graph,
            store_target: Some(store_target),
            graph_open_task_key: self.graph_open_task_key,
        })
    }

    fn take_graph_store_identity(
        &self,
    ) -> Option<(
        tracedecay_store::StoreRuntimeBindingV1,
        tracedecay_store::VerifiedStoreLocatorV1,
    )> {
        let mut state = self
            .relation_graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let SessionGraphAttachmentStateV1::Attached { owner } = &mut *state else {
            return None;
        };
        let owner = owner.take()?;
        self.database.detach_session_relation_graph();
        Some((
            owner.graph.binding().clone(),
            owner.graph.verified_locator().clone(),
        ))
    }
}

/// A canonical project owner map may be passed to recovery orchestration, but
/// the map itself remains the one daemon authority. Clones only share this
/// map's synchronization boundary; they do not retain a database or client.
#[derive(Clone, Default)]
struct ProjectRuntimeOwnerRegistryV1(
    Arc<StdMutex<BTreeMap<ProjectId, ProjectRuntimeOwnerStateV1>>>,
);

impl ProjectRuntimeOwnerRegistryV1 {
    fn lock(
        &self,
    ) -> std::sync::LockResult<
        std::sync::MutexGuard<'_, BTreeMap<ProjectId, ProjectRuntimeOwnerStateV1>>,
    > {
        self.0.lock()
    }

    #[hotpath::measure(label = "daemon.session_registry.list_ready_sessions")]
    fn ready_session_projects(&self) -> Result<Vec<ProjectId>> {
        let entries = self.lock().map_err(|_| {
            session_registry_error(
                "list mounted project session owners",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        Ok(entries
            .iter()
            .filter_map(|(project_id, state)| match state {
                ProjectRuntimeOwnerStateV1::Ready(owners) if owners.sessions.is_some() => {
                    Some(project_id.clone())
                }
                _ => None,
            })
            .collect())
    }

    #[hotpath::measure(label = "daemon.session_registry.wait_session_graph", future = true)]
    async fn wait_for_session_graph(&self, project_id: &ProjectId) -> Result<()> {
        let (relation_graph, graph_settled) = {
            let entries = self.lock().map_err(|_| {
                session_registry_error(
                    "await project session relation graph",
                    "project runtime owner map lock is poisoned".to_owned(),
                )
            })?;
            // An unmounted project has no relation graph to settle. Deferring
            // to `reserve_session_replacement`, which reports the same absence
            // as `Ok(None)`, keeps retirement idempotent: re-running it after a
            // partially completed attempt (remote deletion retries the whole
            // `CancelRuntimeOwners` phase) must be a no-op, not a retryable
            // route refusal that can never clear. Only a project that *is*
            // mounted under another lifecycle state refuses settlement.
            let Some(state) = entries.get(project_id) else {
                return Ok(());
            };
            let ProjectRuntimeOwnerStateV1::Ready(owners) = state else {
                return Err(TraceDecayError::project_route(
                    "project_runtime_replacing_sessions",
                    true,
                    "Project session runtime is not accepting graph settlement",
                ));
            };
            let Some(owner) = owners.sessions.as_ref() else {
                return Ok(());
            };
            (
                Arc::clone(&owner.relation_graph),
                Arc::clone(&owner.graph_settled),
            )
        };
        loop {
            let notified = graph_settled.notified();
            let warming = matches!(
                &*relation_graph
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                SessionGraphAttachmentStateV1::Warming
            );
            if !warming {
                return Ok(());
            }
            notified.await;
        }
    }

    async fn wait_for_serving_session_graph(&self, project_id: &ProjectId) -> Result<()> {
        self.wait_for_session_graph(project_id).await?;
        let entries = self.lock().map_err(|_| {
            session_registry_error(
                "admit project session relation graph for serving",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(ProjectRuntimeOwnerStateV1::Ready(owners)) = entries.get(project_id) else {
            return Err(TraceDecayError::project_route(
                "project_session_graph_unavailable",
                true,
                "Project session runtime is not ready for retrieval serving",
            ));
        };
        let Some(sessions) = owners.sessions.as_ref() else {
            return Err(TraceDecayError::project_route(
                "project_session_graph_unavailable",
                true,
                "Project session runtime has no relation graph owner",
            ));
        };
        let attached = matches!(
            &*sessions
                .relation_graph
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            SessionGraphAttachmentStateV1::Attached { owner: Some(_) }
        );
        if attached {
            Ok(())
        } else {
            Err(TraceDecayError::project_route(
                "project_session_graph_unavailable",
                true,
                sessions.graph_unavailable_reason(),
            ))
        }
    }

    #[hotpath::measure(label = "daemon.session_registry.reserve_session_replacement")]
    fn reserve_session_replacement(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectSessionReplacementReservationV1>> {
        let mut entries = self.lock().map_err(|_| {
            session_registry_error(
                "reserve project session replacement",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = entries.remove(project_id) else {
            return Ok(None);
        };
        let ProjectRuntimeOwnerStateV1::Ready(mut owners) = state else {
            entries.insert(project_id.clone(), state);
            return Err(TraceDecayError::project_route(
                "project_runtime_replacing_sessions",
                true,
                "Project session runtime is not accepting replacement admission",
            ));
        };
        let Some(sessions) = owners.sessions.take() else {
            entries.insert(
                project_id.clone(),
                ProjectRuntimeOwnerStateV1::Ready(owners),
            );
            return Ok(None);
        };
        let sessions = match ProjectSessionRetirementOwnerV1::from_ready(sessions) {
            Ok(sessions) => sessions,
            Err(sessions) => {
                let reason = sessions.graph_unavailable_reason();
                owners.sessions = Some(sessions);
                entries.insert(
                    project_id.clone(),
                    ProjectRuntimeOwnerStateV1::Ready(owners),
                );
                return Err(TraceDecayError::project_route(
                    "project_session_graph_warming",
                    true,
                    reason,
                ));
            }
        };
        entries.insert(
            project_id.clone(),
            ProjectRuntimeOwnerStateV1::ReplacingSessions,
        );
        Ok(Some(ProjectSessionReplacementReservationV1 {
            owners: self.clone(),
            project_id: project_id.clone(),
            sessions: Some(sessions),
            memory: owners.memory,
            recovery_proof: None,
            armed: true,
        }))
    }

    #[hotpath::measure(label = "daemon.session_registry.reserve_session_recovery")]
    fn reserve_session_recovery(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectSessionRecoveryReservationV1>> {
        let mut entries = self.lock().map_err(|_| {
            session_registry_error(
                "reserve project session recovery",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = entries.remove(project_id) else {
            return Ok(None);
        };
        let ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery) = state else {
            let diagnostic = match &state {
                ProjectRuntimeOwnerStateV1::Faulted(faulted) => Some(faulted.recovery_inspection()),
                _ => None,
            };
            entries.insert(project_id.clone(), state);
            return Err(TraceDecayError::project_route(
                "project_runtime_recovery",
                true,
                diagnostic.map_or_else(
                    || "Project session runtime has no resumable recovery state".to_owned(),
                    |diagnostic| {
                        format!(
                            "Project session runtime is faulted and retained for typed recovery inspection: {}",
                            diagnostic.description(),
                        )
                    },
                ),
            ));
        };
        entries.insert(project_id.clone(), ProjectRuntimeOwnerStateV1::Recovering);
        Ok(Some(ProjectSessionRecoveryReservationV1 {
            owners: self.clone(),
            project_id: project_id.clone(),
            recovery: Some(recovery),
            armed: true,
        }))
    }

    /// Rebuilds the fail-closed post-restart recovery record. The durable
    /// quarantine receipt is written only after the old paired owners have
    /// closed; it never reconstructs or remounts that terminal owner.
    #[hotpath::measure(label = "daemon.session_registry.reconstruct_terminal_recovery")]
    fn reconstruct_durable_terminal_recovery(
        &self,
        project_id: &ProjectId,
        proof: ProjectSessionTerminalVacancyAuthorityV1,
    ) -> Result<()> {
        if !proof.matches_project(project_id) {
            return Err(session_registry_error(
                "rebuild terminal remote recovery vacancy",
                "durable remote restore proof does not belong to this project session shard"
                    .to_owned(),
            ));
        }
        let mut entries = self.lock().map_err(|_| {
            session_registry_error(
                "rebuild terminal remote recovery vacancy",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if entries.contains_key(project_id) {
            return Err(TraceDecayError::project_route(
                "project_runtime_recovery",
                true,
                "Project session runtime already has an owner or recovery reservation",
            ));
        }
        entries.insert(
            project_id.clone(),
            ProjectRuntimeOwnerStateV1::RecoveryRequired(ProjectSessionRecoveryRequiredV1 {
                sessions: None,
                candidate_sessions: None,
                memory: None,
                phase: ProjectSessionRecoveryPhaseV1::Terminal(
                    ProjectSessionTerminalProofV1::Durable(Box::new(proof)),
                ),
            }),
        );
        Ok(())
    }
}

#[hotpath::measure(label = "daemon.session_registry.bind_memory_graph")]
fn bind_ready_project_memory_graph(
    owners: &ProjectRuntimeOwnerRegistryV1,
    project_id: &ProjectId,
) -> Result<bool> {
    let (memory, sessions) = {
        let entries = owners.lock().map_err(|_| {
            session_registry_error(
                "bind ready project memory graph",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(ProjectRuntimeOwnerStateV1::Ready(owners)) = entries.get(project_id) else {
            return Ok(false);
        };
        let (Some(memory), Some(sessions)) = (owners.memory.as_ref(), owners.sessions.as_ref())
        else {
            return Ok(false);
        };
        (
            memory.issue_database_lease()?,
            sessions.database.issue_lease().map_err(|error| {
                session_registry_error(
                    "issue project session client for graph binding",
                    format!("{error:?}"),
                )
            })?,
        )
    };
    let Some(graph) = memory.memory_graph_runtime() else {
        return Ok(false);
    };
    sessions.bind_project_graph_runtime(graph).map_err(|_| {
        session_registry_error(
            "bind ready project memory graph",
            "verified project graph runtime does not match the project session shard".to_owned(),
        )
    })?;
    Ok(true)
}

/// The only state which may temporarily separate the exact paired Store
/// target from its Ready graph owner. It never reaches the canonical map.
struct ProjectSessionRetirementOwnerV1 {
    database: RegisteredGlobalDbOwnerV1,
    graph: GraphDbOwnerAttachmentV1,
    store_target: Option<CanonicalGraphStoreOwnerRetirementTargetV1>,
    graph_open_task_key: String,
}

impl ProjectSessionRetirementOwnerV1 {
    fn from_ready(
        owner: RegisteredSessionOwnerV1,
    ) -> std::result::Result<Self, RegisteredSessionOwnerV1> {
        owner.into_retirement()
    }

    fn into_ready(self) -> Result<RegisteredSessionOwnerV1> {
        let store_target = self.store_target.ok_or_else(|| {
            session_registry_error(
                "restore project session runtime owner",
                "session Store retirement target reached an irreversible boundary".to_owned(),
            )
        })?;
        Ok(RegisteredSessionOwnerV1 {
            database: self.database,
            relation_graph: Arc::new(StdMutex::new(SessionGraphAttachmentStateV1::Attached {
                owner: Some(Box::new(SessionGraphOwnerV1 {
                    graph: self.graph,
                    store_target,
                })),
            })),
            graph_settled: Arc::new(tokio::sync::Notify::new()),
            graph_open_task_key: self.graph_open_task_key,
        })
    }

    fn take_store_target(&mut self) -> Result<CanonicalGraphStoreOwnerRetirementTargetV1> {
        self.store_target.take().ok_or_else(|| {
            session_registry_error(
                "reserve session graph Store retirement",
                "session Store retirement target was already consumed".to_owned(),
            )
        })
    }

    fn restore_store_target(
        &mut self,
        target: CanonicalGraphStoreOwnerRetirementTargetV1,
    ) -> Result<()> {
        if self.store_target.is_some() {
            return Err(session_registry_error(
                "restore session graph Store retirement",
                "session Store retirement target was already retained".to_owned(),
            ));
        }
        self.store_target = Some(target);
        Ok(())
    }
}

struct MemoryStoreOwnerV1 {
    database: DatabaseOwnerWeakLeaseIssuerV1,
    graph: Arc<StdMutex<MemoryGraphAttachmentStateV1>>,
    graph_open_task_key: String,
}

enum MemoryGraphAttachmentStateV1 {
    Warming {
        database: Option<DatabaseOwnerV1>,
    },
    Attached {
        runtime: Arc<code_graph::RetainedVerifiedGraphRuntimeV1>,
        reconciliation: Option<MemoryGraphReconciliationTaskOwnerV1>,
        error: Option<String>,
    },
    Detached {
        database: DatabaseOwnerV1,
        error: String,
    },
}

impl MemoryStoreOwnerV1 {
    fn issue_database_lease(&self) -> Result<Database> {
        self.database.issue_lease().map_err(|error| {
            session_registry_error(
                "issue retained memory database client",
                format!("{error:?}"),
            )
        })
    }

    fn issue_database_read_only_lease(&self) -> Result<Database> {
        self.database.issue_read_only_lease().map_err(|error| {
            session_registry_error(
                "issue retained memory read-only database client",
                format!("{error:?}"),
            )
        })
    }

    fn graph_runtime(&self) -> Option<Arc<code_graph::RetainedVerifiedGraphRuntimeV1>> {
        let state = self
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            MemoryGraphAttachmentStateV1::Attached { runtime, .. } => Some(Arc::clone(runtime)),
            MemoryGraphAttachmentStateV1::Warming { .. }
            | MemoryGraphAttachmentStateV1::Detached { .. } => None,
        }
    }

    fn reconciliation_owner(&self) -> Option<MemoryGraphReconciliationTaskOwnerV1> {
        let state = self
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            MemoryGraphAttachmentStateV1::Attached { reconciliation, .. } => reconciliation.clone(),
            MemoryGraphAttachmentStateV1::Warming { .. }
            | MemoryGraphAttachmentStateV1::Detached { .. } => None,
        }
    }

    fn reconciliation_owner_and_attachment(
        &self,
    ) -> Option<(
        MemoryGraphReconciliationTaskOwnerV1,
        Arc<StdMutex<MemoryGraphAttachmentStateV1>>,
    )> {
        self.reconciliation_owner()
            .map(|owner| (owner, Arc::clone(&self.graph)))
    }

    fn clear_reconciliation_owner(
        graph: &StdMutex<MemoryGraphAttachmentStateV1>,
        retired: &MemoryGraphReconciliationTaskOwnerV1,
    ) {
        let mut state = graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let MemoryGraphAttachmentStateV1::Attached { reconciliation, .. } = &mut *state
            && reconciliation
                .as_ref()
                .is_some_and(|owner| owner.same_coordinator(retired))
        {
            *reconciliation = None;
        }
    }

    fn reserve_database_retirement(
        &self,
    ) -> std::result::Result<
        tracedecay_runtime_core::db::DatabaseOwnerRetirementReservationV1,
        String,
    > {
        let state = self
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            MemoryGraphAttachmentStateV1::Attached { runtime, .. } => runtime
                .reserve_database_retirement()
                .map_err(|error| error.to_string()),
            MemoryGraphAttachmentStateV1::Detached { database, .. } => database
                .reserve_retirement()
                .map_err(|error| format!("{error:?}")),
            MemoryGraphAttachmentStateV1::Warming { database } => database
                .as_ref()
                .ok_or_else(|| "memory graph attachment is still warming".to_owned())?
                .reserve_retirement()
                .map_err(|error| format!("{error:?}")),
        }
    }

    fn graph_error(&self) -> Option<String> {
        let state = self
            .graph
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            MemoryGraphAttachmentStateV1::Attached { error, .. } => error.clone(),
            MemoryGraphAttachmentStateV1::Detached { error, .. } => Some(error.clone()),
            MemoryGraphAttachmentStateV1::Warming { .. } => None,
        }
    }
}

struct RemoteNodeStoreOwnerV1 {
    database: DatabaseOwnerV1,
}

enum RemoteNodeOwnerStateV1 {
    Opening,
    Ready(RemoteNodeStoreOwnerV1),
}

enum RemoteNodeOwnerAdmissionV1<'a> {
    Existing(Database),
    Opening(RemoteNodeOwnerOpeningReservationV1<'a>),
}

/// Map admission is installed before a remote runtime opens. Its synchronous
/// drop path makes task cancellation release the exact opening slot without
/// waiting on a second cleanup task.
struct RemoteNodeOwnerOpeningReservationV1<'a> {
    nodes: &'a StdMutex<BTreeMap<BrainNodeId, RemoteNodeOwnerStateV1>>,
    node_id: BrainNodeId,
    armed: bool,
}

impl RemoteNodeOwnerOpeningReservationV1<'_> {
    fn publish(&mut self, owner: RemoteNodeStoreOwnerV1) -> Result<()> {
        let mut nodes = self.nodes.lock().map_err(|_| {
            session_registry_error(
                "publish Remote Brain node owner",
                "remote node owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = nodes.get_mut(&self.node_id) else {
            return Err(session_registry_error(
                "publish Remote Brain node owner",
                "remote node opening reservation disappeared".to_owned(),
            ));
        };
        if !matches!(state, RemoteNodeOwnerStateV1::Opening) {
            return Err(session_registry_error(
                "publish Remote Brain node owner",
                "remote node opening reservation no longer owns the map entry".to_owned(),
            ));
        }
        *state = RemoteNodeOwnerStateV1::Ready(owner);
        self.armed = false;
        Ok(())
    }
}

impl Drop for RemoteNodeOwnerOpeningReservationV1<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut nodes = self
            .nodes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            nodes.get(&self.node_id),
            Some(RemoteNodeOwnerStateV1::Opening)
        ) {
            nodes.remove(&self.node_id);
        }
        self.armed = false;
    }
}

#[derive(Default)]
struct ProjectRuntimeOwnersV1 {
    sessions: Option<RegisteredSessionOwnerV1>,
    memory: Option<MemoryStoreOwnerV1>,
}

enum ProjectRuntimeOwnerStateV1 {
    Opening,
    Ready(ProjectRuntimeOwnersV1),
    ReplacingSessions,
    Recovering,
    RecoveryRequired(ProjectSessionRecoveryRequiredV1),
    Retiring,
    Faulted(ProjectRuntimeFaultedOwnersV1),
}

/// Terminal or abandoned replacement state retains the exact old owners until
/// a recovery authority can resolve their typed outcome. This deliberately
/// preserves the paired graph Store target rather than inventing replacement
/// can restore the old identity.
struct ProjectSessionRecoveryRequiredV1 {
    sessions: Option<ProjectSessionRetirementOwnerV1>,
    candidate_sessions: Option<RegisteredSessionOwnerV1>,
    memory: Option<MemoryStoreOwnerV1>,
    phase: ProjectSessionRecoveryPhaseV1,
}

enum ProjectSessionRecoveryPhaseV1 {
    ReservationAbandoned,
    GraphNativeBoundary,
    GraphTerminal(GraphDbRetirementCommit),
    Terminal(ProjectSessionTerminalProofV1),
}

#[derive(Debug)]
enum ProjectSessionRecoveryInspectionV1 {
    ReservationAbandoned,
    GraphNativeBoundary,
    GraphTerminal { outcomes: usize },
    Terminal { proof_is_exact_and_closed: bool },
}

impl ProjectSessionRecoveryInspectionV1 {
    fn description(&self) -> String {
        match self {
            Self::ReservationAbandoned => {
                "the prior retirement reservation was abandoned before a terminal proof".to_owned()
            }
            Self::GraphNativeBoundary => {
                "the graph retirement reached its native boundary without a terminal proof"
                    .to_owned()
            }
            Self::GraphTerminal { outcomes } => format!(
                "the graph retirement retained {outcomes} terminal outcome(s) without a paired Store proof"
            ),
            Self::Terminal {
                proof_is_exact_and_closed,
            } => format!(
                "the retained terminal proof reports exact paired closure: {proof_is_exact_and_closed}"
            ),
        }
    }
}

impl ProjectSessionRecoveryPhaseV1 {
    fn inspection(&self) -> ProjectSessionRecoveryInspectionV1 {
        match self {
            Self::ReservationAbandoned => ProjectSessionRecoveryInspectionV1::ReservationAbandoned,
            Self::GraphNativeBoundary => ProjectSessionRecoveryInspectionV1::GraphNativeBoundary,
            Self::GraphTerminal(graph) => ProjectSessionRecoveryInspectionV1::GraphTerminal {
                outcomes: graph.outcomes().len(),
            },
            Self::Terminal(proof) => ProjectSessionRecoveryInspectionV1::Terminal {
                proof_is_exact_and_closed: proof.verify(),
            },
        }
    }
}

/// A non-vacuous, exact receipt that the prior paired graph and Store owners
/// were both closed. It is retained through file-swap recovery so a restart
/// may activate only a fresh candidate, never the terminal old owner.
struct ProjectSessionClosedRetirementProofV1 {
    binding: tracedecay_store::StoreRuntimeBindingV1,
    locator: tracedecay_store::VerifiedStoreLocatorV1,
    graph: GraphDbRetirementCommit,
    store: StoreRuntimeRetirementCommit,
}

impl ProjectSessionClosedRetirementProofV1 {
    fn verify(&self) -> bool {
        matches!(
            self.graph.outcomes(),
            [tracedecay_graph_db::GraphDbRetirementOutcome::Closed(target)]
                if target.binding() == &self.binding
                    && target.verified_locator() == &self.locator
        ) && matches!(
            self.store.outcomes(),
            [tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementOutcome::Closed { target }]
                if target.binding() == &self.binding
        )
    }

    fn durable_authority(&self) -> ProjectSessionTerminalVacancyAuthorityV1 {
        ProjectSessionTerminalVacancyAuthorityV1 {
            binding: self.binding.clone(),
            locator: self.locator.clone(),
        }
    }
}

/// The durable, identity-only portion of an exact paired-close receipt. It is
/// persisted in the remote restore journal after `verify()` has accepted the
/// live Graph and Store terminal receipts, so restart recovery may open only
/// a fresh candidate in the same terminal vacancy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectSessionTerminalVacancyAuthorityV1 {
    binding: tracedecay_store::StoreRuntimeBindingV1,
    locator: tracedecay_store::VerifiedStoreLocatorV1,
}

impl ProjectSessionTerminalVacancyAuthorityV1 {
    fn valid(&self) -> bool {
        self.binding.shard_id == self.locator.shard_id
            && self.binding.incarnation == self.locator.incarnation
    }

    fn matches_project(&self, project_id: &ProjectId) -> bool {
        self.valid()
            && matches!(
                &self.binding.shard_id.scope,
                StoreShardScopeV1::ProjectSessions { project_id: bound } if bound == project_id
            )
    }
}

/// A terminal vacancy is proven either by the live, non-cloneable Graph/Store
/// receipts or by their durable journal authority after process restart.
enum ProjectSessionTerminalProofV1 {
    Live(Box<ProjectSessionClosedRetirementProofV1>),
    Durable(Box<ProjectSessionTerminalVacancyAuthorityV1>),
}

impl ProjectSessionTerminalProofV1 {
    fn verify(&self) -> bool {
        match self {
            Self::Live(proof) => proof.verify(),
            Self::Durable(authority) => authority.valid(),
        }
    }

    fn durable_authority(&self) -> ProjectSessionTerminalVacancyAuthorityV1 {
        match self {
            Self::Live(proof) => proof.durable_authority(),
            Self::Durable(authority) => authority.as_ref().clone(),
        }
    }
}

enum ProjectRuntimeRetirementFaultV1 {
    ReservationTargetConsumed,
    Reconciliation(MemoryGraphReconciliationRetirementTerminalV1),
    GraphRefusal(tracedecay_graph_db::GraphDbError),
    StoreStart(tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRegistryFailure),
    Terminal {
        graph: GraphDbRetirementCommit,
        store: StoreRuntimeRetirementCommit,
    },
}

#[derive(Debug)]
enum ProjectRuntimeRetirementFaultInspectionV1 {
    ReservationTargetConsumed,
    Reconciliation {
        terminal: String,
    },
    GraphRefusal {
        error: String,
    },
    StoreStart {
        error: String,
    },
    Terminal {
        graph_outcomes: usize,
        store_outcomes: usize,
    },
}

impl ProjectRuntimeRetirementFaultInspectionV1 {
    fn description(&self) -> String {
        match self {
            Self::ReservationTargetConsumed => {
                "the retirement target was consumed before the map could restore it".to_owned()
            }
            Self::Reconciliation { terminal } => {
                format!("reconciliation retained terminal state: {terminal}")
            }
            Self::GraphRefusal { error } => {
                format!("graph retirement refused after the fence: {error}")
            }
            Self::StoreStart { error } => {
                format!("Store retirement could not start after the fence: {error}")
            }
            Self::Terminal {
                graph_outcomes,
                store_outcomes,
            } => format!(
                "terminal paired retirement retained {graph_outcomes} graph and {store_outcomes} Store outcome(s)"
            ),
        }
    }
}

#[derive(Debug)]
struct ProjectRuntimeFaultRecoveryInspectionV1 {
    retained_sessions: bool,
    retained_memory: bool,
    retiring_sessions: bool,
    fault: ProjectRuntimeRetirementFaultInspectionV1,
}

impl ProjectRuntimeFaultRecoveryInspectionV1 {
    fn description(&self) -> String {
        format!(
            "{}; retained old sessions: {}; retained memory: {}; retiring session owner: {}",
            self.fault.description(),
            self.retained_sessions,
            self.retained_memory,
            self.retiring_sessions,
        )
    }
}

/// Terminal retirement diagnosis retains the exact owners which were still
/// present at the native boundary. Their lifecycle states remain authoritative
/// for inspection/recovery; the daemon never drops them and invents a replacement.
struct ProjectRuntimeFaultedOwnersV1 {
    retained: ProjectRuntimeOwnersV1,
    sessions: Option<ProjectSessionRetirementOwnerV1>,
    fault: ProjectRuntimeRetirementFaultV1,
}

impl ProjectRuntimeFaultedOwnersV1 {
    fn recovery_inspection(&self) -> ProjectRuntimeFaultRecoveryInspectionV1 {
        let fault = match &self.fault {
            ProjectRuntimeRetirementFaultV1::ReservationTargetConsumed => {
                ProjectRuntimeRetirementFaultInspectionV1::ReservationTargetConsumed
            }
            ProjectRuntimeRetirementFaultV1::Reconciliation(terminal) => {
                ProjectRuntimeRetirementFaultInspectionV1::Reconciliation {
                    terminal: format!("{terminal:?}"),
                }
            }
            ProjectRuntimeRetirementFaultV1::GraphRefusal(error) => {
                ProjectRuntimeRetirementFaultInspectionV1::GraphRefusal {
                    error: error.to_string(),
                }
            }
            ProjectRuntimeRetirementFaultV1::StoreStart(error) => {
                ProjectRuntimeRetirementFaultInspectionV1::StoreStart {
                    error: format!("{error:?}"),
                }
            }
            ProjectRuntimeRetirementFaultV1::Terminal { graph, store } => {
                ProjectRuntimeRetirementFaultInspectionV1::Terminal {
                    graph_outcomes: graph.outcomes().len(),
                    store_outcomes: store.outcomes().len(),
                }
            }
        };
        ProjectRuntimeFaultRecoveryInspectionV1 {
            retained_sessions: self.retained.sessions.is_some(),
            retained_memory: self.retained.memory.is_some(),
            retiring_sessions: self.sessions.is_some(),
            fault,
        }
    }
}

enum ProjectRuntimeOwnerAdmissionV1 {
    Existing,
    Opening(Box<ProjectRuntimeOwnerOpeningReservationV1>),
}

/// A project slot enters `Opening` before any runtime publication.  It keeps
/// capacity accounting truthful across awaits and returns the exact old slot
/// if the opener is cancelled before publication completes.
struct ProjectRuntimeOwnerOpeningReservationV1 {
    owners: ProjectRuntimeOwnerRegistryV1,
    project_id: ProjectId,
    previous: Option<ProjectRuntimeOwnersV1>,
    armed: bool,
}

impl ProjectRuntimeOwnerOpeningReservationV1 {
    fn publish_sessions(&mut self, sessions: RegisteredSessionOwnerV1) -> Result<()> {
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "publish project session runtime owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = entries.get_mut(&self.project_id) else {
            return Err(session_registry_error(
                "publish project session runtime owner",
                "project runtime opening reservation disappeared".to_owned(),
            ));
        };
        if !matches!(state, ProjectRuntimeOwnerStateV1::Opening) {
            return Err(session_registry_error(
                "publish project session runtime owner",
                "project runtime opening reservation no longer owns the map entry".to_owned(),
            ));
        }
        let previous = self.previous.take().unwrap_or_default();
        *state = ProjectRuntimeOwnerStateV1::Ready(ProjectRuntimeOwnersV1 {
            sessions: Some(sessions),
            memory: previous.memory,
        });
        self.armed = false;
        Ok(())
    }

    fn publish_memory(&mut self, memory: MemoryStoreOwnerV1) -> Result<()> {
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "publish project memory runtime owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = entries.get_mut(&self.project_id) else {
            return Err(session_registry_error(
                "publish project memory runtime owner",
                "project runtime opening reservation disappeared".to_owned(),
            ));
        };
        if !matches!(state, ProjectRuntimeOwnerStateV1::Opening) {
            return Err(session_registry_error(
                "publish project memory runtime owner",
                "project runtime opening reservation no longer owns the map entry".to_owned(),
            ));
        }
        let previous = self.previous.take().unwrap_or_default();
        *state = ProjectRuntimeOwnerStateV1::Ready(ProjectRuntimeOwnersV1 {
            sessions: previous.sessions,
            memory: Some(memory),
        });
        self.armed = false;
        Ok(())
    }
}

impl Drop for ProjectRuntimeOwnerOpeningReservationV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut entries = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::Opening)
        ) {
            if let Some(previous) = self.previous.take() {
                entries.insert(
                    self.project_id.clone(),
                    ProjectRuntimeOwnerStateV1::Ready(previous),
                );
            } else {
                entries.remove(&self.project_id);
            }
        }
        self.armed = false;
    }
}

/// The map fence is installed before any external retirement reservation.
/// Dropping it before the native-close boundary restores the exact owners and
/// reopens serving admission without recreating a facade or graph authority.
struct ProjectRuntimeOwnerRetirementReservationV1 {
    owners: ProjectRuntimeOwnerRegistryV1,
    project_id: ProjectId,
    retained: Option<ProjectRuntimeOwnersV1>,
    sessions: Option<ProjectSessionRetirementOwnerV1>,
    armed: bool,
}

impl ProjectRuntimeOwnerRetirementReservationV1 {
    fn memory(&self) -> Result<&MemoryStoreOwnerV1> {
        self.retained
            .as_ref()
            .and_then(|owners| owners.memory.as_ref())
            .ok_or_else(|| {
                session_registry_error(
                    "retire project memory runtime",
                    "project has no retained memory runtime owner".to_owned(),
                )
            })
    }

    fn commit_without_memory(mut self) -> Result<()> {
        if let Some(owners) = self.retained.as_mut() {
            owners.memory = None;
        }
        self.commit_ready_or_remove()
    }

    fn commit_ready_or_remove(&mut self) -> Result<()> {
        let mut retained = self.retained.take().ok_or_else(|| {
            session_registry_error(
                "commit project runtime retirement",
                "project retirement reservation was already consumed".to_owned(),
            )
        })?;
        if let Some(sessions) = self.sessions.take() {
            retained.sessions = Some(sessions.into_ready()?);
        }
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "commit project runtime retirement",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::Retiring)
        ) {
            return Err(session_registry_error(
                "commit project runtime retirement",
                "project retirement fence disappeared before terminal commit".to_owned(),
            ));
        }
        if retained.sessions.is_some() || retained.memory.is_some() {
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::Ready(retained),
            );
        } else {
            entries.remove(&self.project_id);
        }
        self.armed = false;
        Ok(())
    }

    fn commit_fault(mut self, fault: ProjectRuntimeRetirementFaultV1) -> Result<()> {
        let retained = self.retained.take().ok_or_else(|| {
            session_registry_error(
                "commit project runtime retirement fault",
                "project retirement reservation was already consumed".to_owned(),
            )
        })?;
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "commit project runtime retirement fault",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::Retiring)
        ) {
            return Err(session_registry_error(
                "commit project runtime retirement fault",
                "project retirement fence disappeared before terminal fault".to_owned(),
            ));
        }
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::Faulted(ProjectRuntimeFaultedOwnersV1 {
                retained,
                sessions: self.sessions.take(),
                fault,
            }),
        );
        self.armed = false;
        Ok(())
    }
}

impl Drop for ProjectRuntimeOwnerRetirementReservationV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(mut retained) = self.retained.take() else {
            return;
        };
        if self
            .sessions
            .as_ref()
            .is_some_and(|sessions| sessions.store_target.is_none())
        {
            let mut entries = self
                .owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(
                entries.get(&self.project_id),
                Some(ProjectRuntimeOwnerStateV1::Retiring)
            ) {
                entries.insert(
                    self.project_id.clone(),
                    ProjectRuntimeOwnerStateV1::Faulted(ProjectRuntimeFaultedOwnersV1 {
                        retained,
                        sessions: self.sessions.take(),
                        fault: ProjectRuntimeRetirementFaultV1::ReservationTargetConsumed,
                    }),
                );
            }
            self.armed = false;
            return;
        }
        if let Some(sessions) = self.sessions.take() {
            match sessions.into_ready() {
                Ok(sessions) => retained.sessions = Some(sessions),
                Err(_) => {
                    self.armed = false;
                    return;
                }
            }
        }
        let mut entries = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::Retiring)
        ) {
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::Ready(retained),
            );
        }
        self.armed = false;
    }
}

/// Exclusive recovery ownership for one mounted project session authority.
///
/// The reservation removes the exact session owner from serving admission
/// before replay/sync quiescence or native retirement begins. Reversible
/// callers must explicitly restore the paired Store target; an abandoned
/// reservation is fail-closed as `RecoveryRequired`, never a fabricated Ready
/// replacement.
struct ProjectSessionReplacementReservationV1 {
    owners: ProjectRuntimeOwnerRegistryV1,
    project_id: ProjectId,
    sessions: Option<ProjectSessionRetirementOwnerV1>,
    memory: Option<MemoryStoreOwnerV1>,
    /// A candidate is only retried after the previous owner has exact paired
    /// terminal proofs. Reversible retirement must keep both this proof and
    /// the candidate in `RecoveryRequired`; it must never reopen the candidate.
    recovery_proof: Option<ProjectSessionTerminalProofV1>,
    armed: bool,
}

impl ProjectSessionReplacementReservationV1 {
    fn replay_descriptor(
        &self,
        path: std::path::PathBuf,
    ) -> Result<(
        tracedecay_global_db::RegisteredGlobalDbWeakLeaseIssuerV1,
        tracedecay_store::StoreRuntimeBindingV1,
        tracedecay_store::VerifiedStoreLocatorV1,
        std::path::PathBuf,
    )> {
        let session = self.sessions.as_ref().ok_or_else(|| {
            session_registry_error(
                "describe replacing project replay target",
                "project session replacement has no retained session owner".to_owned(),
            )
        })?;
        Ok((
            session.database.weak_lease_issuer(),
            session.database.registered_binding().clone(),
            session.database.registered_verified_locator().clone(),
            path,
        ))
    }

    fn is_recovered_candidate(&self) -> bool {
        self.recovery_proof.is_some()
    }

    fn issue_old_lease(&self) -> Result<RegisteredGlobalDbLeaseV1> {
        {
            let entries = self.owners.lock().map_err(|_| {
                session_registry_error(
                    "issue replacing project session lease",
                    "project runtime owner map lock is poisoned".to_owned(),
                )
            })?;
            if !matches!(
                entries.get(&self.project_id),
                Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
            ) {
                return Err(session_registry_error(
                    "issue replacing project session lease",
                    "project session replacement reservation lost its map fence".to_owned(),
                ));
            }
        }
        let session = self.sessions.as_ref().ok_or_else(|| {
            session_registry_error(
                "issue replacing project session lease",
                "project session replacement has no retained session owner".to_owned(),
            )
        })?;
        let database = session.database.issue_lease().map_err(|error| {
            session_registry_error(
                "issue replacing project session database client",
                format!("{error:?}"),
            )
        })?;
        let graph = session.graph.issue_lease().map_err(|error| {
            session_registry_error(
                "issue replacing project relation graph client",
                error.to_string(),
            )
        })?;
        database
            .bind_session_relation_graph(
                SessionRelationScope::project_sessions(self.project_id.clone()),
                graph,
                session.graph.binding().clone(),
                session.graph.verified_locator().clone(),
            )
            .map_err(|_| {
                session_registry_error(
                    "bind replacing project relation graph client",
                    "issued graph client did not match the exact replacing owner".to_owned(),
                )
            })?;
        Ok(database)
    }

    fn detach_old_relation_graph(&self) -> Result<()> {
        let session = self.sessions.as_ref().ok_or_else(|| {
            session_registry_error(
                "detach replacing project relation graph",
                "project session replacement has no retained session owner".to_owned(),
            )
        })?;
        session.database.detach_session_relation_graph();
        Ok(())
    }

    fn graph_retirement_target(&self) -> Result<tracedecay_graph_db::GraphDbRetirementTarget> {
        self.sessions
            .as_ref()
            .map(|session| session.graph.retirement_target())
            .ok_or_else(|| {
                session_registry_error(
                    "reserve replacing project relation graph",
                    "project session replacement has no retained graph owner".to_owned(),
                )
            })
    }

    fn reserve_store_target(
        &mut self,
    ) -> Result<tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementTarget>
    {
        let session = self.sessions.as_mut().ok_or_else(|| {
            session_registry_error(
                "reserve replacing project Store runtime",
                "project session replacement has no retained session owner".to_owned(),
            )
        })?;
        let database = session.database.reserve_retirement().map_err(|error| {
            session_registry_error(
                "reserve replacing project database runtime",
                format!("{error:?}"),
            )
        })?;
        let graph = session.take_store_target()?;
        match database.into_store_retirement_target_with_graph(graph) {
            Ok(target) => Ok(target),
            Err(refusal) => {
                let (error, database, graph) = refusal.into_parts();
                drop(database);
                session.restore_store_target(graph)?;
                Err(session_registry_error(
                    "compose replacing project Store runtime retirement",
                    format!("{error:?}"),
                ))
            }
        }
    }

    fn restore_store_target(
        &mut self,
        target: CanonicalGraphStoreOwnerRetirementTargetV1,
    ) -> Result<()> {
        self.sessions
            .as_mut()
            .ok_or_else(|| {
                session_registry_error(
                    "restore replacing project session Store target",
                    "project session replacement has no retained session owner".to_owned(),
                )
            })?
            .restore_store_target(target)
    }

    fn restore_old_ready(&mut self) -> Result<()> {
        let session = self.sessions.take().ok_or_else(|| {
            session_registry_error(
                "restore replacing project session owner",
                "project session replacement has no retained session owner".to_owned(),
            )
        })?;
        let sessions = session.into_ready()?;
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "restore replacing project session owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            return Err(session_registry_error(
                "restore replacing project session owner",
                "project session replacement map fence disappeared".to_owned(),
            ));
        }
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::Ready(ProjectRuntimeOwnersV1 {
                sessions: Some(sessions),
                memory: self.memory.take(),
            }),
        );
        self.armed = false;
        Ok(())
    }

    fn commit_recovery_required(mut self, phase: ProjectSessionRecoveryPhaseV1) -> Result<()> {
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "commit project session recovery required",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            return Err(session_registry_error(
                "commit project session recovery required",
                "project session replacement map fence disappeared".to_owned(),
            ));
        }
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::RecoveryRequired(ProjectSessionRecoveryRequiredV1 {
                sessions: self.sessions.take(),
                candidate_sessions: None,
                memory: self.memory.take(),
                phase,
            }),
        );
        self.armed = false;
        Ok(())
    }

    /// Restores a previously activated-but-unpublished candidate to its
    /// terminal recovery record after a reversible retirement refusal. The
    /// candidate is deliberately not converted back to a serving owner.
    fn commit_recovered_candidate_required(&mut self) -> Result<()> {
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "retain recovered project session candidate",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            return Err(session_registry_error(
                "retain recovered project session candidate",
                "project session replacement map fence disappeared".to_owned(),
            ));
        }
        let Some(proof) = self.recovery_proof.as_ref() else {
            return Err(session_registry_error(
                "retain recovered project session candidate",
                "replacement did not retain its prior terminal proof".to_owned(),
            ));
        };
        if !proof.verify() {
            return Err(session_registry_error(
                "retain recovered project session candidate",
                "replacement prior terminal proof is not exact and closed".to_owned(),
            ));
        }
        let session = self.sessions.take().ok_or_else(|| {
            session_registry_error(
                "retain recovered project session candidate",
                "replacement lost the candidate owner".to_owned(),
            )
        })?;
        let ProjectSessionRetirementOwnerV1 {
            database,
            graph,
            store_target,
            graph_open_task_key,
        } = session;
        let Some(store_target) = store_target else {
            self.sessions = Some(ProjectSessionRetirementOwnerV1 {
                database,
                graph,
                store_target: None,
                graph_open_task_key,
            });
            return Err(session_registry_error(
                "retain recovered project session candidate",
                "candidate Store target reached an irreversible boundary".to_owned(),
            ));
        };
        let proof = self.recovery_proof.take().ok_or_else(|| {
            session_registry_error(
                "retain recovered project session candidate",
                "replacement lost its prior terminal proof".to_owned(),
            )
        })?;
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::RecoveryRequired(ProjectSessionRecoveryRequiredV1 {
                sessions: None,
                candidate_sessions: Some(RegisteredSessionOwnerV1::with_attached_graph(
                    database,
                    SessionGraphOwnerV1 {
                        graph,
                        store_target,
                    },
                    graph_open_task_key,
                )),
                memory: self.memory.take(),
                phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
            }),
        );
        self.armed = false;
        Ok(())
    }

    fn into_vacancy(
        mut self,
        graph: GraphDbRetirementCommit,
        store: StoreRuntimeRetirementCommit,
    ) -> Result<ProjectSessionReplacementVacancyV1> {
        let session = self.sessions.as_ref().ok_or_else(|| {
            session_registry_error(
                "vacate replacing project session owner",
                "project session replacement has no retained session owner".to_owned(),
            )
        })?;
        let proof = ProjectSessionClosedRetirementProofV1 {
            binding: session.database.registered_binding().clone(),
            locator: session.graph.verified_locator().clone(),
            graph,
            store,
        };
        if !proof.verify() {
            self.commit_recovery_required(ProjectSessionRecoveryPhaseV1::Terminal(
                ProjectSessionTerminalProofV1::Live(Box::new(proof)),
            ))?;
            return Err(session_registry_error(
                "vacate replacing project session owner",
                "project graph and Store retirement must both close before replacement".to_owned(),
            ));
        }
        let entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "vacate replacing project session owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            return Err(session_registry_error(
                "vacate replacing project session owner",
                "project session replacement map fence disappeared".to_owned(),
            ));
        }
        drop(entries);
        self.sessions.take();
        // The new exact proof supersedes the proof which admitted this
        // candidate into recovery.
        self.recovery_proof.take();
        self.armed = false;
        Ok(ProjectSessionReplacementVacancyV1 {
            owners: self.owners.clone(),
            project_id: self.project_id.clone(),
            memory: self.memory.take(),
            proof: Some(ProjectSessionTerminalProofV1::Live(Box::new(proof))),
            armed: true,
        })
    }
}

impl Drop for ProjectSessionReplacementReservationV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.recovery_proof.is_some() && self.commit_recovered_candidate_required().is_ok() {
            return;
        }
        let mut entries = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            let phase = match self.recovery_proof.take() {
                Some(proof) => ProjectSessionRecoveryPhaseV1::Terminal(proof),
                None => ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
            };
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::RecoveryRequired(ProjectSessionRecoveryRequiredV1 {
                    sessions: self.sessions.take(),
                    candidate_sessions: None,
                    memory: self.memory.take(),
                    phase,
                }),
            );
        }
        self.armed = false;
    }
}

/// A verified all-closed session slot. A fresh owner may be published only as
/// the last recovery step; cancellation before publication remains fail-closed.
struct ProjectSessionReplacementVacancyV1 {
    owners: ProjectRuntimeOwnerRegistryV1,
    project_id: ProjectId,
    memory: Option<MemoryStoreOwnerV1>,
    proof: Option<ProjectSessionTerminalProofV1>,
    armed: bool,
}

impl ProjectSessionReplacementVacancyV1 {
    fn require_verified_proof(&self) -> Result<()> {
        let proof = self.proof.as_ref().ok_or_else(|| {
            session_registry_error(
                "use project session replacement vacancy",
                "project session replacement lost its terminal close proof".to_owned(),
            )
        })?;
        if proof.verify() {
            Ok(())
        } else {
            Err(session_registry_error(
                "use project session replacement vacancy",
                "project session replacement terminal proof is not exact and closed".to_owned(),
            ))
        }
    }

    pub fn durable_terminal_authority(&self) -> Result<ProjectSessionTerminalVacancyAuthorityV1> {
        self.require_verified_proof()?;
        self.proof
            .as_ref()
            .map(ProjectSessionTerminalProofV1::durable_authority)
            .ok_or_else(|| {
                session_registry_error(
                    "persist terminal remote recovery vacancy",
                    "project session replacement lost its terminal close proof".to_owned(),
                )
            })
    }

    /// Makes an opened candidate map-owned before an async replay or sync
    /// bind. Once this returns, cancellation may only leave the candidate in
    /// `RecoveryRequired`; it cannot drop the sole owner after a downstream
    /// service has retained one of its counted leases.
    fn begin_candidate_activation(
        mut self,
        sessions: RegisteredSessionOwnerV1,
    ) -> Result<ProjectSessionCandidateActivationV1> {
        self.require_verified_proof()?;
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "begin recovered project session activation",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            return Err(session_registry_error(
                "begin recovered project session activation",
                "project session replacement map fence disappeared".to_owned(),
            ));
        }
        let proof = self.proof.take().ok_or_else(|| {
            session_registry_error(
                "begin recovered project session activation",
                "project session replacement lost its terminal close proof".to_owned(),
            )
        })?;
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::RecoveryRequired(ProjectSessionRecoveryRequiredV1 {
                sessions: None,
                candidate_sessions: Some(sessions),
                memory: self.memory.take(),
                phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
            }),
        );
        self.armed = false;
        Ok(ProjectSessionCandidateActivationV1 {
            owners: self.owners.clone(),
            project_id: self.project_id.clone(),
        })
    }

    fn commit_without_sessions(mut self) -> Result<()> {
        self.require_verified_proof()?;
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "complete project session retirement",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            return Err(session_registry_error(
                "complete project session retirement",
                "project session replacement map fence disappeared".to_owned(),
            ));
        }
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::Ready(ProjectRuntimeOwnersV1 {
                sessions: None,
                memory: self.memory.take(),
            }),
        );
        self.proof.take();
        self.armed = false;
        Ok(())
    }

    fn retain_candidate_for_recovery(mut self, sessions: RegisteredSessionOwnerV1) -> Result<()> {
        self.require_verified_proof()?;
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "retain recovered project session owner candidate",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            return Err(session_registry_error(
                "retain recovered project session owner candidate",
                "project session replacement map fence disappeared".to_owned(),
            ));
        }
        let proof = self.proof.take().ok_or_else(|| {
            session_registry_error(
                "retain recovered project session owner candidate",
                "project session replacement lost its terminal close proof".to_owned(),
            )
        })?;
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::RecoveryRequired(ProjectSessionRecoveryRequiredV1 {
                sessions: None,
                candidate_sessions: Some(sessions),
                memory: self.memory.take(),
                phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
            }),
        );
        self.armed = false;
        Ok(())
    }
}

/// Map-owned activation of a post-close replacement candidate. This guard
/// intentionally owns no database, graph, or Store target itself: its sole
/// authority is the `RecoveryRequired` map entry installed before any await.
struct ProjectSessionCandidateActivationV1 {
    owners: ProjectRuntimeOwnerRegistryV1,
    project_id: ProjectId,
}

impl ProjectSessionCandidateActivationV1 {
    fn issue_lease_with_replay_descriptor(
        &self,
    ) -> Result<(
        RegisteredGlobalDbLeaseV1,
        tracedecay_global_db::RegisteredGlobalDbWeakLeaseIssuerV1,
        tracedecay_store::StoreRuntimeBindingV1,
        tracedecay_store::VerifiedStoreLocatorV1,
    )> {
        let entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "issue recovered project session candidate lease",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery)) =
            entries.get(&self.project_id)
        else {
            return Err(session_registry_error(
                "issue recovered project session candidate lease",
                "project session candidate activation lost its recovery fence".to_owned(),
            ));
        };
        if recovery.sessions.is_some() {
            return Err(session_registry_error(
                "issue recovered project session candidate lease",
                "project session candidate recovery still retains a nonterminal owner".to_owned(),
            ));
        }
        let Some(candidate) = recovery.candidate_sessions.as_ref() else {
            return Err(session_registry_error(
                "issue recovered project session candidate lease",
                "project session candidate activation lost its owner".to_owned(),
            ));
        };
        let ProjectSessionRecoveryPhaseV1::Terminal(proof) = &recovery.phase else {
            return Err(session_registry_error(
                "issue recovered project session candidate lease",
                "project session candidate activation lacks a terminal close proof".to_owned(),
            ));
        };
        if !proof.verify() {
            return Err(session_registry_error(
                "issue recovered project session candidate lease",
                "project session candidate activation terminal proof is invalid".to_owned(),
            ));
        }
        let database = candidate.issue_lease(SessionRelationScope::project_sessions(
            self.project_id.clone(),
        ))?;
        Ok((
            database,
            candidate.database.weak_lease_issuer(),
            candidate.database.registered_binding().clone(),
            candidate.database.registered_verified_locator().clone(),
        ))
    }

    fn publish(self) -> Result<()> {
        let mut entries = self.owners.lock().map_err(|_| {
            session_registry_error(
                "publish recovered project session owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = entries.remove(&self.project_id) else {
            return Err(session_registry_error(
                "publish recovered project session owner",
                "project session candidate activation lost its recovery fence".to_owned(),
            ));
        };
        let ProjectRuntimeOwnerStateV1::RecoveryRequired(mut recovery) = state else {
            entries.insert(self.project_id.clone(), state);
            return Err(session_registry_error(
                "publish recovered project session owner",
                "project session candidate activation state changed before publication".to_owned(),
            ));
        };
        let Some(candidate) = recovery.candidate_sessions.take() else {
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery),
            );
            return Err(session_registry_error(
                "publish recovered project session owner",
                "project session candidate activation lost its owner".to_owned(),
            ));
        };
        let ProjectSessionRecoveryPhaseV1::Terminal(proof) = &recovery.phase else {
            recovery.candidate_sessions = Some(candidate);
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery),
            );
            return Err(session_registry_error(
                "publish recovered project session owner",
                "project session candidate activation lacks a terminal close proof".to_owned(),
            ));
        };
        if !proof.verify() || recovery.sessions.is_some() {
            recovery.candidate_sessions = Some(candidate);
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery),
            );
            return Err(session_registry_error(
                "publish recovered project session owner",
                "project session candidate activation terminal proof is invalid".to_owned(),
            ));
        }
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::Ready(ProjectRuntimeOwnersV1 {
                sessions: Some(candidate),
                memory: recovery.memory,
            }),
        );
        Ok(())
    }
}

impl Drop for ProjectSessionCandidateActivationV1 {
    fn drop(&mut self) {
        // The candidate and exact terminal proof were stored in
        // `RecoveryRequired` before this guard became observable. Dropping the
        // guard therefore deliberately preserves that map-owned state.
    }
}

impl Drop for ProjectSessionReplacementVacancyV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut entries = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::ReplacingSessions)
        ) {
            let phase = match self.proof.take() {
                Some(proof) => ProjectSessionRecoveryPhaseV1::Terminal(proof),
                None => ProjectSessionRecoveryPhaseV1::ReservationAbandoned,
            };
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::RecoveryRequired(ProjectSessionRecoveryRequiredV1 {
                    sessions: None,
                    candidate_sessions: None,
                    memory: self.memory.take(),
                    phase,
                }),
            );
        }
        self.armed = false;
    }
}

/// Atomically moves one fail-closed terminal recovery record out of the map.
/// Its drop path restores the exact record, including terminal receipts and a
/// candidate owner, so cancellation cannot turn recovery into a replacement.
struct ProjectSessionRecoveryReservationV1 {
    owners: ProjectRuntimeOwnerRegistryV1,
    project_id: ProjectId,
    recovery: Option<ProjectSessionRecoveryRequiredV1>,
    armed: bool,
}

impl ProjectSessionRecoveryReservationV1 {
    fn has_candidate(&self) -> bool {
        self.recovery
            .as_ref()
            .is_some_and(|recovery| recovery.candidate_sessions.is_some())
    }

    fn into_candidate_replacement(mut self) -> Result<ProjectSessionReplacementReservationV1> {
        let recovery = self.recovery.take().ok_or_else(|| {
            session_registry_error(
                "retire recovered project session candidate",
                "project session recovery reservation was already consumed".to_owned(),
            )
        })?;
        let ProjectSessionRecoveryRequiredV1 {
            sessions,
            candidate_sessions,
            memory,
            phase,
        } = recovery;
        let candidate = match candidate_sessions {
            Some(candidate) => candidate,
            None => {
                self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                    sessions,
                    candidate_sessions: None,
                    memory,
                    phase,
                });
                return Err(session_registry_error(
                    "retire recovered project session candidate",
                    "project recovery does not retain a candidate owner".to_owned(),
                ));
            }
        };
        if sessions.is_some() {
            self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                sessions,
                candidate_sessions: Some(candidate),
                memory,
                phase,
            });
            return Err(session_registry_error(
                "retire recovered project session candidate",
                "recovery still retains a nonterminal old owner".to_owned(),
            ));
        }
        let proof = match phase {
            ProjectSessionRecoveryPhaseV1::Terminal(proof) if proof.verify() => proof,
            phase => {
                let inspection = phase.inspection();
                self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                    sessions,
                    candidate_sessions: Some(candidate),
                    memory,
                    phase,
                });
                return Err(session_registry_error(
                    "retire recovered project session candidate",
                    format!(
                        "recovery does not retain exact closed proofs for the prior owner: {}",
                        inspection.description(),
                    ),
                ));
            }
        };
        let mut entries = match self.owners.lock() {
            Ok(entries) => entries,
            Err(_) => {
                self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                    sessions,
                    candidate_sessions: Some(candidate),
                    memory,
                    phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
                });
                return Err(session_registry_error(
                    "retire recovered project session candidate",
                    "project runtime owner map lock is poisoned".to_owned(),
                ));
            }
        };
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::Recovering)
        ) {
            drop(entries);
            self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                sessions,
                candidate_sessions: Some(candidate),
                memory,
                phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
            });
            return Err(session_registry_error(
                "retire recovered project session candidate",
                "project session recovery map fence disappeared".to_owned(),
            ));
        }
        let candidate = match ProjectSessionRetirementOwnerV1::from_ready(candidate) {
            Ok(candidate) => candidate,
            Err(candidate) => {
                entries.insert(
                    self.project_id.clone(),
                    ProjectRuntimeOwnerStateV1::Recovering,
                );
                drop(entries);
                self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                    sessions,
                    candidate_sessions: Some(candidate),
                    memory,
                    phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
                });
                return Err(session_registry_error(
                    "retire recovered project session candidate",
                    "recovered candidate relation graph is not attached".to_owned(),
                ));
            }
        };
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::ReplacingSessions,
        );
        drop(entries);
        self.armed = false;
        Ok(ProjectSessionReplacementReservationV1 {
            owners: self.owners.clone(),
            project_id: self.project_id.clone(),
            sessions: Some(candidate),
            memory,
            recovery_proof: Some(proof),
            armed: true,
        })
    }

    fn into_terminal_vacancy(
        mut self,
    ) -> Result<(
        ProjectSessionReplacementVacancyV1,
        Option<RegisteredSessionOwnerV1>,
    )> {
        let recovery = self.recovery.take().ok_or_else(|| {
            session_registry_error(
                "resume project session recovery",
                "project session recovery reservation was already consumed".to_owned(),
            )
        })?;
        let ProjectSessionRecoveryRequiredV1 {
            sessions,
            candidate_sessions,
            memory,
            phase,
        } = recovery;
        if sessions.is_some() {
            self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                sessions,
                candidate_sessions,
                memory,
                phase,
            });
            return Err(session_registry_error(
                "resume project session recovery",
                "old project session owner is not terminal and cannot enter vacancy".to_owned(),
            ));
        }
        let proof = match phase {
            ProjectSessionRecoveryPhaseV1::Terminal(proof) => proof,
            phase => {
                let inspection = phase.inspection();
                self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                    sessions,
                    candidate_sessions,
                    memory,
                    phase,
                });
                return Err(session_registry_error(
                    "resume project session recovery",
                    format!(
                        "project session recovery does not retain exact closed graph and Store proofs: {}",
                        inspection.description(),
                    ),
                ));
            }
        };
        if !proof.verify() {
            self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                sessions,
                candidate_sessions,
                memory,
                phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
            });
            return Err(session_registry_error(
                "resume project session recovery",
                "project session terminal recovery proof is not exact and closed".to_owned(),
            ));
        }
        let mut entries = match self.owners.lock() {
            Ok(entries) => entries,
            Err(_) => {
                self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                    sessions,
                    candidate_sessions,
                    memory,
                    phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
                });
                return Err(session_registry_error(
                    "resume project session recovery",
                    "project runtime owner map lock is poisoned".to_owned(),
                ));
            }
        };
        if !matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::Recovering)
        ) {
            drop(entries);
            self.recovery = Some(ProjectSessionRecoveryRequiredV1 {
                sessions,
                candidate_sessions,
                memory,
                phase: ProjectSessionRecoveryPhaseV1::Terminal(proof),
            });
            return Err(session_registry_error(
                "resume project session recovery",
                "project session recovery map fence disappeared".to_owned(),
            ));
        }
        entries.insert(
            self.project_id.clone(),
            ProjectRuntimeOwnerStateV1::ReplacingSessions,
        );
        drop(entries);
        self.armed = false;
        Ok((
            ProjectSessionReplacementVacancyV1 {
                owners: self.owners.clone(),
                project_id: self.project_id.clone(),
                memory,
                proof: Some(proof),
                armed: true,
            },
            candidate_sessions,
        ))
    }
}

impl Drop for ProjectSessionRecoveryReservationV1 {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(recovery) = self.recovery.take() else {
            return;
        };
        let mut entries = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(
            entries.get(&self.project_id),
            Some(ProjectRuntimeOwnerStateV1::Recovering)
        ) {
            entries.insert(
                self.project_id.clone(),
                ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery),
            );
        }
        self.armed = false;
    }
}

/// One composite pre-native session retirement attempt. It keeps every lower
/// reservation recoverable until Graph/Store terminalization is deliberately
/// accepted, so task cancellation cannot strand a paired Store target outside
/// the map reservation which owns it.
struct ProjectSessionNativeRetirementV1 {
    replacement: Option<ProjectSessionReplacementReservationV1>,
    graph: Option<tracedecay_graph_db::GraphDbRetirementReservation>,
    store:
        Option<tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementReservation>,
    graph_native_boundary: bool,
}

impl ProjectSessionNativeRetirementV1 {
    fn new(
        replacement: ProjectSessionReplacementReservationV1,
        graph: tracedecay_graph_db::GraphDbRetirementReservation,
        store: tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementReservation,
    ) -> Self {
        Self {
            replacement: Some(replacement),
            graph: Some(graph),
            store: Some(store),
            graph_native_boundary: false,
        }
    }

    fn graph_mut(&mut self) -> Result<&mut tracedecay_graph_db::GraphDbRetirementReservation> {
        self.graph.as_mut().ok_or_else(|| {
            session_registry_error(
                "retire replacing project graph owner",
                "project graph retirement reservation was already consumed".to_owned(),
            )
        })
    }

    fn store_mut(
        &mut self,
    ) -> Result<
        &mut tracedecay_runtime_core::store_runtime::registry::StoreRuntimeRetirementReservation,
    > {
        self.store.as_mut().ok_or_else(|| {
            session_registry_error(
                "retire replacing project Store owner",
                "project Store retirement reservation was already consumed".to_owned(),
            )
        })
    }

    fn mark_graph_native_boundary(&mut self) {
        self.graph_native_boundary = true;
    }

    fn cancel_before_native(mut self) -> Result<ProjectSessionReplacementReservationV1> {
        if self.graph_native_boundary {
            return Err(session_registry_error(
                "cancel replacing project session retirement",
                "project graph retirement already crossed its native boundary".to_owned(),
            ));
        }
        let mut store = self.store.take().ok_or_else(|| {
            session_registry_error(
                "cancel replacing project session retirement",
                "project Store retirement reservation was already consumed".to_owned(),
            )
        })?;
        let mut targets = store.cancel().map_err(|error| {
            session_registry_error(
                "cancel replacing project Store retirement",
                format!("{error:?}"),
            )
        })?;
        let target = targets.pop().ok_or_else(|| {
            session_registry_error(
                "recover replacing project Store retirement",
                "Store cancellation omitted the exact paired target".to_owned(),
            )
        })?;
        if !targets.is_empty() {
            return Err(session_registry_error(
                "recover replacing project Store retirement",
                "Store cancellation returned an unexpected target count".to_owned(),
            ));
        }
        let handoff = target.into_database_graph_owner_handoff().map_err(|_| {
            session_registry_error(
                "recover replacing project Store retirement",
                "Store cancellation lost the exact database/graph handoff".to_owned(),
            )
        })?;
        let mut replacement = self.replacement.take().ok_or_else(|| {
            session_registry_error(
                "recover replacing project session owner",
                "project session native retirement guard was already consumed".to_owned(),
            )
        })?;
        replacement.restore_store_target(handoff.cancel_to_ready_graph_target())?;
        drop(self.graph.take());
        Ok(replacement)
    }

    fn recover_after_graph_native_boundary(mut self, graph: GraphDbRetirementCommit) -> Result<()> {
        if !self.graph_native_boundary {
            return Err(session_registry_error(
                "recover replacing project session retirement",
                "project graph retirement did not cross its native boundary".to_owned(),
            ));
        }
        let mut store = self.store.take().ok_or_else(|| {
            session_registry_error(
                "recover replacing project session retirement",
                "project Store retirement reservation was already consumed".to_owned(),
            )
        })?;
        let mut targets = store.cancel().map_err(|error| {
            session_registry_error(
                "cancel replacing project Store retirement after graph terminal",
                format!("{error:?}"),
            )
        })?;
        let target = targets.pop().ok_or_else(|| {
            session_registry_error(
                "recover replacing project Store retirement after graph terminal",
                "Store cancellation omitted the exact paired target".to_owned(),
            )
        })?;
        if !targets.is_empty() {
            return Err(session_registry_error(
                "recover replacing project Store retirement after graph terminal",
                "Store cancellation returned an unexpected target count".to_owned(),
            ));
        }
        let handoff = target.into_database_graph_owner_handoff().map_err(|_| {
            session_registry_error(
                "recover replacing project Store retirement after graph terminal",
                "Store cancellation lost the exact database/graph handoff".to_owned(),
            )
        })?;
        let mut replacement = self.replacement.take().ok_or_else(|| {
            session_registry_error(
                "recover replacing project session owner",
                "project session native retirement guard was already consumed".to_owned(),
            )
        })?;
        replacement.restore_store_target(handoff.cancel_to_ready_graph_target())?;
        drop(self.graph.take());
        replacement.commit_recovery_required(ProjectSessionRecoveryPhaseV1::GraphTerminal(graph))
    }

    fn into_vacancy(
        mut self,
        graph: GraphDbRetirementCommit,
        store: StoreRuntimeRetirementCommit,
    ) -> Result<ProjectSessionReplacementVacancyV1> {
        self.graph.take();
        self.store.take();
        let replacement = self.replacement.take().ok_or_else(|| {
            session_registry_error(
                "vacate replacing project session owner",
                "project session native retirement guard was already consumed".to_owned(),
            )
        })?;
        replacement.into_vacancy(graph, store)
    }
}

impl Drop for ProjectSessionNativeRetirementV1 {
    fn drop(&mut self) {
        let Some(mut replacement) = self.replacement.take() else {
            return;
        };
        let had_store = self.store.is_some();
        let recovered_target = match self.store.take() {
            Some(mut store) => match store.cancel() {
                Ok(mut targets) if targets.len() == 1 => match targets.pop() {
                    Some(target) => match target.into_database_graph_owner_handoff() {
                        Ok(handoff) => Some(handoff.cancel_to_ready_graph_target()),
                        Err(_) => None,
                    },
                    None => None,
                },
                Ok(_) | Err(_) => None,
            },
            None => None,
        };
        drop(self.graph.take());
        let project_id = replacement.project_id.clone();
        let restored = match recovered_target {
            Some(target) => replacement.restore_store_target(target).is_ok(),
            None if !had_store => true,
            None => false,
        };
        let phase = if self.graph_native_boundary {
            ProjectSessionRecoveryPhaseV1::GraphNativeBoundary
        } else {
            ProjectSessionRecoveryPhaseV1::ReservationAbandoned
        };
        let recovered_candidate = replacement.is_recovered_candidate();
        let recovered = if !restored {
            Err(session_registry_error(
                "retain recovered project session candidate",
                "project session native retirement could not restore the exact Store target"
                    .to_owned(),
            ))
        } else if !self.graph_native_boundary && recovered_candidate {
            replacement.commit_recovered_candidate_required()
        } else {
            replacement.commit_recovery_required(phase)
        };
        if recovered.is_err() {
            tracing::error!(
                project_id = %project_id,
                "project session retirement could not retain its recovery state"
            );
        }
    }
}

static LONG_LIVED_SESSION_MAINTENANCE: AtomicBool = AtomicBool::new(false);

fn remote_restore_quarantine_fence_path(database: &Path) -> std::path::PathBuf {
    database.with_extension("remote-restore-quarantine.json")
}

pub fn mark_process_long_lived_for_session_maintenance() {
    LONG_LIVED_SESSION_MAINTENANCE.store(true, Ordering::Relaxed);
}

/// Operator log for store-runtime schema and maintenance events.
///
/// Successful schema convergence (`outcome=complete`) is an ordinary
/// completion and must not occupy the default WARN surface. WARN is reserved
/// for degraded outcomes — a failed converge, a poisoned status lock, or a
/// memory-release failure after the audit.
pub fn log_store_runtime_event(event: &str, fields: &[(&str, String)]) {
    if store_runtime_event_is_success(fields) {
        tracing::info!(event, ?fields, "store-runtime event");
    } else {
        tracing::warn!(event, ?fields, "store-runtime event");
    }
}

fn store_runtime_event_is_success(fields: &[(&str, String)]) -> bool {
    fields
        .iter()
        .any(|(key, value)| *key == "outcome" && value == "complete")
}

#[cfg(test)]
mod store_runtime_event_level_tests {
    use super::store_runtime_event_is_success;

    #[test]
    fn successful_schema_convergence_is_not_an_anomaly() {
        assert!(store_runtime_event_is_success(&[
            ("outcome", "complete".to_owned()),
            ("database", "/tmp/registry.db".to_owned()),
        ]));
    }

    #[test]
    fn degraded_schema_convergence_stays_an_anomaly() {
        assert!(!store_runtime_event_is_success(&[
            ("outcome", "degraded".to_owned()),
            ("error", "authority invariant failed".to_owned()),
        ]));
    }

    #[test]
    fn events_without_a_complete_outcome_stay_loud() {
        assert!(!store_runtime_event_is_success(&[(
            "resource",
            "statuses".to_owned()
        ),]));
    }
}

pub fn release_process_allocator_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `malloc_trim` is a process-wide, thread-safe glibc allocator
        // maintenance operation. It does not invalidate live allocations.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    pub fn profile_id(&self) -> &tracedecay_domain::configuration::UserProfileId {
        self.identity.profile_id()
    }

    pub fn runtime_telemetry(
        &self,
    ) -> tracedecay_runtime_core::store_runtime::telemetry::RuntimeTelemetryProjection {
        let inventory = self.registry.inventory(AdmissionConfigV1::default(), None);
        tracedecay_runtime_core::store_runtime::telemetry::project_runtime_telemetry(&inventory)
    }

    #[hotpath::skip]
    async fn profile_authority_pin(&self, operation: &'static str) -> Result<ProfileAuthorityPin> {
        self.profile_pin
            .lock()
            .await
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                session_registry_error(
                    operation,
                    "profile authority pin has already entered retirement".to_owned(),
                )
            })
    }

    #[hotpath::measure(label = "daemon.session_registry.admit_remote_node")]
    fn admit_remote_node_owner(
        &self,
        node_id: &BrainNodeId,
    ) -> Result<RemoteNodeOwnerAdmissionV1<'_>> {
        let mut nodes = self.remote_nodes.lock().map_err(|_| {
            session_registry_error(
                "admit Remote Brain node owner",
                "remote node owner map lock is poisoned".to_owned(),
            )
        })?;
        match nodes.get(node_id) {
            Some(RemoteNodeOwnerStateV1::Ready(owner)) => {
                let database = owner.database.issue_lease().map_err(|error| {
                    session_registry_error(
                        "issue Remote Brain node database client",
                        format!("{error:?}"),
                    )
                })?;
                return Ok(RemoteNodeOwnerAdmissionV1::Existing(database));
            }
            Some(RemoteNodeOwnerStateV1::Opening) => {
                return Err(TraceDecayError::project_route(
                    "remote_runtime_opening",
                    true,
                    "Remote Brain node runtime is already opening",
                ));
            }
            None => {}
        }
        if nodes.len() >= MAX_RETAINED_REMOTE_NODE_OWNERS {
            return Err(TraceDecayError::project_route(
                "remote_runtime_capacity",
                true,
                format!(
                    "Remote Brain runtime owner capacity {MAX_RETAINED_REMOTE_NODE_OWNERS} is fully occupied"
                ),
            ));
        }
        nodes.insert(node_id.clone(), RemoteNodeOwnerStateV1::Opening);
        Ok(RemoteNodeOwnerAdmissionV1::Opening(
            RemoteNodeOwnerOpeningReservationV1 {
                nodes: &self.remote_nodes,
                node_id: node_id.clone(),
                armed: true,
            },
        ))
    }

    #[hotpath::measure(label = "daemon.session_registry.admit_project_runtime")]
    fn admit_project_runtime_owner(
        &self,
        project_id: &ProjectId,
    ) -> Result<ProjectRuntimeOwnerAdmissionV1> {
        let mut entries = self.project_owners.lock().map_err(|_| {
            session_registry_error(
                "admit project runtime owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        match entries.get(project_id) {
            Some(ProjectRuntimeOwnerStateV1::Ready(_)) => {
                return Ok(ProjectRuntimeOwnerAdmissionV1::Existing);
            }
            Some(ProjectRuntimeOwnerStateV1::Opening) => {
                return Err(TraceDecayError::project_route(
                    "project_runtime_opening",
                    true,
                    "Project runtime is already opening",
                ));
            }
            Some(ProjectRuntimeOwnerStateV1::Retiring) => {
                return Err(TraceDecayError::project_route(
                    "project_runtime_retiring",
                    true,
                    "Project runtime is retiring",
                ));
            }
            Some(
                ProjectRuntimeOwnerStateV1::ReplacingSessions
                | ProjectRuntimeOwnerStateV1::Recovering
                | ProjectRuntimeOwnerStateV1::RecoveryRequired(_),
            ) => {
                return Err(TraceDecayError::project_route(
                    "project_runtime_recovery",
                    true,
                    "Project runtime is replacing sessions or requires recovery",
                ));
            }
            Some(ProjectRuntimeOwnerStateV1::Faulted(_)) => {
                return Err(TraceDecayError::project_route(
                    "project_runtime_faulted",
                    true,
                    "Project runtime retirement reached a terminal fault",
                ));
            }
            None => {}
        }
        if entries.len() >= MAX_RETAINED_PROJECT_RUNTIME_OWNERS {
            return Err(TraceDecayError::project_route(
                "project_runtime_capacity",
                true,
                format!(
                    "Project runtime owner capacity {MAX_RETAINED_PROJECT_RUNTIME_OWNERS} is fully occupied"
                ),
            ));
        }
        entries.insert(project_id.clone(), ProjectRuntimeOwnerStateV1::Opening);
        Ok(ProjectRuntimeOwnerAdmissionV1::Opening(Box::new(
            ProjectRuntimeOwnerOpeningReservationV1 {
                owners: self.project_owners.clone(),
                project_id: project_id.clone(),
                previous: None,
                armed: true,
            },
        )))
    }

    #[hotpath::measure(label = "daemon.session_registry.extend_project_runtime")]
    fn extend_project_runtime_owner(
        &self,
        project_id: &ProjectId,
    ) -> Result<ProjectRuntimeOwnerAdmissionV1> {
        let mut entries = self.project_owners.lock().map_err(|_| {
            session_registry_error(
                "extend project runtime owner",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = entries.remove(project_id) else {
            if entries.len() >= MAX_RETAINED_PROJECT_RUNTIME_OWNERS {
                return Err(TraceDecayError::project_route(
                    "project_runtime_capacity",
                    true,
                    format!(
                        "Project runtime owner capacity {MAX_RETAINED_PROJECT_RUNTIME_OWNERS} is fully occupied"
                    ),
                ));
            }
            entries.insert(project_id.clone(), ProjectRuntimeOwnerStateV1::Opening);
            return Ok(ProjectRuntimeOwnerAdmissionV1::Opening(Box::new(
                ProjectRuntimeOwnerOpeningReservationV1 {
                    owners: self.project_owners.clone(),
                    project_id: project_id.clone(),
                    previous: None,
                    armed: true,
                },
            )));
        };
        let ProjectRuntimeOwnerStateV1::Ready(previous) = state else {
            entries.insert(project_id.clone(), state);
            return Err(TraceDecayError::project_route(
                "project_runtime_opening",
                true,
                "Project runtime is already opening",
            ));
        };
        entries.insert(project_id.clone(), ProjectRuntimeOwnerStateV1::Opening);
        Ok(ProjectRuntimeOwnerAdmissionV1::Opening(Box::new(
            ProjectRuntimeOwnerOpeningReservationV1 {
                owners: self.project_owners.clone(),
                project_id: project_id.clone(),
                previous: Some(previous),
                armed: true,
            },
        )))
    }

    #[hotpath::measure(label = "daemon.session_registry.reserve_runtime_retirement")]
    fn reserve_project_runtime_retirement(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectRuntimeOwnerRetirementReservationV1>> {
        let mut entries = self.project_owners.lock().map_err(|_| {
            session_registry_error(
                "reserve project runtime retirement",
                "project runtime owner map lock is poisoned".to_owned(),
            )
        })?;
        let Some(state) = entries.remove(project_id) else {
            return Ok(None);
        };
        let ProjectRuntimeOwnerStateV1::Ready(mut owners) = state else {
            entries.insert(project_id.clone(), state);
            return Err(TraceDecayError::project_route(
                "project_runtime_retiring",
                true,
                "Project runtime is not accepting retirement admission",
            ));
        };
        let sessions = match owners.sessions.take() {
            Some(owner) => match ProjectSessionRetirementOwnerV1::from_ready(owner) {
                Ok(sessions) => Some(sessions),
                Err(owner) => {
                    owners.sessions = Some(owner);
                    None
                }
            },
            None => None,
        };
        entries.insert(project_id.clone(), ProjectRuntimeOwnerStateV1::Retiring);
        Ok(Some(ProjectRuntimeOwnerRetirementReservationV1 {
            owners: self.project_owners.clone(),
            project_id: project_id.clone(),
            retained: Some(owners),
            sessions,
            armed: true,
        }))
    }

    #[hotpath::skip]
    async fn reserve_project_session_replacement(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<ProjectSessionReplacementReservationV1>> {
        self.project_owners
            .wait_for_session_graph(project_id)
            .await?;
        self.project_owners.reserve_session_replacement(project_id)
    }
}

/// One canonical registry and profile pin shared by every daemon session shard.
pub struct DaemonSessionRuntimeRegistryV1 {
    identity: LocalProfileIdentityAuthorityV1,
    incarnation: StoreIncarnationV1,
    resolver: Arc<LocalStoreRuntimeResolverV1>,
    registry: StoreRuntimeRegistry,
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    graph_manifest_provider: Arc<code_graph_manifest::DaemonCodeGraphManifestProviderV1>,
    graph_lifecycle_cancelled: Arc<AtomicBool>,
    profile_pin: Mutex<Option<ProfileAuthorityPin>>,
    profile_database_mount: Mutex<()>,
    profile_database: StdMutex<Option<RegisteredGlobalDbOwnerV1>>,
    profile_memory: StdMutex<Option<MemoryStoreOwnerV1>>,
    profile_sessions_mount: Mutex<()>,
    profile_sessions: StdMutex<Option<RegisteredSessionOwnerV1>>,
    remote_nodes: StdMutex<BTreeMap<BrainNodeId, RemoteNodeOwnerStateV1>>,
    remote_credential_authority: Arc<crate::DaemonRemoteCredentialAuthorityV1>,
    remote_replay_transaction:
        Arc<crate::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1>,
    remote_recovery_authorities: Mutex<
        BTreeMap<
            BrainNodeId,
            Arc<tracedecay_rusqlite_runtime::remote::RemoteRecoverySqliteAuthorityV1>,
        >,
    >,
    project_owners: ProjectRuntimeOwnerRegistryV1,
    /// One set of graph-publication locks per project publication shard.
    /// Every worktree/branch scope of a project stages into the one shared
    /// staging store, so corpus-sized builds must serialize on the project
    /// shard rather than on `code_shard`. Entries are weak: a cell lives only
    /// while a retained runtime holds it. The locks live here because the
    /// retained code graph runtime itself is minted fresh per activation call.
    code_graph_publication_gates: StdMutex<
        BTreeMap<
            tracedecay_store::StoreShardIdV1,
            Weak<code_graph::CodeGraphShardPublicationLocksV1>,
        >,
    >,
    registered_schema_convergence: RegisteredSchemaConvergenceMaintenance,
    retained_hook_tasks: RetainedHookTasks,
    session_sync_service:
        Arc<OnceLock<Arc<tracedecay_session_runtime::session_sync::DaemonSessionSyncService>>>,
    remote_recovery_project_lifecycle: Arc<OnceLock<Arc<dyn RemoteRecoveryProjectLifecycle>>>,
    /// Fixed at construction: whether this registry's process runs long-lived
    /// session maintenance (background historical schema convergence) for the
    /// shards it attaches. Short-lived CLI/hook processes stay `false`.
    long_lived_session_maintenance: bool,
    /// Test-only hold applied immediately after each session relation-graph
    /// open task publishes its settled state.
    #[cfg(any(test, feature = "test-helpers"))]
    session_graph_publication_gate: StdMutex<Option<Arc<SessionGraphPublicationTestGateState>>>,
}

impl DaemonSessionRuntimeRegistryV1 {
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn lookup_store_runtime(
        &self,
        expected: &tracedecay_store::StoreRuntimeBindingV1,
    ) -> tracedecay_runtime_core::store_runtime::registry::StoreRuntimeLookup {
        self.registry.lookup(expected)
    }

    /// Whether the canonical graph registry can admit the code and session
    /// relation owners created by one previously unmounted project.
    pub fn has_project_graph_admission_capacity(&self) -> Result<bool> {
        let capacity = self.graph_registry.capacity().map_err(|error| {
            session_registry_error("read graph runtime capacity", error.to_string())
        })?;
        Ok(capacity.available_after_eviction() >= PROJECT_GRAPH_OWNER_ADMISSION_DEMAND)
    }
}

impl DaemonSessionRuntimeRegistryV1 {
    /// Awaits settlement (attached or detached) of the profile session
    /// relation graph, which `profile_sessions` opens as bounded background
    /// work. Production callers tolerate the warming window through typed
    /// retryable refusals; deterministic fixtures await settlement instead so
    /// graph-dependent operations do not race the open task.
    #[hotpath::measure(label = "daemon.session_registry.settle_profile_graph", future = true)]
    pub async fn settle_profile_session_graph(&self) -> Result<()> {
        let waiter = {
            let mounted = self
                .profile_sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            mounted.as_ref().map(|owner| {
                (
                    Arc::clone(&owner.relation_graph),
                    Arc::clone(&owner.graph_settled),
                )
            })
        };
        let Some((relation_graph, graph_settled)) = waiter else {
            return Ok(());
        };
        loop {
            let notified = graph_settled.notified();
            let warming = matches!(
                &*relation_graph
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                SessionGraphAttachmentStateV1::Warming
            );
            if !warming {
                return Ok(());
            }
            notified.await;
        }
    }

    /// Project-scope counterpart of [`Self::settle_profile_session_graph`].
    #[hotpath::measure(label = "daemon.session_registry.settle_project_graph", future = true)]
    pub async fn settle_project_session_graph(&self, project_id: &ProjectId) -> Result<()> {
        self.project_owners.wait_for_session_graph(project_id).await
    }

    /// Waits for the project's background relation-graph open and requires an
    /// attached owner before a retrieval-capable surface is published.
    #[hotpath::measure(
        label = "daemon.session_registry.settle_project_graph_for_serving",
        future = true
    )]
    pub async fn settle_project_session_graph_for_serving(
        &self,
        project_id: &ProjectId,
    ) -> Result<()> {
        self.project_owners
            .wait_for_serving_session_graph(project_id)
            .await
    }

    pub fn install_session_sync_service(
        &self,
        service: &Arc<tracedecay_session_runtime::session_sync::DaemonSessionSyncService>,
    ) -> Result<()> {
        let service = Arc::clone(service);
        if let Some(retained) = self.session_sync_service.get() {
            return if Arc::ptr_eq(retained, &service) {
                Ok(())
            } else {
                Err(TraceDecayError::Config {
                    message:
                        "session runtime registry already has a different session sync service"
                            .to_owned(),
                })
            };
        }
        match self.session_sync_service.set(service) {
            Ok(()) => Ok(()),
            Err(service)
                if self
                    .session_sync_service
                    .get()
                    .is_some_and(|retained| Arc::ptr_eq(retained, &service)) =>
            {
                Ok(())
            }
            Err(_) => Err(TraceDecayError::Config {
                message: "session runtime registry session sync installation raced".to_owned(),
            }),
        }
    }

    pub fn install_remote_recovery_project_lifecycle(
        &self,
        lifecycle: Arc<dyn RemoteRecoveryProjectLifecycle>,
    ) -> Result<()> {
        if let Some(retained) = self.remote_recovery_project_lifecycle.get() {
            return if Arc::ptr_eq(retained, &lifecycle) {
                Ok(())
            } else {
                Err(TraceDecayError::Config {
                    message: "session runtime registry already has a different remote recovery project lifecycle".to_owned(),
                })
            };
        }
        match self.remote_recovery_project_lifecycle.set(lifecycle) {
            Ok(()) => Ok(()),
            Err(lifecycle)
                if self
                    .remote_recovery_project_lifecycle
                    .get()
                    .is_some_and(|retained| Arc::ptr_eq(retained, &lifecycle)) =>
            {
                Ok(())
            }
            Err(_) => Err(TraceDecayError::Config {
                message: "session runtime registry remote recovery lifecycle installation raced"
                    .to_owned(),
            }),
        }
    }

    fn session_sync_service(
        &self,
    ) -> Arc<OnceLock<Arc<tracedecay_session_runtime::session_sync::DaemonSessionSyncService>>>
    {
        Arc::clone(&self.session_sync_service)
    }

    fn active_session_sync_service(
        &self,
        operation: &'static str,
    ) -> Result<Arc<tracedecay_session_runtime::session_sync::DaemonSessionSyncService>> {
        self.session_sync_service.get().cloned().ok_or_else(|| {
            session_registry_error(
                operation,
                "project session sync authority is unavailable".to_owned(),
            )
        })
    }

    #[hotpath::measure(label = "daemon.session_registry.retire_session_sync", future = true)]
    async fn retire_project_session_sync(&self, project_id: &ProjectId) -> Result<()> {
        self.active_session_sync_service("retire project session sync")?
            .retire_project(self.identity.profile_id(), project_id)
            .await
            .map(|_| ())
            .map_err(|error| session_registry_error("retire project session sync", error))
    }

    #[hotpath::measure(label = "daemon.session_registry.rebind_session_sync", future = true)]
    async fn rebind_project_session_sync(
        &self,
        project_id: &ProjectId,
        database: &RegisteredGlobalDbLeaseV1,
    ) -> Result<()> {
        self.active_session_sync_service("rebind project session sync")?
            .rebind_project(self.identity.profile_id(), project_id, database)
            .await
            .map(|_| ())
            .map_err(|error| session_registry_error("rebind project session sync", error))
    }

    fn remote_recovery_project_lifecycle(
        &self,
    ) -> Arc<OnceLock<Arc<dyn RemoteRecoveryProjectLifecycle>>> {
        Arc::clone(&self.remote_recovery_project_lifecycle)
    }

    pub fn retain_hook_task<F, Fut>(&self, provider: &str, session_id: &str, operation: F) -> bool
    where
        F: FnOnce(ObservationCancellation) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        self.retained_hook_tasks
            .retain(provider, session_id, operation)
    }
}

impl ProfileRuntime for DaemonSessionRuntimeRegistryV1 {
    fn profile_id(&self) -> &tracedecay_domain::configuration::UserProfileId {
        self.identity.profile_id()
    }

    fn profile_sessions(&self) -> RuntimeFuture<'_, RegisteredGlobalDbLeaseV1> {
        Box::pin(DaemonSessionRuntimeRegistryV1::profile_sessions(self))
    }

    fn open_user_memory_db(&self) -> RuntimeFuture<'_, Database> {
        Box::pin(open_user_memory_db(self))
    }
}

#[hotpath::measure(label = "daemon.session_registry.runtime_incarnation")]
fn runtime_incarnation(identity: &LocalProfileIdentityAuthorityV1) -> Result<StoreIncarnationV1> {
    let process_run_id = tracedecay_runtime_core::runtime_identity::process_run_id();
    let daemon_generation =
        tracedecay_daemon_identity::authority::current_record(identity.profile_root())?
            .filter(|record| {
                record.process_run_id == process_run_id
                    && record.profile_root == identity.profile_root()
                    && record.brain_id.as_ref() == Some(identity.brain_id())
                    && record.profile_id.as_ref() == Some(identity.profile_id())
            })
            .map(|record| record.epoch);
    let generation = match daemon_generation {
        Some(generation) => generation,
        None => process_runtime_generation(process_run_id).ok_or_else(|| {
            session_registry_error(
                "create store incarnation",
                "process runtime generation has an unsupported format".to_owned(),
            )
        })?,
    };
    StoreIncarnationV1::new(generation)
        .map_err(|error| session_registry_error("create store incarnation", error.to_string()))
}

pub fn process_runtime_generation(process_run_id: &str) -> Option<u64> {
    let raw = process_run_id
        .get(..16)
        .and_then(|prefix| u64::from_str_radix(prefix, 16).ok())
        .or_else(|| {
            process_run_id
                .strip_prefix("mcp-")
                .and_then(|value| value.parse::<u64>().ok())
                .map(|timestamp| timestamp ^ u64::from(std::process::id()))
        })?;
    Some((raw & i64::MAX as u64).max(1))
}

struct StoreRuntimeOpenSpec {
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    database_authority: Option<DatabaseAuthority>,
    initialize_if_missing: bool,
    operation: &'static str,
}

impl StoreRuntimeOpenSpec {
    fn new(
        shard_id: StoreShardIdV1,
        incarnation: StoreIncarnationV1,
        profile_pin: Option<ProfileAuthorityPin>,
        database_authority: Option<DatabaseAuthority>,
        initialize_if_missing: bool,
        operation: &'static str,
    ) -> Self {
        Self {
            shard_id,
            incarnation,
            profile_pin,
            database_authority,
            initialize_if_missing,
            operation,
        }
    }
}

async fn open_runtime(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    spec: StoreRuntimeOpenSpec,
) -> Result<StoreRuntimeClientLease> {
    let StoreRuntimeOpenSpec {
        shard_id,
        incarnation,
        profile_pin,
        database_authority,
        initialize_if_missing,
        operation,
    } = spec;
    open_runtime_with_presence(
        registry,
        resolver,
        shard_id,
        incarnation,
        profile_pin,
        database_authority,
        initialize_if_missing,
        false,
        None,
        operation,
    )
    .await
    .map(|(runtime, _)| runtime)
}

async fn open_runtime_during_remote_restore(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    expected_opened_file_identity: u64,
    operation: &'static str,
) -> Result<StoreRuntimeClientLease> {
    open_runtime_with_presence(
        registry,
        resolver,
        shard_id,
        incarnation,
        profile_pin,
        None,
        false,
        true,
        Some(expected_opened_file_identity),
        operation,
    )
    .await
    .map(|(runtime, _)| runtime)
}

#[allow(clippy::too_many_arguments)]
async fn open_runtime_with_presence(
    registry: &StoreRuntimeRegistry,
    resolver: &LocalStoreRuntimeResolverV1,
    shard_id: StoreShardIdV1,
    incarnation: StoreIncarnationV1,
    profile_pin: Option<ProfileAuthorityPin>,
    database_authority: Option<DatabaseAuthority>,
    initialize_if_missing: bool,
    allow_remote_restore_fence: bool,
    required_opened_file_identity: Option<u64>,
    operation: &'static str,
) -> Result<(StoreRuntimeClientLease, bool)> {
    let key = StoreRuntimeKey::new(shard_id.clone(), incarnation);
    let locator = match hotpath::measure_block!(
        "daemon.session_registry.store_open.resolve",
        resolver.resolve_key(&key)
    ) {
        LocalStoreLocatorResolutionV1::Resolved(locator) => locator,
        LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.session_registry.store_open.failed_total").inc(1_u64);
            return Err(session_registry_error(
                operation,
                format!(
                    "registered store locator unavailable: {:?}",
                    unavailable.reason
                ),
            ));
        }
    };
    let authority = match database_authority {
        Some(authority) => authority,
        None => hotpath::measure_block!(
            "daemon.session_registry.store_open.resolve",
            DatabaseAuthority::for_runtime(locator.locator().path(), operation)
        )?,
    };
    if authority.canonical_database_path() != locator.locator().path() {
        return Err(session_registry_error(
            operation,
            format!(
                "registered locator {} does not match originating database authority {}",
                locator.locator().path().display(),
                authority.canonical_database_path().display()
            ),
        ));
    }
    let expected_opened_file_identity = if let Some(expected) = required_opened_file_identity {
        Some(expected)
    } else if !allow_remote_restore_fence
        && matches!(&shard_id.scope, StoreShardScopeV1::ProjectSessions { .. })
    {
        hotpath::measure_block!(
            "daemon.session_registry.store_open.restore_fence_check",
            remote_recovery::remote_restore_activated_open_identity(locator.locator().path())
        )?
    } else {
        None
    };
    let exists = locator
        .locator()
        .path()
        .try_exists()
        .map_err(|error| session_registry_error(operation, error.to_string()))?;
    let request = if initialize_if_missing && !exists {
        StoreRuntimeOpenRequest::new_initialize_authorized(
            shard_id,
            incarnation,
            profile_pin,
            authority,
        )
    } else {
        StoreRuntimeOpenRequest::new_authorized(shard_id, incarnation, profile_pin, authority)
    };
    let request = match expected_opened_file_identity {
        Some(expected) => request.require_opened_file_identity(expected),
        None => request,
    };
    match hotpath::future!(
        registry.open(request),
        label = "daemon.session_registry.store_open.registry_open"
    )
    .await
    {
        StoreRuntimeOpenResult::Published(runtime) => Ok((runtime, exists)),
        StoreRuntimeOpenResult::Failed(failure) => {
            #[cfg(feature = "hotpath")]
            hotpath::gauge!("daemon.session_registry.store_open.failed_total").inc(1_u64);
            Err(registry_open_error(
                "open registered session runtime",
                failure,
            ))
        }
    }
}

pub fn registry_open_error(
    operation: &'static str,
    failure: StoreRuntimeRegistryFailure,
) -> TraceDecayError {
    match failure {
        StoreRuntimeRegistryFailure::ResetRequired { authority, reason } => {
            TraceDecayError::reset_required(authority, reason)
        }
        failure => session_registry_error(operation, format!("{failure:?}")),
    }
}

fn session_registry_error(operation: &'static str, message: String) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message,
    }
}

#[cfg(test)]
mod durable_terminal_vacancy_tests {
    use tracedecay_domain::{BrainId, LocatorDigest, UserProfileId};
    use tracedecay_store::{StoreAuthorityEpochV1, StoreIncarnationV1, VerifiedStoreLocatorV1};

    use super::*;

    fn durable_authority(project_id: &ProjectId) -> ProjectSessionTerminalVacancyAuthorityV1 {
        let shard_id = StoreShardIdV1::project_sessions(
            BrainId::new("brain.durable-terminal-vacancy").expect("brain identity"),
            UserProfileId::new("profile.durable-terminal-vacancy").expect("profile identity"),
            project_id.clone(),
        );
        let incarnation = StoreIncarnationV1::new(17).expect("store incarnation");
        ProjectSessionTerminalVacancyAuthorityV1 {
            binding: tracedecay_store::StoreRuntimeBindingV1::new(
                shard_id.clone(),
                incarnation,
                StoreAuthorityEpochV1::new(23).expect("authority epoch"),
            ),
            locator: VerifiedStoreLocatorV1::new(
                shard_id,
                incarnation,
                LocatorDigest::new(format!("sha256:{}", "b".repeat(64))).expect("locator digest"),
            ),
        }
    }

    #[test]
    fn durable_terminal_journal_reconstructs_fail_closed_recovery_before_resume() {
        let project_id =
            ProjectId::new("project.durable-terminal-vacancy").expect("project identity");
        let owners = ProjectRuntimeOwnerRegistryV1::default();
        owners
            .reconstruct_durable_terminal_recovery(&project_id, durable_authority(&project_id))
            .expect("rebuild terminal recovery from durable journal");
        {
            let entries = owners.lock().expect("project owner map");
            let Some(ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery)) =
                entries.get(&project_id)
            else {
                panic!("durable journal must reconstruct RecoveryRequired before resume");
            };
            assert!(recovery.sessions.is_none());
            assert!(recovery.candidate_sessions.is_none());
            assert!(matches!(
                &recovery.phase,
                ProjectSessionRecoveryPhaseV1::Terminal(proof) if proof.verify()
            ));
        }

        // Resuming may enter the temporary vacancy, but dropping it before a
        // candidate is activated must return to the same durable recovery
        // record rather than synthesize Ready or resurrect the terminal owner.
        let recovery = owners
            .reserve_session_recovery(&project_id)
            .expect("reserve reconstructed recovery")
            .expect("reconstructed recovery record");
        let (vacancy, candidate) = recovery
            .into_terminal_vacancy()
            .expect("resume durable terminal vacancy");
        assert!(candidate.is_none());
        drop(vacancy);

        let entries = owners.lock().expect("project owner map");
        let Some(ProjectRuntimeOwnerStateV1::RecoveryRequired(recovery)) = entries.get(&project_id)
        else {
            panic!("durable vacancy restart must remain fail-closed");
        };
        assert!(recovery.sessions.is_none());
        assert!(recovery.candidate_sessions.is_none());
        assert!(matches!(
            &recovery.phase,
            ProjectSessionRecoveryPhaseV1::Terminal(proof) if proof.verify()
        ));
    }
}

#[cfg(test)]
mod verified_graph_runtime_port_contract_tests;

#[cfg(test)]
mod graph_shutdown_contract_tests;

#[cfg(test)]
mod project_memory_relation_graph_contract_tests;

#[cfg(test)]
mod semantic_vector_restart_tests;
