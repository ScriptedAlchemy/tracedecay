//! `tracedecay analytics …` entry points: thin daemon admin-CLI round-trips.

use std::path::PathBuf;
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_contracts::retrieval::{AdminCliResultV1, AdminCliSurfaceRequestV1};

use crate::commands::{admin_cli_result, admin_cli_result_mismatch};

fn cli_project_root(profile: &ProfileRoot) -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| profile.discover_project_root(&cwd))
}
/// `tracedecay analytics sync`: import hook JSONL rows into the durable
/// `analytics_events` table and print what happened.
pub async fn run_analytics_sync(profile: &ProfileRoot) -> tracedecay_domain::errors::Result<()> {
    let project_root = cli_project_root(profile);
    let outcome = match admin_cli_result(
        profile,
        project_root.as_deref(),
        AdminCliSurfaceRequestV1::AnalyticsSync {},
    )
    .await?
    {
        AdminCliResultV1::AnalyticsSync(outcome) => outcome,
        _ => return Err(admin_cli_result_mismatch("analytics_sync")),
    };
    println!("{}", serde_json::to_string_pretty(&outcome)?);
    Ok(())
}
/// `tracedecay analytics diagnostics`: the CLI wrapper around the dashboard
/// diagnostics summary, durable `analytics_events` plus merged hook JSONL.
pub async fn run_analytics_diagnostics(
    profile: &ProfileRoot,
    all_projects: bool,
    no_sync: bool,
) -> tracedecay_domain::errors::Result<()> {
    let project_root = cli_project_root(profile);
    let summary = match admin_cli_result(
        profile,
        project_root.as_deref(),
        AdminCliSurfaceRequestV1::AnalyticsDiagnostics {
            all: all_projects,
            no_sync,
        },
    )
    .await?
    {
        AdminCliResultV1::AnalyticsDiagnostics(summary) => summary,
        _ => return Err(admin_cli_result_mismatch("analytics_diagnostics")),
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
