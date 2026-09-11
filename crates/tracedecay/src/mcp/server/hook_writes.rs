//! Injectable detached read-refresh writer boundary used by daemon-coordinated
//! construction.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Complete detached reconciliation admission requested by the MCP server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackgroundRefreshModeV1 {
    ForceReconcile,
    FreshnessProbe,
}

#[derive(Clone)]
pub(crate) struct BackgroundRefreshRequest {
    pub(crate) graph: Arc<TraceDecay>,
    pub(crate) project_root: PathBuf,
    pub(crate) mode: BackgroundRefreshModeV1,
    pub(crate) reconcile_sink: Option<super::CodeIndexReconcileSink>,
    pub(crate) freshness_probe_sink: Option<super::CodeIndexFreshnessProbeSink>,
}

pub(crate) enum BackgroundRefreshOutcome {
    Admitted(Option<HashMap<String, u64>>),
    LinkedWorktreeDisabled,
}

/// Injectable ownership boundary for detached reconciliation admission.
/// Production returns no token map because scheduler acceptance is distinct
/// from index publication; test writers may return one to verify injection.
pub(crate) type BackgroundRefreshWriter = Arc<
    dyn Fn(
            BackgroundRefreshRequest,
        ) -> Pin<Box<dyn Future<Output = Result<BackgroundRefreshOutcome>> + Send>>
        + Send
        + Sync
        + 'static,
>;

pub(crate) fn direct_background_refresh_writer() -> BackgroundRefreshWriter {
    Arc::new(|request| Box::pin(execute_background_refresh_direct(request)))
}

pub(crate) async fn execute_background_refresh_direct(
    request: BackgroundRefreshRequest,
) -> Result<BackgroundRefreshOutcome> {
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
    let active_branch = tracedecay_runtime_core::branch::current_branch(&canonical_root);
    if request.graph.project_root() != canonical_root
        || request.graph.active_branch() != active_branch.as_deref()
    {
        return Err(TraceDecayError::Config {
            message: "retained background refresh graph is stale".to_string(),
        });
    }
    let accepted = match request.mode {
        // The server's own catch-up and read refreshes are the daemon acting
        // on its own; they never widen a route past its watch policy.
        BackgroundRefreshModeV1::ForceReconcile => request
            .reconcile_sink
            .map(|sink| sink(canonical_root, super::CodeIndexReconcileDemandV1::Automatic))
            .ok_or_else(|| {
                TraceDecayError::project_route(
                    "code_index_scheduler_unavailable",
                    true,
                    "background refresh requires the daemon code-index scheduler",
                )
            })?,
        BackgroundRefreshModeV1::FreshnessProbe => request
            .freshness_probe_sink
            .map(|sink| sink(canonical_root))
            .ok_or_else(|| {
                TraceDecayError::project_route(
                    "code_index_scheduler_unavailable",
                    true,
                    "background freshness probing requires the daemon code-index scheduler",
                )
            })?,
    };
    match accepted.await {
        super::CodeIndexAdmission::Accepted => Ok(BackgroundRefreshOutcome::Admitted(None)),
        super::CodeIndexAdmission::LinkedWorktreeDisabled => {
            Ok(BackgroundRefreshOutcome::LinkedWorktreeDisabled)
        }
        super::CodeIndexAdmission::Unavailable => Err(TraceDecayError::project_route(
            "code_index_scheduler_unavailable",
            true,
            "background refresh was not accepted by the code-index scheduler",
        )),
    }
}
