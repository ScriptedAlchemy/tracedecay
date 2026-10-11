//! `tracedecay monitor`: the live token-savings TUI over the global mmap ring.

use std::collections::BTreeMap;
use std::io::Write;
use tracedecay_runtime_core::config::ProfileRoot;

use tracedecay_runtime_core::text::format_number;
use tracedecay_session_memory::monitor_ring::{
    FILE_SIZE, LOCK_FILENAME, MMAP_FILENAME, MmapReader, RingCursor, RingDrain,
};

mod cost;

use cost::{CostCache, CostCacheState};

/// Tool calls the monitor has read from the ring since it started or was
/// reset. Totals cover observed calls only; the ring cannot replay calls
/// overwritten before a read, so those are counted in `missed`.
#[derive(Default)]
struct Tally {
    /// Saved tokens per project, then per tool.
    by_project: BTreeMap<String, BTreeMap<String, u64>>,
    saved: u64,
    missed: u64,
    recent: Vec<(String, String)>,
}

impl Tally {
    fn record(&mut self, drain: RingDrain) {
        self.missed += drain.dropped;
        for entry in drain.entries {
            push_recent_update(&mut self.recent, &entry.project, &entry.tool_name);
            self.saved += entry.delta;
            *self
                .by_project
                .entry(entry.project)
                .or_default()
                .entry(entry.tool_name)
                .or_default() += entry.delta;
        }
    }
}

/// Run the monitor TUI. Blocks until Ctrl+C.
pub fn run(profile: &ProfileRoot) -> std::io::Result<()> {
    use crossterm::{
        cursor, execute, terminal,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen},
    };
    let dir = profile.data_dir();
    std::fs::create_dir_all(dir)?;

    // Single-instance lock, held for the lifetime of `run` (shared sidecar
    // helper; see the sidecar-lock module note in runtime-core `storage`).
    let lock_path = dir.join(LOCK_FILENAME);
    let Some(lock_file) = tracedecay_runtime_core::storage::try_acquire_sidecar_lock(&lock_path)?
    else {
        eprintln!("Monitor already running.");
        return Ok(());
    };

    let mmap_path = dir.join(MMAP_FILENAME);
    if !mmap_path.exists() {
        let f = std::fs::File::create(&mmap_path)?;
        f.set_len(FILE_SIZE as u64)?;
    }

    let mut reader = MmapReader::open_at(profile.data_dir())?;
    let mut ring_cursor = RingCursor::default();
    let mut tally = Tally::default();
    // Entries overwritten before the monitor started are history, not loss.
    tally.record(RingDrain {
        dropped: 0,
        ..reader.drain(&mut ring_cursor)
    });

    let mut stdout = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;

    let result = monitor_loop(
        profile,
        &mut reader,
        &mut ring_cursor,
        &mut tally,
        &mut stdout,
    );

    execute!(stdout, cursor::Show, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;

    drop(lock_file);
    let _ = std::fs::remove_file(&lock_path);

    result
}

fn monitor_loop(
    profile: &ProfileRoot,
    reader: &mut MmapReader,
    ring_cursor: &mut RingCursor,
    tally: &mut Tally,
    stdout: &mut std::io::Stdout,
) -> std::io::Result<()> {
    use crossterm::{cursor, event, execute, terminal};

    let mut cost_cache = CostCache::new();
    let mut scroll_offset: usize = 0;
    let mut last_log_lines: usize = 20;

    loop {
        // Poll for key events (100ms timeout = our refresh rate).
        if event::poll(std::time::Duration::from_millis(100))?
            && let event::Event::Key(key) = event::read()?
        {
            match key.code {
                event::KeyCode::Char('c')
                    if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                {
                    break;
                }
                event::KeyCode::Char('r')
                    if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                {
                    *tally = Tally::default();
                    scroll_offset = 0;
                }
                event::KeyCode::Up => {
                    scroll_offset = scroll_offset.saturating_add(1);
                }
                event::KeyCode::Down => {
                    scroll_offset = scroll_offset.saturating_sub(1);
                }
                event::KeyCode::PageUp => {
                    scroll_offset = scroll_offset.saturating_add(last_log_lines.max(1));
                }
                event::KeyCode::PageDown => {
                    scroll_offset = scroll_offset.saturating_sub(last_log_lines.max(1));
                }
                _ => {}
            }
        }

        let _ = reader.refresh();
        tally.record(reader.drain(ring_cursor));

        cost_cache.poll_refresh();
        if cost_cache.is_stale() {
            cost_cache.begin_refresh(profile);
        }

        let (width, height) = terminal::size().unwrap_or((80, 24));
        let w = width as usize;
        let h = height as usize;

        execute!(stdout, cursor::MoveTo(0, 0))?;

        // Layout: optional cost/status panel + separator + log + footer.
        let mut cost_panel = Vec::new();
        if let Some(snapshot) = cost_cache
            .snapshot
            .as_ref()
            .filter(|snapshot| snapshot.today_cost >= 0.001 || snapshot.week_cost >= 0.001)
        {
            let saved_str =
                tracedecay_runtime_core::text::format_token_count(snapshot.tokens_saved);
            cost_panel.push(format!(
                "  Spent: ${:.2} today | ${:.2} 7d    Saved: {}",
                snapshot.today_cost, snapshot.week_cost, saved_str
            ));
            match (&snapshot.top_model, snapshot.top_model_cost) {
                (Some(model), Some(cost)) => cost_panel.push(format!(
                    "  Efficiency: {:.0}%    Top model: {model} (${cost:.2})",
                    snapshot.efficiency_pct
                )),
                _ => cost_panel.push(format!(
                    "  Efficiency: {:.0}%    Top model: unavailable",
                    snapshot.efficiency_pct
                )),
            }
        }
        match &cost_cache.state {
            CostCacheState::Fresh => {}
            CostCacheState::Stale(error) => {
                cost_panel.push(format!("  Cost accounting stale: {error}"));
            }
            CostCacheState::Unavailable(error) => {
                cost_panel.push(format!("  Cost accounting unavailable: {error}"));
            }
        }
        let cost_lines = if cost_panel.is_empty() {
            0
        } else {
            cost_panel.len() + 1
        };
        let footer_lines = 4; // separator + 2 footer lines + bottom separator
        let log_lines = h.saturating_sub(cost_lines + footer_lines).max(1);
        last_log_lines = log_lines;

        // ── Cost panel ──
        if !cost_panel.is_empty() {
            let sep = "\u{2500}".repeat(w);
            for line in &cost_panel {
                write!(
                    stdout,
                    "\r\x1b[36m{}\x1b[0m{}\r\n",
                    line,
                    " ".repeat(w.saturating_sub(line.len()))
                )?;
            }
            write!(stdout, "\r{sep}\r\n")?;
        }

        // ── Grouped log entries ──
        // Each line carries an optional ANSI color prefix; padding is computed
        // from the plain text length so escape bytes don't affect alignment.
        let mut all_lines: Vec<(&'static str, String)> = Vec::new();
        let mut grand_total: u64 = 0;

        for (project, methods) in tally
            .by_project
            .iter()
            .filter(|(p, _)| !is_temp_dir_name(p) && !p.is_empty())
        {
            let project_total: u64 = methods.values().sum::<u64>();
            grand_total += project_total;

            all_lines.push((
                "",
                format!("{} ({})", project, format_number(project_total)),
            ));
            for (method, delta) in methods {
                let color = update_color_for(&tally.recent, project, method);
                all_lines.push((color, format!("  {}  {}", method, format_number(*delta))));
            }
        }
        all_lines.push(("", format!("TOTAL  {}", format_number(grand_total))));

        let max_offset = all_lines.len().saturating_sub(log_lines);
        if scroll_offset > max_offset {
            scroll_offset = max_offset;
        }

        let total = all_lines.len();
        let end = total.saturating_sub(scroll_offset);
        let start = end.saturating_sub(log_lines);
        let visible_lines = &all_lines[start..end];
        let blank_lines = log_lines.saturating_sub(visible_lines.len());

        for _ in 0..blank_lines {
            write!(stdout, "\r{}\r\n", " ".repeat(w))?;
        }

        for (color, line) in visible_lines {
            let padding = w.saturating_sub(line.len());
            if color.is_empty() {
                write!(stdout, "\r{}{}\r\n", line, " ".repeat(padding))?;
            } else {
                write!(
                    stdout,
                    "\r{}{}\x1b[0m{}\r\n",
                    color,
                    line,
                    " ".repeat(padding)
                )?;
            }
        }

        // ── Footer ──
        let sep = "\u{2500}".repeat(w);
        let mut footer_content = format!(
            "TraceDecay Monitor  {} saved tokens observed",
            format_number(tally.saved)
        );
        if tally.missed > 0 {
            footer_content.push_str(&format!(", {} calls missed", format_number(tally.missed)));
        }
        let footer_padding = w.saturating_sub(footer_content.len());
        let hint = "\u{2191}\u{2193}/PgUp/PgDn scroll | Ctrl+R reset | Ctrl+C quit";
        let hint_padding = w.saturating_sub(hint.len());

        write!(stdout, "\r{sep}\r\n")?;
        write!(
            stdout,
            "\r{}{}\r\n",
            " ".repeat(footer_padding),
            footer_content
        )?;
        write!(stdout, "\r{}{}\r\n", " ".repeat(hint_padding), hint)?;
        write!(stdout, "\r{sep}")?;

        stdout.flush()?;
    }
    Ok(())
}

fn is_temp_dir_name(name: &str) -> bool {
    name.starts_with(".tmp") && name.len() > 4
}

/// Push a (project, `tool_name`) pair onto the front of the recent-updates list.
/// If the pair is already present, it is moved to the front (no duplicates).
/// The list is truncated to the three most recent distinct pairs.
fn push_recent_update(recent: &mut Vec<(String, String)>, project: &str, tool_name: &str) {
    recent.retain(|(p, t)| !(p == project && t == tool_name));
    recent.insert(0, (project.to_string(), tool_name.to_string()));
    recent.truncate(3);
}

/// Return the ANSI color prefix for a method line based on its recency.
/// Latest = green, 2nd latest = orange, 3rd latest = yellow, else no color.
fn update_color_for(recent: &[(String, String)], project: &str, tool_name: &str) -> &'static str {
    match recent
        .iter()
        .position(|(p, t)| p == project && t == tool_name)
    {
        Some(0) => "\x1b[32m",       // green: latest
        Some(1) => "\x1b[38;5;208m", // orange: 2nd latest
        Some(2) => "\x1b[33m",       // yellow: 3rd latest
        _ => "",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{Tally, push_recent_update, update_color_for};
    use tracedecay_session_memory::monitor_ring::{MonitorEntry, RingDrain};

    fn call(project: &str, tool_name: &str, delta: u64) -> MonitorEntry {
        MonitorEntry {
            prefix: "tracedecay".to_string(),
            project: project.to_string(),
            tool_name: tool_name.to_string(),
            delta,
            before: 0,
            timestamp: 0,
        }
    }

    #[test]
    fn tally_accumulates_observed_calls_and_missed_counts_across_drains() {
        let mut tally = Tally::default();
        tally.record(RingDrain {
            entries: vec![call("alpha", "search", 10), call("alpha", "context", 5)],
            dropped: 0,
        });
        tally.record(RingDrain {
            entries: vec![call("alpha", "search", 7), call("bravo", "search", 1)],
            dropped: 44,
        });
        tally.record(RingDrain::default());

        assert_eq!((tally.saved, tally.missed), (23, 44));
        assert_eq!(tally.by_project["alpha"]["search"], 17);
        assert_eq!(tally.by_project["alpha"]["context"], 5);
        assert_eq!(tally.by_project["bravo"]["search"], 1);
        assert_eq!(tally.recent[0], ("bravo".to_string(), "search".to_string()));
    }

    #[test]
    fn push_recent_update_dedups_and_bumps_to_front() {
        let mut recent: Vec<(String, String)> = Vec::new();
        push_recent_update(&mut recent, "proj", "tool_a");
        push_recent_update(&mut recent, "proj", "tool_b");
        push_recent_update(&mut recent, "proj", "tool_a"); // already present
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].1, "tool_a");
        assert_eq!(recent[1].1, "tool_b");
    }

    #[test]
    fn update_color_for_returns_three_colors_by_position() {
        let recent = vec![
            ("p".to_string(), "newest".to_string()),
            ("p".to_string(), "mid".to_string()),
            ("p".to_string(), "oldest".to_string()),
        ];
        assert_eq!(update_color_for(&recent, "p", "newest"), "\x1b[32m");
        assert_eq!(update_color_for(&recent, "p", "mid"), "\x1b[38;5;208m");
        assert_eq!(update_color_for(&recent, "p", "oldest"), "\x1b[33m");
        assert_eq!(update_color_for(&recent, "p", "other"), "");
        assert_eq!(update_color_for(&recent, "other_proj", "newest"), "");
    }
}
