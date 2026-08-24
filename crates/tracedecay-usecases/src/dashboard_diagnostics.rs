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
use tracedecay_application::{ApplicationOperation, CancellationSignal, Deadline, RequestId};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use tracedecay_code_index::graph_projection::CodeGraphInteractiveReader;
use tracedecay_graph_db::GraphCancellation;
use tracedecay_lsp::analyzer::activity::{active_languages_for_files, documents_for_adapter};
use tracedecay_lsp::analyzer::adapters::builtin_adapters;
use tracedecay_lsp::analyzer::broker::{
    DiagnosticBroker, DiagnosticsSnapshot, EngineState, NodeSpan,
};
use tracedecay_lsp::analyzer::host_ownership::HostAnalyzerOwnership;
use tracedecay_lsp::analyzer::settings::{
    CodeDiagnosticsSettings, IdleBackfillMode, save_settings,
};
use tracedecay_runtime_core::errors::{Result, TraceDecayError};

use crate::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadAdmissionPort, CodeGraphReadAdmissionRequest,
    CodeGraphReadRequest, application_graph_cancellation, map_code_graph_read_runtime_error,
};

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

#[derive(Clone)]
pub struct DashboardDiagnosticsGraphRequestV1 {
    pub operation: ApplicationOperation,
    pub request_id: RequestId,
    pub deadline: Deadline,
    pub cancellation: CancellationSignal,
    pub observed_at: tracedecay_domain::UtcMicros,
}

impl DashboardDiagnosticsGraphRequestV1 {
    pub fn new(
        operation: ApplicationOperation,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: CancellationSignal,
        observed_at: tracedecay_domain::UtcMicros,
    ) -> Self {
        Self {
            operation,
            request_id,
            deadline,
            cancellation,
            observed_at,
        }
    }

    fn admission(&self) -> CodeGraphReadAdmissionRequest<'_> {
        CodeGraphReadAdmissionRequest::new(
            &self.operation,
            self.request_id.clone(),
            self.deadline.clone(),
            &self.cancellation,
            self.observed_at,
        )
    }
}

pub fn diagnostic_broker(
    project_root: PathBuf,
    settings: CodeDiagnosticsSettings,
) -> DiagnosticBroker {
    let mut adapters = builtin_adapters();
    adapters.extend(settings.custom_adapters.clone());
    DiagnosticBroker::new(project_root, adapters, settings)
}

/// Opens the broker every dashboard mounts when a daemon does not hand it one,
/// reading the project's persisted analyzer settings. The daemon, the MCP
/// server, and the directly served dashboard all route through here so the
/// code-diagnostics surface does not depend on which entry point started the
/// dashboard.
pub async fn open_diagnostic_broker(
    project_root: PathBuf,
    dashboard_root: &std::path::Path,
) -> Arc<Mutex<DiagnosticBroker>> {
    // `load_settings` already returns the defaults as `Ok` for a project that
    // has no settings file, so an `Err` is a file that exists and could not be
    // read or parsed. Falling back to the defaults there drops every
    // `custom_adapters` entry the user configured, and the broker has to say
    // so rather than report the fallback as the user's configuration.
    let mut broker = match tracedecay_lsp::analyzer::settings::load_settings(dashboard_root).await {
        Ok(settings) => diagnostic_broker(project_root, settings),
        Err(error) => {
            tracing::warn!(
                dashboard_root = %dashboard_root.display(),
                error = %error,
                "code diagnostics settings could not be loaded; serving defaults as degraded"
            );
            let mut broker = diagnostic_broker(project_root, CodeDiagnosticsSettings::default());
            broker.record_settings_unavailable(error.to_string());
            broker
        }
    };
    // The OpenCode installer registers TraceDecay at the project level and at
    // the home level. Construction reads the project file; the home-level
    // declaration is adopted here so a host that was only registered in
    // `~/.config/opencode/opencode.json` still keeps its retained analyzers.
    let home_ownership = HostAnalyzerOwnership::from_opencode_process_home();
    if home_ownership.is_engaged() {
        let merged = broker.host_analyzer_ownership().union(&home_ownership);
        broker.adopt_host_analyzer_ownership(merged);
    }
    Arc::new(Mutex::new(broker))
}

#[derive(Clone)]
pub struct DashboardDiagnosticsAuthorityV1 {
    inner: Arc<DashboardDiagnosticsAuthorityInnerV1>,
}

struct DashboardDiagnosticsAuthorityInnerV1 {
    project_root: PathBuf,
    settings_root: PathBuf,
    graph_admission: Arc<dyn CodeGraphReadAdmissionPort>,
    graph_projection: Arc<dyn CodeGraphProjectionReadPort>,
    broker: Arc<Mutex<DiagnosticBroker>>,
    idle_backfill_started: AtomicBool,
}

impl DashboardDiagnosticsAuthorityV1 {
    pub fn new(
        project_root: PathBuf,
        settings_root: PathBuf,
        graph_admission: Arc<dyn CodeGraphReadAdmissionPort>,
        graph_projection: Arc<dyn CodeGraphProjectionReadPort>,
        broker: Arc<Mutex<DiagnosticBroker>>,
    ) -> Self {
        Self {
            inner: Arc::new(DashboardDiagnosticsAuthorityInnerV1 {
                project_root,
                settings_root,
                graph_admission,
                graph_projection,
                broker,
                idle_backfill_started: AtomicBool::new(false),
            }),
        }
    }

    pub async fn overview(
        &self,
        request: DashboardDiagnosticsGraphRequestV1,
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let (reader, cancellation) = self.open_graph(&request).await?;
        let snapshot = self.snapshot_with_graph(&reader, cancellation).await?;
        self.maybe_spawn_idle_backfill(&snapshot, request);
        Ok(snapshot)
    }

    pub(crate) async fn snapshot(
        &self,
        request: &DashboardDiagnosticsGraphRequestV1,
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let (reader, cancellation) = self.open_graph(request).await?;
        self.snapshot_with_graph(&reader, cancellation).await
    }

    async fn snapshot_with_graph(
        &self,
        reader: &CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        self.reconcile_project_language_activity(reader, cancellation)
            .await?;
        Ok(self.inner.broker.lock().await.snapshot())
    }

    async fn open_graph(
        &self,
        request: &DashboardDiagnosticsGraphRequestV1,
    ) -> DashboardDiagnosticsResultV1<(CodeGraphInteractiveReader, Arc<dyn GraphCancellation>)>
    {
        let context = self
            .inner
            .graph_admission
            .admit(request.admission())
            .await
            .map_err(map_code_graph_read_runtime_error)?;
        let cancellation = application_graph_cancellation(&request.cancellation);
        let verified = self
            .inner
            .graph_projection
            .open(CodeGraphReadRequest::new(
                &context,
                request.observed_at,
                Arc::clone(&cancellation),
            ))
            .await
            .map_err(map_code_graph_read_runtime_error)?;
        let reader = verified
            .reader_with_cancellation(&context, request.observed_at, Arc::clone(&cancellation))
            .map_err(map_code_graph_read_runtime_error)?;
        Ok((reader, cancellation))
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
        request: &DashboardDiagnosticsGraphRequestV1,
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
        self.snapshot(request).await
    }

    pub async fn refresh_all(
        &self,
        request: &DashboardDiagnosticsGraphRequestV1,
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let (reader, cancellation) = self.open_graph(request).await?;
        let snapshot = self
            .snapshot_with_graph(&reader, Arc::clone(&cancellation))
            .await?;
        let languages = backfill_languages(&snapshot);
        for language in languages {
            self.refresh_one_reconciled(&reader, Arc::clone(&cancellation), &language)
                .await?;
        }
        self.snapshot_with_graph(&reader, cancellation).await
    }

    pub async fn refresh_language(
        &self,
        request: &DashboardDiagnosticsGraphRequestV1,
        language: &str,
    ) -> DashboardDiagnosticsResultV1<DiagnosticsSnapshot> {
        let (reader, cancellation) = self.open_graph(request).await?;
        self.reconcile_project_language_activity(&reader, Arc::clone(&cancellation))
            .await?;
        self.refresh_one_reconciled(&reader, Arc::clone(&cancellation), language)
            .await?;
        self.snapshot_with_graph(&reader, cancellation).await
    }

    async fn refresh_one_reconciled(
        &self,
        reader: &CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
        language: &str,
    ) -> DashboardDiagnosticsResultV1<()> {
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
        let files = indexed_files(reader, Arc::clone(&cancellation))?;
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
                let reader = reader.clone();
                let cancellation = Arc::clone(&cancellation);
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
                            let project_root = authority.inner.project_root.clone();
                            broker
                                .resolve_enclosing_nodes(move |file| {
                                    let reader = reader.clone();
                                    let cancellation = Arc::clone(&cancellation);
                                    let project_root = project_root.clone();
                                    async move {
                                        node_spans_for_file(
                                            &reader,
                                            cancellation,
                                            &project_root,
                                            &file,
                                        )
                                    }
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

    async fn reconcile_project_language_activity(
        &self,
        reader: &CodeGraphInteractiveReader,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> DashboardDiagnosticsResultV1<()> {
        let files = indexed_files(reader, cancellation)?;
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

    fn maybe_spawn_idle_backfill(
        &self,
        snapshot: &DiagnosticsSnapshot,
        request: DashboardDiagnosticsGraphRequestV1,
    ) {
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
                let _ = authority.refresh_language(&request, &language).await;
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

fn node_spans_for_file(
    reader: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    project_root: &std::path::Path,
    file: &str,
) -> Vec<NodeSpan> {
    let Ok(source) = std::fs::read(project_root.join(file)) else {
        return Vec::new();
    };
    let Ok(symbols) = reader.symbols_in_logical_file(file, 100_000, cancellation) else {
        return Vec::new();
    };
    symbols
        .into_iter()
        .filter_map(|symbol| {
            let span = symbol.binding?.source_span?;
            let start_line = byte_line(&source, span.start_byte)?;
            let end_line = byte_line(&source, span.end_byte.saturating_sub(1))?;
            Some(NodeSpan {
                start_line,
                end_line,
                qualified_name: symbol.metadata?.qualified_name,
            })
        })
        .collect()
}

fn indexed_files(
    reader: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<Vec<String>> {
    let mut files = reader
        .files(500_000, cancellation)
        .map_err(|error| {
            crate::graph::map_code_graph_read_runtime_error(crate::graph::map_projection_error(
                error,
            ))
        })?
        .into_iter()
        .map(|file| file.logical_path)
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn byte_line(source: &[u8], offset: u64) -> Option<u32> {
    let offset = usize::try_from(offset).ok()?;
    (offset <= source.len()).then(|| {
        u32::try_from(
            source[..offset]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count(),
        )
        .unwrap_or(u32::MAX)
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Isolates `HOME`/`XDG_CONFIG_HOME` for tests that open a broker.
    ///
    /// `open_diagnostic_broker` reads the process user's home-level OpenCode
    /// registration, and a test must never read the operator's real host
    /// configuration. The lock serializes every test in this module that
    /// touches the ambient environment.
    struct HomeGuard {
        previous_home: Option<std::ffi::OsString>,
        previous_userprofile: Option<std::ffi::OsString>,
        previous_xdg: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeGuard {
        fn isolate(home: &std::path::Path) -> Self {
            static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous_home = std::env::var_os("HOME");
            let previous_userprofile = std::env::var_os("USERPROFILE");
            let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: the module env lock is held for the guard's lifetime,
            // so no sibling test observes the override.
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("USERPROFILE", home);
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            Self {
                previous_home,
                previous_userprofile,
                previous_xdg,
                _lock: lock,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: see `HomeGuard::isolate`; the lock is still held.
            unsafe {
                match self.previous_home.take() {
                    Some(previous) => std::env::set_var("HOME", previous),
                    None => std::env::remove_var("HOME"),
                }
                match self.previous_userprofile.take() {
                    Some(previous) => std::env::set_var("USERPROFILE", previous),
                    None => std::env::remove_var("USERPROFILE"),
                }
                match self.previous_xdg.take() {
                    Some(previous) => std::env::set_var("XDG_CONFIG_HOME", previous),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
        }
    }

    #[tokio::test]
    async fn unreadable_settings_mount_a_broker_that_reports_itself_degraded() {
        let home = tempfile::tempdir().expect("isolated home");
        let _env = HomeGuard::isolate(home.path());
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
        let home = tempfile::tempdir().expect("isolated home");
        let _env = HomeGuard::isolate(home.path());
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

    #[tokio::test]
    async fn a_home_level_opencode_registration_retains_its_analyzers() {
        let home = tempfile::tempdir().expect("isolated home");
        let _env = HomeGuard::isolate(home.path());
        let opencode_dir = home.path().join(".config").join("opencode");
        tokio::fs::create_dir_all(&opencode_dir)
            .await
            .expect("home opencode config dir");
        tokio::fs::write(
            opencode_dir.join("opencode.json"),
            serde_json::json!({
                "lsp": {
                    "tracedecay": {
                        "initialization": {
                            "tracedecay": {
                                "duplicateAnalyzerAvoidance": true,
                                "analyzerOwnership": {
                                    "mode": "projection_only",
                                    "retainedByExtension": { ".rs": ["rust-analyzer"] }
                                }
                            }
                        }
                    }
                }
            })
            .to_string(),
        )
        .await
        .expect("write home opencode.json");
        let project_root = tempfile::tempdir().expect("project root");

        let broker =
            open_diagnostic_broker(project_root.path().to_path_buf(), project_root.path()).await;

        assert_eq!(
            broker.lock().await.host_retained_analyzer("rust"),
            Some("rust-analyzer"),
            "a host registered only at the home level still keeps its analyzer"
        );
    }

    #[tokio::test]
    async fn a_home_level_registration_does_not_revoke_the_project_level_one() {
        let home = tempfile::tempdir().expect("isolated home");
        let _env = HomeGuard::isolate(home.path());
        let opencode_dir = home.path().join(".config").join("opencode");
        tokio::fs::create_dir_all(&opencode_dir)
            .await
            .expect("home opencode config dir");
        tokio::fs::write(
            opencode_dir.join("opencode.json"),
            serde_json::json!({
                "lsp": {
                    "tracedecay": {
                        "initialization": {
                            "tracedecay": {
                                "duplicateAnalyzerAvoidance": true,
                                "analyzerOwnership": {
                                    "retainedByExtension": { ".ts": ["typescript"] }
                                }
                            }
                        }
                    }
                }
            })
            .to_string(),
        )
        .await
        .expect("write home opencode.json");
        let project_root = tempfile::tempdir().expect("project root");
        tokio::fs::write(
            project_root.path().join("opencode.json"),
            serde_json::json!({
                "lsp": {
                    "tracedecay": {
                        "initialization": {
                            "tracedecay": {
                                "duplicateAnalyzerAvoidance": true,
                                "analyzerOwnership": {
                                    "retainedByExtension": { ".rs": ["rust-analyzer"] }
                                }
                            }
                        }
                    }
                }
            })
            .to_string(),
        )
        .await
        .expect("write project opencode.json");

        let broker =
            open_diagnostic_broker(project_root.path().to_path_buf(), project_root.path()).await;

        let broker = broker.lock().await;
        assert_eq!(
            broker.host_retained_analyzer("rust"),
            Some("rust-analyzer"),
            "adopting the home level must not drop the project-level claim"
        );
        assert_eq!(
            broker.host_retained_analyzer("typescript"),
            Some("typescript")
        );
    }
}
