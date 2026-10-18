//! Status table rendering for the CLI.
//!
//! All the formatting and layout logic for `tracedecay status` output,
//! extracted from main.rs to keep the CLI entry point focused on dispatch.

use std::fmt::Write as _;

use tracedecay_contracts::code_index_freshness::CodeIndexWorktreeFreshnessV1;
use tracedecay_runtime_core::runtime_telemetry::GenerationCensusSnapshot;
use tracedecay_runtime_core::text::{
    format_bytes, format_number, format_relative_time, format_token_count,
};
use tracedecay_runtime_core::timeutil::format_yyyy_mm_dd;

/// Formats a single table cell with left-aligned label and right-aligned value.
fn format_cell(label: &str, value: &str, width: usize) -> String {
    let content_len = label.len() + value.len();
    let pad = width.saturating_sub(2 + content_len);
    format!(" {}{}{} ", label, " ".repeat(pad), value)
}

/// Builds a horizontal separator line (e.g. ├──┬──┤).
fn table_separator(
    left: char,
    mid: char,
    right: char,
    cell_width: usize,
    num_cols: usize,
) -> String {
    let mut line = String::from(left);
    for i in 0..num_cols {
        line.push_str(&"─".repeat(cell_width));
        line.push(if i < num_cols - 1 { mid } else { right });
    }
    line
}

/// Optional branch info for the status display.
pub struct BranchInfo {
    pub branch: String,
    pub parent: Option<String>,
    pub is_fallback: bool,
}

/// Prints the compact status table; `header_only` stops after the version,
/// freshness, and branch rows.
///
/// The readings come straight from the daemon status contract: the sealed
/// generation census (`graph_statistics`) and the scheduler's freshness view
/// (`code_index_freshness`). Absent readings render as their typed reasons.
/// nothing is defaulted to zero.
pub fn print_status_table(
    census: &GenerationCensusSnapshot,
    freshness: Option<&CodeIndexWorktreeFreshnessV1>,
    branch_info: Option<&BranchInfo>,
    header_only: bool,
) {
    let num_cols = 3;
    let census_cells = census_cells(census);
    let cell_width = compute_cell_width(&census_cells);
    let inner_width = cell_width * num_cols + (num_cols - 1);

    println!("{}", table_separator('╭', '─', '╮', cell_width, num_cols));
    print_version_row(inner_width);
    print_freshness_row(freshness, inner_width);
    if let Some(bi) = branch_info {
        print_branch_row(bi, inner_width);
    }
    if header_only {
        println!("{}", table_separator('╰', '─', '╯', cell_width, num_cols));
        return;
    }
    match census_cells {
        Some(cells) => {
            println!("{}", table_separator('├', '┬', '┤', cell_width, num_cols));
            print_table_rows(&[cells], cell_width, num_cols);
            println!("{}", table_separator('╰', '┴', '╯', cell_width, num_cols));
        }
        None => {
            // The census is a typed absence: print its reason rather than a
            // row of fabricated zero counts.
            let line = census_absence_line(census);
            let available = inner_width.saturating_sub(2);
            let pad = available.saturating_sub(line.len());
            println!("│ \x1b[2m{}\x1b[0m{} │", line, " ".repeat(pad));
            println!("{}", table_separator('╰', '─', '╯', cell_width, num_cols));
        }
    }
}

/// The three census cells, or `None` when the census is a typed absence.
fn census_cells(census: &GenerationCensusSnapshot) -> Option<Vec<(&'static str, String)>> {
    match census {
        GenerationCensusSnapshot::Observed { statistics, .. } => Some(vec![
            ("Symbols", format_number(statistics.symbol_count)),
            ("Edges", format_number(statistics.edge_count)),
            ("Source", format_bytes(statistics.source_total_bytes)),
        ]),
        GenerationCensusSnapshot::Unavailable { .. } => None,
    }
}

fn census_absence_line(census: &GenerationCensusSnapshot) -> String {
    match census {
        GenerationCensusSnapshot::Observed { .. } => String::new(),
        GenerationCensusSnapshot::Unavailable { reason } => {
            format!("graph census unavailable: {}", reason.as_str())
        }
    }
}

/// Maximum cell width, caps total table width at 100 columns.
const MAX_CELL_WIDTH: usize = 32;

/// Compute cell width from the widest census cell, capped at `MAX_CELL_WIDTH`.
fn compute_cell_width(cells: &Option<Vec<(&'static str, String)>>) -> usize {
    let widest = cells
        .iter()
        .flatten()
        .map(|(label, value)| label.len() + value.len())
        .max()
        .unwrap_or(15);
    (widest + 3).clamp(22, MAX_CELL_WIDTH)
}

/// Print the top title row: the running version.
fn print_version_row(inner_width: usize) {
    let version = crate::product_runtime::PRODUCT_BUILD_VERSION;
    let title = format!("   TraceDecay v{version}");
    let pad = inner_width.saturating_sub(2).saturating_sub(title.len());
    println!("│ {}{} │", title, " ".repeat(pad));
}

/// Print the freshness row: the scheduler's freshness view, right-aligned
/// in dim. An absent reading names itself instead of rendering "never".
fn print_freshness_row(freshness: Option<&CodeIndexWorktreeFreshnessV1>, inner_width: usize) {
    let sync_text = match freshness {
        Some(freshness) => {
            let mut parts = Vec::new();
            if let Some(reconciled) = freshness.last_reconcile_micros {
                parts.push(format!(
                    "Reconciled {}",
                    format_relative_time(unix_seconds_from_micros(reconciled))
                ));
            }
            if let Some(sealed) = freshness.sealed_at_micros {
                parts.push(format!(
                    "Sealed {}",
                    format_relative_time(unix_seconds_from_micros(sealed))
                ));
            }
            if let Some(state) = freshness.staleness_state {
                parts.push(state.as_str().to_owned());
            }
            if let Some(restore) = freshness.restore_progress.as_ref() {
                parts.push(format!(
                    "{} / {} checks, {} remaining",
                    restore.authenticated_completed,
                    restore.authenticated_total,
                    restore.authenticated_remaining
                ));
            }
            if parts.is_empty() {
                "no sealed generation yet".to_owned()
            } else {
                parts.join("  ")
            }
        }
        None => "code index freshness unavailable".to_owned(),
    };
    let available = inner_width.saturating_sub(2);
    let pad = available.saturating_sub(sync_text.len());
    println!("│ {}\x1b[2m{}\x1b[0m │", " ".repeat(pad), sync_text);
}

/// Whole seconds since the Unix epoch from a non-negative microsecond stamp.
fn unix_seconds_from_micros(micros: i64) -> u64 {
    u64::try_from(micros / 1_000_000).unwrap_or(0)
}

fn print_branch_row(info: &BranchInfo, inner_width: usize) {
    let mut text = format!("Branch: {}", info.branch);
    if let Some(ref parent) = info.parent {
        let _ = write!(text, "  (from {parent})");
    }
    if info.is_fallback {
        text.push_str("  \x1b[33m[fallback]\x1b[0m");
    }
    let available = inner_width.saturating_sub(2);
    // Strip ANSI for length calculation
    let visible_len = text.replace("\x1b[33m", "").replace("\x1b[0m", "").len();
    let pad = available.saturating_sub(visible_len);
    println!("│ {}{} │", " ".repeat(pad), text);
}

/// Print rows of label-value pairs in a bordered table.
fn print_table_rows(rows: &[Vec<(&str, String)>], cell_width: usize, num_cols: usize) {
    for row in rows {
        print!("│");
        for (i, (label, value)) in row.iter().enumerate() {
            if label.is_empty() {
                print!("{}", " ".repeat(cell_width));
            } else {
                print!("{}", format_cell(label, value, cell_width));
            }
            print!("{}", if i < num_cols - 1 { "│" } else { "│\n" });
        }
    }
}

pub fn print_gain_total(
    project: &str,
    range: &str,
    saved_tokens: u64,
    calls: u64,
    usd: Option<f64>,
) {
    let saved_str = format_token_count(saved_tokens);
    println!("  {:<28} {:>12}", "Scope", project);
    println!("  {:<28} {:>12}", "Range", range);
    println!("  {:<28} {:>12}", "Tool calls", calls);
    println!("  {:<28} {:>12}", "Tokens saved", saved_str);
    println!(
        "  {:<28} {:>12}",
        "USD saved (Sonnet input)",
        usd.map_or_else(|| "unavailable".to_owned(), |value| format!("${value:.2}"))
    );
}

pub fn print_gain_history<F: Fn(u64) -> Option<f64>>(
    rows: &[tracedecay_global_db::SavingsDay],
    to_usd: F,
) {
    println!(
        "  {:<12} {:>10} {:>8} {:>10}",
        "Day (UTC)", "Tokens", "Calls", "USD"
    );
    for r in rows {
        let days_since_epoch = r.day / 86_400;
        let date = format_yyyy_mm_dd(days_since_epoch);
        let saved_str = format_token_count(r.saved_tokens);
        let usd = to_usd(r.saved_tokens);
        println!(
            "  {:<12} {:>10} {:>8} {:>10}",
            date,
            saved_str,
            r.calls,
            usd.map_or_else(|| "unavailable".to_owned(), |value| format!("${value:.2}"))
        );
    }
}
