//! `tracedecay analytics …` entry points: thin daemon admin-CLI round-trips.

use std::path::PathBuf;

use serde_json::json;

use crate::commands::daemon_tool_json;

fn cli_project_root() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| tracedecay_runtime_core::config::discover_project_root(&cwd))
}
/// `tracedecay analytics sync`: import hook JSONL rows into the durable
/// `analytics_events` table and print what happened.
pub async fn run_analytics_sync() -> tracedecay_domain::errors::Result<()> {
    let project_root = cli_project_root();
    let outcome = daemon_tool_json(
        project_root.as_deref(),
        "tracedecay_admin_cli",
        json!({ "action": "analytics_sync" }),
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&outcome).unwrap_or_default()
    );
    Ok(())
}
/// `tracedecay analytics diagnostics`: the CLI wrapper around the dashboard
/// diagnostics summary — durable `analytics_events` plus merged hook JSONL.
pub async fn run_analytics_diagnostics(
    all_projects: bool,
    no_sync: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_root = cli_project_root();
    let summary = daemon_tool_json(
        project_root.as_deref(),
        "tracedecay_admin_cli",
        json!({
            "action": "analytics_diagnostics",
            "all": all_projects,
            "no_sync": no_sync,
        }),
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    Ok(())
}
