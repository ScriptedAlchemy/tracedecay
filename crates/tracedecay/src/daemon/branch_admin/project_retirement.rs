use std::sync::Arc;

use super::super::store_shutdown::{ShutdownTaskOutcome, ShutdownTaskReceipt, ShutdownTaskStatus};
use super::{StoreAdministration, StoreOwnerKey};

pub(super) struct ProjectServerRetirement {
    pub(super) owner: StoreOwnerKey,
    completion: tokio::sync::watch::Receiver<ProjectServerRetirementStatus>,
    _task: tokio::task::JoinHandle<()>,
    _fence: Option<std::sync::Arc<ProjectRetirementFenceV1>>,
    capacity_reuse: bool,
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
    /// Snapshot only the already-tracked retirements for this exact physical
    /// project owner. Capacity reuse awaits these receipts inside its detached
    /// task, without joining unrelated project retirement work.
    pub(crate) fn prior_completions_for_owner(
        &self,
        owner: &StoreOwnerKey,
    ) -> Vec<ProjectServerCapacityRetirementCompletion> {
        self.retirements
            .iter()
            .filter(|retirement| {
                &retirement.owner == owner
                    && !matches!(
                        &*retirement.completion.borrow(),
                        ProjectServerRetirementStatus::Clean
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
        let task = tokio::spawn(retirement);
        track_project_server_retirement_after_admission(&mut self.retirements, owner, task, false);
    }

    pub(crate) fn spawn_and_track_fallible<Task>(
        &mut self,
        owner: StoreOwnerKey,
        retirement: Task,
    ) -> ProjectServerCapacityRetirementCompletion
    where
        Task: std::future::Future<Output = tracedecay_domain::errors::Result<()>> + Send + 'static,
    {
        self.retirements.retain(|retirement| {
            !matches!(
                &*retirement.completion.borrow(),
                ProjectServerRetirementStatus::Clean
            )
        });
        let (task_completion, completion) =
            tokio::sync::watch::channel(ProjectServerRetirementStatus::Pending);
        hotpath::gauge!("retirement_pending").inc(1.0);
        let finalizer = ProjectServerRetirementFinalizer {
            completion: task_completion,
            terminal: false,
        };
        let task = tokio::spawn(async move {
            let status = match retirement.await {
                Ok(()) => ProjectServerRetirementStatus::Clean,
                Err(error) => ProjectServerRetirementStatus::Failed(error.to_string()),
            };
            finalizer.complete(status);
        });
        self.retirements.push(ProjectServerRetirement {
            owner,
            completion: completion.clone(),
            _task: task,
            _fence: None,
            capacity_reuse: true,
        });
        ProjectServerCapacityRetirementCompletion { completion }
    }
}

pub(in crate::daemon) struct ProjectRetirementFenceV1 {
    // Field order is lifecycle order: reopen roots before releasing the store
    // writer gate so a deletion owner can never have its permanent fence
    // removed by this temporary recovery guard.
    _invocation: tracedecay_daemon_service::ProjectRuntimeRootQuiescenceV1,
    _project_open: crate::daemon::project_open_admission::ProjectOpenIdentityQuiescenceV1,
    _writer: crate::daemon::store_writer_gate::WriterAdmissionGuard,
}

impl ProjectRetirementFenceV1 {
    pub(super) fn new(
        invocation: tracedecay_daemon_service::ProjectRuntimeRootQuiescenceV1,
        project_open: crate::daemon::project_open_admission::ProjectOpenIdentityQuiescenceV1,
        writer: crate::daemon::store_writer_gate::WriterAdmissionGuard,
    ) -> Self {
        Self {
            _invocation: invocation,
            _project_open: project_open,
            _writer: writer,
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
        match &status {
            ProjectServerRetirementStatus::Clean => {
                hotpath::gauge!("daemon.branch_admin.retirement.clean_total").inc(1_u64);
            }
            ProjectServerRetirementStatus::Failed(_) => {
                hotpath::gauge!("daemon.branch_admin.retirement.failed_total").inc(1_u64);
            }
            ProjectServerRetirementStatus::Pending => {}
        }
        self.completion.send_replace(status);
        self.terminal = true;
        hotpath::gauge!("retirement_pending").inc(-1.0);
    }
}

impl Drop for ProjectServerRetirementFinalizer {
    fn drop(&mut self) {
        if !self.terminal {
            hotpath::gauge!("daemon.branch_admin.retirement.abandoned_total").inc(1_u64);
            self.completion
                .send_replace(ProjectServerRetirementStatus::Failed(
                    "retirement tracking task ended without a terminal receipt".to_owned(),
                ));
            hotpath::gauge!("retirement_pending").inc(-1.0);
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

fn track_project_server_retirement_after_admission(
    retirements: &mut Vec<ProjectServerRetirement>,
    owner: StoreOwnerKey,
    task: tokio::task::JoinHandle<()>,
    cancelled_is_clean: bool,
) {
    retirements.retain(|retirement| {
        !matches!(
            &*retirement.completion.borrow(),
            ProjectServerRetirementStatus::Clean
        )
    });
    let (task_completion, completion) =
        tokio::sync::watch::channel(ProjectServerRetirementStatus::Pending);
    hotpath::gauge!("retirement_pending").inc(1.0);
    let finalizer = ProjectServerRetirementFinalizer {
        completion: task_completion,
        terminal: false,
    };
    let task = tokio::spawn(async move {
        let status = match task.await {
            Ok(()) => ProjectServerRetirementStatus::Clean,
            Err(error) if cancelled_is_clean && error.is_cancelled() => {
                ProjectServerRetirementStatus::Clean
            }
            Err(error) => ProjectServerRetirementStatus::Failed(error.to_string()),
        };
        finalizer.complete(status);
    });
    retirements.push(ProjectServerRetirement {
        owner,
        completion,
        _task: task,
        _fence: None,
        capacity_reuse: false,
    });
}

async fn track_project_server_retirement(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    owner: StoreOwnerKey,
    task: tokio::task::JoinHandle<()>,
    cancelled_is_clean: bool,
) {
    let mut retirements = retirements.lock().await;
    track_project_server_retirement_after_admission(
        &mut retirements,
        owner,
        task,
        cancelled_is_clean,
    );
}

pub(super) async fn attach_project_retirement_fence(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    profile_root: &std::path::Path,
    project_id: &str,
    fence: std::sync::Arc<ProjectRetirementFenceV1>,
) {
    let mut retirements = retirements.lock().await;
    for retirement in retirements.iter_mut().filter(|retirement| {
        retirement.owner.profile_root == profile_root
            && retirement.owner.project_id.as_deref() == Some(project_id)
            && !matches!(
                &*retirement.completion.borrow(),
                ProjectServerRetirementStatus::Clean
            )
    }) {
        retirement._fence.get_or_insert_with(|| Arc::clone(&fence));
    }
}

pub(super) async fn track_retirement_task(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    owner: StoreOwnerKey,
    task: tokio::task::JoinHandle<()>,
) {
    track_project_server_retirement(retirements, owner, task, false).await;
}

pub(super) async fn track_aborted_retirement_task(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    owner: StoreOwnerKey,
    task: tokio::task::JoinHandle<()>,
) {
    track_project_server_retirement(retirements, owner, task, true).await;
}

pub(super) async fn settle_project_retirements(
    retirements: &tokio::sync::Mutex<Vec<ProjectServerRetirement>>,
    profile_root: &std::path::Path,
    project_id: &str,
    deadline: tokio::time::Instant,
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
    let mut receipt = ShutdownTaskReceipt::default();
    for (owner, completion) in completions {
        let status =
            match tokio::time::timeout_at(deadline, wait_for_project_server_retirement(completion))
                .await
            {
                Ok(ProjectServerRetirementStatus::Clean) => ShutdownTaskStatus::Clean,
                Ok(ProjectServerRetirementStatus::Failed(error)) => {
                    ShutdownTaskStatus::Failed(error)
                }
                Ok(ProjectServerRetirementStatus::Pending) => ShutdownTaskStatus::TimedOut,
                Err(_) => ShutdownTaskStatus::TimedOut,
            };
        receipt.outcomes.push(ShutdownTaskOutcome { owner, status });
    }
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
        track_project_server_retirement(&self.project_server_retirements, owner, task, false).await;
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

    pub(crate) async fn completed_capacity_retirement_failure(&self) -> Option<String> {
        self.project_server_retirements
            .lock()
            .await
            .iter()
            .filter(|retirement| retirement.capacity_reuse)
            .find_map(|retirement| match &*retirement.completion.borrow() {
                ProjectServerRetirementStatus::Failed(error) => Some(error.clone()),
                ProjectServerRetirementStatus::Pending | ProjectServerRetirementStatus::Clean => {
                    None
                }
            })
    }

    /// Bounded retirement join for daemon shutdown: every tracked retirement
    /// is awaited up to `deadline` and reported under its owner's identity, so
    /// a hung retirement surfaces as a typed timeout instead of a silent hang.
    pub(crate) async fn join_project_server_retirements_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> ShutdownTaskReceipt {
        let completions =
            match tokio::time::timeout_at(deadline, self.project_server_retirements.lock()).await {
                Ok(retirements) => retirements
                    .iter()
                    .map(|retirement| {
                        (
                            retirement_shutdown_owner_label(&retirement.owner),
                            retirement.completion.clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
                Err(_) => {
                    return ShutdownTaskReceipt::timed_out("project_server_retirement_registry");
                }
            };
        let mut receipt = ShutdownTaskReceipt::default();
        for (owner, completion) in completions {
            let status = match tokio::time::timeout_at(
                deadline,
                wait_for_project_server_retirement(completion),
            )
            .await
            {
                Ok(ProjectServerRetirementStatus::Clean) => ShutdownTaskStatus::Clean,
                Ok(ProjectServerRetirementStatus::Failed(error)) => {
                    ShutdownTaskStatus::Failed(error)
                }
                Ok(ProjectServerRetirementStatus::Pending) => ShutdownTaskStatus::TimedOut,
                Err(_) => ShutdownTaskStatus::TimedOut,
            };
            receipt.outcomes.push(ShutdownTaskOutcome { owner, status });
        }
        if let Ok(mut retirements) =
            tokio::time::timeout_at(deadline, self.project_server_retirements.lock()).await
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
    use crate::daemon::store_writer_gate::{StoreWriterClass, WriterScope};

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
        crate::tracedecay::TraceDecay,
        crate::host_admission::HostAdmissionTestRuntimeV1,
    ) {
        std::fs::create_dir_all(profile_root).expect("isolated profile root");
        std::fs::create_dir_all(project_root).expect("isolated project root");
        let project_id = tracedecay_domain::ProjectId::new(project_id.to_owned())
            .expect("typed project identity");
        let runtime = crate::host_admission::HostAdmissionTestRuntimeV1::project(
            profile_root,
            project_root,
            project_id,
        )
        .await
        .expect("isolated host-admission runtime");
        let graph = runtime
            .initialize_project_graph_for_test(
                project_root,
                crate::tracedecay::TraceDecayOpenOptions {
                    profile_root: Some(profile_root.to_path_buf()),
                    global_db_path: None,
                },
            )
            .await
            .expect("isolated registered graph");
        (graph, runtime)
    }

    async fn isolated_sibling_graph(
        runtime: &crate::host_admission::HostAdmissionTestRuntimeV1,
        profile_root: &std::path::Path,
        project_root: &std::path::Path,
        project_id: &str,
    ) -> (
        crate::tracedecay::TraceDecay,
        crate::host_admission::HostAdmissionTestRuntimeV1,
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
                crate::tracedecay::TraceDecayOpenOptions {
                    profile_root: Some(profile_root.to_path_buf()),
                    global_db_path: None,
                },
            )
            .await
            .expect("isolated sibling registered graph");
        (graph, sibling)
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

        let roots = [std::path::PathBuf::from("/repository")]
            .into_iter()
            .collect();
        let invocation_registry = tracedecay_daemon_service::ProjectRuntimeRegistryV1::default();
        let invocation = invocation_registry
            .quiesce_roots(&roots)
            .await
            .expect("quiesce invocation roots");
        let open_tasks = crate::daemon::project_open_admission::ProjectOpenTasks::default();
        let project_open = open_tasks
            .quiesce_project_identity(std::path::Path::new("/profile"), "project-a", &roots)
            .await
            .expect("quiesce project-open identity");
        let scope = WriterScope::store("/profile/projects/project-a", StoreWriterClass::Owner);
        let writer = administration.gate.acquire(&scope).await;
        let fence = Arc::new(ProjectRetirementFenceV1::new(
            invocation,
            project_open,
            writer,
        ));
        attach_project_retirement_fence(
            retirements,
            std::path::Path::new("/profile"),
            "project-a",
            Arc::clone(&fence),
        )
        .await;
        drop(fence);

        let first = settle_project_retirements(
            retirements,
            std::path::Path::new("/profile"),
            "project-a",
            tokio::time::Instant::now(),
        )
        .await;
        assert_eq!(first.status(), ShutdownTaskStatus::TimedOut);
        assert_eq!(retirements.lock().await.len(), 2);
        assert!(
            administration.gate.try_acquire(&scope).is_none(),
            "a timed-out retirement must keep replacement publication fenced"
        );

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
        assert!(
            administration.gate.try_acquire(&scope).is_none(),
            "one completed owner cannot release the aggregate project fence"
        );
        let partially_settled = settle_project_retirements(
            retirements,
            std::path::Path::new("/profile"),
            "project-a",
            tokio::time::Instant::now(),
        )
        .await;
        assert_eq!(partially_settled.status(), ShutdownTaskStatus::TimedOut);
        assert_eq!(retirements.lock().await.len(), 1);
        assert!(
            administration.gate.try_acquire(&scope).is_none(),
            "the remaining owner retains its copy of the aggregate project fence"
        );
        second_release.notify_one();
        let second = settle_project_retirements(
            retirements,
            std::path::Path::new("/profile"),
            "project-a",
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .await;
        assert!(second.is_clean());
        assert!(retirements.lock().await.is_empty());
        assert!(
            administration.gate.try_acquire(&scope).is_some(),
            "retry releases the fence only after observing the retained owner complete"
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
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
        )
        .await;
        assert!(matches!(receipt.status(), ShutdownTaskStatus::Failed(_)));
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

        let mut admission = administration
            .acquire_project_server_retirement_admission()
            .await;
        let failed = admission.spawn_and_track_fallible(owner("project-failed"), async move {
            Err(tracedecay_domain::errors::TraceDecayError::Config {
                message: "synthetic capacity cleanup failure".to_owned(),
            })
        });
        drop(admission);
        drop(failed);
        let failure = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(failure) = administration.completed_capacity_retirement_failure().await
                {
                    break failure;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed capacity failure must remain replayable");
        assert!(failure.contains("synthetic capacity cleanup failure"));
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

        let mut shutdown = Box::pin(project_server_lifecycle::shutdown_project_servers(
            tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            &administration,
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
                    && outcome.status == ShutdownTaskStatus::Clean
            }),
            "shutdown must report the exact evicted owner through its retirement receipt"
        );
        assert!(
            idle_witness.upgrade().is_none(),
            "joined retirement must release the exact evicted server"
        );
    }
}
