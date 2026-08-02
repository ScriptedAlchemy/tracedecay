use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use super::super::client::{LspDocument, LspRefreshTimeouts, StdioLspClient};
use super::super::error::AnalyzerRuntimeError as TraceDecayError;
use super::{CodeDiagnostic, EngineState};

pub(crate) struct RefreshBatch {
    pub(crate) workspace_root: PathBuf,
    pub(crate) documents: Vec<LspDocument>,
    pub(crate) client: Arc<Mutex<Option<StdioLspClient>>>,
}

/// Each analyzer may wait behind at most this many independent workspace-root
/// batches. More roots are a truthful saturation result, never an unbounded
/// process fan-out.
pub const MAX_ANALYZER_QUEUED_ROOT_BATCHES: usize = 128;
/// Root batches share one analyzer configuration but run independently.
pub const MAX_ANALYZER_CONCURRENT_ROOT_FANOUTS: usize = 4;

/// Broker-owned capacity shared by every refresh prepared from one broker.
/// Queue permits reserve bounded work before a caller can retain it; run
/// permits live inside spawned batches, so completion, cancellation, and task
/// abortion release them without a separate cleanup path.
#[derive(Clone)]
pub(crate) struct BrokerRefreshCapacity {
    running: Arc<Semaphore>,
    queued: Arc<Semaphore>,
}

impl BrokerRefreshCapacity {
    pub(crate) fn new() -> Self {
        Self {
            running: Arc::new(Semaphore::new(MAX_ANALYZER_CONCURRENT_ROOT_FANOUTS)),
            queued: Arc::new(Semaphore::new(MAX_ANALYZER_QUEUED_ROOT_BATCHES)),
        }
    }

    pub(crate) fn reserve(&self, batches: usize) -> Option<PreparedRefreshReservation> {
        let batches = u32::try_from(batches).ok()?;
        self.queued
            .clone()
            .try_acquire_many_owned(batches)
            .ok()
            .map(|queued| PreparedRefreshReservation {
                _queued: queued,
                running: Arc::clone(&self.running),
            })
    }
}

pub(crate) struct PreparedRefreshReservation {
    _queued: OwnedSemaphorePermit,
    running: Arc<Semaphore>,
}

impl PreparedRefreshReservation {
    async fn acquire_run(&self) -> Option<OwnedSemaphorePermit> {
        self.running.clone().acquire_owned().await.ok()
    }
}

pub struct PreparedRefresh {
    language: String,
    project_root: PathBuf,
    command: String,
    args: Vec<String>,
    epoch: u64,
    batches: Vec<RefreshBatch>,
    reservation: PreparedRefreshReservation,
}

pub struct CompletedRefresh {
    pub(crate) language: String,
    pub(crate) command: String,
    pub(crate) epoch: u64,
    pub(crate) result: std::result::Result<Vec<CodeDiagnostic>, RefreshFailure>,
}

impl CompletedRefresh {
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }
}

impl PreparedRefresh {
    pub(crate) fn new(
        language: String,
        project_root: PathBuf,
        command: String,
        args: Vec<String>,
        epoch: u64,
        batches: Vec<RefreshBatch>,
        reservation: PreparedRefreshReservation,
    ) -> Self {
        Self {
            language,
            project_root,
            command,
            args,
            epoch,
            batches,
            reservation,
        }
    }

    pub async fn collect_diagnostics(
        self,
        diagnostics_quiet_timeout: Duration,
    ) -> CompletedRefresh {
        self.collect_diagnostics_with_timeouts(LspRefreshTimeouts::from_diagnostics_quiet_window(
            diagnostics_quiet_timeout,
        ))
        .await
    }

    pub async fn collect_diagnostics_with_timeouts(
        self,
        timeouts: LspRefreshTimeouts,
    ) -> CompletedRefresh {
        let language = self.language.clone();
        let command = self.command.clone();
        let epoch = self.epoch;
        let result = self.collect(timeouts).await;
        CompletedRefresh {
            language,
            command,
            epoch,
            result,
        }
    }

    async fn collect(
        self,
        timeouts: LspRefreshTimeouts,
    ) -> std::result::Result<Vec<CodeDiagnostic>, RefreshFailure> {
        let mut pending = tokio::task::JoinSet::new();
        let mut batches = self.batches.into_iter().enumerate();
        let mut completed = Vec::new();

        while pending.len() < MAX_ANALYZER_CONCURRENT_ROOT_FANOUTS {
            let Some((ordinal, batch)) = batches.next() else {
                break;
            };
            let run_permit = self.reservation.acquire_run().await.ok_or_else(|| {
                RefreshFailure::crashed_message("broker run capacity unavailable".to_owned())
            })?;
            pending.spawn(collect_refresh_batch(
                ordinal,
                batch,
                self.project_root.clone(),
                self.command.clone(),
                self.args.clone(),
                timeouts,
                run_permit,
            ));
        }

        while let Some(result) = pending.join_next().await {
            let result = result.map_err(|error| {
                RefreshFailure::crashed_message(format!(
                    "analyzer refresh task terminated: {error}"
                ))
            })?;
            completed.push(result?);
            if let Some((ordinal, batch)) = batches.next() {
                let run_permit = self.reservation.acquire_run().await.ok_or_else(|| {
                    RefreshFailure::crashed_message("broker run capacity unavailable".to_owned())
                })?;
                pending.spawn(collect_refresh_batch(
                    ordinal,
                    batch,
                    self.project_root.clone(),
                    self.command.clone(),
                    self.args.clone(),
                    timeouts,
                    run_permit,
                ));
            }
        }
        completed.sort_by_key(|(ordinal, _)| *ordinal);
        Ok(completed
            .into_iter()
            .flat_map(|(_, diagnostics)| diagnostics)
            .collect())
    }
}

async fn collect_refresh_batch(
    ordinal: usize,
    batch: RefreshBatch,
    project_root: PathBuf,
    command: String,
    args: Vec<String>,
    timeouts: LspRefreshTimeouts,
    _run_permit: OwnedSemaphorePermit,
) -> std::result::Result<(usize, Vec<CodeDiagnostic>), RefreshFailure> {
    let mut client_slot = batch.client.lock().await;
    let mut client = match client_slot.take() {
        Some(client) => client,
        None => {
            StdioLspClient::start_with_timeouts(&command, &args, &batch.workspace_root, timeouts)
                .await
                .map_err(|error| RefreshFailure::crashed(&error))?
        }
    };
    match client
        .collect_document_diagnostics(&project_root, batch.documents, timeouts)
        .await
    {
        Ok(diagnostics) => {
            *client_slot = Some(client);
            Ok((ordinal, diagnostics))
        }
        Err(error) => {
            *client_slot = None;
            Err(RefreshFailure::crashed(&error))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefreshFailure {
    pub(crate) state: EngineState,
    pub(crate) message: String,
}

impl RefreshFailure {
    fn crashed(error: &TraceDecayError) -> Self {
        Self::crashed_message(error.to_string())
    }

    fn crashed_message(message: String) -> Self {
        Self {
            state: EngineState::Crashed,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn prepared_refreshes_share_four_running_batch_permits() {
        let capacity = BrokerRefreshCapacity::new();
        let first = capacity.reserve(2).expect("first prepared refresh");
        let second = capacity.reserve(2).expect("second prepared refresh");
        let (first_a, first_b, second_a, second_b) = tokio::join!(
            first.acquire_run(),
            first.acquire_run(),
            second.acquire_run(),
            second.acquire_run(),
        );
        let permits = [first_a, first_b, second_a, second_b];
        assert!(permits.iter().all(Option::is_some));
        assert!(capacity.running.clone().try_acquire_owned().is_err());
    }

    #[test]
    fn global_queued_batch_reservation_rejects_the_129th_batch() {
        let capacity = BrokerRefreshCapacity::new();
        let _full = capacity
            .reserve(MAX_ANALYZER_QUEUED_ROOT_BATCHES)
            .expect("full queue reservation");

        assert!(capacity.reserve(1).is_none());
    }

    #[tokio::test]
    async fn aborted_prepared_work_releases_queued_and_running_capacity() {
        let capacity = BrokerRefreshCapacity::new();
        let reservation = capacity
            .reserve(MAX_ANALYZER_QUEUED_ROOT_BATCHES)
            .expect("full queue reservation");
        let (started, observed) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _run = reservation.acquire_run().await;
            let _ = started.send(());
            pending::<()>().await;
        });
        observed.await.expect("run permit acquired");
        task.abort();
        let _ = task.await;

        let released = capacity
            .reserve(MAX_ANALYZER_QUEUED_ROOT_BATCHES)
            .expect("aborted work releases queued capacity");
        let (one, two, three, four) = tokio::join!(
            released.acquire_run(),
            released.acquire_run(),
            released.acquire_run(),
            released.acquire_run(),
        );
        let permits = [one, two, three, four];
        assert!(permits.iter().all(Option::is_some));
        assert!(capacity.running.clone().try_acquire_owned().is_err());
    }

    #[tokio::test]
    async fn dropped_prepared_work_releases_queued_and_running_capacity() {
        let capacity = BrokerRefreshCapacity::new();
        let reservation = capacity
            .reserve(MAX_ANALYZER_QUEUED_ROOT_BATCHES)
            .expect("initial reservation should fit");
        let running = reservation
            .acquire_run()
            .await
            .expect("run permit should be available");

        drop(reservation);
        assert!(capacity.reserve(MAX_ANALYZER_QUEUED_ROOT_BATCHES).is_some());

        drop(running);
        let (one, two, three, four) = tokio::join!(
            capacity.running.clone().acquire_owned(),
            capacity.running.clone().acquire_owned(),
            capacity.running.clone().acquire_owned(),
            capacity.running.clone().acquire_owned(),
        );
        assert!(one.is_ok() && two.is_ok() && three.is_ok() && four.is_ok());
    }
}
