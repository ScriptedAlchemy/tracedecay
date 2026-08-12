/// Completion latch shared by daemon-owned background maintenance tasks.
pub(in crate::daemon) struct MaintenanceTaskTermination {
    finished: tokio::sync::watch::Sender<bool>,
}

impl MaintenanceTaskTermination {
    pub(in crate::daemon) fn pending() -> Self {
        let (finished, _) = tokio::sync::watch::channel(false);
        Self { finished }
    }

    pub(in crate::daemon) async fn wait(&self) {
        self.wait_for_finish(self.finished.subscribe()).await;
    }

    async fn wait_for_finish(&self, mut finished: tokio::sync::watch::Receiver<bool>) {
        while !*finished.borrow_and_update() {
            if finished.changed().await.is_err() {
                return;
            }
        }
    }

    pub(in crate::daemon) fn finish(&self) {
        self.finished.send_replace(true);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::MaintenanceTaskTermination;

    #[tokio::test(start_paused = true)]
    async fn finish_before_wait_is_latched() {
        let termination = MaintenanceTaskTermination::pending();
        termination.finish();

        timeout(Duration::from_secs(1), termination.wait())
            .await
            .expect("finish before wait must not hang");
    }

    #[tokio::test(start_paused = true)]
    async fn finish_between_registration_and_condition_check_is_latched() {
        let termination = MaintenanceTaskTermination::pending();
        let registered = termination.finished.subscribe();
        termination.finish();

        timeout(
            Duration::from_secs(1),
            termination.wait_for_finish(registered),
        )
        .await
        .expect("finish after waiter registration must not hang");
    }
}
