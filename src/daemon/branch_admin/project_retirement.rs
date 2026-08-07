use std::path::Path;

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

async fn wait_for_project_server_retirement(mut completion: tokio::sync::watch::Receiver<bool>) {
    while !*completion.borrow() {
        if completion.changed().await.is_err() {
            return;
        }
    }
}

impl StoreAdministration {
    pub(super) async fn track_project_server_retirement(
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

    pub(super) async fn join_project_server_retirements(&self) {
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
