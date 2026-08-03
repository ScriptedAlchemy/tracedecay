use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::cli::MigrateAction;

pub(crate) async fn handle_migrate_action(action: MigrateAction) -> tracedecay::errors::Result<()> {
    match action {
        MigrateAction::StorageReport {
            profile_root,
            project_id,
            project_root,
            json,
        } => handle_migrate_storage_report(profile_root, project_id, project_root, json).await,
        MigrateAction::BackupProfile { to, backup_id } => {
            handle_migrate_backup_profile(to, backup_id)
        }
        MigrateAction::RehearseProfileBackup { backup, restore } => {
            handle_migrate_rehearse_profile_backup(backup, restore)
        }
    }
}

async fn brokered_storage_report(
    project_id: Option<&str>,
    project_root: Option<&Path>,
) -> tracedecay::errors::Result<tracedecay::retention::storage_report::StorageReport> {
    const PAGE_LIMIT: usize = 8;
    const MAX_PAGES: usize = 4096;

    let mut report = tracedecay::retention::storage_report::StorageReport::default();
    let mut cursor = None;
    for _ in 0..MAX_PAGES {
        let request = super::daemon::daemon_tool_json(
            None,
            "tracedecay_admin_cli",
            serde_json::json!({
                "action": "storage_report",
                "project_id": project_id,
                "project_root": project_root,
                "cursor": cursor,
                "limit": PAGE_LIMIT,
            }),
        );
        let value = tokio::time::timeout(Duration::from_secs(10), request)
            .await
            .map_err(|_| tracedecay::errors::TraceDecayError::Config {
                message: "daemon storage report authority timed out after 10 seconds".to_string(),
            })??;
        let page: tracedecay::retention::storage_report::StorageReport =
            serde_json::from_value(value)?;
        merge_storage_report_page(&mut report, page);
        if report.coverage.state
            == tracedecay::retention::storage_report::StorageReportCoverageState::Complete
        {
            return Ok(report);
        }
        cursor = report.coverage.next_cursor.clone();
        if cursor.is_none() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: "partial daemon storage report omitted its continuation cursor".to_owned(),
            });
        }
    }
    Ok(report)
}

fn merge_storage_report_page(
    report: &mut tracedecay::retention::storage_report::StorageReport,
    page: tracedecay::retention::storage_report::StorageReport,
) {
    if report.profile_root.is_empty() {
        report.profile_root = page.profile_root;
    }
    report.stores.extend(page.stores);
    report
        .code_generation_retention
        .extend(page.code_generation_retention);
    report
        .code_generation_retention_availability
        .extend(page.code_generation_retention_availability);
    report.unregistered_dir_count = report
        .unregistered_dir_count
        .saturating_add(page.unregistered_dir_count);
    report.unregistered_bytes = report
        .unregistered_bytes
        .saturating_add(page.unregistered_bytes);
    report.global_db_bytes = report.global_db_bytes.max(page.global_db_bytes);
    report.coverage = page.coverage;
}

/// Read-only per-store size / free-page-ratio / unregistered-directory report
/// (plan 38 §7). The active profile routes through the daemon's retained
/// authority; explicit offline profiles retain the bounded read-only path.
async fn handle_migrate_storage_report(
    profile_root: Option<String>,
    project_id: Option<String>,
    project_root: Option<String>,
    json: bool,
) -> tracedecay::errors::Result<()> {
    let default_profile_root = tracedecay::storage::default_profile_root()?;
    let profile_root = match profile_root {
        Some(path) => PathBuf::from(path),
        None => default_profile_root.clone(),
    };
    let daemon_owns_profile = profile_root == default_profile_root;
    let project_root = project_root.map(PathBuf::from);
    let report = if daemon_owns_profile && tracedecay::daemon::daemon_reachable() {
        brokered_storage_report(project_id.as_deref(), project_root.as_deref()).await?
    } else {
        let offline = match (&project_id, &project_root) {
            (Some(project_id), Some(project_root)) => {
                tracedecay::retention::storage_report::build_project_storage_report(
                    &profile_root,
                    project_id,
                    project_root,
                )
            }
            (None, None) => {
                tracedecay::retention::storage_report::build_storage_report(&profile_root).await
            }
            _ => unreachable!("clap requires project id and root together"),
        };
        match offline {
            Ok(report) => report,
            Err(_) if daemon_owns_profile && tracedecay::daemon::daemon_reachable() => {
                brokered_storage_report(project_id.as_deref(), project_root.as_deref()).await?
            }
            Err(error) => return Err(error),
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("storage report: {}", report.profile_root);
    let profile_total = report.profile_total_size();
    // A partial total is a floor, not the profile size; say which families are
    // missing rather than printing a number that reads as complete.
    match profile_total.state {
        tracedecay::retention::storage_report::ProfileTotalCoverageStateV1::Complete => {
            println!(
                "  profile total: {} bytes",
                format_bytes(profile_total.accounted_bytes)
            );
        }
        tracedecay::retention::storage_report::ProfileTotalCoverageStateV1::Partial => {
            println!(
                "  profile total: at least {} bytes (incomplete)",
                format_bytes(profile_total.accounted_bytes)
            );
            for family in &profile_total.excluded_families {
                println!("    not sized: {family}");
            }
        }
    }
    println!(
        "  global.db: {} bytes",
        format_bytes(report.global_db_bytes)
    );
    println!("  registered stores: {}", report.stores.len());
    for store in &report.stores {
        // Free pages are unsampled when the store was busy or unreadable; say
        // so rather than printing a zero that reads as "no bloat".
        let free = match (store.free_bytes, store.free_page_ratio) {
            (Some(free_bytes), Some(ratio)) => format!(
                "{} free ({:.1}% free pages)",
                format_bytes(free_bytes),
                ratio * 100.0
            ),
            _ => "free pages not sampled (store busy or unreadable)".to_string(),
        };
        println!(
            "    {} ({}): {} total, {free}",
            store.project_id,
            store.canonical_root,
            format_bytes(store.total_bytes),
        );
    }
    for retention in &report.code_generation_retention {
        println!(
            "  code-index retention dry run for {}: active {} ({}), rollback floor {}",
            retention.project_id,
            retention.active_generation_id,
            retention.active_generation_file,
            retention.rollback_floor
        );
        println!(
            "    superseded: {} generation(s), {} bytes ({})",
            retention.superseded_generation_count,
            retention.superseded_generation_bytes,
            format_bytes(retention.superseded_generation_bytes)
        );
        println!(
            "    would delete: {} generation(s), {} bytes ({})",
            retention.collectable_generation_count,
            retention.collectable_generation_bytes,
            format_bytes(retention.collectable_generation_bytes)
        );
        for generation in &retention.collectable_generations {
            println!(
                "      {}/code-generations-v1/{} ({} bytes, sealed_at_micros={})",
                retention.store_root,
                generation.generation_file,
                generation.size_bytes,
                generation.sealed_at_micros
            );
        }
    }
    for availability in &report.code_generation_retention_availability {
        if availability.state
            == tracedecay::retention::storage_report::StorageReportAvailabilityState::Unavailable
        {
            println!(
                "  code-index retention unavailable for {}: {}",
                availability.project_id,
                availability.reason.as_deref().unwrap_or("unspecified")
            );
        }
    }
    println!(
        "  unregistered directories: {} ({})",
        report.unregistered_dir_count,
        format_bytes(report.unregistered_bytes)
    );
    if report.unregistered_dir_count > 0 {
        println!(
            "  run the daemon's automatic sweep, or `tracedecay tool tracedecay_admin_cli` \
             orphan-store collection, to reclaim unregistered directories"
        );
    }
    if report.coverage.state
        == tracedecay::retention::storage_report::StorageReportCoverageState::Partial
    {
        println!(
            "  coverage: partial; resume with cursor {}",
            report
                .coverage
                .next_cursor
                .as_deref()
                .unwrap_or("<missing>")
        );
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn handle_migrate_backup_profile(
    destination: String,
    backup_id: String,
) -> tracedecay::errors::Result<()> {
    let profile_root = tracedecay::storage::default_profile_root()?;
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| tracedecay::errors::TraceDecayError::Config {
            message: format!("system clock is before Unix epoch: {error}"),
        })?
        .as_secs()
        .try_into()
        .map_err(|_| tracedecay::errors::TraceDecayError::Config {
            message: "system clock exceeds supported backup timestamp range".to_owned(),
        })?;
    let backup = tracedecay::daemon::with_quiesced_installed_service(
        "complete profile backup",
        |lifecycle| {
            tracedecay::migrate::profile_backup::create_complete_profile_backup(
                &profile_root,
                Path::new(&destination),
                &backup_id,
                created_at,
                lifecycle,
            )
            .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })
        },
    )?;
    println!(
        "complete profile backup created and verified: {}",
        backup.display()
    );
    Ok(())
}

fn handle_migrate_rehearse_profile_backup(
    backup: String,
    restore: String,
) -> tracedecay::errors::Result<()> {
    let manifest = tracedecay::migrate::profile_backup::rehearse_complete_profile_backup(
        Path::new(&backup),
        Path::new(&restore),
    )
    .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
    println!(
        "complete profile backup rehearsed: {} entries restored to {}",
        manifest.entries.len(),
        restore
    );
    Ok(())
}
