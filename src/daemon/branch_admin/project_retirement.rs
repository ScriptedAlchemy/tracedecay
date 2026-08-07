use std::path::Path;

use super::super::store_shutdown::{ShutdownTaskOutcome, ShutdownTaskReceipt, ShutdownTaskStatus};
use super::{StoreAdministration, StoreOwnerKey};

pub(super) struct ProjectServerRetirement {
    pub(super) owner: StoreOwnerKey,
    completion: tokio::sync::watch::Receiver<bool>,
    _task: tokio::task::JoinHandle<()>,
}

struct ProjectServerRetirementFinalizer(tokio::sync::watch::Sender<bool>);

impl Drop for ProjectServerRetirementFinalizer {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

fn retirement_shutdown_owner_label(owner: &StoreOwnerKey) -> String {
    match &owner.project_id {
        Some(project_id) => format!("project_server_retirement[{project_id}]"),
        None => format!("project_server_retirement[{}]", owner.store_root.display()),
    }
}

async fn wait_for_project_server_retirement(mut completion: tokio::sync::watch::Receiver<bool>) {
    while !*completion.borrow() {
        if completion.changed().await.is_err() {
            return;
        }
    }
}

impl StoreAdministration {
    // pub(crate): project_server_lifecycle registers retirements from outside
    // branch_admin when a project server shuts down.
    pub(crate) async fn track_project_server_retirement(
        &self,
        owner: StoreOwnerKey,
        task: tokio::task::JoinHandle<()>,
    ) {
        let mut retirements = self.project_server_retirements.lock().await;
        retirements.retain(|retirement| !*retirement.completion.borrow());
        let (task_completion, completion) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let _completion = ProjectServerRetirementFinalizer(task_completion);
            let _ = task.await;
        });
        retirements.push(ProjectServerRetirement {
            owner,
            completion,
            _task: task,
        });
    }

    // pub(crate): the test-transport production harness joins retirements from
    // outside branch_admin during its shutdown sequence.
    pub(crate) async fn join_project_server_retirements(&self) {
        let completions = self
            .project_server_retirements
            .lock()
            .await
            .iter()
            .map(|retirement| retirement.completion.clone())
            .collect::<Vec<_>>();
        for completion in completions {
            wait_for_project_server_retirement(completion).await;
        }
        self.project_server_retirements
            .lock()
            .await
            .retain(|retirement| !*retirement.completion.borrow());
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
                Ok(()) => ShutdownTaskStatus::Clean,
                Err(_) => ShutdownTaskStatus::TimedOut,
            };
            receipt.outcomes.push(ShutdownTaskOutcome { owner, status });
        }
        if let Ok(mut retirements) =
            tokio::time::timeout_at(deadline, self.project_server_retirements.lock()).await
        {
            retirements.retain(|retirement| !*retirement.completion.borrow());
        }
        receipt
    }

    pub(super) async fn settle_project_server_retirements_for_project(
        &self,
        profile_root: &Path,
        project_id: &str,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        let completions = self
            .project_server_retirements
            .lock()
            .await
            .iter()
            .filter(|retirement| {
                retirement.owner.profile_root == profile_root
                    && retirement.owner.project_id.as_deref() == Some(project_id)
            })
            .map(|retirement| retirement.completion.clone())
            .collect::<Vec<_>>();
        let mut settled = true;
        for completion in completions {
            settled &=
                tokio::time::timeout_at(deadline, wait_for_project_server_retirement(completion))
                    .await
                    .is_ok();
        }
        self.project_server_retirements
            .lock()
            .await
            .retain(|retirement| !*retirement.completion.borrow());
        settled
    }
}
