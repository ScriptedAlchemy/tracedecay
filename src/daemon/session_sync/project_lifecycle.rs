use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock, Weak};

use tracedecay_application::session_sync::{
    SessionSyncCommandV1, SessionSyncJournalStatusV1, SessionSyncJournalV1, SessionSyncOutcomeV1,
    SessionSyncRequestV1, SessionSyncScopeV1, SessionTranscriptImportV1,
};
use tracedecay_application::{
    CancellationSignal, Deadline, IdempotencyKey, OperationTermination, RequestId, now_micros,
};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId, UtcMicros};
use tracedecay_store::{StoreShardScopeV1, VerifiedStoreLocatorV1};

use crate::global_db::RegisteredGlobalDb;

use super::{
    ActiveSessionImport, DaemonSessionSyncConfig, DaemonSessionSyncService,
    SESSION_SYNC_SHUTDOWN_DEADLINE, contract_error, import_scope_key, journal_decode_error,
    journal_key, journal_prefix, store_error, work,
};

const SESSION_SYNC_STARTUP_DEADLINE_MICROS: i64 = 60_000_000;

pub(super) struct SessionSyncProjectContext {
    pub(super) brain_id: BrainId,
    pub(super) profile_id: UserProfileId,
    pub(super) project_id: ProjectId,
    pub(super) profile_root: PathBuf,
    pub(super) project_root: PathBuf,
    pub(super) transcript_source_home: Option<PathBuf>,
    pub(super) project_sessions: RwLock<Weak<RegisteredGlobalDb>>,
    project_sessions_locator: VerifiedStoreLocatorV1,
    pub(super) user_sessions: Arc<RegisteredGlobalDb>,
    pub(super) registry: Arc<RegisteredGlobalDb>,
    pub(super) project_refresh:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
    pub(super) user_refresh:
        crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake,
}

pub(super) struct SessionSyncTaskV1 {
    pub(super) scope: SessionSyncScopeV1,
    pub(super) key: String,
    pub(super) cancellation: CancellationSignal,
    pub(super) task: tokio::task::JoinHandle<()>,
}

impl SessionSyncProjectContext {
    pub(super) fn project_sessions(&self) -> Result<Arc<RegisteredGlobalDb>, String> {
        self.project_sessions
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade()
            .ok_or_else(|| {
                format!(
                    "session sync project '{}' is retired",
                    self.project_id.as_str()
                )
            })
    }

    fn retire_project_sessions(&self) -> Option<Arc<RegisteredGlobalDb>> {
        let mut project_sessions = self
            .project_sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let database = project_sessions.upgrade();
        *project_sessions = Weak::new();
        database
    }

    pub(super) fn rebind_project_sessions(
        &self,
        database: &Arc<RegisteredGlobalDb>,
    ) -> Result<(), String> {
        let shard = &database.binding().shard_id;
        let locator = database.runtime().locator().verified();
        if shard.brain_id != self.brain_id
            || shard.profile_id != self.profile_id
            || locator != &self.project_sessions_locator
            || !matches!(
                &shard.scope,
                StoreShardScopeV1::ProjectSessions { project_id }
                    if project_id == &self.project_id
            )
        {
            return Err(format!(
                "session sync project '{}' cannot bind a foreign session shard",
                self.project_id.as_str()
            ));
        }
        *self
            .project_sessions
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(database);
        Ok(())
    }
}

impl DaemonSessionSyncService {
    pub(super) fn project_gate(&self, scope: &SessionSyncScopeV1) -> Arc<tokio::sync::Mutex<()>> {
        let key = session_sync_project_key(scope);
        Arc::clone(
            self.project_gates
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    async fn recover_project(
        &self,
        context: &Arc<SessionSyncProjectContext>,
        project_sessions: &Arc<RegisteredGlobalDb>,
    ) -> crate::errors::Result<bool> {
        let scope = SessionSyncScopeV1::new(context.project_id.clone(), context.profile_id.clone());
        let prefix = journal_prefix(&scope);
        let journals = context
            .registry
            .list_session_sync_journals(&prefix)
            .await
            .map_err(store_error)?;
        let mut recovered_import = false;
        for (key, encoded) in journals {
            let mut journal: SessionSyncJournalV1 =
                serde_json::from_str(&encoded).map_err(journal_decode_error)?;
            if journal.scope != scope || journal.status == SessionSyncJournalStatusV1::Complete {
                continue;
            }
            journal = self
                .refresh_source_frontiers_with_project_sessions(context, project_sessions, &key)
                .await?;
            if let Some(primary) = journal.coalesced_primary.clone() {
                let primary_key = journal_key(&scope, &primary);
                if self
                    .mirror_primary_terminal(context, &key, &primary_key)
                    .await?
                    .is_some()
                {
                    continue;
                }
                if journal.cancel_requested_at.is_some() {
                    self.persist_terminal(
                        context,
                        &key,
                        OperationTermination::Cancelled,
                        journal.stats,
                        journal.coverage,
                        journal.source_frontiers,
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
                        journal.stats,
                        journal.coverage,
                        journal.source_frontiers,
                        Vec::new(),
                    )
                    .await?;
                    continue;
                }
                let cancellation = CancellationSignal::active(format!(
                    "session-sync.recovered-alias.{}",
                    journal.admission.operation_id.as_str()
                ))
                .map_err(contract_error)?;
                recovered_import = true;
                self.coalesce_import(
                    Arc::clone(context),
                    Arc::clone(project_sessions),
                    key,
                    journal,
                    primary_key,
                    cancellation,
                );
                continue;
            }
            if journal.cancel_requested_at.is_some() {
                self.persist_terminal(
                    context,
                    &key,
                    OperationTermination::Cancelled,
                    journal.stats,
                    journal.coverage,
                    journal.source_frontiers,
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
                    journal.stats,
                    journal.coverage,
                    journal.source_frontiers,
                    Vec::new(),
                )
                .await?;
                continue;
            }
            let active_import =
                matches!(journal.source, SessionSyncCommandV1::ImportTranscripts(_))
                    .then(|| {
                        self.active_imports
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .get(&import_scope_key(&journal.scope))
                            .cloned()
                    })
                    .flatten();
            if let Some(active_import) = active_import {
                recovered_import = true;
                journal = self
                    .update_journal(context, &key, |journal| {
                        journal.coalesced_primary =
                            Some(active_import.admission.idempotency_key.clone());
                        journal.updated_at = now_micros();
                    })
                    .await?;
                let cancellation = CancellationSignal::active(format!(
                    "session-sync.recovered-coalesced.{}",
                    journal.admission.operation_id.as_str()
                ))
                .map_err(contract_error)?;
                self.coalesce_import(
                    Arc::clone(context),
                    Arc::clone(project_sessions),
                    key,
                    journal,
                    active_import.journal_key,
                    cancellation,
                );
                continue;
            }
            let cancellation = CancellationSignal::active(format!(
                "session-sync.recovered.{}",
                journal.admission.operation_id.as_str()
            ))
            .map_err(contract_error)?;
            let admission = journal.admission.clone();
            let request = SessionSyncRequestV1::new(
                journal.admission.operation_id,
                journal.admission.idempotency_key,
                journal.scope,
                journal.deadline,
                cancellation,
                journal.source,
            );
            recovered_import |= matches!(
                request.command(),
                SessionSyncCommandV1::ImportTranscripts(_)
            );
            let _ = self.enqueue(
                Arc::clone(context),
                Arc::clone(project_sessions),
                key,
                request,
                admission,
            );
        }
        Ok(recovered_import)
    }

    pub(crate) async fn register_project(
        &self,
        config: DaemonSessionSyncConfig,
    ) -> crate::errors::Result<()> {
        let scope = SessionSyncScopeV1::new(config.project_id.clone(), config.profile_id.clone());
        let project_gate = self.project_gate(&scope);
        let project = project_gate.lock().await;
        let project_sessions = Arc::clone(&config.project_sessions);
        let project_sessions_locator = project_sessions.runtime().locator().verified().clone();
        let context = Arc::new(SessionSyncProjectContext {
            brain_id: config.brain_id,
            profile_id: config.profile_id,
            project_id: config.project_id,
            profile_root: config.profile_root,
            project_root: config.project_root,
            transcript_source_home: config.transcript_source_home,
            project_sessions: RwLock::new(Weak::new()),
            project_sessions_locator,
            user_sessions: config.user_sessions,
            registry: config.registry,
            project_refresh: config.project_refresh,
            user_refresh: config.user_refresh,
        });
        context
            .rebind_project_sessions(&config.project_sessions)
            .map_err(contract_error)?;
        let previous = if let Some(previous) = self.context_for(&scope) {
            let previous_database = previous.retire_project_sessions();
            if let Err(error) = self.drain_project_tasks(&scope).await {
                if let Some(previous_database) = previous_database.as_ref() {
                    previous
                        .rebind_project_sessions(previous_database)
                        .map_err(contract_error)?;
                }
                return Err(contract_error(error));
            }
            self.remove_context_if_same(&scope, &previous);
            Some((previous, previous_database))
        } else {
            None
        };
        self.contexts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session_sync_project_key(&scope), Arc::clone(&context));
        let recovered_import = match self.recover_project(&context, &project_sessions).await {
            Ok(recovered_import) => recovered_import,
            Err(error) => {
                context.retire_project_sessions();
                let drain = self.drain_project_tasks(&scope).await;
                return match drain {
                    Ok(()) => {
                        self.remove_context_if_same(&scope, &context);
                        match self.restore_previous_context(&scope, previous).await {
                            Ok(()) => Err(error),
                            Err(restore_error) => Err(contract_error(format!(
                                "session sync recovery failed: {error}; prior context restore failed: {restore_error}"
                            ))),
                        }
                    }
                    Err(drain_error) => Err(contract_error(format!(
                        "session sync recovery failed: {error}; rollback failed: {drain_error}"
                    ))),
                };
            }
        };
        if config.startup_import
            && !recovered_import
            && let Err(error) = self
                .schedule_startup_import(&context, &project_sessions, scope.clone())
                .await
        {
            context.retire_project_sessions();
            let drain = self.drain_project_tasks(&scope).await;
            return match drain {
                Ok(()) => {
                    self.remove_context_if_same(&scope, &context);
                    match self.restore_previous_context(&scope, previous).await {
                        Ok(()) => Err(error),
                        Err(restore_error) => Err(contract_error(format!(
                            "session sync startup admission failed: {error}; prior context restore failed: {restore_error}"
                        ))),
                    }
                }
                Err(drain_error) => Err(contract_error(format!(
                    "session sync startup admission failed: {error}; rollback failed: {drain_error}"
                ))),
            };
        }
        drop(project);
        Ok(())
    }

    async fn schedule_startup_import(
        &self,
        context: &Arc<SessionSyncProjectContext>,
        project_sessions: &Arc<RegisteredGlobalDb>,
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
        match self
            .execute_request_admitted(request, Arc::clone(context), Arc::clone(project_sessions))
            .await
        {
            SessionSyncOutcomeV1::Accepted(_)
            | SessionSyncOutcomeV1::Joined(_)
            | SessionSyncOutcomeV1::Complete(_) => Ok(()),
            outcome => Err(contract_error(format!(
                "session sync startup import was not admitted: {outcome:?}"
            ))),
        }
    }

    pub(super) fn context_for(
        &self,
        scope: &SessionSyncScopeV1,
    ) -> Option<Arc<SessionSyncProjectContext>> {
        self.contexts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session_sync_project_key(scope))
            .cloned()
    }

    pub(super) async fn execute_request(
        &self,
        request: SessionSyncRequestV1,
    ) -> SessionSyncOutcomeV1 {
        let project_gate = self.project_gate(request.scope());
        let _project = project_gate.lock().await;
        let Some(context) = self.context_for(request.scope()) else {
            return SessionSyncOutcomeV1::WrongScope;
        };
        let Ok(project_sessions) = context.project_sessions() else {
            return SessionSyncOutcomeV1::Unavailable {
                reason_code: "session_sync_project_retired",
            };
        };
        self.execute_request_admitted(request, Arc::clone(&context), project_sessions)
            .await
    }

    pub(crate) async fn retire_project(
        &self,
        profile_id: &UserProfileId,
        project_id: &ProjectId,
    ) -> Result<bool, String> {
        let scope = SessionSyncScopeV1::new(project_id.clone(), profile_id.clone());
        let project_gate = self.project_gate(&scope);
        let _project = project_gate.lock().await;
        let context = self
            .contexts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session_sync_project_key(&scope))
            .cloned();
        let Some(context) = context else {
            return Ok(false);
        };
        let previous_database = context.retire_project_sessions();
        if let Err(error) = self.drain_project_tasks(&scope).await {
            if let Some(previous_database) = previous_database {
                context.rebind_project_sessions(&previous_database)?;
            }
            return Err(error);
        }
        Ok(true)
    }

    pub(crate) async fn rebind_project(
        &self,
        profile_id: &UserProfileId,
        project_id: &ProjectId,
        database: &Arc<RegisteredGlobalDb>,
    ) -> Result<bool, String> {
        let scope = SessionSyncScopeV1::new(project_id.clone(), profile_id.clone());
        let project_gate = self.project_gate(&scope);
        let _project = project_gate.lock().await;
        let context = self
            .contexts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session_sync_project_key(&scope))
            .cloned();
        let Some(context) = context else {
            return Ok(false);
        };
        context.rebind_project_sessions(database)?;
        if let Err(error) = self.recover_project(&context, database).await {
            context.retire_project_sessions();
            let drain = self.drain_project_tasks(&scope).await.err();
            return Err(format!(
                "session sync project '{}' could not recover after rebind: {error}{}",
                project_id.as_str(),
                drain
                    .map(|drain| format!("; rollback failed: {drain}"))
                    .unwrap_or_default(),
            ));
        }
        Ok(true)
    }

    async fn drain_project_tasks(&self, scope: &SessionSyncScopeV1) -> Result<(), String> {
        let mut project_tasks = {
            let mut tasks = self.tasks.lock().unwrap_or_else(PoisonError::into_inner);
            let mut retained = Vec::with_capacity(tasks.len());
            let mut project_tasks = Vec::new();
            for task in std::mem::take(&mut *tasks) {
                if task.scope == *scope {
                    project_tasks.push(task);
                } else {
                    retained.push(task);
                }
            }
            *tasks = retained;
            project_tasks
        };
        let cancelled_at = now_micros();
        for task in &project_tasks {
            task.cancellation.cancel(cancelled_at);
        }
        let joined = tokio::time::timeout(SESSION_SYNC_SHUTDOWN_DEADLINE, async {
            futures_util::future::join_all(project_tasks.iter_mut().map(|task| &mut task.task))
                .await
        })
        .await;
        let Ok(results) = joined else {
            project_tasks.retain(|task| !task.task.is_finished());
            self.tasks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(project_tasks);
            return Err(format!(
                "session sync tasks for project '{}' did not retire before the deadline",
                scope.project_id().as_str()
            ));
        };
        for result in results {
            work::log_session_sync_join(result);
        }
        let retired_keys = project_tasks
            .iter()
            .map(|task| task.key.clone())
            .collect::<Vec<_>>();
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|key, _| !retired_keys.contains(key));
        self.active_imports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|_, import: &mut ActiveSessionImport| {
                !retired_keys.contains(&import.journal_key)
            });
        Ok(())
    }

    fn remove_context_if_same(
        &self,
        scope: &SessionSyncScopeV1,
        expected: &Arc<SessionSyncProjectContext>,
    ) {
        let key = session_sync_project_key(scope);
        let mut contexts = self
            .contexts
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if contexts
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            contexts.remove(&key);
        }
    }

    async fn restore_previous_context(
        &self,
        scope: &SessionSyncScopeV1,
        previous: Option<(
            Arc<SessionSyncProjectContext>,
            Option<Arc<RegisteredGlobalDb>>,
        )>,
    ) -> Result<(), String> {
        let Some((previous, database)) = previous else {
            return Ok(());
        };
        if let Some(database) = database.as_ref() {
            previous.rebind_project_sessions(database)?;
        }
        self.contexts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session_sync_project_key(scope), Arc::clone(&previous));
        if let Some(database) = database {
            if let Err(error) = self.recover_project(&previous, &database).await {
                previous.retire_project_sessions();
                let drain = self.drain_project_tasks(scope).await;
                self.remove_context_if_same(scope, &previous);
                return Err(match drain {
                    Ok(()) => format!("prior session sync recovery failed: {error}"),
                    Err(drain_error) => format!(
                        "prior session sync recovery failed: {error}; rollback failed: {drain_error}"
                    ),
                });
            }
        }
        Ok(())
    }
}

fn session_sync_project_key(scope: &SessionSyncScopeV1) -> String {
    let profile = scope.profile_id().as_str();
    let project = scope.project_id().as_str();
    format!("p{}:{profile}.r{}:{project}", profile.len(), project.len())
}
