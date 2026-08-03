use super::{HealthPassReport, HealthPassWarning};

pub(super) fn render_missing_profile_report() -> HealthPassReport {
    let report = HealthPassReport {
        warnings: vec![HealthPassWarning::new(
            "could not determine the profile data directory",
        )],
        ..HealthPassReport::default()
    };
    render_warnings(&report.warnings);
    report
}

/// Prints the doctor-style summary for a computed report.
pub(super) fn render_health_pass_report(report: &HealthPassReport) {
    if report.quarantined_branch_meta.is_empty() {
        eprintln!("  \x1b[32m✔\x1b[0m No corrupt branch metadata files");
    } else {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Quarantined {} corrupt branch metadata file(s):",
            report.quarantined_branch_meta.len()
        );
        for quarantine in &report.quarantined_branch_meta {
            eprintln!("      • {}", quarantine.quarantined.display());
        }
    }

    match report.purged_temp_registry_rows {
        Some(0) => eprintln!("  \x1b[32m✔\x1b[0m No stale temp-root registry rows"),
        Some(purged) => {
            eprintln!("  \x1b[32m✔\x1b[0m Purged {purged} stale temp-root registry row(s)");
        }
        None => {}
    }

    if report.reconciled_store_roots.is_empty() {
        eprintln!("  \x1b[32m✔\x1b[0m No stale store manifest roots to reconcile");
    } else {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Reconciled {} stale store manifest root(s):",
            report.reconciled_store_roots.len()
        );
        for reconciled in &report.reconciled_store_roots {
            eprintln!("      • {}", reconciled.manifest_path.display());
            if let Some(config_path) = &reconciled.config_path {
                eprintln!("        (config: {})", config_path.display());
            }
        }
    }

    if report.remaining_findings.is_empty() {
        eprintln!("  \x1b[32m✔\x1b[0m No remaining doctor findings");
    } else {
        eprintln!("  Remaining findings (not auto-fixed — run `tracedecay doctor` for details):");
        for finding in &report.remaining_findings {
            eprintln!("      • {finding}");
        }
    }
    render_warnings(&report.warnings);
}

pub(super) fn render_warnings(warnings: &[HealthPassWarning]) {
    for warning in warnings {
        eprintln!("  \x1b[33mwarning:\x1b[0m health pass: {warning}");
    }
}
