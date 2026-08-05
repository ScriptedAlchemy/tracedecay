//! One daemon-wide authority for bounded native transcript acquisition and session/Git sync.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use tracedecay_application::session_sync::{
    SessionGitSyncV1, SessionSyncAdmissionErrorV1, SessionSyncCommandV1,
    SessionSyncCompletionReceiptV1, SessionSyncControlV1, SessionSyncFuture,
    SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1, SessionSyncRequestV1,
    SessionSyncScopeV1, SessionSyncServicePort, SessionSyncShutdownFuture, SessionSyncStatsV1,
    SessionTranscriptImportV1,
};
use tracedecay_application::{
    CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId, now_micros,
};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId, UtcMicros};

use crate::global_db::{AnalyticsEventQuery, RegisteredGlobalDb};
use crate::store::{GlobalDbGitCorrelationStore, GlobalDbSessionIngestAuthority};

const MAX_SESSION_SYNC_OPERATIONS: usize = 128;
const SESSION_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SESSION_SYNC_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(2);
const SESSION_SYNC_STARTUP_DEADLINE_MICROS: i64 = 60_000_000;
const GIT_SYNC_ANALYTICS_LIMIT: usize = 500_000;

#[derive(Clone)]
pub(crate) struct DaemonSessionSyncService {
    contexts: Arc<RwLock<BTreeMap<String, Arc<SessionSyncProjectContext>>>>,
    active: Arc<Mutex<BTreeMap<String, CancellationSignal>>>,
    tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    scan_slots: Arc<tokio::sync::Semaphore>,
    shutdown: crate::application::observation::ObservationCancellation,
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
    pub startup_import: bool,
    pub project_refresh:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub user_refresh: crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
}

#[derive(Clone)]
struct SessionSyncProjectContext {
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
    project_refresh: crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    user_refresh: crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
}

enum SessionSyncWorkResult {
    Finished {
        committed: bool,
        stats: SessionSyncStatsV1,
        failure_codes: Vec<String>,
    },
    Cancelled,
    TimedOut,
    Shutdown,
}

impl Default for DaemonSessionSyncService {
    fn default() -> Self {
        Self {
            contexts: Arc::new(RwLock::new(BTreeMap::new())),
            active: Arc::new(Mutex::new(BTreeMap::new())),
            tasks: Arc::new(Mutex::new(Vec::new())),
            scan_slots: Arc::new(tokio::sync::Semaphore::new(1)),
            shutdown: crate::application::observation::ObservationCancellation::default(),
        }
    }
}

impl DaemonSessionSyncService {
    pub(crate) async fn register_project(
        &self,
        config: DaemonSessionSyncConfig,
    ) -> crate::errors::Result<()> {
        let scope = SessionSyncScopeV1::new(config.project_id.clone(), config.profile_id.clone());
        let context = Arc::new(SessionSyncProjectContext {
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
            project_refresh: config.project_refresh,
            user_refresh: config.user_refresh,
        });
        self.contexts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(scope.project_id().as_str().to_owned(), Arc::clone(&context));
        self.recover_project(&context).await?;
        if config.startup_import {
            self.schedule_startup_import(scope).await?;
        }
        Ok(())
    }

    fn context_for(&self, scope: &SessionSyncScopeV1) -> Option<Arc<SessionSyncProjectContext>> {
        self.contexts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(scope.project_id().as_str())
            .filter(|context| &context.profile_id == scope.profile_id())
            .cloned()
    }

    async fn schedule_startup_import(
        &self,
        scope: SessionSyncScopeV1,
    ) -> crate::errors::Result<()> {
        let stable = format!(
            "session-sync.startup.{}.{}",
            crate::runtime_identity::process_run_id(),
            scope.project_id().as_str()
        );
        let operation_id = RequestId::new(stable.clone()).map_err(contract_error)?;
        let idempotency_key = IdempotencyKey::new(stable.clone()).map_err(contract_error)?;
        let cancellation =
            CancellationSignal::active(format!("{stable}.cancellation")).map_err(contract_error)?;
        let deadline = Deadline::new(UtcMicros(
            now_micros()
                .0
                .saturating_add(SESSION_SYNC_STARTUP_DEADLINE_MICROS),
        ))
        .map_err(contract_error)?;
        let request = SessionSyncRequestV1::new(
            operation_id,
            idempotency_key,
            scope,
            deadline,
            cancellation,
            SessionSyncCommandV1::ImportTranscripts(SessionTranscriptImportV1::all_hosts()),
        );
        let _ = self.execute_request(request).await;
        Ok(())
    }

    async fn recover_project(
        &self,
        context: &Arc<SessionSyncProjectContext>,
    ) -> crate::errors::Result<()> {
        let scope = SessionSyncScopeV1::new(context.project_id.clone(), context.profile_id.clone());
        let prefix = journal_prefix(&scope);
        let journals = context
            .registry
            .list_session_sync_journals(&prefix)
            .await
            .map_err(store_error)?;
        for (key, encoded) in journals {
            let journal: SessionSyncJournalV1 =
                serde_json::from_str(&encoded).map_err(journal_decode_error)?;
            if journal.scope != scope || journal.status == SessionSyncJournalStatusV1::Complete {
                continue;
            }
            if journal.cancel_requested_at.is_some() {
                self.persist_terminal(
                    context,
                    &key,
                    OperationTermination::Cancelled,
                    journal.frontier,
                    Vec::new(),
                )
                .await?;
                continue;
            }
            if journal.deadline.is_elapsed_at(now_micros()) {
                self.persist_terminal(
                    context,
                    &key,
                    OperationTermination::TimedOut,
                    journal.frontier,
                    Vec::new(),
                )
                .await?;
                continue;
            }
            let cancellation = CancellationSignal::active(format!(
                "session-sync.recovered.{}",
                journal.admission.operation_id.as_str()
            ))
            .map_err(contract_error)?;
            let request = SessionSyncRequestV1::new(
                journal.admission.operation_id,
                journal.admission.idempotency_key,
                journal.scope,
                journal.deadline,
                cancellation,
                journal.source,
            );
            let _ = self.enqueue(Arc::clone(context), key, request);
        }
        Ok(())
    }

    async fn execute_request(&self, request: SessionSyncRequestV1) -> SessionSyncOutcomeV1 {
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
        let Some(context) = self.context_for(request.scope()) else {
            return SessionSyncOutcomeV1::WrongScope;
        };
        let key = journal_key(request.scope(), request.idempotency_key());
        match context.registry.read_session_sync_journal(&key).await {
            Ok(Some(encoded)) => {
                return match decode_matching_journal(&encoded, &request) {
                    Ok(journal) => {
                        if journal.status != SessionSyncJournalStatusV1::Complete
                            && !self.enqueue(context, key, request)
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
                    .status_request(SessionSyncControlV1::new(
                        request.scope().clone(),
                        request.idempotency_key().clone(),
                    ))
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
        if !self.enqueue(context, key, request) {
            return SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_capacity_reached",
            };
        }
        SessionSyncOutcomeV1::Accepted(admission)
    }

    fn enqueue(
        &self,
        context: Arc<SessionSyncProjectContext>,
        key: String,
        request: SessionSyncRequestV1,
    ) -> bool {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.contains_key(&key) {
            return true;
        }
        if active.len() >= MAX_SESSION_SYNC_OPERATIONS {
            return false;
        }
        active.insert(key.clone(), request.cancellation().clone());
        drop(active);
        let service = self.clone();
        let task = tokio::spawn(async move {
            service.run_operation(context, key.clone(), request).await;
            service
                .active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
        });
        let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
        true
    }

    async fn run_operation(
        &self,
        context: Arc<SessionSyncProjectContext>,
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
                        let _ = self.persist_terminal(
                            &context,
                            &key,
                            OperationTermination::Cancelled,
                            SessionSyncStatsV1::default(),
                            Vec::new(),
                        ).await;
                        return;
                    }
                    if request.deadline().is_elapsed_at(now_micros()) {
                        let _ = self.persist_terminal(
                            &context,
                            &key,
                            OperationTermination::TimedOut,
                            SessionSyncStatsV1::default(),
                            Vec::new(),
                        ).await;
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
                .persist_terminal(
                    &context,
                    &key,
                    OperationTermination::Cancelled,
                    SessionSyncStatsV1::default(),
                    Vec::new(),
                )
                .await;
            return;
        }
        if request.deadline().is_elapsed_at(now_micros()) {
            drop(permit);
            let _ = self
                .persist_terminal(
                    &context,
                    &key,
                    OperationTermination::TimedOut,
                    SessionSyncStatsV1::default(),
                    Vec::new(),
                )
                .await;
            return;
        }
        if self.transition_running(&context, &key).await.is_err() {
            drop(permit);
            return;
        }
        let work = match request.command() {
            SessionSyncCommandV1::ImportTranscripts(_) => {
                context.import_transcripts(&request, &self.shutdown).await
            }
            SessionSyncCommandV1::SynchronizeGit(options) => {
                context
                    .synchronize_git(&request, options, &self.shutdown)
                    .await
            }
        };
        drop(permit);
        match work {
            SessionSyncWorkResult::Shutdown => {}
            SessionSyncWorkResult::Cancelled => {
                let _ = self
                    .persist_terminal(
                        &context,
                        &key,
                        OperationTermination::Cancelled,
                        SessionSyncStatsV1::default(),
                        Vec::new(),
                    )
                    .await;
            }
            SessionSyncWorkResult::TimedOut => {
                let _ = self
                    .persist_terminal(
                        &context,
                        &key,
                        OperationTermination::TimedOut,
                        SessionSyncStatsV1::default(),
                        Vec::new(),
                    )
                    .await;
            }
            SessionSyncWorkResult::Finished {
                committed,
                stats,
                failure_codes,
            } => {
                let termination =
                    completion_termination(committed, &stats, failure_codes.is_empty());
                if self
                    .persist_terminal(&context, &key, termination, stats, failure_codes)
                    .await
                    .is_ok()
                    && committed
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
    ) -> crate::errors::Result<()> {
        self.update_journal(context, key, |journal| {
            if journal.status != SessionSyncJournalStatusV1::Complete {
                journal.status = SessionSyncJournalStatusV1::Running;
                journal.updated_at = now_micros();
            }
        })
        .await
        .map(|_| ())
    }

    async fn persist_terminal(
        &self,
        context: &SessionSyncProjectContext,
        key: &str,
        termination: OperationTermination,
        stats: SessionSyncStatsV1,
        failure_codes: Vec<String>,
    ) -> crate::errors::Result<SessionSyncJournalV1> {
        self.update_journal(context, key, |journal| {
            if journal.status == SessionSyncJournalStatusV1::Complete {
                return;
            }
            let completed_at = now_micros();
            journal.status = SessionSyncJournalStatusV1::Complete;
            journal.frontier = stats.clone();
            journal.completion = Some(SessionSyncCompletionReceiptV1 {
                admission: journal.admission.clone(),
                completed_at,
                termination,
                stats: stats.clone(),
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

    async fn status_request(&self, control: SessionSyncControlV1) -> SessionSyncOutcomeV1 {
        let Some(context) = self.context_for(control.scope()) else {
            return SessionSyncOutcomeV1::WrongScope;
        };
        let key = journal_key(control.scope(), control.idempotency_key());
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

    async fn cancel_request(&self, control: SessionSyncControlV1) -> SessionSyncOutcomeV1 {
        let Some(context) = self.context_for(control.scope()) else {
            return SessionSyncOutcomeV1::WrongScope;
        };
        let key = journal_key(control.scope(), control.idempotency_key());
        let updated = self
            .update_journal(&context, &key, |journal| {
                if journal.scope == *control.scope()
                    && journal.admission.idempotency_key == *control.idempotency_key()
                    && journal.status != SessionSyncJournalStatusV1::Complete
                    && journal.cancel_requested_at.is_none()
                {
                    journal.cancel_requested_at = Some(now_micros());
                    journal.updated_at = now_micros();
                }
            })
            .await;
        let journal = match updated {
            Ok(journal) => journal,
            Err(error) => {
                tracing::warn!(%error, "session sync cancellation journal write failed");
                return SessionSyncOutcomeV1::Unavailable {
                    reason_code: "session_sync_cancel_failed",
                };
            }
        };
        if journal.scope != *control.scope()
            || journal.admission.idempotency_key != *control.idempotency_key()
        {
            return SessionSyncOutcomeV1::WrongScope;
        }
        if journal.status == SessionSyncJournalStatusV1::Complete {
            return journal.outcome();
        }
        if let Some(signal) = self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            signal.cancel(now_micros());
            journal.outcome()
        } else {
            match self
                .persist_terminal(
                    &context,
                    &key,
                    OperationTermination::Cancelled,
                    journal.frontier,
                    Vec::new(),
                )
                .await
            {
                Ok(journal) => journal.outcome(),
                Err(error) => {
                    tracing::warn!(%error, "session sync cancellation completion failed");
                    SessionSyncOutcomeV1::Unavailable {
                        reason_code: "session_sync_cancel_failed",
                    }
                }
            }
        }
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
            if tokio::time::timeout(
                SESSION_SYNC_SHUTDOWN_DEADLINE,
                futures_util::future::join_all(tasks.iter_mut()),
            )
            .await
            .is_err()
            {
                for task in &tasks {
                    task.abort();
                }
                for result in futures_util::future::join_all(tasks).await {
                    if let Err(error) = result
                        && !error.is_cancelled()
                    {
                        tracing::warn!(%error, "session sync worker abort failed");
                    }
                }
            }
        })
    }
}

impl SessionSyncProjectContext {
    async fn import_transcripts(
        &self,
        request: &SessionSyncRequestV1,
        shutdown: &crate::application::observation::ObservationCancellation,
    ) -> SessionSyncWorkResult {
        let cancellation = crate::application::observation::ObservationCancellation::default();
        let pass_cancellation = cancellation.clone();
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
        let mut interrupted = None;
        let (project, user) = loop {
            tokio::select! {
                outcomes = &mut pass => break outcomes,
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if shutdown.is_cancelled() {
                        cancellation.cancel();
                        interrupted = Some(SessionSyncWorkResult::Shutdown);
                    } else if request.cancellation().is_cancelled() {
                        cancellation.cancel();
                        interrupted = Some(SessionSyncWorkResult::Cancelled);
                    } else if request.deadline().is_elapsed_at(now_micros()) {
                        cancellation.cancel();
                        interrupted = Some(SessionSyncWorkResult::TimedOut);
                    }
                }
            }
        };
        let combined = project.stats.merge(user.stats);
        let stats = SessionSyncStatsV1 {
            sessions_imported: combined.sessions_upserted,
            messages_imported: combined.messages_upserted,
            ..SessionSyncStatsV1::default()
        };
        let failure_codes = project
            .failures
            .into_iter()
            .chain(user.failures)
            .map(|failure| failure.reason_code.to_owned())
            .collect::<Vec<_>>();
        let committed = stats != SessionSyncStatsV1::default();
        if committed {
            return SessionSyncWorkResult::Finished {
                committed: true,
                stats,
                failure_codes,
            };
        }
        match interrupted {
            Some(interrupted) => interrupted,
            None => SessionSyncWorkResult::Finished {
                committed: false,
                stats,
                failure_codes,
            },
        }
    }

    async fn synchronize_git(
        &self,
        request: &SessionSyncRequestV1,
        options: SessionGitSyncV1,
        shutdown: &crate::application::observation::ObservationCancellation,
    ) -> SessionSyncWorkResult {
        let Some(analytics) = self.analytics.as_ref() else {
            return SessionSyncWorkResult::Finished {
                committed: false,
                stats: SessionSyncStatsV1::default(),
                failure_codes: vec!["analytics_authority_unavailable".to_owned()],
            };
        };
        let project_key = RegisteredGlobalDb::canonical_project_key(&self.project_root);
        let query = analytics.query_analytics_events(&AnalyticsEventQuery {
            project_id: Some(project_key),
            since: Some(options.since_unix()),
            limit: GIT_SYNC_ANALYTICS_LIMIT,
            ..AnalyticsEventQuery::default()
        });
        tokio::pin!(query);
        let events = loop {
            tokio::select! {
                result = &mut query => {
                    match result {
                        Ok(events) => break events,
                        Err(error) => {
                            tracing::warn!(%error, "session git sync analytics read failed");
                            return SessionSyncWorkResult::Finished {
                                committed: false,
                                stats: SessionSyncStatsV1::default(),
                                failure_codes: vec!["analytics_read_failed".to_owned()],
                            };
                        }
                    }
                }
                () = tokio::time::sleep(SESSION_SYNC_POLL_INTERVAL) => {
                    if shutdown.is_cancelled() {
                        return SessionSyncWorkResult::Shutdown;
                    }
                    if request.cancellation().is_cancelled() {
                        return SessionSyncWorkResult::Cancelled;
                    }
                    if request.deadline().is_elapsed_at(now_micros()) {
                        return SessionSyncWorkResult::TimedOut;
                    }
                }
            }
        };
        if shutdown.is_cancelled() {
            return SessionSyncWorkResult::Shutdown;
        }
        if request.cancellation().is_cancelled() {
            return SessionSyncWorkResult::Cancelled;
        }
        if request.deadline().is_elapsed_at(now_micros()) {
            return SessionSyncWorkResult::TimedOut;
        }
        let result = GlobalDbGitCorrelationStore::new(Arc::clone(&self.project_sessions))
            .run_backfill(
                &events,
                &crate::sessions::git_correlation::SystemGit,
                &crate::sessions::git_correlation::BackfillOptions {
                    since: options.since_unix(),
                    limit_sessions: options.max_sessions(),
                    merge_gap_secs: crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS,
                    max_commits_per_repo: 5_000,
                    dry_run: options.dry_run(),
                },
            )
            .await;
        match result {
            Ok(stats) => SessionSyncWorkResult::Finished {
                committed: !options.dry_run(),
                stats: SessionSyncStatsV1 {
                    sessions_scanned: saturating_usize_to_u64(stats.sessions_scanned),
                    spans_written: saturating_usize_to_u64(stats.spans_written),
                    commits_attributed: saturating_usize_to_u64(stats.commits_attributed),
                    skipped: saturating_usize_to_u64(stats.skipped_total()),
                    ..SessionSyncStatsV1::default()
                },
                failure_codes: Vec::new(),
            },
            Err(error) => {
                tracing::warn!(%error, "session git sync failed");
                SessionSyncWorkResult::Finished {
                    committed: false,
                    stats: SessionSyncStatsV1::default(),
                    failure_codes: vec!["git_sync_failed".to_owned()],
                }
            }
        }
    }
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
    committed: bool,
    stats: &SessionSyncStatsV1,
    failures_empty: bool,
) -> OperationTermination {
    if failures_empty {
        OperationTermination::Completed
    } else if committed || stats != &SessionSyncStatsV1::default() {
        OperationTermination::Partial
    } else {
        OperationTermination::Failed
    }
}

fn saturating_usize_to_u64(value: usize) -> u64 {
    match u64::try_from(value) {
        Ok(value) => value,
        Err(_) => u64::MAX,
    }
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
