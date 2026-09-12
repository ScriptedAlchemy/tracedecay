//! One daemon-wide authority for bounded native transcript acquisition and session/Git sync.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use tracedecay_contracts::session_sync::{
    SessionGitSyncV1, SessionSyncAdmissionErrorV1, SessionSyncCommandV1,
    SessionSyncCompletionReceiptV1, SessionSyncControlV1, SessionSyncCoverageV1, SessionSyncFuture,
    SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncScopeV1, SessionSyncServicePort, SessionSyncShutdownFuture,
    SessionSyncSourceCoverageV1, SessionSyncSourceFrontierV1, SessionSyncStatsV1,
};
use tracedecay_contracts::{
    CancellationSignal, Deadline, IdempotencyKey, OperationTermination, now_micros,
};
use tracedecay_domain::{BrainId, ProjectId, SessionId, UserProfileId, UtcMicros};

use tracedecay_global_db::GlobalDbGitCorrelationStore;
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_sessions::admission::{SESSION_INGEST_DISABLED_REASON_V1, session_ingest_disabled};
use tracedecay_sessions::serving::{
    SessionProjectionServingState, SessionProjectionServingStatusPort,
};

const MAX_SESSION_SYNC_OPERATIONS: usize = 128;
const COALESCED_JOURNAL_RECHECK_INTERVAL: Duration = Duration::from_millis(250);
const SESSION_SYNC_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);
const SESSION_SYNC_SHUTDOWN_ABORT_GRACE: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct DaemonSessionSyncService {
    contexts: Arc<RwLock<BTreeMap<String, Arc<SessionSyncProjectContext>>>>,
    active: Arc<Mutex<BTreeMap<String, CancellationSignal>>>,
    tasks: Arc<Mutex<Vec<SessionSyncTaskV1>>>,
    scan_slots: Arc<tokio::sync::Semaphore>,
    project_gates: Arc<Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    active_imports: Arc<Mutex<BTreeMap<String, ActiveSessionImport>>>,
    shutdown: tracedecay_application::observation::ObservationCancellation,
    shutdown_notify: Arc<tokio::sync::Notify>,
    journal_changed: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct ActiveSessionImport {
    admission: tracedecay_contracts::session_sync::SessionSyncAdmissionReceiptV1,
    journal_key: String,
}

/// Owns the handles while shutdown awaits them. If the daemon-wide deadline
/// cancels that wait, unfinished handles return to the service so a retry can
/// join them instead of detaching lease-owning tasks.
struct SessionSyncTaskShutdownV1 {
    registry: Arc<Mutex<Vec<SessionSyncTaskV1>>>,
    tasks: Vec<SessionSyncTaskV1>,
}

impl SessionSyncTaskShutdownV1 {
    fn take(registry: &Arc<Mutex<Vec<SessionSyncTaskV1>>>) -> Self {
        let tasks = {
            let mut tasks = registry.lock().unwrap_or_else(PoisonError::into_inner);
            std::mem::take(&mut *tasks)
        };
        Self {
            registry: Arc::clone(registry),
            tasks,
        }
    }
}

impl Drop for SessionSyncTaskShutdownV1 {
    fn drop(&mut self) {
        self.tasks.retain(|task| !task.task.is_finished());
        if self.tasks.is_empty() {
            return;
        }
        self.registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(std::mem::take(&mut self.tasks));
    }
}

pub struct DaemonSessionSyncConfig {
    pub brain_id: BrainId,
    pub profile_id: UserProfileId,
    pub project_id: ProjectId,
    pub profile_root: std::path::PathBuf,
    pub project_root: std::path::PathBuf,
    pub transcript_source_home: Option<std::path::PathBuf>,
    pub project_sessions: RegisteredGlobalDbLeaseV1,
    pub user_sessions: RegisteredGlobalDbLeaseV1,
    pub registry: RegisteredGlobalDbLeaseV1,
    /// The process background CPU authority transcript-ingest admissions
    /// prepare captures under; the daemon injects the one its worker plan
    /// installed.
    pub background_cpu: Arc<ProcessBackgroundCpuV1>,
    pub startup_import: bool,
    pub project_refresh: crate::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub user_refresh: crate::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
}

pub enum SessionSyncWorkResult {
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

struct SessionSyncTerminalMaterial {
    termination: OperationTermination,
    stats: SessionSyncStatsV1,
    coverage: Vec<SessionSyncSourceCoverageV1>,
    source_frontiers: Vec<SessionSyncSourceFrontierV1>,
    failure_codes: Vec<String>,
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
            shutdown: tracedecay_application::observation::ObservationCancellation::default(),
            shutdown_notify: Arc::new(tokio::sync::Notify::new()),
            journal_changed: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

impl DaemonSessionSyncService {
    #[hotpath::skip]
    async fn execute_request_admitted(
        &self,
        request: SessionSyncRequestV1,
        context: Arc<SessionSyncProjectContext>,
        project_sessions: RegisteredGlobalDbLeaseV1,
    ) -> SessionSyncOutcomeV1 {
        if session_ingest_disabled() {
            tracing::info!(
                event = "session_sync_ingest_disabled",
                "session sync refused: TRACEDECAY_SESSION_INGEST_DISABLED is set"
            );
            return SessionSyncOutcomeV1::Unavailable {
                reason_code: SESSION_INGEST_DISABLED_REASON_V1,
            };
        }
        let observed_at = now_micros();
        let key = journal_key(request.scope(), request.idempotency_key());
        match hotpath::future!(
            context.registry.read_session_sync_journal(&key),
            label = "daemon.session_sync.journal.admission_read"
        )
        .await
        {
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
                                    project_sessions.clone(),
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
                                project_sessions.clone(),
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
            match hotpath::future!(
                context.registry.insert_session_sync_journal(&key, &encoded),
                label = "daemon.session_sync.journal.coalesced_admission_write"
            )
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
        match hotpath::future!(
            context.registry.insert_session_sync_journal(&key, &encoded),
            label = "daemon.session_sync.journal.admission_write"
        )
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

    pub fn active_contains(&self, key: &str) -> bool {
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
        project_sessions: RegisteredGlobalDbLeaseV1,
        key: String,
        request: SessionSyncRequestV1,
        admission: tracedecay_contracts::session_sync::SessionSyncAdmissionReceiptV1,
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

    #[hotpath::skip]
    async fn run_operation(
        &self,
        context: Arc<SessionSyncProjectContext>,
        project_sessions: RegisteredGlobalDbLeaseV1,
        key: String,
        request: SessionSyncRequestV1,
    ) {
        let acquire = Arc::clone(&self.scan_slots).acquire_owned();
        tokio::pin!(acquire);
        let permit = tokio::select! {
            permit = &mut acquire => {
                match permit {
                    Ok(permit) => permit,
                    Err(_) => return,
                }
            }
            interruption = self.wait_for_interruption(&request) => {
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
                return;
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
        let _running = match self.transition_running(&context, &key).await {
            Ok(running) => running,
            Err(_) => {
                drop(permit);
                return;
            }
        };
        let work = match request.command() {
            SessionSyncCommandV1::ImportTranscripts(_) => {
                hotpath::future!(
                    context.import_transcripts(self, &key, &request, project_sessions.clone(),),
                    label = "daemon.session_sync.import_transcripts"
                )
                .await
            }
            SessionSyncCommandV1::SynchronizeGit(options) => {
                hotpath::future!(
                    context.synchronize_git(self, &request, options, project_sessions.clone()),
                    label = "daemon.session_sync.synchronize_git"
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
                mut interruption,
                committed,
                stats,
                coverage,
                source_frontiers,
                mut failure_codes,
            } => {
                let mut interrupted = interruption.is_some();
                let is_import = matches!(
                    request.command(),
                    SessionSyncCommandV1::ImportTranscripts(_)
                );
                let mut projection_current = false;
                let coverage_complete = !coverage.is_empty()
                    && coverage.iter().all(|entry| entry.coverage.is_complete());
                if !projection_current
                    && !interrupted
                    && coverage_complete
                    && failure_codes.is_empty()
                    && is_import
                {
                    match self
                        .await_import_projection(&context, &project_sessions, &request)
                        .await
                    {
                        Ok(()) => projection_current = true,
                        Err(Some(reason)) => {
                            interruption = Some(reason);
                            interrupted = true;
                        }
                        Err(None) => {
                            failure_codes
                                .push("session_temporal_projection_not_current".to_owned());
                        }
                    }
                }
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
                        SessionSyncTerminalMaterial {
                            termination,
                            stats,
                            coverage,
                            source_frontiers,
                            failure_codes,
                        },
                    )
                    .await
                    .is_ok()
                    && committed
                    && !interrupted
                    && !projection_current
                {
                    context.project_refresh.wake();
                    context.user_refresh.wake();
                }
            }
        }
    }

    async fn await_import_history(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &RegisteredGlobalDbLeaseV1,
        request: &SessionSyncRequestV1,
    ) -> Result<
        crate::session_temporal_refresh_scheduler::history::SessionHistoricalIngestProgress,
        Option<work::SessionSyncInterruption>,
    > {
        let remaining_micros = request
            .deadline()
            .expires_at
            .0
            .saturating_sub(now_micros().0);
        let Ok(remaining_micros) = u64::try_from(remaining_micros) else {
            return Err(Some(work::SessionSyncInterruption::TimedOut));
        };
        if remaining_micros == 0 {
            return Err(Some(work::SessionSyncInterruption::TimedOut));
        }
        let timeout = Duration::from_micros(remaining_micros);
        let history = async {
            tokio::join!(
                context
                    .project_refresh
                    .wake_history_and_wait_until_idle(timeout),
                context
                    .user_refresh
                    .wake_history_and_wait_until_idle(timeout),
            )
        };
        tokio::pin!(history);
        let settled = tokio::select! {
            settled = &mut history => settled,
            interruption = self.wait_for_interruption(request) => {
                return Err(Some(interruption));
            }
        };
        if let (Some(project), Some(user)) = settled
            && matches!(
                context.project_refresh.serving_status().state,
                SessionProjectionServingState::Current
            )
            && matches!(
                context.user_refresh.serving_status().state,
                SessionProjectionServingState::Current
            )
            && self
                .projection_store_is_current(project_sessions, request)
                .await?
            && self
                .projection_store_is_current(&context.user_sessions, request)
                .await?
        {
            Ok(
                crate::session_temporal_refresh_scheduler::history::SessionHistoricalIngestProgress {
                    stats: project.stats.merge(user.stats),
                    committed: project.committed || user.committed,
                },
            )
        } else {
            Err(None)
        }
    }

    async fn await_import_projection(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &RegisteredGlobalDbLeaseV1,
        request: &SessionSyncRequestV1,
    ) -> Result<(), Option<work::SessionSyncInterruption>> {
        let remaining_micros = request
            .deadline()
            .expires_at
            .0
            .saturating_sub(now_micros().0);
        let Ok(remaining_micros) = u64::try_from(remaining_micros) else {
            return Err(Some(work::SessionSyncInterruption::TimedOut));
        };
        if remaining_micros == 0 {
            return Err(Some(work::SessionSyncInterruption::TimedOut));
        }
        let timeout = Duration::from_micros(remaining_micros);
        let projection = async {
            tokio::join!(
                context.project_refresh.wake_and_wait_until_idle(timeout),
                context.user_refresh.wake_and_wait_until_idle(timeout),
            )
        };
        tokio::pin!(projection);
        let settled = tokio::select! {
            settled = &mut projection => settled,
            interruption = self.wait_for_interruption(request) => {
                return Err(Some(interruption));
            }
        };
        let project = context.project_refresh.status();
        let user = context.user_refresh.status();
        if settled.0
            && settled.1
            && project.backlog == 0
            && user.backlog == 0
            && project.unavailable_reason.is_none()
            && user.unavailable_reason.is_none()
            && matches!(
                context.project_refresh.serving_status().state,
                SessionProjectionServingState::Current
            )
            && matches!(
                context.user_refresh.serving_status().state,
                SessionProjectionServingState::Current
            )
            && self
                .projection_store_is_current(project_sessions, request)
                .await?
            && self
                .projection_store_is_current(&context.user_sessions, request)
                .await?
        {
            Ok(())
        } else {
            Err(None)
        }
    }

    async fn projection_store_is_current(
        &self,
        database: &RegisteredGlobalDbLeaseV1,
        request: &SessionSyncRequestV1,
    ) -> Result<bool, Option<work::SessionSyncInterruption>> {
        let page_limit =
            crate::session_temporal_refresh_scheduler::projector::SessionTemporalRefreshPolicy::default()
                .max_begin_requests_per_pass;
        let active_scan_slots = page_limit / 2;

        let mut active_after: Option<SessionId> = None;
        loop {
            let page = {
                let discovery = database.pending_session_temporal_refresh_page_result(
                    page_limit,
                    active_scan_slots,
                    active_after.as_ref(),
                );
                tokio::pin!(discovery);
                tokio::select! {
                    page = &mut discovery => page.map_err(|_| None)?,
                    interruption = self.wait_for_interruption(request) => {
                        return Err(Some(interruption));
                    }
                }
            };
            let (pending, next_active, has_more) = page.into_parts();
            if !pending.is_empty() {
                return Ok(false);
            }
            if !has_more {
                return Ok(true);
            }
            active_after = next_active;
        }
    }

    #[hotpath::skip]
    async fn transition_running(
        &self,
        context: &SessionSyncProjectContext,
        key: &str,
    ) -> tracedecay_domain::errors::Result<SessionSyncJournalV1> {
        self.update_journal(context, key, |journal| {
            if journal.status != SessionSyncJournalStatusV1::Complete {
                journal.status = SessionSyncJournalStatusV1::Running;
                journal.updated_at = now_micros();
            }
        })
        .await
    }

    #[hotpath::skip]
    async fn persist_interruption_with_project_sessions(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &RegisteredGlobalDbLeaseV1,
        key: &str,
        termination: OperationTermination,
    ) -> tracedecay_domain::errors::Result<SessionSyncJournalV1> {
        let journal = self
            .refresh_source_frontiers_with_project_sessions(context, project_sessions, key)
            .await?;
        self.persist_terminal(
            context,
            key,
            SessionSyncTerminalMaterial {
                termination,
                stats: journal.stats,
                coverage: journal.coverage,
                source_frontiers: journal.source_frontiers,
                failure_codes: Vec::new(),
            },
        )
        .await
    }

    #[hotpath::skip]
    async fn persist_terminal(
        &self,
        context: &SessionSyncProjectContext,
        key: &str,
        material: SessionSyncTerminalMaterial,
    ) -> tracedecay_domain::errors::Result<SessionSyncJournalV1> {
        let SessionSyncTerminalMaterial {
            termination,
            stats,
            mut coverage,
            source_frontiers,
            failure_codes,
        } = material;
        self.update_journal(context, key, |journal| {
            if journal.status == SessionSyncJournalStatusV1::Complete {
                return;
            }
            if coverage.is_empty()
                && termination != OperationTermination::Completed
                && matches!(journal.source, SessionSyncCommandV1::ImportTranscripts(_))
            {
                coverage = transcript_import_requested_coverage();
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

    #[hotpath::skip]
    async fn update_journal(
        &self,
        context: &SessionSyncProjectContext,
        key: &str,
        mut update: impl FnMut(&mut SessionSyncJournalV1),
    ) -> tracedecay_domain::errors::Result<SessionSyncJournalV1> {
        loop {
            let current = hotpath::future!(
                context.registry.read_session_sync_journal(key),
                label = "daemon.session_sync.journal.update_read"
            )
            .await
            .map_err(store_error)?
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
                message: "session sync journal disappeared".to_owned(),
            })?;
            let mut journal: SessionSyncJournalV1 =
                serde_json::from_str(&current).map_err(journal_decode_error)?;
            update(&mut journal);
            let replacement = serde_json::to_string(&journal).map_err(journal_encode_error)?;
            if replacement == current {
                return Ok(journal);
            }
            if hotpath::future!(
                context
                    .registry
                    .compare_and_swap_session_sync_journal(key, &current, &replacement),
                label = "daemon.session_sync.journal.update_write"
            )
            .await
            .map_err(store_error)?
            {
                self.journal_changed.notify_waiters();
                return Ok(journal);
            }
        }
    }

    #[hotpath::skip]
    async fn persist_progress(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &RegisteredGlobalDbLeaseV1,
        key: &str,
        stats: SessionSyncStatsV1,
        coverage: Vec<SessionSyncSourceCoverageV1>,
    ) -> tracedecay_domain::errors::Result<Vec<SessionSyncSourceFrontierV1>> {
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

    #[hotpath::skip]
    async fn refresh_source_frontiers_with_project_sessions(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: &RegisteredGlobalDbLeaseV1,
        key: &str,
    ) -> tracedecay_domain::errors::Result<SessionSyncJournalV1> {
        let current = context
            .registry
            .read_session_sync_journal(key)
            .await
            .map_err(store_error)?
            .ok_or_else(|| tracedecay_domain::errors::TraceDecayError::Config {
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

    #[hotpath::skip]
    async fn status_request_admitted(
        &self,
        context: &SessionSyncProjectContext,
        project_sessions: Option<&RegisteredGlobalDbLeaseV1>,
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

    #[hotpath::skip]
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
        Box::pin(async move {
            hotpath::future!(
                self.execute_request(request),
                label = "daemon.session_sync.execute"
            )
            .await
        })
    }

    fn status(&self, control: SessionSyncControlV1) -> SessionSyncFuture<'_> {
        Box::pin(async move {
            hotpath::future!(
                self.status_request(control),
                label = "daemon.session_sync.status"
            )
            .await
        })
    }

    fn cancel(&self, control: SessionSyncControlV1) -> SessionSyncFuture<'_> {
        Box::pin(async move {
            hotpath::future!(
                self.cancel_request(control),
                label = "daemon.session_sync.cancel"
            )
            .await
        })
    }

    fn shutdown(&self) -> SessionSyncShutdownFuture<'_> {
        Box::pin(async move {
            self.shutdown.cancel();
            self.shutdown_notify.notify_waiters();
            let mut tasks = SessionSyncTaskShutdownV1::take(&self.tasks);
            let grace_deadline = tokio::time::Instant::now() + SESSION_SYNC_SHUTDOWN_ABORT_GRACE;
            tokio::select! {
                results = futures_util::future::join_all(
                    tasks.tasks.iter_mut().map(|task| &mut task.task)
                ) => {
                    for result in results {
                        work::log_session_sync_join(result);
                    }
                    self.contexts
                        .write()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clear();
                    return;
                }
                () = tokio::time::sleep_until(grace_deadline) => {}
            }
            for task in &tasks.tasks {
                task.task.abort();
            }
            for result in
                futures_util::future::join_all(tasks.tasks.iter_mut().map(|task| &mut task.task))
                    .await
            {
                work::log_session_sync_join(result);
            }
            self.contexts
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        })
    }
}

pub mod git_topology;
mod project_lifecycle;
pub mod work;

pub use project_lifecycle::{SessionSyncProjectContext, SessionSyncTaskV1};

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_harness {
    use std::sync::{Arc, PoisonError};
    use std::time::Duration;

    use super::{DaemonSessionSyncService, SessionSyncTaskV1};
    use crate::session_temporal_refresh_scheduler::projector::{
        SessionTemporalRefreshPolicy, SessionTemporalRefreshProjector,
    };
    use crate::session_temporal_refresh_scheduler::registry::{
        SessionTemporalRefreshSchedulerRegistry, session_refresh_retry_delay as retry_delay,
    };
    use crate::session_temporal_refresh_scheduler::wake::{
        RecoverySelectionGuard, SessionTemporalRefreshRetryClass,
    };
    use tokio::sync::Semaphore;
    use tracedecay_contracts::session_sync::{
        SessionSyncJournalV1, SessionSyncRequestV1, SessionSyncScopeV1, SessionSyncStatsV1,
    };
    use tracedecay_contracts::{
        CancellationSignal, Deadline, IdempotencyKey, OperationTermination,
    };

    pub use crate::session_temporal_refresh_scheduler::registry::SessionTemporalRefreshPassReport;
    pub use crate::session_temporal_refresh_scheduler::wake::SessionTemporalRefreshWakeState;
    pub use crate::session_temporal_refresh_scheduler::{
        apply_refresh_effect, begin_admitted_session_refreshes, process_refresh_begin_requests,
        run_session_temporal_refresh_pass,
    };

    #[hotpath::skip]
    pub async fn wait_for_interruption(
        service: &DaemonSessionSyncService,
        cancellation: &CancellationSignal,
        deadline: &Deadline,
    ) -> super::work::SessionSyncInterruption {
        service
            .wait_for_interruption_parts(cancellation, deadline)
            .await
    }

    pub fn context_count(service: &DaemonSessionSyncService) -> usize {
        service
            .contexts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn task_count(service: &DaemonSessionSyncService) -> usize {
        service
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn push_task(service: &DaemonSessionSyncService, task: SessionSyncTaskV1) {
        service
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(task);
    }

    pub fn extend_tasks(
        service: &DaemonSessionSyncService,
        tasks: impl IntoIterator<Item = SessionSyncTaskV1>,
    ) {
        service
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(tasks);
    }

    pub fn scan_slots(service: &DaemonSessionSyncService) -> Arc<Semaphore> {
        Arc::clone(&service.scan_slots)
    }

    pub fn journal_prefix(scope: &SessionSyncScopeV1) -> String {
        super::journal_prefix(scope)
    }

    pub fn journal_key(scope: &SessionSyncScopeV1, key: &IdempotencyKey) -> String {
        super::journal_key(scope, key)
    }

    pub fn decode_matching_journal(
        encoded: &str,
        request: &SessionSyncRequestV1,
    ) -> Result<SessionSyncJournalV1, &'static str> {
        super::decode_matching_journal(encoded, request)
    }

    pub fn completion_termination(
        requested: Option<OperationTermination>,
        committed: bool,
        stats: &SessionSyncStatsV1,
        coverage_complete: bool,
        failures_empty: bool,
    ) -> OperationTermination {
        super::completion_termination(
            requested,
            committed,
            stats,
            coverage_complete,
            failures_empty,
        )
    }

    pub fn configure_scheduler(
        registry: &mut SessionTemporalRefreshSchedulerRegistry,
        projector: Arc<dyn SessionTemporalRefreshProjector>,
        policy: SessionTemporalRefreshPolicy,
    ) {
        registry.configure_for_test(projector, policy);
    }

    pub fn session_refresh_retry_delay(
        class: SessionTemporalRefreshRetryClass,
        attempt: u32,
    ) -> Duration {
        retry_delay(class, attempt)
    }

    pub fn complete_recovery_selection(
        state: &SessionTemporalRefreshWakeState,
        pending: Vec<String>,
        completed: &[&str],
    ) -> Vec<String> {
        let mut selection = RecoverySelectionGuard::new(state, pending);
        for operation in completed {
            selection.complete(operation);
        }
        drop(selection);
        state.pending_recovery_operations()
    }
}

impl DaemonSessionSyncService {
    fn observed_interruption(
        &self,
        cancellation: &CancellationSignal,
        deadline: &Deadline,
    ) -> Option<work::SessionSyncInterruption> {
        if self.shutdown.is_cancelled() {
            Some(work::SessionSyncInterruption::Shutdown)
        } else if cancellation.is_cancelled() {
            Some(work::SessionSyncInterruption::Cancelled)
        } else if deadline.is_elapsed_at(now_micros()) {
            Some(work::SessionSyncInterruption::TimedOut)
        } else {
            None
        }
    }

    #[hotpath::skip]
    async fn wait_for_interruption(
        &self,
        request: &SessionSyncRequestV1,
    ) -> work::SessionSyncInterruption {
        self.wait_for_interruption_parts(request.cancellation(), request.deadline())
            .await
    }

    #[hotpath::skip]
    async fn wait_for_interruption_parts(
        &self,
        cancellation: &CancellationSignal,
        deadline: &Deadline,
    ) -> work::SessionSyncInterruption {
        let shutdown = self.shutdown_notify.notified();
        tokio::pin!(shutdown);
        let _ = shutdown.as_mut().enable();
        if let Some(interruption) = self.observed_interruption(cancellation, deadline) {
            return interruption;
        }
        tokio::select! {
            biased;
            () = &mut shutdown => work::SessionSyncInterruption::Shutdown,
            () = cancellation.cancelled() => work::SessionSyncInterruption::Cancelled,
            () = sleep_until_deadline(deadline) => work::SessionSyncInterruption::TimedOut,
        }
    }
}

fn transcript_import_requested_coverage() -> Vec<SessionSyncSourceCoverageV1> {
    // One transcript import requests the complete provider set for each
    // registered store scope. Before discovery starts, each aggregate scope
    // is therefore one wholly deferred work unit.
    ["project", "profile"]
        .into_iter()
        .map(|store_scope| SessionSyncSourceCoverageV1 {
            store_scope: store_scope.to_owned(),
            coverage: SessionSyncCoverageV1::Partial { deferred_units: 1 },
        })
        .collect()
}

fn sleep_until_deadline(deadline: &tracedecay_contracts::Deadline) -> impl Future<Output = ()> {
    let remaining_micros = deadline.expires_at.0.saturating_sub(now_micros().0);
    let remaining = u64::try_from(remaining_micros).unwrap_or(0);
    tokio::time::sleep(Duration::from_micros(remaining))
}

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

fn contract_error(error: impl std::fmt::Display) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: error.to_string(),
    }
}

fn store_error(error: impl std::fmt::Display) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("session sync journal store failed: {error}"),
    }
}

fn journal_decode_error(
    error: impl std::fmt::Display,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("session sync journal decode failed: {error}"),
    }
}

fn journal_encode_error(
    error: impl std::fmt::Display,
) -> tracedecay_domain::errors::TraceDecayError {
    tracedecay_domain::errors::TraceDecayError::Config {
        message: format!("session sync journal encode failed: {error}"),
    }
}
