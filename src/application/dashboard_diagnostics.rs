//! Canonical daemon/application authority for dashboard diagnostics controls.
//!
//! Dashboard HTTP handlers are transport adapters only. They receive this
//! already-mounted authority and never construct an analyzer broker, persist
//! analyzer settings, or schedule analyzer work themselves.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Mutex;

use crate::db::Database;
use crate::diagnostics::lsp::activity::{active_languages_for_files, documents_for_adapter};
use crate::diagnostics::lsp::adapters::builtin_adapters;
use crate::diagnostics::lsp::broker::{
    DiagnosticBroker, DiagnosticsSnapshot, EngineState, NodeSpan,
};
use crate::diagnostics::lsp::settings::{CodeDiagnosticsSettings, IdleBackfillMode, save_settings};
use crate::errors::{Result, TraceDecayError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum DashboardDiagnosticsErrorV1 {
    #[error("no code diagnostics adapter registered for language '{language}'")]
    AdapterUnavailable { language: String },
    #[error(transparent)]
    Runtime(#[from] TraceDecayError),
}

type DashboardDiagnosticsResultV1<T> = std::result::Result<T, DashboardDiagnosticsErrorV1>;

pub(crate) fn diagnostic_broker(
    project_root: PathBuf,
    settings: CodeDiagnosticsSettings,
) -> DiagnosticBroker {
    let mut adapters = builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    DiagnosticBroker::new(project_root, adapters, settings)
}

#[derive(Clone)]
pub(crate) struct DashboardDiagnosticsAuthorityV1 {
    inner: Arc<DashboardDiagnosticsAuthorityInnerV1>,
}

struct DashboardDiagnosticsAuthorityInnerV1 {
    project_root: PathBuf,
    settings_root: PathBuf,
    database: Arc<Database>,
    broker: Arc<Mutex<DiagnosticBroker>>,
    idle_backfill_started: AtomicBool,
}

impl DashboardDiagnosticsAuthorityV1 {
    pub(crate) fn new(
        project_root: PathBuf,
        settings_root: PathBuf,
        database: Arc<Database>,
        broker: Arc<Mutex<DiagnosticBroker>>,
    ) -> Self {
        Self {
            inner: Arc::new(DashboardDiagnosticsAuthorityInnerV1 {
                project_root,
                settings_root,
                database,
                broker,
                idle_backfill_started: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) async fn overview(&self) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let snapshot = self.snapshot().await?;
        self.maybe_spawn_idle_backfill(&snapshot);
        Ok(snapshot)
    }

    pub(crate) async fn snapshot(&self) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        self.reconcile_project_language_activity().await?;
        Ok(self.inner.broker.lock().await.snapshot())
    }

    pub(crate) async fn update_settings(
        &self,
        settings: CodeDiagnosticsSettings,
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        save_settings(&self.inner.settings_root, &settings).await?;
        let mut adapters = builtin_adapters();
        adapters.extend(settings.custom_adapters.clone());
        let mut broker = self.inner.broker.lock().await;
        broker.update_adapters(adapters);
        broker.update_settings(settings);
        drop(broker);
        self.snapshot().await
    }

    pub(crate) async fn refresh_all(&self) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let languages = self.refreshable_languages().await?;
        for language in languages {
            self.refresh_one_reconciled(&language).await?;
        }
        self.snapshot().await
    }

    pub(crate) async fn refresh_language(
        &self,
        language: &str,
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        self.reconcile_project_language_activity().await?;
        self.refresh_one_reconciled(language).await?;
        self.snapshot().await
    }

    async fn refreshable_languages(&self) -> DashboardDiagnosticsResultV1<Vec<String>> {
        let snapshot = self.snapshot().await?;
        Ok(backfill_languages(&snapshot))
    }

    async fn refresh_one_reconciled(&self, language: &str) -> DashboardDiagnosticsResultV1<()> {
        let snapshot = self.inner.broker.lock().await.snapshot();
        if !snapshot.settings.language_enabled(language) {
            self.inner
                .broker
                .lock()
                .await
                .set_language_enabled(language, false);
            return Ok(());
        }
        let Some(adapter) = self.inner.broker.lock().await.adapter_for(language) else {
            return Err(DashboardDiagnosticsErrorV1::AdapterUnavailable {
                language: language.to_owned(),
            });
        };
        let files = indexed_files(&self.inner.database).await?;
        let documents = documents_for_adapter(&self.inner.project_root, &adapter, files).await?;
        let document_count = documents.len();
        self.inner.broker.lock().await.record_backfill_progress(
            language,
            document_count,
            document_count,
            0,
            None,
        );
        if documents.is_empty() {
            self.inner.broker.lock().await.record_backfill_progress(
                language,
                0,
                0,
                0,
                Some(crate::tracedecay::current_timestamp()),
            );
            return Ok(());
        }
        let prepared = self
            .inner
            .broker
            .lock()
            .await
            .prepare_refresh(language, documents);
        let mut progress_recorded_in_task = false;
        let refresh_ok = match prepared {
            Ok(Some(prepared)) => {
                progress_recorded_in_task = true;
                let authority = self.clone();
                let language = language.to_owned();
                let (tx, rx) = tokio::sync::oneshot::channel();
                tokio::spawn(async move {
                    let completed = prepared.collect_diagnostics(Duration::from_secs(5)).await;
                    let refresh_ok = completed.is_ok();
                    {
                        let mut broker = authority.inner.broker.lock().await;
                        let _ = broker.finish_refresh(completed);
                        let database = Arc::clone(&authority.inner.database);
                        broker
                            .resolve_enclosing_nodes(move |file| {
                                let database = Arc::clone(&database);
                                async move { node_spans_for_file(&database, &file).await }
                            })
                            .await;
                        let snapshot = broker.snapshot();
                        let files_with_diagnostics = files_with_diagnostics(&snapshot, &language);
                        broker.record_backfill_progress(
                            &language,
                            document_count,
                            document_count,
                            files_with_diagnostics,
                            refresh_ok.then(crate::tracedecay::current_timestamp),
                        );
                    }
                    let _ = tx.send(refresh_ok);
                });
                rx.await.unwrap_or(false)
            }
            Ok(None) => true,
            Err(_) => false,
        };
        if !progress_recorded_in_task {
            let snapshot = self.inner.broker.lock().await.snapshot();
            let files_with_diagnostics = files_with_diagnostics(&snapshot, language);
            self.inner.broker.lock().await.record_backfill_progress(
                language,
                document_count,
                document_count,
                files_with_diagnostics,
                refresh_ok.then(crate::tracedecay::current_timestamp),
            );
        }
        Ok(())
    }

    async fn reconcile_project_language_activity(&self) -> DashboardDiagnosticsResultV1<()> {
        let files = indexed_files(&self.inner.database).await?;
        let adapters = {
            let broker = self.inner.broker.lock().await;
            broker
                .snapshot()
                .engines
                .into_iter()
                .filter_map(|engine| broker.adapter_for(&engine.language))
                .collect::<Vec<_>>()
        };
        let active_languages =
            active_languages_for_files(&self.inner.project_root, &adapters, &files);
        self.inner
            .broker
            .lock()
            .await
            .update_project_languages(active_languages);
        Ok(())
    }

    fn maybe_spawn_idle_backfill(&self, snapshot: &DiagnosticsSnapshot) {
        if snapshot.settings.idle_backfill != IdleBackfillMode::Idle {
            return;
        }
        let languages = backfill_languages(snapshot);
        if languages.is_empty()
            || self
                .inner
                .idle_backfill_started
                .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let authority = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(750)).await;
            for language in languages {
                let _ = authority.refresh_language(&language).await;
                tokio::task::yield_now().await;
            }
        });
    }
}

fn backfill_languages(snapshot: &DiagnosticsSnapshot) -> Vec<String> {
    snapshot
        .engines
        .iter()
        .filter(|engine| {
            engine.enabled
                && !matches!(
                    engine.state,
                    EngineState::Disabled | EngineState::Inactive | EngineState::Unavailable
                )
        })
        .map(|engine| engine.language.clone())
        .collect()
}

fn files_with_diagnostics(snapshot: &DiagnosticsSnapshot, language: &str) -> usize {
    snapshot
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.language == language)
        .map(|diagnostic| diagnostic.file.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

async fn node_spans_for_file(database: &Database, file: &str) -> Vec<NodeSpan> {
    database
        .get_nodes_by_file(file)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|node| NodeSpan {
            start_line: node.start_line,
            end_line: node.end_line,
            qualified_name: node.qualified_name,
        })
        .collect()
}

async fn indexed_files(database: &Database) -> Result<Vec<String>> {
    let mut files = database
        .get_all_files()
        .await?
        .into_iter()
        .map(|file| file.path)
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}
