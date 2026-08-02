use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

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

pub struct PreparedRefresh {
    language: String,
    project_root: PathBuf,
    command: String,
    args: Vec<String>,
    epoch: u64,
    batches: Vec<RefreshBatch>,
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
    ) -> Self {
        Self {
            language,
            project_root,
            command,
            args,
            epoch,
            batches,
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
            pending.spawn(collect_refresh_batch(
                ordinal,
                batch,
                self.project_root.clone(),
                self.command.clone(),
                self.args.clone(),
                timeouts,
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
                pending.spawn(collect_refresh_batch(
                    ordinal,
                    batch,
                    self.project_root.clone(),
                    self.command.clone(),
                    self.args.clone(),
                    timeouts,
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
