//! Best-effort hint-outcome settlement for the served project.
//!
//! Hooks append hint rows to `hook_analytics.jsonl`; only this pass moves
//! them into `analytics_events` and correlates their outcomes. Transcript
//! ingest awaits it, while the hook actions every hint-producing host reaches
//! (Codex prompts, Codex and Cursor session starts) run it detached so it
//! adds no hook latency.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, PoisonError};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_runtime_core::native_runtime::NativeTask;

/// One settlement pass per project at a time: concurrent passes would both
/// see a hint as unresolved and append duplicate `hint_outcome` rows.
// ponytail: entries are never evicted; bounded by the projects this process serves.
static PROJECT_SETTLEMENTS: LazyLock<StdMutex<HashMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(Default::default);

fn project_settlement(project_root: &Path) -> Arc<Mutex<()>> {
    PROJECT_SETTLEMENTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(project_root.to_path_buf())
        .or_default()
        .clone()
}

/// Settles the project's emitted hints after waiting for any pass already
/// running for it. Every unavailable or failed state is a typed JSON outcome,
/// never an error.
pub(super) async fn settle_hint_outcomes(
    project_root: &Path,
    profile_root: Option<&Path>,
    analytics: Option<&RegisteredGlobalDb>,
    sessions: Option<&RegisteredGlobalDb>,
) -> Value {
    let _pass = project_settlement(project_root).lock_owned().await;
    settle(project_root, profile_root, analytics, sessions).await
}

/// Starts a settlement pass off the hook's response path. A pass already
/// running for the project makes this a no-op: that pass, or the next
/// trigger, imports the rows appended since.
pub(super) fn spawn_hint_settlement(
    project_root: &Path,
    profile_root: Option<&Path>,
    analytics: Option<&RegisteredGlobalDbLeaseV1>,
    sessions: Option<&RegisteredGlobalDbLeaseV1>,
) {
    let Ok(pass) = project_settlement(project_root).try_lock_owned() else {
        return;
    };
    let work = settle_detached(
        pass,
        project_root.to_path_buf(),
        profile_root.map(Path::to_path_buf),
        analytics.cloned(),
        sessions.cloned(),
    );
    if let Err(error) = NativeTask::spawn(work) {
        tracing::warn!(%error, "hint-outcome settlement did not start");
    }
}

async fn settle_detached(
    _pass: OwnedMutexGuard<()>,
    project_root: PathBuf,
    profile_root: Option<PathBuf>,
    analytics: Option<RegisteredGlobalDbLeaseV1>,
    sessions: Option<RegisteredGlobalDbLeaseV1>,
) {
    let outcome = settle(
        &project_root,
        profile_root.as_deref(),
        analytics.as_deref(),
        sessions.as_deref(),
    )
    .await;
    if outcome["status"] == "ok" {
        tracing::debug!(%outcome, "hint-outcome settlement pass finished");
    } else {
        tracing::warn!(%outcome, "hint-outcome settlement pass did not settle");
    }
}

async fn settle(
    project_root: &Path,
    profile_root: Option<&Path>,
    analytics: Option<&RegisteredGlobalDb>,
    sessions: Option<&RegisteredGlobalDb>,
) -> Value {
    let Some(profile_root) = profile_root else {
        return json!({ "status": "unavailable", "reason": "profile_root_unavailable" });
    };
    let sources = match tracedecay_application::analytics_bridge::hook_import_sources(
        profile_root,
        Some(project_root),
    ) {
        Ok(sources) => sources,
        Err(error) => {
            return json!({
                "status": "failed",
                "operation": "hook_import_sources",
                "detail": error.to_string(),
            });
        }
    };
    tracing::Instrument::instrument(
        tracedecay_agent_hosts::hooks::hint_outcomes::settlement::settle_project_hint_outcomes(
            analytics,
            sessions,
            sources,
            project_root,
            tracedecay_runtime_core::tracedecay::current_timestamp(),
        ),
        tracing::trace_span!("mcp.hook_runtime.hint_settle"),
    )
    .await
    .as_json()
}
