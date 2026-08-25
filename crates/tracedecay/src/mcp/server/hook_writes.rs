//! Injectable detached read-refresh writer boundary used by daemon-coordinated
//! construction.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;

/// Complete detached reconciliation admission requested by the MCP server.
#[derive(Clone)]
pub(crate) struct BackgroundRefreshRequest {
    pub(crate) graph: Arc<TraceDecay>,
    pub(crate) project_root: PathBuf,
    pub(crate) reconcile_sink: Option<super::CodeIndexReconcileSink>,
}

/// Injectable ownership boundary for detached reconciliation admission.
/// Production returns no token map because scheduler acceptance is distinct
/// from index publication; test writers may return one to verify injection.
pub(crate) type BackgroundRefreshWriter = Arc<
    dyn Fn(
            BackgroundRefreshRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Option<HashMap<String, u64>>>> + Send>>
        + Send
        + Sync
        + 'static,
>;

pub(crate) fn direct_background_refresh_writer() -> BackgroundRefreshWriter {
    Arc::new(|request| Box::pin(execute_background_refresh_direct(request)))
}

pub(crate) async fn execute_background_refresh_direct(
    request: BackgroundRefreshRequest,
) -> Result<Option<HashMap<String, u64>>> {
    let canonical_root =
        request
            .project_root
            .canonicalize()
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "could not resolve retained background refresh root {}: {error}",
                    request.project_root.display()
                ),
            })?;
    let active_branch = crate::branch::current_branch(&canonical_root);
    if request.graph.project_root() != canonical_root
        || request.graph.active_branch() != active_branch.as_deref()
    {
        return Err(TraceDecayError::Config {
            message: "retained background refresh graph is stale".to_string(),
        });
    }
    let Some(reconcile_sink) = request.reconcile_sink else {
        return Err(TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "background refresh requires the daemon code-index scheduler",
        ));
    };
    if !reconcile_sink(canonical_root).await {
        return Err(TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "background refresh was not accepted by the code-index scheduler",
        ));
    }
    Ok(None)
}
