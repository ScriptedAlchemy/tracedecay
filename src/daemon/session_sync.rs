//! One daemon-wide authority for bounded native transcript acquisition and session/Git sync.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use tracedecay_application::session_sync::{
    SessionGitSyncV1, SessionSyncAdmissionErrorV1, SessionSyncCommandV1,
    SessionSyncCompletionReceiptV1, SessionSyncControlV1, SessionSyncCoverageV1, SessionSyncFuture,
    SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncScopeV1, SessionSyncServicePort, SessionSyncShutdownFuture,
    SessionSyncSourceCoverageV1, SessionSyncSourceFrontierV1, SessionSyncStatsV1,
};
use tracedecay_application::{
    CancellationSignal, IdempotencyKey, OperationTermination, now_micros,
};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId, UtcMicros};

use crate::global_db::RegisteredGlobalDb;
use crate::store::{GlobalDbGitCorrelationStore, GlobalDbSessionIngestAuthority};

const MAX_SESSION_SYNC_OPERATIONS: usize = 128;
const SESSION_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SESSION_SYNC_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);
const SESSION_SYNC_SHUTDOWN_ABORT_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(crate) struct DaemonSessionSyncService {
    contexts: Arc<RwLock<BTreeMap<String, Arc<SessionSyncProjectContext>>>>,
    active: Arc<Mutex<BTreeMap<String, CancellationSignal>>>,
    tasks: Arc<Mutex<Vec<SessionSyncTaskV1>>>,
    scan_slots: Arc<tokio::sync::Semaphore>,
    project_gates: Arc<Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    active_imports: Arc<Mutex<BTreeMap<String, ActiveSessionImport>>>,
    completed_profile_sweeps: Arc<Mutex<BTreeMap<String, UtcMicros>>>,
    shutdown: tracedecay_usecases::observation::ObservationCancellation,
}

#[derive(Clone)]
struct ActiveSessionImport {
    admission: tracedecay_application::session_sync::SessionSyncAdmissionReceiptV1,
    journal_key: String,
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
    pub startup_import: bool,
    pub project_refresh:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub user_refresh: crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
}

enum SessionSyncWorkResult {
    Finished {
        interruption: Option<work::SessionSyncInterruption>,
        committed: bool,
        stats: SessionSyncStatsV1,
        coverage: Vec<SessionSyncSourceCoverageV1>,
        source_frontiers: Vec<SessionSyncSourceFrontierV1>,
        failure_codes: Vec<String>,
    },
    Interrupted(work::SessionSyncInterruption),
}

impl Default for DaemonSessionSyncService {
    fn default() -> Self {
        Self {
            contexts: Arc::new(RwLock::new(BTreeMap::new())),
            active: Arc::new(Mutex::new(BTreeMap::new())),
            tasks: Arc::new(Mutex::new(Vec::new())),
            scan_slots: Arc::new(tokio::sync::Semaphore::new(1)),
            project_gates: Arc::new(Mutex::new(BTreeMap::new())),
            active_imports: Arc::new(Mutex::new(BTreeMap::new())),
            completed_profile_sweeps: Arc::new(Mutex::new(BTreeMap::new())),
            shutdown: tracedecay_usecases::observation::ObservationCancellation::default(),
        }
    }
}

impl DaemonSessionSyncService {
    async fn execute_request_admitted(
        &self,
        request: SessionSyncRequestV1,
        context: Arc<SessionSyncProjectContext>,
        project_sessions: Arc<RegisteredGlobalDb>,
    ) -> SessionSyncOutcomeV1 {
        let observed_at = now_micros();
        let key = journal_key(request.scope(), request.idempotency_key());
        match context.registry.read_session_sync_journal(&key).await {
            Ok(Some(encoded)) => {
                return match decode_matching_journal(&encoded, &request) {
                    Ok(journal) => {
                        let admission = journal.admission.clone();
                        if journal.status != SessionSyncJournalStatusV1::Complete
                            && let Some(primary) = journal.coalesced_primary.clone()
                        {
                            let primary_key = journal_key(request.scope(), &primary);
                            match self
                                .mirror_primary_terminal(&context, &key, &primary_key)
                                .await
                            {
                                Ok(Some(journal)) => return journal.outcome(),
                                Ok(None) => {}
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "session sync alias replay reconciliation failed"
                                    );
                                    return SessionSyncOutcomeV1::Unavailable {
                                        reason_code: "session_sync_coalesced_journal_read_failed",
                                    };
                                }
                            }
                            if journal.deadline.is_elapsed_at(observed_at) {
                                return match self
                                    .persist_interruption_with_project_sessions(
                                        &context,
                                        &project_sessions,
                                        &key,
                                        OperationTermination::TimedOut,
                                    )
                                    .await
                                {
                                    Ok(journal) => journal.outcome(),
                                    Err(_) => SessionSyncOutcomeV1::Unavailable {
                                        reason_code: "session_sync_journal_write_failed",
                                    },
                                };
                            }
                            if !self.active_contains(&key) {
                                self.coalesce_import(
                                    Arc::clone(&context),
                                    Arc::clone(&project_sessions),
                                    key,
                                    journal.clone(),
                                    primary_key,
                                    request.cancellation().clone(),
                                );
                            }
                        } else if journal.status != SessionSyncJournalStatusV1::Complete
                            && journal.deadline.is_elapsed_at(observed_at)
                        {
                            return match self
                                .persist_interruption_with_project_sessions(
                                    &context,
                                    &project_sessions,
                                    &key,
                                    OperationTermination::TimedOut,
                                )
                                .await
                            {
                                Ok(journal) => journal.outcome(),
                                Err(_) => SessionSyncOutcomeV1::Unavailable {
                                    reason_code: "session_sync_journal_write_failed",
                                },
                            };
                        } else if journal.status != SessionSyncJournalStatusV1::Complete
                            && !self.enqueue(
                                context,
                                Arc::clone(&project_sessions),
                                key,
                                request,
                                admission,
                            )
                        {
                            return SessionSyncOutcomeV1::Unavailable {
                                reason_code: "session_sync_capacity_reached",
                            };
                        }
                        journal.outcome()
                    }
                    Err(reason_code) => SessionSyncOutcomeV1::Unavailable { reason_code },
                };
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "session sync journal read failed");
                return SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_journal_read_failed",
                };
            }
        }
        match request.admit_at(observed_at) {
            Ok(()) => {}
            Err(SessionSyncAdmissionErrorV1::Cancelled) => {
                return SessionSyncOutcomeV1::Cancelled;
            }
            Err(SessionSyncAdmissionErrorV1::DeadlineExceeded) => {
                return SessionSyncOutcomeV1::DeadlineExceeded;
            }
        }
        let active_import = matches!(
            request.command(),
            SessionSyncCommandV1::ImportTranscripts(_)
        )
        .then(|| {
            self.active_imports
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&import_scope_key(request.scope()))
                .cloned()
        })
        .flatten();
        if let Some(primary) = active_import {
            if self.active_count() >= MAX_SESSION_SYNC_OPERATIONS {
                return SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_capacity_reached",
                };
            }
            let journal = SessionSyncJournalV1::coalesced(
                &request,
                observed_at,
                primary.admission.idempotency_key.clone(),
            );
            let encoded = match serde_json::to_string(&journal) {
                Ok(encoded) => encoded,
                Err(error) => {
                    tracing::warn!(%error, "session sync alias journal encoding failed");
                    return SessionSyncOutcomeV1::Unavailable {
                        reason_code: "session_sync_journal_encode_failed",
                    };
                }
            };
            match context
                .registry
                .insert_session_sync_journal(&key, &encoded)
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    return self
                        .status_request_admitted(
                            &context,
                            Some(&project_sessions),
                            SessionSyncControlV1::new(
                                request.scope().clone(),
                                request.idempotency_key().clone(),
                            ),
                        )
                        .await;
                }
                Err(error) => {
                    tracing::warn!(%error, "session sync alias journal admission failed");
                    return SessionSyncOutcomeV1::Unavailable {
                        reason_code: "session_sync_journal_write_failed",
                    };
                }
            }
            let admission = journal.admission.clone();
            self.coalesce_import(
                context,
                project_sessions,
                key,
                journal,
                primary.journal_key,
                request.cancellation().clone(),
            );
            return SessionSyncOutcomeV1::Accepted(admission);
        }
        if self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
            >= MAX_SESSION_SYNC_OPERATIONS
        {
            return SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_capacity_reached",
            };
        }
        let journal = SessionSyncJournalV1::queued(&request, observed_at);
        let encoded = match serde_json::to_string(&journal) {
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::warn!(%error, "session sync journal encoding failed");
                return SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_journal_encode_failed",
                };
            }
        };
        match context
            .registry
            .insert_session_sync_journal(&key, &encoded)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return self
                    .status_request_admitted(
                        &context,
                        Some(&project_sessions),
                        SessionSyncControlV1::new(
                            request.scope().clone(),
                            request.idempotency_key().clone(),
                        ),
                    )
                    .await;
            }
            Err(error) => {
                tracing::warn!(%error, "session sync journal admission failed");
                return SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_journal_write_failed",
                };
            }
        }
        let admission = journal.admission.clone();
        if !self.enqueue(context, project_sessions, key, request, admission.clone()) {
            return SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_capacity_reached",
            };
        }
        SessionSyncOutcomeV1::Accepted(admission)
    }

    fn active_contains(&self, key: &str) -> bool {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(key)
    }

    fn active_count(&self) -> usize {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    fn enqueue(
        &self,
        context: Arc<SessionSyncProjectContext>,
        project_sessions: Arc<RegisteredGlobalDb>,
        key: String,
        request: SessionSyncRequestV1,
        admission: tracedecay_application::session_sync::SessionSyncAdmissionReceiptV1,
    ) -> bool {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.contains_key(&key) {
            return true;
        }
        if active.len() >= MAX_SESSION_SYNC_OPERATIONS {
            return false;
        }
        active.insert(key.clone(), request.cancellation().clone());
        let import_scope = matches!(
            request.command(),
            SessionSyncCommandV1::ImportTranscripts(_)
        )
        .then(|| import_scope_key(request.scope()));
        if let Some(import_scope) = import_scope.as_ref() {
            self.active_imports
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(
                    import_scope.clone(),
                    ActiveSessionImport {
                        admission,
                        journal_key: key.clone(),
                    },
                );
        }
        drop(active);
        let service = self.clone();
        let task_scope = request.scope().clone();
        let task_key = key.clone();
        let task_cancellation = request.cancellation().clone();
        let task = tokio::spawn(async move {
            service
                .run_operation(context, project_sessions, key.clone(), request)
                .await;
            service
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
            if let Some(import_scope) = import_scope {
                service
                    .active_imports
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&import_scope);
            }
        });
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|task| !task.task.is_finished());
        tasks.push(SessionSyncTaskV1 {
            scope: task_scope,
            key: task_key,
            cancellation: task_cancellation,
            task,
        });
        true
    }

    async fn run_operation(
        &self,
        context: Arc<SessionSyncProjectContext>,
        project_sessions: Arc<RegisteredGlobalDb>,
        key: String,
        request: SessionSyncRequestV1,
    ) {
        let permit = loop {
            tokio::select! {
                permit = Arc::clone(&self.scan_slots).acquire_owned() => {
                    match permit {
                        Ok(permit) => break permit,
                        Err(_) => return,
                    }
                }
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if self.shutdown.is_cancelled() {
                        return;
                    }
                    if request.cancellation().is_cancelled() {
                        let _ = self
                            .persist_interruption_with_project_sessions(
                                &context,
                                &project_sessions,
                                &key,
                                OperationTermination::Cancelled,
                            )
                            .await;
                        return;
                    }
                    if request.deadline().is_elapsed_at(now_micros()) {
                        let _ = self
                            .persist_interruption_with_project_sessions(
                                &context,
                                &project_sessions,
                                &key,
                                OperationTermination::TimedOut,
                            )
                            .await;
                        return;
                    }
                }
            }
        };
        if self.shutdown.is_cancelled() {
            drop(permit);
            return;
        }
        if request.cancellation().is_cancelled() {
            drop(permit);
            let _ = self
                .persist_interruption_with_project_sessions(
                    &context,
                    &project_sessions,
                    &key,
                    OperationTermination::Cancelled,
                )
                .await;
            return;
        }
        if request.deadline().is_elapsed_at(now_micros()) {
            drop(permit);
            let _ = self
                .persist_interruption_with_project_sessions(
                    &context,
                    &project_sessions,
                    &key,
                    OperationTermination::TimedOut,
                )
                .await;
            return;
        }
        let running = match self.transition_running(&context, &key).await {
            Ok(running) => running,
            Err(_) => {
                drop(permit);
                return;
            }
        };
        let work = match request.command() {
            SessionSyncCommandV1::ImportTranscripts(_) => {
                context
                    .import_transcripts(
                        self,
                        &key,
                        running.admission.accepted_at,
                        &request,
                        &self.shutdown,
                        Arc::clone(&project_sessions),
                    )
                    .await
            }
            SessionSyncCommandV1::SynchronizeGit(options) => {
                context
                    .synchronize_git(
                        &request,
                        options,
                        &self.shutdown,
                        Arc::clone(&project_sessions),
                    )
                    .await
            }
        };
        drop(permit);
        match work {
            SessionSyncWorkResult::Interrupted(interruption) => {
                if let Some(termination) = interruption.termination() {
                    let _ = self
                        .persist_interruption_with_project_sessions(
                            &context,
                            &project_sessions,
                            &key,
                            termination,
                        )
                        .await;
                }
            }
            SessionSyncWorkResult::Finished {
                interruption,
                committed,
                stats,
                coverage,
                source_frontiers,
                failure_codes,
            } => {
                let interrupted = interruption.is_some();
                let coverage_complete = !coverage.is_empty()
                    && coverage.iter().all(|entry| entry.coverage.is_complete());
                let termination = completion_termination(
                    interruption.and_then(work::SessionSyncInterruption::termination),
                    committed,
                    &stats,
                    coverage_complete,
                    failure_codes.is_empty(),
                );
                if self
                    .persist_terminal(
                        &context,
                        &key,
                        termination,
                        stats,
                        coverage,
                        source_frontiers,
                        failure_codes,
                    )
                    .await
                    .is_ok()
                    && committed
                    && !interrupted
                {
                    context.project_refresh.wake();
                    context.user_refresh.wake();
                }
            }
        }
    }

    async fn transition_running(
        &self,
        context: &SessionSyncProjectContext,
        key: &str,
    ) -> crate::errors::Result<SessionSyncJournalV1> {
        self.update_journal(context, key, |journal| {
            if journal.status != SessionSyncJournalStatusV1::Complete {
                journal.status = SessionSyncJournalStatusV1::Running;
                journal.updated_at = now_micros();
            }
        })
        .await
    }

    async fn persist_interruption_with_project_sessions(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &Arc<RegisteredGlobalDb>,
        key: &str,
        termination: OperationTermination,
    ) -> crate::errors::Result<SessionSyncJournalV1> {
        let journal = self
            .refresh_source_frontiers_with_project_sessions(context, project_sessions, key)
            .await?;
        self.persist_terminal(
            context,
            key,
            termination,
            journal.stats,
            journal.coverage,
            journal.source_frontiers,
            Vec::new(),
        )
        .await
    }

    async fn persist_terminal(
        &self,
        context: &SessionSyncProjectContext,
        key: &str,
        termination: OperationTermination,
        stats: SessionSyncStatsV1,
        coverage: Vec<SessionSyncSourceCoverageV1>,
        source_frontiers: Vec<SessionSyncSourceFrontierV1>,
        failure_codes: Vec<String>,
    ) -> crate::errors::Result<SessionSyncJournalV1> {
        self.update_journal(context, key, |journal| {
            if journal.status == SessionSyncJournalStatusV1::Complete {
                return;
            }
            let completed_at = now_micros();
            journal.status = SessionSyncJournalStatusV1::Complete;
            journal.stats = stats.clone();
            journal.coverage.clone_from(&coverage);
            journal.source_frontiers.clone_from(&source_frontiers);
            journal.completion = Some(SessionSyncCompletionReceiptV1 {
                admission: journal.admission.clone(),
                coalesced_primary: journal.coalesced_primary.clone(),
                completed_at,
                termination,
                stats: stats.clone(),
                coverage: coverage.clone(),
                source_frontiers: source_frontiers.clone(),
                failure_codes: failure_codes.clone(),
            });
            journal.updated_at = completed_at;
        })
        .await
    }

    async fn update_journal(
        &self,
        context: &SessionSyncProjectContext,
        key: &str,
        mut update: impl FnMut(&mut SessionSyncJournalV1),
    ) -> crate::errors::Result<SessionSyncJournalV1> {
        loop {
            let current = context
                .registry
                .read_session_sync_journal(key)
                .await
                .map_err(store_error)?
                .ok_or_else(|| crate::errors::TraceDecayError::Config {
                    message: "session sync journal disappeared".to_owned(),
                })?;
            let mut journal: SessionSyncJournalV1 =
                serde_json::from_str(&current).map_err(journal_decode_error)?;
            update(&mut journal);
            let replacement = serde_json::to_string(&journal).map_err(journal_encode_error)?;
            if replacement == current
                || context
                    .registry
                    .compare_and_swap_session_sync_journal(key, &current, &replacement)
                    .await
                    .map_err(store_error)?
            {
                return Ok(journal);
            }
        }
    }

    async fn persist_progress(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &Arc<RegisteredGlobalDb>,
        key: &str,
        stats: SessionSyncStatsV1,
        coverage: Vec<SessionSyncSourceCoverageV1>,
    ) -> crate::errors::Result<Vec<SessionSyncSourceFrontierV1>> {
        let source_frontiers = context.source_frontiers(project_sessions).await?;
        self.update_journal(context, key, |journal| {
            if journal.status != SessionSyncJournalStatusV1::Complete {
                journal.stats = stats.clone();
                journal.coverage.clone_from(&coverage);
                journal.source_frontiers.clone_from(&source_frontiers);
                journal.updated_at = now_micros();
            }
        })
        .await?;
        Ok(source_frontiers)
    }

    async fn refresh_source_frontiers_with_project_sessions(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &Arc<RegisteredGlobalDb>,
        key: &str,
    ) -> crate::errors::Result<SessionSyncJournalV1> {
        let current = context
            .registry
            .read_session_sync_journal(key)
            .await
            .map_err(store_error)?
            .ok_or_else(|| crate::errors::TraceDecayError::Config {
                message: "session sync journal disappeared".to_owned(),
            })?;
        let journal: SessionSyncJournalV1 =
            serde_json::from_str(&current).map_err(journal_decode_error)?;
        let source_frontiers = context
            .source_frontiers_for(project_sessions, &journal.source)
            .await?;
        self.update_journal(context, key, |journal| {
            if journal.status != SessionSyncJournalStatusV1::Complete
                && journal.source_frontiers != source_frontiers
            {
                journal.source_frontiers.clone_from(&source_frontiers);
                journal.updated_at = now_micros();
            }
        })
        .await
    }

    async fn status_request_admitted(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: Option<&Arc<RegisteredGlobalDb>>,
        control: SessionSyncControlV1,
    ) -> SessionSyncOutcomeV1 {
        let key = journal_key(control.scope(), control.idempotency_key());
        if let Some(project_sessions) = project_sessions
            && let Err(error) = self
                .refresh_source_frontiers_with_project_sessions(context, project_sessions, &key)
                .await
        {
            tracing::warn!(%error, "session sync frontier refresh failed");
        }
        match context.registry.read_session_sync_journal(&key).await {
            Ok(Some(encoded)) => match serde_json::from_str::<SessionSyncJournalV1>(&encoded) {
                Ok(journal)
                    if journal.scope == *control.scope()
                        && journal.admission.idempotency_key == *control.idempotency_key() =>
                {
                    journal.outcome()
                }
                Ok(_) => SessionSyncOutcomeV1::WrongScope,
                Err(error) => {
                    tracing::warn!(%error, "session sync journal decode failed");
                    SessionSyncOutcomeV1::Unavailable {
                        reason_code: "session_sync_journal_invalid",
                    }
                }
            },
            Ok(None) => SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_operation_not_found",
            },
            Err(error) => {
                tracing::warn!(%error, "session sync journal status read failed");
                SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_journal_read_failed",
                }
            }
        }
    }

    async fn status_request(&self, control: SessionSyncControlV1) -> SessionSyncOutcomeV1 {
        let project_gate = self.project_gate(control.scope());
        let _project = project_gate.lock().await;
        let Some(context) = self.context_for(control.scope()) else {
            return SessionSyncOutcomeV1::WrongScope;
        };
        let project_sessions = context.project_sessions().ok();
        self.status_request_admitted(&context, project_sessions.as_ref(), control)
            .await
    }
}

impl SessionSyncServicePort for DaemonSessionSyncService {
    fn execute(&self, request: SessionSyncRequestV1) -> SessionSyncFuture<'_> {
        Box::pin(async move { self.execute_request(request).await })
    }

    fn status(&self, control: SessionSyncControlV1) -> SessionSyncFuture<'_> {
        Box::pin(async move { self.status_request(control).await })
    }

    fn cancel(&self, control: SessionSyncControlV1) -> SessionSyncFuture<'_> {
        Box::pin(async move { self.cancel_request(control).await })
    }

    fn shutdown(&self) -> SessionSyncShutdownFuture<'_> {
        Box::pin(async move {
            self.shutdown.cancel();
            let mut tasks = {
                let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
                std::mem::take(&mut *tasks)
            };
            let joined = tokio::time::timeout(SESSION_SYNC_SHUTDOWN_DEADLINE, async {
                let grace_deadline =
                    tokio::time::Instant::now() + SESSION_SYNC_SHUTDOWN_ABORT_GRACE;
                tokio::select! {
                    results = futures_util::future::join_all(
                        tasks.iter_mut().map(|task| &mut task.task)
                    ) => {
                        for result in results {
                            work::log_session_sync_join(result);
                        }
                        return;
                    }
                    () = tokio::time::sleep_until(grace_deadline) => {}
                }
                for task in &tasks {
                    task.task.abort();
                }
                for result in
                    futures_util::future::join_all(tasks.into_iter().map(|task| task.task)).await
                {
                    work::log_session_sync_join(result);
                }
            })
            .await;
            if joined.is_err() {
                tracing::warn!("session sync shutdown exceeded its total join deadline");
            }
        })
    }
}

mod git_topology;
mod project_lifecycle;
mod work;

use project_lifecycle::{SessionSyncProjectContext, SessionSyncTaskV1};
fn journal_prefix(scope: &SessionSyncScopeV1) -> String {
    let profile_id = scope.profile_id().as_str();
    let project_id = scope.project_id().as_str();
    format!(
        "session-sync.v1.p{}:{profile_id}.r{}:{project_id}.",
        profile_id.len(),
        project_id.len(),
    )
}

fn import_scope_key(scope: &SessionSyncScopeV1) -> String {
    format!(
        "p{}:{}.r{}:{}",
        scope.profile_id().as_str().len(),
        scope.profile_id().as_str(),
        scope.project_id().as_str().len(),
        scope.project_id().as_str(),
    )
}

fn completed_profile_sweep_covers(
    sweep_started_at: Option<&UtcMicros>,
    admitted_at: UtcMicros,
) -> bool {
    sweep_started_at.is_some_and(|sweep_started_at| *sweep_started_at >= admitted_at)
}

fn source_coverage(
    store_scope: &str,
    coverage: tracedecay_sessions::runtime::IngestPassCoverage,
) -> SessionSyncSourceCoverageV1 {
    let coverage = match coverage {
        tracedecay_sessions::runtime::IngestPassCoverage::Complete => {
            SessionSyncCoverageV1::Complete
        }
        tracedecay_sessions::runtime::IngestPassCoverage::Partial { deferred_units } => {
            SessionSyncCoverageV1::Partial { deferred_units }
        }
        tracedecay_sessions::runtime::IngestPassCoverage::Backpressured {
            admitted_units,
            rejected_units,
        } => SessionSyncCoverageV1::Backpressured {
            admitted_units,
            rejected_units,
        },
    };
    SessionSyncSourceCoverageV1 {
        store_scope: store_scope.to_owned(),
        coverage,
    }
}

fn journal_key(scope: &SessionSyncScopeV1, key: &IdempotencyKey) -> String {
    format!("{}{}", journal_prefix(scope), key.as_str())
}

fn decode_matching_journal(
    encoded: &str,
    request: &SessionSyncRequestV1,
) -> Result<SessionSyncJournalV1, &'static str> {
    let journal: SessionSyncJournalV1 =
        serde_json::from_str(encoded).map_err(|_| "session_sync_journal_invalid")?;
    if journal.scope != *request.scope()
        || journal.admission.idempotency_key != *request.idempotency_key()
        || journal.source != request.command()
    {
        return Err("session_sync_idempotency_conflict");
    }
    Ok(journal)
}

fn completion_termination(
    requested: Option<OperationTermination>,
    committed: bool,
    stats: &SessionSyncStatsV1,
    coverage_complete: bool,
    failures_empty: bool,
) -> OperationTermination {
    if let Some(requested) = requested {
        requested
    } else if failures_empty && coverage_complete {
        OperationTermination::Completed
    } else if committed || stats != &SessionSyncStatsV1::default() {
        OperationTermination::Partial
    } else {
        OperationTermination::Failed
    }
}

fn saturating_usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn contract_error(error: impl std::fmt::Display) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: error.to_string(),
    }
}

fn store_error(error: impl std::fmt::Display) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: format!("session sync journal store failed: {error}"),
    }
}

fn journal_decode_error(error: impl std::fmt::Display) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: format!("session sync journal decode failed: {error}"),
    }
}

fn journal_encode_error(error: impl std::fmt::Display) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: format!("session sync journal encode failed: {error}"),
    }
}

#[cfg(test)]
#[path = "session_sync_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_sync/project_lifecycle_tests.rs"]
mod project_lifecycle_tests;
