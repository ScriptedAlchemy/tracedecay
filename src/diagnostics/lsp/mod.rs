//! Compatibility facade for LSP diagnostics owned by `tracedecay-lsp`.

pub use tracedecay_lsp::{activity, adapters};

pub mod settings {
    pub use tracedecay_lsp::settings::{
        CodeDiagnosticsSettings, IdleBackfillMode, LanguageDiagnosticsSettings, settings_path,
    };

    pub async fn load_settings(
        dashboard_root: &std::path::Path,
    ) -> crate::errors::Result<CodeDiagnosticsSettings> {
        tracedecay_lsp::settings::load_settings(dashboard_root)
            .await
            .map_err(Into::into)
    }

    pub async fn save_settings(
        dashboard_root: &std::path::Path,
        settings: &CodeDiagnosticsSettings,
    ) -> crate::errors::Result<()> {
        tracedecay_lsp::settings::save_settings(dashboard_root, settings)
            .await
            .map_err(Into::into)
    }
}

pub mod client {
    pub use tracedecay_lsp::client::{LspDocument, LspRefreshTimeouts};

    pub struct StdioLspClient(tracedecay_lsp::client::StdioLspClient);

    impl StdioLspClient {
        pub async fn start_with_timeouts(
            command: &str,
            args: &[String],
            project_root: &std::path::Path,
            timeouts: LspRefreshTimeouts,
        ) -> crate::errors::Result<Self> {
            tracedecay_lsp::client::StdioLspClient::start_with_timeouts(
                command,
                args,
                project_root,
                timeouts,
            )
            .await
            .map(Self)
            .map_err(Into::into)
        }

        pub async fn collect_document_diagnostics(
            &mut self,
            project_root: &std::path::Path,
            documents: Vec<LspDocument>,
            timeouts: LspRefreshTimeouts,
        ) -> crate::errors::Result<Vec<super::broker::CodeDiagnostic>> {
            self.0
                .collect_document_diagnostics(project_root, documents, timeouts)
                .await
                .map_err(Into::into)
        }
    }

    pub async fn collect_document_diagnostics(
        command: &str,
        args: &[String],
        project_root: &std::path::Path,
        documents: Vec<LspDocument>,
        diagnostics_quiet_timeout: std::time::Duration,
    ) -> crate::errors::Result<Vec<super::broker::CodeDiagnostic>> {
        tracedecay_lsp::client::collect_document_diagnostics(
            command,
            args,
            project_root,
            documents,
            diagnostics_quiet_timeout,
        )
        .await
        .map_err(Into::into)
    }

    pub async fn collect_document_diagnostics_with_timeouts(
        command: &str,
        args: &[String],
        project_root: &std::path::Path,
        documents: Vec<LspDocument>,
        timeouts: LspRefreshTimeouts,
    ) -> crate::errors::Result<Vec<super::broker::CodeDiagnostic>> {
        tracedecay_lsp::client::collect_document_diagnostics_with_timeouts(
            command,
            args,
            project_root,
            documents,
            timeouts,
        )
        .await
        .map_err(Into::into)
    }
}

pub mod broker {
    pub use tracedecay_lsp::broker::{
        BackfillProgress, CodeDiagnostic, CompletedRefresh, DiagnosticSeverity,
        DiagnosticsSnapshot, DiagnosticsSummary, EngineState, EngineStatus, NodeSpan,
        PreparedRefresh, command_available, enclosing_node_for_line,
    };

    pub struct DiagnosticBroker(tracedecay_lsp::broker::DiagnosticBroker);

    impl DiagnosticBroker {
        pub fn new(
            project_root: impl Into<std::path::PathBuf>,
            adapters: Vec<super::adapters::LspAdapterDefinition>,
            settings: super::settings::CodeDiagnosticsSettings,
        ) -> Self {
            Self(tracedecay_lsp::broker::DiagnosticBroker::new(
                project_root,
                adapters,
                settings,
            ))
        }

        pub fn new_for_test(
            project_root: impl Into<std::path::PathBuf>,
            adapters: Vec<super::adapters::LspAdapterDefinition>,
        ) -> Self {
            Self(tracedecay_lsp::broker::DiagnosticBroker::new_for_test(
                project_root,
                adapters,
            ))
        }

        pub fn prepare_refresh(
            &mut self,
            language: &str,
            documents: Vec<super::client::LspDocument>,
        ) -> crate::errors::Result<Option<PreparedRefresh>> {
            self.0
                .prepare_refresh(language, documents)
                .map_err(Into::into)
        }

        pub async fn refresh_documents(
            &mut self,
            language: &str,
            documents: Vec<super::client::LspDocument>,
            diagnostics_quiet_timeout: std::time::Duration,
        ) -> crate::errors::Result<()> {
            self.0
                .refresh_documents(language, documents, diagnostics_quiet_timeout)
                .await
                .map_err(Into::into)
        }

        pub async fn refresh_documents_with_timeouts(
            &mut self,
            language: &str,
            documents: Vec<super::client::LspDocument>,
            timeouts: super::client::LspRefreshTimeouts,
        ) -> crate::errors::Result<()> {
            self.0
                .refresh_documents_with_timeouts(language, documents, timeouts)
                .await
                .map_err(Into::into)
        }

        pub fn finish_refresh(&mut self, completed: CompletedRefresh) -> crate::errors::Result<()> {
            self.0.finish_refresh(completed).map_err(Into::into)
        }

        pub(crate) fn from_inner(inner: tracedecay_lsp::broker::DiagnosticBroker) -> Self {
            Self(inner)
        }
    }

    impl std::ops::Deref for DiagnosticBroker {
        type Target = tracedecay_lsp::broker::DiagnosticBroker;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl std::ops::DerefMut for DiagnosticBroker {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }
}
