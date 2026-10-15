use super::{StoreAdministration, StoreOwnerKey};
use std::time::Instant;

use tracedecay_runtime_core::native_runtime::{self, NativeTask, NativeTaskOutcome, TimeoutError};
use tracedecay_store_runtime::{ShutdownStatus, ShutdownTaskOutcome, ShutdownTaskReceipt};

/// Drops the process-global Context Scout owner when this project's other
/// owners are torn down. A retired or remotely-deleted project must not keep
/// a strong `Database` handle for the process lifetime. `graph_db_path` is the
/// database the retiring runtime bound: a late retirement of a runtime whose
/// owner was already replaced by a newer database identity leaves the live
/// replacement registered.
pub(crate) fn retire_registered_context_scout_owner(
    project_id: &tracedecay_domain::ProjectId,
    graph_db_path: &std::path::Path,
) {
    let hook_id = tracedecay_hooks::envelope_identity_hash16("project", project_id.as_str());
    let _ = tracedecay_agent_hosts::agents::context_scout::owner::unregister_registered_context_scout_owner(
        hook_id,
        graph_db_path,
    );
}

pub(super) struct ProjectServerRetirement {
    pub(super) owner: StoreOwnerKey,
    completion: tokio::sync::watch::Receiver<ProjectServerRetirementStatus>,
}

pub(crate) struct ProjectServerCapacityRetirementCompletion {
    completion: tokio::sync::watch::Receiver<ProjectServerRetirementStatus>,
}

impl ProjectServerCapacityRetirementCompletion {
    pub(crate) async fn wait(self) -> tracedecay_domain::errors::Result<()> {
        match wait_for_project_server_retirement(self.completion).await {
            ProjectServerRetirementStatus::Clean => Ok(()),
            ProjectServerRetirementStatus::Failed(error) => {
                Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: format!(
                        "project server retirement failed before capacity reuse: {error}"
                    ),
                })
            }
            ProjectServerRetirementStatus::Pending => {
                Err(tracedecay_domain::errors::TraceDecayError::Config {
                    message: "project server retirement returned a non-terminal receipt".to_owned(),
                })
            }
        }
    }
}

/// Owned admission to the canonical project-server retirement tracker.
///
/// Project-open cache replacement takes this admission before the owner
/// registry. That order is deliberate: a cancelled waiter cannot remove an
/// idle server until it owns the synchronous handoff that tracks its
/// retirement. No caller may await this admission while holding the owner
/// registry, and shutdown never holds the owner registry while joining it.
pub(crate) struct ProjectServerRetirementAdmission<'a> {
    retirements: tokio::sync::MutexGuard<'a, Vec<ProjectServerRetirement>>,
}

impl ProjectServerRetirementAdmission<'_> {
    /// Snapshot only the still-pending retirements for this exact physical
    /// project owner. Capacity reuse awaits these receipts inside its detached
    /// task, without joining unrelated project retirement work. A failed
    /// receipt was already reported to the open that retired the owner, so a
    /// later retirement never replays it.
    pub(crate) fn prior_completions_for_owner(
        &self,
        owner: &StoreOwnerKey,
    ) -> Vec<ProjectServerCapacityRetirementCompletion> {
        self.retirements
            .iter()
            .filter(|retirement| {
                &retirement.owner == owner
                    && matches!(
                        &*retirement.completion.borrow(),
                        ProjectServerRetirementStatus::Pending
                    )
            })
            .map(|retirement| ProjectServerCapacityRetirementCompletion {
                completion: retirement.completion.clone(),
            })
            .collect()
    }

    /// Spawn and record one retirement with no cancellation point between the
    /// two transitions. Consuming the exact evicted server here means its
    /// shutdown and join ownership cannot be detached from the caller.
    pub(crate) fn spawn_and_track<Task>(&mut self, owner: StoreOwnerKey, retirement: Task)
    where
        Task: std::future::Future<Output = ()> + Send + 'static,
    {
        track_project_server_retirement_after_admission(
            &mut self.retirements,
            owner,
            spawn_tokio_retirement(retirement),
        );
    }

    /// Track one capacity retirement whose outcome the retiring open awaits.
    /// It supersedes an earlier failed receipt of the same owner.
    pub(crate) fn spawn_and_track_fallible<Task>(
        &mut self,
        owner: StoreOwnerKey,
        retirement: Task,
    ) -> ProjectServerCapacityRetirementCompletion
    where
        Task: std::future::Future<Output = tracedecay_domain::errors::Result<()>> + Send + 'static,
    {
        self.retirements
            .retain(|tracked| match &*tracked.completion.borrow() {
                ProjectServerRetirementStatus::Clean => false,
                ProjectServerRetirementStatus::Failed(_) => tracked.owner != owner,
                ProjectServerRetirementStatus::Pending => true,
            });
        let completion = spawn_fallible_retirement(retirement);
        self.retirements.push(ProjectServerRetirement {
            owner,
            completion: completion.clone(),
        });
        ProjectServerCapacityRetirementCompletion { completion }
    }
}

fn spawn_fallible_retirement<Task>(
    retirement: Task,
) -> tokio::sync::watch::Receiver<ProjectServerRetirementStatus>
where
    Task: std::future::Future<Output = tracedecay_domain::errors::Result<()>> + Send + 'static,
{
    let (task_completion, completion) =
        tokio::sync::watch::channel(ProjectServerRetirementStatus::Pending);
    let finalizer = ProjectServerRetirementFinalizer {
        completion: task_completion,
        terminal: false,
    };
    let retirement = async move {
        let status = match retirement.await {
            Ok(()) => ProjectServerRetirementStatus::Clean,
            Err(error) => ProjectServerRetirementStatus::Failed(error.to_string()),
        };
        finalizer.complete(status);
    };
    // Without a Tokio runtime the retirement is dropped unstarted, and its
    // finalizer publishes the failed receipt.
    if let Some(runtime) = native_runtime::tokio_handle(RETIREMENT_TOKIO_SITE) {
        runtime.spawn(retirement);
    }
    completion
}

/// Retirements reach `project_server_lifecycle`, whose timers are Tokio's, so
/// they run on the daemon's Tokio runtime.
const RETIREMENT_TOKIO_SITE: &str = "daemon.project_retirement";

/// Spawns `retirement` on the Tokio runtime and returns its receipt. Without
/// a Tokio runtime the retirement never runs and its receipt is a failure.
fn spawn_tokio_retirement(
    retirement: impl std::future::Future<Output = ()> + Send + 'static,
) -> impl std::future::Future<Output = ProjectServerRetirementStatus> + Send + 'static {
    let task = native_runtime::tokio_handle(RETIREMENT_TOKIO_SITE)
        .map(|runtime| runtime.spawn(retirement));
    async move {
        match task {
            Some(task) => tokio_retirement_status(task).await,
            None => ProjectServerRetirementStatus::Failed(
                "no Tokio runtime ran the project server retirement".to_owned(),
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProjectServerRetirementStatus {
    Pending,
    Clean,
    Failed(String),
}

struct ProjectServerRetirementFinalizer {
    completion: tokio::sync::watch::Sender<ProjectServerRetirementStatus>,
    terminal: bool,
}

impl ProjectServerRetirementFinalizer {
    fn complete(mut self, status: ProjectServerRetirementStatus) {
        self.completion.send_replace(status);
        self.terminal = true;
    }
}

impl Drop for ProjectServerRetirementFinalizer {
    fn drop(&mut self) {
        if !self.terminal {
            self.completion
                .send_replace(ProjectServerRetirementStatus::Failed(
                    "retirement tracking task ended without a terminal receipt".to_owned(),
                ));
        }
    }
}

fn retirement_shutdown_owner_label(owner: &StoreOwnerKey) -> String {
    match &owner.project_id {
        Some(project_id) => format!("project_server_retirement[{project_id}]"),
        None => format!("project_server_retirement[{}]", owner.store_root.display()),
    }
}

async fn wait_for_project_server_retirement(
    mut completion: tokio::sync::watch::Receiver<ProjectServerRetirementStatus>,
) -> ProjectServerRetirementStatus {
    loop {
        let observed = completion.borrow().clone();
        if observed != ProjectServerRetirementStatus::Pending {
            return observed;
        }
        if completion.changed().await.is_err() {
            return ProjectServerRetirementStatus::Failed(
                "retirement receipt authority ended before settlement".to_owned(),
            );
        }
    }
}

async fn tokio_retirement_status(
    task: tokio::task::JoinHandle<()>,
) -> ProjectServerRetirementStatus {
    match task.await {
        Ok(()) => ProjectServerRetirementStatus::Clean,
        Err(error) => ProjectServerRetirementStatus::Failed(error.to_string()),
    }
}

/// The receipt of a native owner aborted for retirement: its cancellation is
/// the clean outcome.
async fn aborted_native_retirement_status(task: NativeTask<()>) -> ProjectServerRetirementStatus {
    match task.wait().await {
        NativeTaskOutcome::Completed(()) | NativeTaskOutcome::Cancelled => {
            ProjectServerRetirementStatus::Clean
        }
        NativeTaskOutcome::Panicked(panic) => {
            ProjectServerRetirementStatus::Failed(format!("task panicked: {panic}"))
        }
    }
}

/// Records one retirement and publishes `joined`'s status from a native
/// task. When the native runtime refuses that task, its finalizer publishes
/// a failed receipt.
fn track_project_server_retirement_after_admission(
    retirements: &mut Vec<ProjectServerRetirement>,
    owner: StoreOwnerKey,
    joined: impl std::future::Future<Output = ProjectServerRetirementStatus> + Send + 'static,
) {
    retirements.retain(|retirement| {
        !matches!(
            &*retirement.completion.borrow(),
            ProjectServerRetirementStatus::Clean
        )
    });
    let (task_completion, completion) =
        tokio::sync::watch::channel(ProjectServerRetirementStatus::Pending);
    let finalizer = ProjectServerRetirementFinalizer {
        completion: task_completion,
        terminal: false,
    };
    // Detached: the receipt, not the task, is what callers wait on.
    if let Err(error) = NativeTask::spawn(async move { finalizer.complete(joined.await) }) {
        tracing::error!(%error, "project server retirement tracker was refused");
    }
    retirements.push(ProjectServerRetirement { owner, completion });
}

async fn track_project_server_retirement(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    owner: StoreOwnerKey,
    joined: impl std::future::Future<Output = ProjectServerRetirementStatus> + Send + 'static,
) {
    let mut retirements = retirements.lock().await;
    track_project_server_retirement_after_admission(&mut retirements, owner, joined);
}

#[cfg(test)]
pub(super) async fn track_retirement_task(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    owner: StoreOwnerKey,
    task: tokio::task::JoinHandle<()>,
) {
    track_project_server_retirement(retirements, owner, tokio_retirement_status(task)).await;
}

/// Spawns `retirement` on the Tokio runtime and tracks its receipt.
pub(super) async fn track_tokio_retirement(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    owner: StoreOwnerKey,
    retirement: impl std::future::Future<Output = ()> + Send + 'static,
) {
    track_project_server_retirement(retirements, owner, spawn_tokio_retirement(retirement)).await;
}

pub(super) async fn track_aborted_native_retirement_task(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    owner: StoreOwnerKey,
    task: NativeTask<()>,
) {
    track_project_server_retirement(retirements, owner, aborted_native_retirement_status(task))
        .await;
}

async fn shutdown_receipt(
    completions: Vec<(
        String,
        tokio::sync::watch::Receiver<ProjectServerRetirementStatus>,
    )>,
    deadline: Instant,
) -> ShutdownTaskReceipt {
    let mut receipt = ShutdownTaskReceipt::default();
    for (owner, completion) in completions {
        let status = match native_runtime::timeout_at(
            deadline,
            wait_for_project_server_retirement(completion),
        )
        .await
        {
            Ok(ProjectServerRetirementStatus::Clean) => ShutdownStatus::Clean,
            Ok(ProjectServerRetirementStatus::Failed(error)) => ShutdownStatus::Failed(error),
            Ok(ProjectServerRetirementStatus::Pending) | Err(TimeoutError::Elapsed) => {
                ShutdownStatus::TimedOut
            }
            Err(error @ TimeoutError::TimerUnavailable(_)) => {
                ShutdownStatus::Failed(error.to_string())
            }
        };
        receipt.outcomes.push(ShutdownTaskOutcome { owner, status });
    }
    receipt
}

pub(super) async fn settle_project_retirements(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    profile_root: &std::path::Path,
    project_id: &str,
    deadline: Instant,
) -> ShutdownTaskReceipt {
    let completions = retirements
        .lock()
        .await
        .iter()
        .filter(|retirement| {
            retirement.owner.profile_root == profile_root
                && retirement.owner.project_id.as_deref() == Some(project_id)
        })
        .map(|retirement| {
            (
                retirement_shutdown_owner_label(&retirement.owner),
                retirement.completion.clone(),
            )
        })
        .collect::<Vec<_>>();
    let receipt = shutdown_receipt(completions, deadline).await;
    retirements.lock().await.retain(|retirement| {
        !matches!(
            &*retirement.completion.borrow(),
            ProjectServerRetirementStatus::Clean
        )
    });
    receipt
}

impl StoreAdministration {
    /// Acquires the canonical retirement handoff before an upstream mutation.
    ///
    /// The caller must take this before the owner registry whenever it may
    /// evict or replace a live server, then call
    /// [`ProjectServerRetirementAdmission::spawn_and_track`] without awaiting.
    pub(crate) async fn acquire_project_server_retirement_admission(
        &self,
    ) -> ProjectServerRetirementAdmission<'_> {
        ProjectServerRetirementAdmission {
            retirements: self.project_server_retirements.lock().await,
        }
    }

    // pub(crate): daemon bootstrap tests register retirements from outside
    // branch_admin to exercise the shutdown join path.
    #[cfg(test)]
    pub(crate) async fn track_project_server_retirement(
        &self,
        owner: StoreOwnerKey,
        task: tokio::task::JoinHandle<()>,
    ) {
        track_project_server_retirement(
            &self.project_server_retirements,
            owner,
            tokio_retirement_status(task),
        )
        .await;
    }

    // pub(crate): the test-transport production harness joins retirements from
    // outside branch_admin during its shutdown sequence.
    #[cfg(any(test, feature = "test-transport"))]
    pub(crate) async fn join_project_server_retirements(&self) {
        let completions = self
            .project_server_retirements
            .lock()
            .await
            .iter()
            .map(|retirement| retirement.completion.clone())
            .collect::<Vec<_>>();
        for completion in completions {
            let _ = wait_for_project_server_retirement(completion).await;
        }
        self.project_server_retirements
            .lock()
            .await
            .retain(|retirement| {
                !matches!(
                    &*retirement.completion.borrow(),
                    ProjectServerRetirementStatus::Clean
                )
            });
    }

    /// Bounded retirement join for daemon shutdown: every tracked retirement
    /// is awaited up to `deadline` and reported under its owner's identity, so
    /// a hung retirement surfaces as a typed timeout instead of a silent hang.
    pub(crate) async fn join_project_server_retirements_until(
        &self,
        deadline: impl Into<Instant>,
    ) -> ShutdownTaskReceipt {
        let deadline = deadline.into();
        let completions = match native_runtime::timeout_at(
            deadline,
            self.project_server_retirements.lock(),
        )
        .await
        {
            Ok(retirements) => retirements
                .iter()
                .map(|retirement| {
                    (
                        retirement_shutdown_owner_label(&retirement.owner),
                        retirement.completion.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            Err(TimeoutError::Elapsed) => {
                return ShutdownTaskReceipt::timed_out("project_server_retirement_registry");
            }
            Err(error @ TimeoutError::TimerUnavailable(_)) => {
                return ShutdownTaskReceipt {
                    outcomes: vec![ShutdownTaskOutcome {
                        owner: "project_server_retirement_registry".to_owned(),
                        status: ShutdownStatus::Failed(error.to_string()),
                    }],
                };
            }
        };
        let receipt = shutdown_receipt(completions, deadline).await;
        if let Ok(mut retirements) =
            native_runtime::timeout_at(deadline, self.project_server_retirements.lock()).await
        {
            retirements.retain(|retirement| {
                !matches!(
                    &*retirement.completion.borrow(),
                    ProjectServerRetirementStatus::Clean
                )
            });
        }
        receipt
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::daemon::project_server_lifecycle;

    fn owner(project_id: &str) -> StoreOwnerKey {
        isolated_owner(std::path::Path::new("/profile"), project_id)
    }

    fn isolated_owner(profile_root: &std::path::Path, project_id: &str) -> StoreOwnerKey {
        StoreOwnerKey {
            profile_root: profile_root.to_path_buf(),
            global_db_path: profile_root.join("profile.db"),
            project_id: Some(project_id.to_owned()),
            store_root: profile_root.join("projects").join(project_id),
            graph_db_path: profile_root
                .join("projects")
                .join(project_id)
                .join("graph.db"),
        }
    }

    async fn isolated_registered_graph(
        profile_root: &std::path::Path,
        project_root: &std::path::Path,
        project_id: &str,
    ) -> (
        tracedecay_project::project::TraceDecay,
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1,
    ) {
        std::fs::create_dir_all(profile_root).expect("isolated profile root");
        std::fs::create_dir_all(project_root).expect("isolated project root");
        let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned())
            .expect("typed project identity");
        let runtime =
            tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
                profile_root,
                project_root,
                project_id,
            )
            .await
            .expect("isolated host-admission runtime");
        let graph = runtime
            .initialize_project_graph_for_test(
                project_root,
                tracedecay_project::project::TraceDecayOpenOptions {
                    profile_root: Some(profile_root.to_path_buf()),
                    global_db_path: None,
                },
            )
            .await
            .expect("isolated registered graph");
        (graph, runtime)
    }

    async fn isolated_sibling_graph(
        runtime: &tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1,
        profile_root: &std::path::Path,
        project_root: &std::path::Path,
        project_id: &str,
    ) -> (
        tracedecay_project::project::TraceDecay,
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1,
    ) {
        std::fs::create_dir_all(project_root).expect("isolated sibling project root");
        let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned())
            .expect("typed project identity");
        let sibling = runtime
            .sibling_project(project_root, project_id)
            .await
            .expect("isolated sibling host-admission runtime");
        let graph = sibling
            .initialize_project_graph_for_test(
                project_root,
                tracedecay_project::project::TraceDecayOpenOptions {
                    profile_root: Some(profile_root.to_path_buf()),
                    global_db_path: None,
                },
            )
            .await
            .expect("isolated sibling registered graph");
        (graph, sibling)
    }

    #[tokio::test]
    async fn project_retirement_unregisters_context_scout_owner_and_rebind_is_fresh() {
        let root = tempfile::TempDir::new().expect("retirement fixture");
        let profile = root.path().join("profile");
        let project = root.path().join("project");
        let (graph, _runtime) =
            isolated_registered_graph(&profile, &project, "proj_retire_scout").await;
        let project_id =
            tracedecay_domain::ProjectId::new("proj_retire_scout".to_owned()).expect("project id");
        let hook_id = tracedecay_hooks::envelope_identity_hash16("project", project_id.as_str());
        let retired = graph
            .context_scout_owner()
            .expect("writable open registers a Context Scout owner");
        assert_eq!(
            tracedecay_agent_hosts::agents::context_scout::owner::lookup_registered_context_scout_owners(
                hook_id
            )
            .len(),
            1
        );

        retire_registered_context_scout_owner(&project_id, &graph.db_path());
        assert!(
            tracedecay_agent_hosts::agents::context_scout::owner::lookup_registered_context_scout_owners(
                hook_id
            )
            .is_empty(),
            "retirement must remove the process-global Context Scout owner"
        );

        tracedecay_global_db::register_registered_schema_installer();
        let replacement_root = tempfile::TempDir::new().expect("replacement database");
        let replacement_path = replacement_root.path().join("graph.db");
        let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
            &replacement_path,
            "re-registered Context Scout owner",
        )
        .expect("replacement authority");
        let replacement = tracedecay_runtime_core::db::Database::publish_test_runtime(
            &replacement_path,
            &authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .expect("replacement database")
        .0;
        let fresh =
            tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1::startup(
                replacement.clone(),
                hook_id,
                tracedecay_domain::UtcMicros(2),
                None,
            )
            .await
            .expect("fresh owner after retirement");
        assert!(
            !std::sync::Arc::ptr_eq(&retired, &fresh),
            "re-registering after retirement must bind a new owner"
        );
        assert_eq!(
            fresh.store().database().canonical_database_path(),
            replacement.canonical_database_path()
        );
        tracedecay_agent_hosts::agents::context_scout::owner::unregister_registered_context_scout_owner(
            hook_id,
            replacement.canonical_database_path(),
        );
    }

    #[tokio::test]
    async fn timed_out_retirement_stays_owned_until_retry_observes_completion() {
        let administration = StoreAdministration::default();
        let retirements = &administration.project_server_retirements;
        let first_release = Arc::new(tokio::sync::Notify::new());
        let first_task = tokio::spawn({
            let release = Arc::clone(&first_release);
            async move { release.notified().await }
        });
        track_retirement_task(retirements, owner("project-a"), first_task).await;
        let second_release = Arc::new(tokio::sync::Notify::new());
        let second_task = tokio::spawn({
            let release = Arc::clone(&second_release);
            async move { release.notified().await }
        });
        track_retirement_task(retirements, owner("project-a"), second_task).await;

        let first = settle_project_retirements(
            retirements,
            std::path::Path::new("/profile"),
            "project-a",
            Instant::now(),
        )
        .await;
        assert_eq!(first.status(), ShutdownStatus::TimedOut);
        assert_eq!(retirements.lock().await.len(), 2);

        first_release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let observed = retirements
                    .lock()
                    .await
                    .first()
                    .map(|retirement| retirement.completion.borrow().clone());
                if observed == Some(ProjectServerRetirementStatus::Clean) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first retirement owner should complete before the test deadline");
        let partially_settled = settle_project_retirements(
            retirements,
            std::path::Path::new("/profile"),
            "project-a",
            Instant::now(),
        )
        .await;
        assert_eq!(partially_settled.status(), ShutdownStatus::TimedOut);
        assert_eq!(retirements.lock().await.len(), 1);
        second_release.notify_one();
        let second = settle_project_retirements(
            retirements,
            std::path::Path::new("/profile"),
            "project-a",
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .await;
        assert!(second.is_clean());
        assert!(retirements.lock().await.is_empty());
    }

    // Remote recovery settles retirements under `native_runtime::block_on`
    // from a blocking thread, where no Tokio runtime is entered.
    #[test]
    fn shutdown_receipt_settles_outside_any_tokio_runtime() {
        let (_clean_sender, clean) =
            tokio::sync::watch::channel(ProjectServerRetirementStatus::Clean);
        let (_pending_sender, pending) =
            tokio::sync::watch::channel(ProjectServerRetirementStatus::Pending);
        let deadline = Instant::now() + std::time::Duration::from_millis(50);

        let receipt = native_runtime::block_on(shutdown_receipt(
            vec![("clean".to_owned(), clean), ("pending".to_owned(), pending)],
            deadline,
        ))
        .expect("native runtime");

        assert!(Instant::now() >= deadline);
        assert_eq!(
            receipt.outcomes,
            [
                ShutdownTaskOutcome {
                    owner: "clean".to_owned(),
                    status: ShutdownStatus::Clean,
                },
                ShutdownTaskOutcome {
                    owner: "pending".to_owned(),
                    status: ShutdownStatus::TimedOut,
                },
            ]
        );
    }

    #[tokio::test]
    async fn cancelled_retirement_is_a_retained_failure_not_false_completion() {
        let retirements = tokio::sync::Mutex::new(Vec::new());
        let task = tokio::spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        track_retirement_task(&retirements, owner("project-a"), task).await;
        abort.abort();

        let receipt = settle_project_retirements(
            &retirements,
            std::path::Path::new("/profile"),
            "project-a",
            Instant::now() + std::time::Duration::from_secs(1),
        )
        .await;
        assert!(matches!(receipt.status(), ShutdownStatus::Failed(_)));
        assert_eq!(retirements.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn cancelling_admitted_retirement_caller_keeps_shutdown_join_ownership() {
        let administration = StoreAdministration::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let caller_administration = administration.clone();
        let caller = tokio::spawn({
            let release = Arc::clone(&release);
            async move {
                let mut admission = caller_administration
                    .acquire_project_server_retirement_admission()
                    .await;
                admission.spawn_and_track(owner("project-a"), async move {
                    let _ = started_tx.send(());
                    release.notified().await;
                });
                drop(admission);
                std::future::pending::<()>().await;
            }
        });
        started_rx
            .await
            .expect("the tracked retirement must start before caller cancellation");
        caller.abort();
        assert!(
            caller
                .await
                .expect_err("caller cancellation must surface")
                .is_cancelled(),
            "the caller must be cancelled after retirement admission"
        );

        let mut shutdown = Box::pin(administration.join_project_server_retirements());
        std::future::poll_fn(|context| {
            assert!(
                shutdown.as_mut().poll(context).is_pending(),
                "daemon shutdown must retain and join the admitted retirement"
            );
            std::task::Poll::Ready(())
        })
        .await;

        release.notify_one();
        shutdown.await;
        assert!(
            administration
                .project_server_retirements
                .lock()
                .await
                .is_empty(),
            "joined retirement ownership must be released after clean completion"
        );
    }

    #[tokio::test]
    async fn capacity_retirement_receipt_survives_cancellation_without_global_head_of_line() {
        let administration = StoreAdministration::default();
        let capacity_gate = Arc::new(tokio::sync::Mutex::new(()));
        let capacity_admission = Arc::clone(&capacity_gate).lock_owned().await;
        let release_capacity = Arc::new(tokio::sync::Notify::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut admission = administration
            .acquire_project_server_retirement_admission()
            .await;
        let completion = admission.spawn_and_track_fallible(owner("project-capacity"), {
            let release_capacity = Arc::clone(&release_capacity);
            async move {
                let _capacity_admission = capacity_admission;
                let _ = started_tx.send(());
                release_capacity.notified().await;
                Ok(())
            }
        });
        drop(admission);
        drop(completion);
        started_rx.await.expect("capacity retirement started");
        assert!(
            Arc::clone(&capacity_gate).try_lock_owned().is_err(),
            "a cancelled caller cannot release capacity while detached cleanup is pending"
        );
        release_capacity.notify_one();
        let recovered_capacity = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            Arc::clone(&capacity_gate).lock_owned(),
        )
        .await
        .expect("detached cleanup must release capacity after completion");

        let mut admission = administration
            .acquire_project_server_retirement_admission()
            .await;
        let second_clean = admission
            .spawn_and_track_fallible(owner("project-capacity-replacement"), async move { Ok(()) });
        assert_eq!(
            admission.retirements.len(),
            1,
            "a new fallible capacity retirement must reap prior clean receipts"
        );
        drop(admission);
        second_clean
            .wait()
            .await
            .expect("replacement capacity retirement completes cleanly");

        let release_unrelated = Arc::new(tokio::sync::Notify::new());
        let release_prior = Arc::new(tokio::sync::Notify::new());
        let mut admission = administration
            .acquire_project_server_retirement_admission()
            .await;
        admission.spawn_and_track(owner("project-unrelated"), {
            let release_unrelated = Arc::clone(&release_unrelated);
            async move { release_unrelated.notified().await }
        });
        let exact_owner = owner("project-exact");
        admission.spawn_and_track(exact_owner.clone(), {
            let release_prior = Arc::clone(&release_prior);
            async move { release_prior.notified().await }
        });
        let prior = admission.prior_completions_for_owner(&exact_owner);
        let exact = admission.spawn_and_track_fallible(exact_owner, async move {
            for completion in prior {
                completion.wait().await?;
            }
            drop(recovered_capacity);
            Ok(())
        });
        drop(admission);
        let mut exact = Box::pin(exact.wait());
        std::future::poll_fn(|context| {
            assert!(
                exact.as_mut().poll(context).is_pending(),
                "capacity cleanup must await a prior retirement for its exact owner"
            );
            std::task::Poll::Ready(())
        })
        .await;
        release_prior.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), exact)
            .await
            .expect("exact capacity receipt must not join unrelated retirement")
            .expect("exact capacity retirement completes cleanly");
        release_unrelated.notify_one();
        administration.join_project_server_retirements().await;
    }

    #[tokio::test]
    async fn failed_capacity_retirement_is_reported_once_and_superseded_by_the_next() {
        let administration = StoreAdministration::default();
        let blocked = owner("project-blocked");
        let mut admission = administration
            .acquire_project_server_retirement_admission()
            .await;
        let failed = admission.spawn_and_track_fallible(blocked.clone(), async {
            Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "store client still leased".to_owned(),
            })
        });
        drop(admission);
        assert_eq!(
            failed
                .wait()
                .await
                .expect_err("a refused release fails its retirement")
                .to_string(),
            "config error: project server retirement failed before capacity reuse: \
             config error: store client still leased"
        );

        let mut admission = administration
            .acquire_project_server_retirement_admission()
            .await;
        assert_eq!(
            admission.prior_completions_for_owner(&blocked).len(),
            0,
            "a reported failure is never replayed into the owner's next retirement"
        );
        let next = admission.spawn_and_track_fallible(blocked, async { Ok(()) });
        assert_eq!(admission.retirements.len(), 1);
        drop(admission);
        next.wait()
            .await
            .expect("the next retirement of the owner settles on its own outcome");
    }

    #[tokio::test]
    async fn cancellation_before_eviction_admission_preserves_owner_then_shutdown_joins_retirement()
    {
        let homes = tempfile::tempdir().expect("isolated profile home");
        let profile = homes.path().join("shared-profile");
        let projects = tempfile::tempdir().expect("project roots");
        let idle_project = projects.path().join("idle");
        let replacement_project = projects.path().join("replacement");
        let (idle_graph, idle_runtime) =
            isolated_registered_graph(&profile, &idle_project, "project.retirement-idle").await;
        let idle_server = crate::mcp::McpServer::new(idle_graph, None).await;
        let idle_lifecycle = idle_server.project_server_response_lifecycle();
        let idle_witness = Arc::downgrade(&idle_server);
        let (replacement_graph, _replacement_runtime) = isolated_sibling_graph(
            &idle_runtime,
            &profile,
            &replacement_project,
            "project.retirement-replacement",
        )
        .await;
        let replacement_server = crate::mcp::McpServer::new(replacement_graph, None).await;
        let administration = StoreAdministration::default();
        let idle_key = crate::daemon::ProjectServerKey {
            owner: isolated_owner(&profile, "project-idle"),
            project_root: idle_project.clone(),
            scope_prefix: None,
        };
        let idle_route = crate::daemon::ProjectRouteKey {
            profile_root: idle_key.owner.profile_root.clone(),
            global_db_path: idle_key.owner.global_db_path.clone(),
            project_path: idle_project,
            scope_prefix: None,
        };
        {
            let mut servers = administration.project_servers().lock().await;
            // Bounded replacement evicts only RegisteredHostIngest owners in
            // this route set. insert_route publishes that exact idle state.
            servers.insert_route(idle_route.clone(), idle_key.clone(), idle_server);
        }

        let admission_blocker = administration
            .acquire_project_server_retirement_admission()
            .await;
        let attempted_admission = Arc::new(AtomicBool::new(false));
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled_administration = administration.clone();
        let cancelled_attempt = Arc::clone(&attempted_admission);
        let cancelled_started = Arc::clone(&started);
        let cancelled = tokio::spawn(async move {
            cancelled_attempt.store(true, Ordering::Release);
            cancelled_started.notify_one();
            let _admission = cancelled_administration
                .acquire_project_server_retirement_admission()
                .await;
            panic!("cancelled open reached owner mutation without admission contention");
        });
        started.notified().await;
        assert!(
            attempted_admission.load(Ordering::Acquire),
            "the cancelled caller must contend on retirement admission before mutation"
        );
        cancelled.abort();
        assert!(
            cancelled
                .await
                .expect_err("cancelled admission caller must stop")
                .is_cancelled()
        );
        assert!(
            Arc::ptr_eq(
                administration
                    .project_servers()
                    .lock()
                    .await
                    .get(&idle_key)
                    .expect("cancelled caller must preserve idle owner"),
                &idle_witness
                    .upgrade()
                    .expect("registered owner must remain alive after cancellation"),
            ),
            "cancellation before admission must not evict the victim"
        );
        drop(admission_blocker);

        let replacement_key = crate::daemon::ProjectServerKey {
            owner: isolated_owner(&profile, "project-replacement"),
            project_root: replacement_project.clone(),
            scope_prefix: None,
        };
        let replacement_route = crate::daemon::ProjectRouteKey {
            profile_root: replacement_key.owner.profile_root.clone(),
            global_db_path: replacement_key.owner.global_db_path.clone(),
            project_path: replacement_project,
            scope_prefix: None,
        };
        let mut admission = administration
            .acquire_project_server_retirement_admission()
            .await;
        let (replacement, inserted, retired) = {
            let mut servers = administration.project_servers().lock().await;
            servers
                .bind_or_insert_route_bounded(
                    replacement_route,
                    replacement_key,
                    replacement_server,
                    1,
                    |server| Arc::strong_count(server) > 1,
                )
                .expect("admitted replacement must evict the idle owner")
        };
        assert!(inserted);
        assert_eq!(retired.len(), 1);
        assert_eq!(&retired[0].0, &idle_key);
        let request = Arc::clone(idle_lifecycle.response_gate())
            .read_owned()
            .await;
        for (retired_key, retired_server) in retired {
            admission.spawn_and_track(
                retired_key.owner,
                project_server_lifecycle::retire_project_servers(vec![retired_server], None),
            );
        }
        drop(admission);
        drop(replacement);

        let http_application_registry =
            crate::daemon::http_application::DaemonHttpApplicationRegistry::default();
        let mut shutdown = Box::pin(project_server_lifecycle::shutdown_project_servers(
            tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            &administration,
            &http_application_registry,
        ));
        std::future::poll_fn(|context| {
            assert!(
                shutdown.as_mut().poll(context).is_pending(),
                "daemon shutdown must join the admitted eviction retirement"
            );
            std::task::Poll::Ready(())
        })
        .await;
        drop(request);
        let receipt = shutdown.await;
        assert!(receipt.is_clean());
        let retirement_owners: Vec<&str> = receipt
            .outcomes
            .iter()
            .filter(|outcome| outcome.owner.starts_with("project_server_retirement["))
            .map(|outcome| outcome.owner.as_str())
            .collect();
        assert_eq!(
            retirement_owners,
            ["project_server_retirement[project-idle]"],
            "shutdown must join only this isolated eviction, not a leaked foreign retirement"
        );
        assert!(
            receipt.outcomes.iter().any(|outcome| {
                outcome.owner == "project_server_retirement[project-idle]"
                    && outcome.status == ShutdownStatus::Clean
            }),
            "shutdown must report the exact evicted owner through its retirement receipt"
        );
        assert!(
            idle_witness.upgrade().is_none(),
            "joined retirement must release the exact evicted server"
        );
    }

    /// The tracker of a Tokio-owned retirement runs on the native runtime, so
    /// a caller with no Tokio runtime can track and settle it.
    #[test]
    fn a_tokio_retirement_is_tracked_and_settled_without_a_tokio_runtime() {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()
            .expect("tokio runtime");
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let task = tokio.spawn(async move {
            let _ = released.await;
        });
        let hazards = crate::test_support::native_runtime_hazards();
        let receipt = native_runtime::block_on(async {
            assert!(tokio::runtime::Handle::try_current().is_err());
            let administration = StoreAdministration::default();
            let retirements = &administration.project_server_retirements;
            track_retirement_task(retirements, owner("project-a"), task).await;
            let _ = release.send(());
            settle_project_retirements(
                retirements,
                std::path::Path::new("/profile"),
                "project-a",
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .await
        })
        .expect("native runtime");
        assert_eq!(crate::test_support::native_runtime_hazards(), hazards);
        assert_eq!(receipt.status(), ShutdownStatus::Clean);
    }

    #[test]
    fn an_aborted_native_owner_settles_its_retirement_clean() {
        let receipt = native_runtime::block_on(async {
            let administration = StoreAdministration::default();
            let retirements = &administration.project_server_retirements;
            let task = NativeTask::spawn(std::future::pending::<()>()).expect("native task");
            task.abort();
            track_aborted_native_retirement_task(retirements, owner("project-a"), task).await;
            settle_project_retirements(
                retirements,
                std::path::Path::new("/profile"),
                "project-a",
                Instant::now() + std::time::Duration::from_secs(5),
            )
            .await
        })
        .expect("native runtime");
        assert_eq!(receipt.status(), ShutdownStatus::Clean);
    }
}
