//! `tracedecay analytics …` entry points: thin daemon admin-CLI round-trips.

use std::path::PathBuf;

use serde_json::{Value, json};

fn cli_project_root() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| tracedecay_runtime_core::config::discover_project_root(&cwd))
}

/// `tracedecay analytics sync`: import hook JSONL rows into the durable
/// `analytics_events` table and print what happened.
pub async fn run_analytics_sync() -> tracedecay_domain::errors::Result<()> {
    let project_root = cli_project_root();
    let outcome = call_admin_cli(project_root, json!({ "action": "analytics_sync" })).await?;
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
    let summary = call_admin_cli(
        project_root,
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

#[hotpath::measure(label = "cli.analytics.admin_call", future = true)]
async fn call_admin_cli(
    project_root: Option<PathBuf>,
    arguments: Value,
) -> tracedecay_domain::errors::Result<Value> {
    let handshake =
        tracedecay::daemon::handshake_for_current_client(project_root, None, false, false)?;
    let result =
        tracedecay::daemon::call_default_tool(&handshake, "tracedecay_admin_cli", arguments)
            .await?;
    tracedecay::daemon::tool_json_payload(&result, "tracedecay_admin_cli")
}
