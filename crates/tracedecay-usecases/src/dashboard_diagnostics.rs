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
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use tracedecay_lsp::analyzer::activity::{active_languages_for_files, documents_for_adapter};
use tracedecay_lsp::analyzer::adapters::builtin_adapters;
use tracedecay_lsp::analyzer::broker::{
    DiagnosticBroker, DiagnosticsSnapshot, EngineState, NodeSpan,
};
use tracedecay_lsp::analyzer::settings::{
    CodeDiagnosticsSettings, IdleBackfillMode, save_settings,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

#[derive(Debug, thiserror::Error)]
pub enum DashboardDiagnosticsErrorV1 {
    #[error("no code diagnostics adapter registered for language '{language}'")]
    AdapterUnavailable { language: String },
    #[error("code diagnostics language is disabled for '{language}'")]
    LanguageDisabled { language: String },
    #[error("code diagnostics settings changed after this edit began")]
    RevisionConflict {
        expected: ManifestDigest,
        actual: ManifestDigest,
    },
    #[error(transparent)]
    Runtime(#[from] TraceDecayError),
}

/// Content-addressed identity of one code-diagnostics settings state.
///
/// These settings are a file the broker also holds in memory, so there is no
/// store revision to hand out. Digesting the settings themselves gives an
/// exact compare-and-set token: two writers agree only when they agree about
/// the whole settings value.
pub fn settings_revision(settings: &CodeDiagnosticsSettings) -> Result<ManifestDigest> {
    canonical_sha256(&("tracedecay.code-diagnostics.settings.v1", settings)).map_err(|error| {
        TraceDecayError::Config {
            message: format!("could not compute code diagnostics settings revision: {error}"),
        }
    })
}

pub type DashboardDiagnosticsResultV1<T> = std::result::Result<T, DashboardDiagnosticsErrorV1>;

pub fn diagnostic_broker(
    project_root: PathBuf,
    settings: CodeDiagnosticsSettings,
) -> DiagnosticBroker {
    let mut adapters = builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    DiagnosticBroker::new(project_root, adapters, settings)
}

/// Opens the broker every dashboard mounts when a daemon does not hand it one,
/// reading the project's persisted analyzer settings. Both the MCP server and
/// the directly served dashboard route through here so the code-diagnostics
/// surface does not depend on which entry point started the dashboard.
pub async fn open_diagnostic_broker(
    project_root: PathBuf,
    dashboard_root: &std::path::Path,
) -> Arc<Mutex<DiagnosticBroker>> {
    // `load_settings` already returns the defaults as `Ok` for a project that
    // has no settings file, so an `Err` is a file that exists and could not be
    // read or parsed. Falling back to the defaults there drops every
    // `custom_adapters` entry the user configured, and the broker has to say
    // so rather than report the fallback as the user's configuration.
    match tracedecay_lsp::analyzer::settings::load_settings(dashboard_root).await {
        Ok(settings) => Arc::new(Mutex::new(diagnostic_broker(project_root, settings))),
        Err(error) => {
            tracing::warn!(
                dashboard_root = %dashboard_root.display(),
                error = %error,
                "code diagnostics settings could not be loaded; serving defaults as degraded"
            );
            let mut broker = diagnostic_broker(project_root, CodeDiagnosticsSettings::default());
            broker.record_settings_unavailable(error.to_string());
            Arc::new(Mutex::new(broker))
        }
    }
}

#[derive(Clone)]
pub struct DashboardDiagnosticsAuthorityV1 {
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
    pub fn new(
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

    pub async fn overview(&self) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let snapshot = self.snapshot().await?;
        self.maybe_spawn_idle_backfill(&snapshot);
        Ok(snapshot)
    }

    pub(crate) async fn snapshot(&self) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        self.reconcile_project_language_activity().await?;
        Ok(self.inner.broker.lock().await.snapshot())
    }

    /// Applies `patch` to the settings the caller last read, or rejects the
    /// write.
    ///
    /// The read, the revision check, and the write all happen under the broker
    /// lock. Splitting them — reading the settings, editing them, then writing
    /// the result back — is what let a second writer land between the two and
    /// be overwritten while both callers were told they had succeeded.
    pub async fn update_settings(
        &self,
        expected_revision: &ManifestDigest,
        patch: impl FnOnce(&mut CodeDiagnosticsSettings),
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        {
            let mut broker = self.inner.broker.lock().await;
            let mut settings = broker.settings().clone();
            let actual = settings_revision(&settings)?;
            if actual != *expected_revision {
                return Err(DashboardDiagnosticsErrorV1::RevisionConflict {
                    expected: expected_revision.clone(),
                    actual,
                });
            }
            patch(&mut settings);
            save_settings(&self.inner.settings_root, &settings)
                .await
                .map_err(TraceDecayError::from)?;
            let mut adapters = builtin_adapters();
            adapters.extend(settings.custom_adapters.clone());
            broker.update_adapters(adapters);
            broker.update_settings(settings);
        }
        self.snapshot().await
    }

    pub async fn refresh_all(&self) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let languages = self.refreshable_languages().await?;
        for language in languages {
            self.refresh_one_reconciled(&language).await?;
        }
        self.snapshot().await
    }

    pub async fn refresh_language(
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
            // Reconcile the reported engine state, then refuse. `refresh_all`
            // filters disabled engines out before it gets here, so the only
            // caller that reaches this is one that named this language — and
            // answering it with a success snapshot it did nothing to earn
            // would report a refresh that never ran.
            self.inner
                .broker
                .lock()
                .await
                .set_language_enabled(language, false);
            return Err(DashboardDiagnosticsErrorV1::LanguageDisabled {
                language: language.to_owned(),
            });
        }
        let Some(adapter) = self.inner.broker.lock().await.adapter_for(language) else {
            return Err(DashboardDiagnosticsErrorV1::AdapterUnavailable {
                language: language.to_owned(),
            });
        };
        let files = indexed_files(&self.inner.database).await?;
        let documents = documents_for_adapter(&self.inner.project_root, &adapter, files)
            .await
            .map_err(TraceDecayError::from)?;
        let document_count = documents.len();
        self.inner.broker.lock().await.record_backfill_progress(
            language,
            document_count,
            0,
            0,
            None,
        );
        if documents.is_empty() {
            self.inner.broker.lock().await.record_backfill_progress(
                language,
                0,
                0,
                0,
                Some(tracedecay_runtime_core::tracedecay::current_timestamp()),
            );
            return Ok(());
        }
        let prepared = self
            .inner
            .broker
            .lock()
            .await
            .prepare_refresh(language, documents);
        match prepared {
            Ok(Some(prepared)) => {
                let authority = self.clone();
                let task_language = language.to_owned();
                let (tx, rx) = tokio::sync::oneshot::channel::<Result<()>>();
                tokio::spawn(async move {
                    let completed = prepared.collect_diagnostics(Duration::from_secs(5)).await;
                    let refresh_result = {
                        let mut broker = authority.inner.broker.lock().await;
                        let refresh_result = broker
                            .finish_refresh(completed)
                            .map_err(TraceDecayError::from);
                        if refresh_result.is_ok() {
                            let database = Arc::clone(&authority.inner.database);
                            broker
                                .resolve_enclosing_nodes(move |file| {
                                    let database = Arc::clone(&database);
                                    async move { node_spans_for_file(&database, &file).await }
                                })
                                .await;
                        }
                        let snapshot = broker.snapshot();
                        let files_with_diagnostics =
                            files_with_diagnostics(&snapshot, &task_language);
                        broker.record_backfill_progress(
                            &task_language,
                            document_count,
                            if refresh_result.is_ok() {
                                document_count
                            } else {
                                0
                            },
                            files_with_diagnostics,
                            refresh_result
                                .is_ok()
                                .then(tracedecay_runtime_core::tracedecay::current_timestamp),
                        );
                        refresh_result
                    };
                    let _ = tx.send(refresh_result);
                });
                rx.await.map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "code diagnostics refresh task failed for '{language}': {error}"
                    ),
                })??;
            }
            Ok(None) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "code diagnostics refresh became unavailable for '{language}'"
                    ),
                }
                .into());
            }
            Err(error) => return Err(TraceDecayError::from(error).into()),
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unreadable_settings_mount_a_broker_that_reports_itself_degraded() {
        let dashboard_root = tempfile::tempdir().expect("dashboard root");
        tokio::fs::write(
            tracedecay_lsp::analyzer::settings::settings_path(dashboard_root.path()),
            b"{ this is not settings json",
        )
        .await
        .expect("unparseable settings file");

        let broker =
            open_diagnostic_broker(dashboard_root.path().to_path_buf(), dashboard_root.path())
                .await;
        let snapshot = broker.lock().await.snapshot();

        let reason = snapshot
            .settings_unavailable
            .expect("settings that cannot be parsed must not be reported as the user's settings")
            .reason;
        assert!(
            reason.contains("failed to parse code diagnostics settings"),
            "the degradation must name the failed read: {reason}"
        );
        assert!(
            snapshot.settings.custom_adapters.is_empty(),
            "the fallback settings carry no custom analyzers, which is exactly why the \
             degradation has to be reported"
        );
    }

    #[tokio::test]
    async fn absent_settings_mount_a_healthy_broker_on_the_defaults() {
        let dashboard_root = tempfile::tempdir().expect("dashboard root");

        let broker =
            open_diagnostic_broker(dashboard_root.path().to_path_buf(), dashboard_root.path())
                .await;

        assert_eq!(
            broker.lock().await.snapshot().settings_unavailable,
            None,
            "a project that never configured analyzer settings is not degraded"
        );
    }
}
