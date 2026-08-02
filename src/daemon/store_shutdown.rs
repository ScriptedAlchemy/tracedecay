use super::StoreAdministration;

impl StoreAdministration {
    pub(super) async fn track_project_server_retirement(&self, task: tokio::task::JoinHandle<()>) {
        let mut retirements = self.project_server_retirements.lock().await;
        retirements.retain(|retirement| !retirement.is_finished());
        retirements.push(task);
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub(super) async fn join_project_server_retirements(&self) {
        let retirements = std::mem::take(&mut *self.project_server_retirements.lock().await);
        for retirement in retirements {
            let _ = retirement.await;
        }
    }

    pub(super) async fn join_project_server_retirements_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> usize {
        let mut retirements = std::mem::take(&mut *self.project_server_retirements.lock().await);
        let mut unfinished = 0usize;
        for retirement in &mut retirements {
            if tokio::time::timeout_at(deadline, &mut *retirement)
                .await
                .is_err()
            {
                unfinished = unfinished.saturating_add(1);
                retirement.abort();
            }
        }
        unfinished
    }

    pub(super) async fn shutdown_host_admission_replay(&self) {
        self.profile_host_admission_replay.shutdown().await;
    }

    pub(super) fn cancel_host_admission_replay(&self) {
        self.profile_host_admission_replay.cancel();
    }
}
