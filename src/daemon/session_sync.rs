//! Daemon-owned bounded convergence of native host transcripts and session/Git links.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tracedecay_application::session_sync::{
    SessionSyncAdmissionErrorV1, SessionSyncAdmissionReceiptV1, SessionSyncCommandV1,
    SessionSyncCompletionReceiptV1, SessionSyncFuture, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncServicePort, SessionSyncShutdownFuture, SessionSyncStatsV1,
};
use tracedecay_application::{OperationTermination, now_micros};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId};

use crate::global_db::{AnalyticsEventQuery, RegisteredGlobalDb};
use crate::store::{GlobalDbGitCorrelationStore, GlobalDbSessionIngestAuthority};

const MAX_RETAINED_SESSION_SYNC_OPERATIONS: usize = 128;
const SESSION_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(10);
const GIT_SYNC_ANALYTICS_LIMIT: usize = 500_000;

pub(crate) struct DaemonSessionSyncService {
    worker: SessionSyncWorker,
    operations: Arc<Mutex<BTreeMap<String, SessionSyncOperationState>>>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

pub(crate) struct DaemonSessionSyncConfig {
    pub brain_id: BrainId,
    pub profile_id: UserProfileId,
    pub project_id: ProjectId,
    pub profile_root: std::path::PathBuf,
    pub project_root: std::path::PathBuf,
    pub transcript_source_home: Option<std::path::PathBuf>,
    pub project_sessions: Arc<RegisteredGlobalDb>,
    pub user_sessions: Arc<RegisteredGlobalDb>,
    pub registry: Arc<RegisteredGlobalDb>,
    pub analytics: Option<Arc<RegisteredGlobalDb>>,
}

#[derive(Clone)]
struct SessionSyncWorker {
    brain_id: BrainId,
    profile_id: UserProfileId,
    project_id: ProjectId,
    profile_root: std::path::PathBuf,
    project_root: std::path::PathBuf,
    transcript_source_home: Option<std::path::PathBuf>,
    project_sessions: Arc<RegisteredGlobalDb>,
    user_sessions: Arc<RegisteredGlobalDb>,
    registry: Arc<RegisteredGlobalDb>,
    analytics: Option<Arc<RegisteredGlobalDb>>,
    shutdown: crate::application::observation::ObservationCancellation,
}

#[derive(Clone)]
enum SessionSyncOperationState {
    Running(SessionSyncAdmissionReceiptV1),
    Complete(SessionSyncCompletionReceiptV1),
}

impl DaemonSessionSyncService {
    pub(crate) fn new(config: DaemonSessionSyncConfig) -> Self {
        Self {
            worker: SessionSyncWorker {
                brain_id: config.brain_id,
                profile_id: config.profile_id,
                project_id: config.project_id,
                profile_root: config.profile_root,
                project_root: config.project_root,
                transcript_source_home: config.transcript_source_home,
                project_sessions: config.project_sessions,
                user_sessions: config.user_sessions,
                registry: config.registry,
                analytics: config.analytics,
                shutdown: crate::application::observation::ObservationCancellation::default(),
            },
            operations: Arc::new(Mutex::new(BTreeMap::new())),
            tasks: Mutex::new(Vec::new()),
        }
    }

    fn execute_request(&self, request: SessionSyncRequestV1) -> SessionSyncOutcomeV1 {
        let observed_at = now_micros();
        match request.admit_at(observed_at) {
            Ok(()) => {}
            Err(SessionSyncAdmissionErrorV1::Cancelled) => {
                return SessionSyncOutcomeV1::Cancelled;
            }
            Err(SessionSyncAdmissionErrorV1::DeadlineExceeded) => {
                return SessionSyncOutcomeV1::DeadlineExceeded;
            }
        }
        if request.scope().project_id() != &self.worker.project_id
            || request.scope().profile_id() != &self.worker.profile_id
        {
            return SessionSyncOutcomeV1::WrongScope;
        }
        let key = request.idempotency_key().as_str().to_owned();
        let mut operations = self
            .operations
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(existing) = operations.get(&key) {
            return existing_operation_outcome(existing);
        }
        if operations.len() >= MAX_RETAINED_SESSION_SYNC_OPERATIONS
            && let Some(completed_key) = operations.iter().find_map(|(key, state)| {
                matches!(state, SessionSyncOperationState::Complete(_)).then(|| key.clone())
            })
        {
            operations.remove(&completed_key);
        }
        if operations.len() >= MAX_RETAINED_SESSION_SYNC_OPERATIONS {
            return SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_capacity_reached",
            };
        }
        let admission = SessionSyncAdmissionReceiptV1 {
            operation_id: request.operation_id().clone(),
            idempotency_key: request.idempotency_key().clone(),
            accepted_at: observed_at,
        };
        operations.insert(
            key.clone(),
            SessionSyncOperationState::Running(admission.clone()),
        );
        drop(operations);

        let worker = self.worker.clone();
        let operations = Arc::clone(&self.operations);
        let task = tokio::spawn(async move {
            let completion = worker.run(request, admission).await;
            operations
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key, SessionSyncOperationState::Complete(completion));
        });
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
        SessionSyncOutcomeV1::Accepted(admission)
    }
}

impl SessionSyncServicePort for DaemonSessionSyncService {
    fn execute(&self, request: SessionSyncRequestV1) -> SessionSyncFuture<'_> {
        Box::pin(async move { self.execute_request(request) })
    }

    fn shutdown(&self) -> SessionSyncShutdownFuture<'_> {
        Box::pin(async move {
            self.worker.shutdown.cancel();
            let tasks = {
                let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
                std::mem::take(&mut *tasks)
            };
            for mut task in tasks {
                match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) if error.is_cancelled() => {}
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "session sync worker failed during shutdown");
                    }
                    Err(_) => {
                        task.abort();
                        if let Err(error) = task.await
                            && !error.is_cancelled()
                        {
                            tracing::warn!(%error, "session sync worker abort failed");
                        }
                    }
                }
            }
        })
    }
}

impl SessionSyncWorker {
    async fn run(
        self,
        request: SessionSyncRequestV1,
        admission: SessionSyncAdmissionReceiptV1,
    ) -> SessionSyncCompletionReceiptV1 {
        let (stats, failure_codes) = match request.command() {
            SessionSyncCommandV1::ImportTranscripts(_) => self.import_transcripts(&request).await,
            SessionSyncCommandV1::SynchronizeGit(options) => {
                self.synchronize_git(&request, options).await
            }
        };
        let cancellation = request.cancellation().is_cancelled() || self.shutdown.is_cancelled();
        let deadline = request.deadline().is_elapsed_at(now_micros());
        let termination =
            completion_termination(cancellation, deadline, &stats, failure_codes.is_empty());
        SessionSyncCompletionReceiptV1 {
            admission,
            completed_at: now_micros(),
            termination,
            stats,
            failure_codes,
        }
    }

    async fn import_transcripts(
        &self,
        request: &SessionSyncRequestV1,
    ) -> (SessionSyncStatsV1, Vec<String>) {
        let cancellation = crate::application::observation::ObservationCancellation::default();
        let pass_cancellation = cancellation.clone();
        let signal = request.cancellation().clone();
        let deadline = request.deadline().clone();
        let brain_id = self.brain_id.clone();
        let profile_id = self.profile_id.clone();
        let project_id = self.project_id.clone();
        let project_root = self.project_root.clone();
        let profile_root = self.profile_root.clone();
        let project_sessions = Arc::clone(&self.project_sessions);
        let user_sessions = Arc::clone(&self.user_sessions);
        let registry = Arc::clone(&self.registry);
        let pass = async move {
            let project_authority = GlobalDbSessionIngestAuthority::new(project_sessions);
            let project = crate::sessions::ingest_project_sources_for_provider_with_cancellation(
                &brain_id,
                &profile_id,
                &project_authority,
                &project_root,
                Some(project_id),
                None,
                true,
                &pass_cancellation,
            )
            .await;
            let user_authority = GlobalDbSessionIngestAuthority::new(user_sessions);
            let registry_authority = GlobalDbSessionIngestAuthority::new(registry);
            let user =
                crate::sessions::ingest_user_global_sources_for_provider_with_authorities_and_cancellation(
                    &brain_id,
                    &profile_id,
                    &user_authority,
                    &registry_authority,
                    &profile_root,
                    None,
                    &pass_cancellation,
                )
                .await;
            (project, user)
        };
        let pass = async {
            match &self.transcript_source_home {
                Some(home) => {
                    crate::sessions::with_transcript_source_home(home.clone(), pass).await
                }
                None => pass.await,
            }
        };
        tokio::pin!(pass);
        let (project, user) = loop {
            tokio::select! {
                outcomes = &mut pass => break outcomes,
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if signal.is_cancelled()
                        || self.shutdown.is_cancelled()
                        || deadline.is_elapsed_at(now_micros())
                    {
                        cancellation.cancel();
                    }
                }
            }
        };
        let combined = project.stats.merge(user.stats);
        let failure_codes = project
            .failures
            .into_iter()
            .chain(user.failures)
            .map(|failure| failure.reason_code.to_owned())
            .collect();
        (
            SessionSyncStatsV1 {
                sessions_imported: combined.sessions_upserted,
                messages_imported: combined.messages_upserted,
                ..SessionSyncStatsV1::default()
            },
            failure_codes,
        )
    }

    async fn synchronize_git(
        &self,
        request: &SessionSyncRequestV1,
        options: tracedecay_application::session_sync::SessionGitSyncV1,
    ) -> (SessionSyncStatsV1, Vec<String>) {
        let Some(analytics) = self.analytics.as_ref() else {
            return (
                SessionSyncStatsV1::default(),
                vec!["analytics_authority_unavailable".to_owned()],
            );
        };
        let project_key = RegisteredGlobalDb::canonical_project_key(&self.project_root);
        let analytics_query = AnalyticsEventQuery {
            project_id: Some(project_key),
            since: Some(options.since_unix()),
            limit: GIT_SYNC_ANALYTICS_LIMIT,
            ..AnalyticsEventQuery::default()
        };
        let query = analytics.query_analytics_events(&analytics_query);
        tokio::pin!(query);
        let events = match loop {
            tokio::select! {
                result = &mut query => break Some(result),
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if request.cancellation().is_cancelled()
                        || self.shutdown.is_cancelled()
                        || request.deadline().is_elapsed_at(now_micros())
                    {
                        break None;
                    }
                }
            }
        } {
            Some(Ok(events)) => events,
            Some(Err(error)) => {
                tracing::warn!(%error, "session git sync analytics read failed");
                return (
                    SessionSyncStatsV1::default(),
                    vec!["analytics_read_failed".to_owned()],
                );
            }
            None => {
                return (
                    SessionSyncStatsV1::default(),
                    vec!["session_sync_interrupted".to_owned()],
                );
            }
        };
        let store = GlobalDbGitCorrelationStore::new(Arc::clone(&self.project_sessions));
        let sync = store.run_backfill(
            &events,
            &crate::sessions::git_correlation::SystemGit,
            &crate::sessions::git_correlation::BackfillOptions {
                since: options.since_unix(),
                limit_sessions: options.max_sessions(),
                merge_gap_secs: crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
                max_commits_per_repo: 5_000,
                dry_run: options.dry_run(),
            },
        );
        tokio::pin!(sync);
        let result = loop {
            tokio::select! {
                result = &mut sync => break Some(result),
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if request.cancellation().is_cancelled()
                        || self.shutdown.is_cancelled()
                        || request.deadline().is_elapsed_at(now_micros())
                    {
                        break None;
                    }
                }
            }
        };
        match result {
            Some(Ok(stats)) => (
                SessionSyncStatsV1 {
                    sessions_scanned: saturating_usize_to_u64(stats.sessions_scanned),
                    spans_written: saturating_usize_to_u64(stats.spans_written),
                    commits_attributed: saturating_usize_to_u64(stats.commits_attributed),
                    skipped: saturating_usize_to_u64(stats.skipped_total()),
                    ..SessionSyncStatsV1::default()
                },
                Vec::new(),
            ),
            Some(Err(error)) => {
                tracing::warn!(%error, "session git sync failed");
                (
                    SessionSyncStatsV1::default(),
                    vec!["git_sync_failed".to_owned()],
                )
            }
            None => (
                SessionSyncStatsV1::default(),
                vec!["session_sync_interrupted".to_owned()],
            ),
        }
    }
}

fn saturating_usize_to_u64(value: usize) -> u64 {
    match u64::try_from(value) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
}

fn existing_operation_outcome(state: &SessionSyncOperationState) -> SessionSyncOutcomeV1 {
    match state {
        SessionSyncOperationState::Running(receipt) => {
            SessionSyncOutcomeV1::Joined(receipt.clone())
        }
        SessionSyncOperationState::Complete(receipt) => {
            SessionSyncOutcomeV1::Complete(receipt.clone())
        }
    }
}

fn completion_termination(
    cancelled: bool,
    deadline_elapsed: bool,
    stats: &SessionSyncStatsV1,
    failures_empty: bool,
) -> OperationTermination {
    if cancelled {
        OperationTermination::Cancelled
    } else if deadline_elapsed {
        OperationTermination::TimedOut
    } else if failures_empty {
        OperationTermination::Completed
    } else if stats != &SessionSyncStatsV1::default() {
        OperationTermination::Partial
    } else {
        OperationTermination::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionSyncOperationState, completion_termination, existing_operation_outcome};
    use tracedecay_application::session_sync::{
        SessionSyncAdmissionReceiptV1, SessionSyncOutcomeV1, SessionSyncStatsV1,
    };
    use tracedecay_application::{IdempotencyKey, OperationTermination, RequestId};
    use tracedecay_domain::UtcMicros;

    fn admission() -> SessionSyncAdmissionReceiptV1 {
        SessionSyncAdmissionReceiptV1 {
            operation_id: RequestId::new("session-sync.duplicate").unwrap(),
            idempotency_key: IdempotencyKey::new("session-sync.duplicate").unwrap(),
            accepted_at: UtcMicros(10),
        }
    }

    #[test]
    fn duplicate_running_request_joins_the_original_receipt() {
        let outcome = existing_operation_outcome(&SessionSyncOperationState::Running(admission()));

        assert!(matches!(
            outcome,
            SessionSyncOutcomeV1::Joined(receipt)
                if receipt.operation_id.as_str() == "session-sync.duplicate"
                    && receipt.accepted_at == UtcMicros(10)
        ));
    }

    #[test]
    fn committed_rows_plus_a_provider_failure_are_partial_coverage() {
        let stats = SessionSyncStatsV1 {
            sessions_imported: 1,
            messages_imported: 3,
            ..SessionSyncStatsV1::default()
        };

        assert_eq!(
            completion_termination(false, false, &stats, false),
            OperationTermination::Partial
        );
        assert_eq!(
            completion_termination(false, false, &SessionSyncStatsV1::default(), false),
            OperationTermination::Failed
        );
    }

    #[test]
    fn cancellation_and_deadline_remain_distinct_terminal_receipts() {
        assert_eq!(
            completion_termination(true, false, &SessionSyncStatsV1::default(), true),
            OperationTermination::Cancelled
        );
        assert_eq!(
            completion_termination(false, true, &SessionSyncStatsV1::default(), true),
            OperationTermination::TimedOut
        );
    }
}
