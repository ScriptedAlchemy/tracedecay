//! Status table rendering for the CLI.
//!
//! All the formatting and layout logic for `tracedecay status` output,
//! extracted from main.rs to keep the CLI entry point focused on dispatch.

use std::fmt::Write as _;

use crate::dashboard::code_index_freshness_api::CodeIndexWorktreeFreshnessV1;
use crate::runtime_telemetry::GenerationCensusSnapshot;
use crate::timeutil::format_yyyy_mm_dd;

/// Formats a token count as a compact string (e.g. "1.2M", "45.3k").
pub fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Formats a UNIX timestamp as a human-readable relative time (e.g. "2m ago", "3d ago").
/// Returns "never" when the timestamp is 0.
pub fn format_relative_time(timestamp: u64) -> String {
    if timestamp == 0 {
        return "never".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let delta = now.saturating_sub(timestamp);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86400)
    }
}

/// Formats a byte count into a human-readable string (e.g. "798.0 MB").
pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Formats a number with comma separators (e.g. 243302 -> "243,302").
pub fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

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

/// Data for the cost row in the status header.
pub struct CostRow {
    pub today_cost: f64,
    pub week_cost: f64,
    pub efficiency_pct: f64,
}

/// Prints only the header section of the status table (version, tokens, sync times).
/// Optional branch info for the status display.
pub struct BranchInfo {
    pub branch: String,
    pub parent: Option<String>,
    pub is_fallback: bool,
}

pub fn print_status_header(
    census: &GenerationCensusSnapshot,
    freshness: Option<&CodeIndexWorktreeFreshnessV1>,
    tokens_saved: u64,
    global_tokens_saved: Option<u64>,
    worldwide: Option<u64>,
    country_flags: &[String],
    branch_info: Option<&BranchInfo>,
    cost_info: Option<&CostRow>,
) {
    let num_cols = 3;
    let cell_width = compute_cell_width(&census_cells(census));
    let inner_width = cell_width * num_cols + (num_cols - 1);

    println!("{}", table_separator('╭', '─', '╮', cell_width, num_cols));
    print_version_flags_row(country_flags, inner_width);
    print_tokens_row(tokens_saved, global_tokens_saved, worldwide, inner_width);
    if let Some(ci) = cost_info {
        print_cost_row(ci, inner_width);
    }
    print_freshness_row(freshness, inner_width);
    if let Some(bi) = branch_info {
        print_branch_row(bi, inner_width);
    }
    println!("{}", table_separator('╰', '─', '╯', cell_width, num_cols));
}

/// Inputs for rendering the compact status table.
///
/// The readings come straight from the daemon status contract: the sealed
/// generation census (`graph_statistics`) and the scheduler's freshness view
/// (`code_index_freshness`). Absent readings render as their typed reasons —
/// nothing is defaulted to zero.
#[derive(Clone, Copy)]
pub struct StatusTable<'a> {
    pub census: &'a GenerationCensusSnapshot,
    pub freshness: Option<&'a CodeIndexWorktreeFreshnessV1>,
    pub tokens_saved: u64,
    pub global_tokens_saved: Option<u64>,
    pub worldwide: Option<u64>,
    pub country_flags: &'a [String],
    pub branch_info: Option<&'a BranchInfo>,
    pub cost_info: Option<&'a CostRow>,
}

/// Prints the status output from named inputs.
pub fn print_status_table_with(table: StatusTable<'_>) {
    let StatusTable {
        census,
        freshness,
        tokens_saved,
        global_tokens_saved,
        worldwide,
        country_flags,
        branch_info,
        cost_info,
    } = table;
    let num_cols = 3;
    let census_cells = census_cells(census);
    let cell_width = compute_cell_width(&census_cells);
    let inner_width = cell_width * num_cols + (num_cols - 1);

    println!("{}", table_separator('╭', '─', '╮', cell_width, num_cols));
    print_version_flags_row(country_flags, inner_width);
    print_tokens_row(tokens_saved, global_tokens_saved, worldwide, inner_width);
    if let Some(ci) = cost_info {
        print_cost_row(ci, inner_width);
    }
    print_freshness_row(freshness, inner_width);
    if let Some(bi) = branch_info {
        print_branch_row(bi, inner_width);
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
        GenerationCensusSnapshot::Observed { statistics } => Some(vec![
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

/// Maximum cell width — caps total table width at 100 columns.
const MAX_CELL_WIDTH: usize = 32;

/// Maximum number of country flags to display in the title row.
/// Derived from `MAX_CELL_WIDTH`: available = 3*32 = 96, title ~16, gap 2 → 78 cols for flags.
/// Each flag = 3 cols (2 emoji + 1 space), first = 2 → fits 26; use 25 for margin.
const MAX_DISPLAY_FLAGS: usize = 25;

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

/// Print the top title row: version (left) + country flags (right).
/// Returns a shuffled copy of `flags` using xorshift64 seeded from time + PID.
///
/// Avoids pulling in `rand` for what is purely a cosmetic per-render shuffle.
fn shuffle_flags(flags: &[String]) -> Vec<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut out = flags.to_vec();
    if out.len() < 2 {
        return out;
    }
    let mut state: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0xdead_beef_cafe_babe, |d| d.as_nanos() as u64)
        .wrapping_add(u64::from(std::process::id()));
    if state == 0 {
        state = 0xdead_beef_cafe_babe;
    }
    for i in (1..out.len()).rev() {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        out.swap(i, j);
    }
    out
}

fn print_version_flags_row(country_flags: &[String], inner_width: usize) {
    let version = crate::version::build_version();
    let title = format!("   TraceDecay v{version}");
    let title_display_width = title.len();
    let available = inner_width.saturating_sub(2);

    if country_flags.is_empty() {
        let pad = available.saturating_sub(title_display_width);
        println!("│ {}{} │", title, " ".repeat(pad));
        return;
    }

    // Shuffle on each render so a different sample is shown when the list is
    // longer than `MAX_DISPLAY_FLAGS` and gets truncated by available width.
    let shuffled = shuffle_flags(country_flags);
    let capped = &shuffled[..shuffled.len().min(MAX_DISPLAY_FLAGS)];
    let has_overflow = shuffled.len() > MAX_DISPLAY_FLAGS;
    let mut flags_str = String::new();
    let mut display_width = 0;
    let flag_width = 2; // emoji flags are 2 columns wide
    // Reserve space for title + at least 2 spaces gap
    let max_flags_width = available.saturating_sub(title_display_width + 2);
    for (i, flag) in capped.iter().enumerate() {
        let needed = if i == 0 { flag_width } else { 1 + flag_width };
        let more_coming = has_overflow || i + 1 < capped.len();
        let reserve = if more_coming { 2 } else { 0 };
        if display_width + needed + reserve > max_flags_width {
            flags_str.push_str(" …");
            display_width += 2;
            break;
        }
        if i > 0 {
            flags_str.push(' ');
            display_width += 1;
        }
        flags_str.push_str(flag);
        display_width += flag_width;
        if i + 1 == capped.len() && has_overflow {
            flags_str.push_str(" …");
            display_width += 2;
        }
    }

    let pad = available.saturating_sub(title_display_width + display_width);
    println!("│ {}{}{} │", title, " ".repeat(pad), flags_str);
}

/// Print the second title row: token counts right-aligned in green.
fn print_tokens_row(
    tokens_saved: u64,
    global_tokens_saved: Option<u64>,
    worldwide: Option<u64>,
    inner_width: usize,
) {
    let tokens_text = {
        let mut parts = Vec::new();
        match global_tokens_saved {
            Some(global) => {
                parts.push(format!("Project ~{}", format_token_count(tokens_saved)));
                parts.push(format!(
                    "All projects ~{}",
                    format_token_count(tokens_saved + global)
                ));
            }
            None => {
                parts.push(format!("Saved ~{}", format_token_count(tokens_saved)));
            }
        }
        if let Some(ww) = worldwide {
            parts.push(format!("Worldwide ~{}", format_token_count(ww)));
        }
        parts.join("  ")
    };
    let available = inner_width.saturating_sub(2);
    let pad = available.saturating_sub(tokens_text.len());
    println!("│ {}\x1b[32m{}\x1b[0m │", " ".repeat(pad), tokens_text);
}

/// Print the third title row: the scheduler's freshness view, right-aligned
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
            if let Some(state) = freshness.staleness_state.as_deref() {
                parts.push(state.to_owned());
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

/// Print the cost summary row: today's cost, 7-day cost, efficiency ratio.
fn print_cost_row(cost_info: &CostRow, inner_width: usize) {
    let mut parts = Vec::new();
    if cost_info.today_cost >= 0.001 {
        parts.push(format!("Today ${:.2}", cost_info.today_cost));
    }
    if cost_info.week_cost >= 0.001 {
        parts.push(format!("7d ${:.2}", cost_info.week_cost));
    }
    if cost_info.efficiency_pct > 0.0 {
        parts.push(format!("Efficiency {:.0}%", cost_info.efficiency_pct));
    }
    if parts.is_empty() {
        return;
    }
    let text = parts.join("  ");
    let available = inner_width.saturating_sub(2);
    let pad = available.saturating_sub(text.len());
    println!("│ {}\x1b[36m{}\x1b[0m │", " ".repeat(pad), text);
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
    rows: &[crate::global_db::SavingsDay],
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
