use std::sync::Arc;

use super::super::store_shutdown::{ShutdownTaskOutcome, ShutdownTaskReceipt, ShutdownTaskStatus};
use super::{StoreAdministration, StoreOwnerKey};

pub(super) struct ProjectServerRetirement {
    pub(super) owner: StoreOwnerKey,
    completion: tokio::sync::watch::Receiver<ProjectServerRetirementStatus>,
    _task: tokio::task::JoinHandle<()>,
    _fence: Option<std::sync::Arc<ProjectRetirementFenceV1>>,
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
}

pub(in crate::daemon) struct ProjectRetirementFenceV1 {
    // Field order is lifecycle order: reopen roots before releasing the store
    // writer gate so a deletion owner can never have its permanent fence
    // removed by this temporary recovery guard.
    _invocation: crate::daemon::service::project_runtime::ProjectRuntimeRootQuiescenceV1,
    _project_open: crate::daemon::project_open_admission::ProjectOpenIdentityQuiescenceV1,
    _writer: crate::daemon::store_writer_gate::WriterAdmissionGuard,
}

impl ProjectRetirementFenceV1 {
    pub(super) fn new(
        invocation: crate::daemon::service::project_runtime::ProjectRuntimeRootQuiescenceV1,
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
    let task = tokio::spawn(async move {
        let finalizer = ProjectServerRetirementFinalizer {
            completion: task_completion,
            terminal: false,
        };
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
        StoreOwnerKey {
            profile_root: std::path::PathBuf::from("/profile"),
            global_db_path: std::path::PathBuf::from("/profile/profile.db"),
            project_id: Some(project_id.to_owned()),
            store_root: std::path::PathBuf::from(format!("/profile/projects/{project_id}")),
            graph_db_path: std::path::PathBuf::from(format!(
                "/profile/projects/{project_id}/graph.db"
            )),
        }
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
        let invocation_registry =
            crate::daemon::service::project_runtime::ProjectRuntimeRegistryV1::default();
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
    async fn cancellation_before_eviction_admission_preserves_owner_then_shutdown_joins_retirement()
    {
        let _pin = crate::config::PinnedUserDataDir::new();
        let projects = tempfile::tempdir().expect("project roots");
        let idle_project = projects.path().join("idle");
        let replacement_project = projects.path().join("replacement");
        std::fs::create_dir_all(&idle_project).expect("idle project root");
        std::fs::create_dir_all(&replacement_project).expect("replacement project root");
        let (idle_graph, _idle_runtime) =
            crate::tracedecay::TraceDecay::init_test_fixture_with_registered_runtime(
                &idle_project,
                "project.retirement-idle",
            )
            .await
            .expect("registered idle graph");
        let idle_server = crate::mcp::McpServer::new(idle_graph, None).await;
        let idle_lifecycle = idle_server.project_server_response_lifecycle();
        let idle_witness = Arc::downgrade(&idle_server);
        let (replacement_graph, _replacement_runtime) =
            crate::tracedecay::TraceDecay::init_test_fixture_with_registered_runtime(
                &replacement_project,
                "project.retirement-replacement",
            )
            .await
            .expect("registered replacement graph");
        let replacement_server = crate::mcp::McpServer::new(replacement_graph, None).await;
        let administration = StoreAdministration::default();
        let idle_key = crate::daemon::ProjectServerKey {
            owner: owner("project-idle"),
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
            servers.insert_pending_route(idle_route.clone(), idle_key.clone(), idle_server);
            assert!(servers.mark_ready(&idle_key));
        }

        let admission_blocker = administration
            .acquire_project_server_retirement_admission()
            .await;
        let attempted_admission = Arc::new(AtomicBool::new(false));
        let cancelled_administration = administration.clone();
        let cancelled_attempt = Arc::clone(&attempted_admission);
        let cancelled = tokio::spawn(async move {
            cancelled_attempt.store(true, Ordering::Release);
            let _admission = cancelled_administration
                .acquire_project_server_retirement_admission()
                .await;
            panic!("cancelled open reached owner mutation without admission contention");
        });
        tokio::task::yield_now().await;
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
            owner: owner("project-replacement"),
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
