//! `tracedecay monitor`: the live token-savings TUI over the global mmap ring.

use std::io::Write;

use tracedecay_runtime_core::monitor_ring::{
    FILE_SIZE, LOCK_FILENAME, MMAP_FILENAME, MmapReader, MonitorEntry, RING_CAPACITY,
};
use tracedecay_runtime_core::text::format_number;

mod cost;

use cost::{CostCache, CostCacheState};

/// Run the monitor TUI. Blocks until Ctrl+C.
pub fn run() -> std::io::Result<()> {
    use crossterm::{
        cursor, execute, terminal,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen},
    };
    let dir = tracedecay_runtime_core::config::user_data_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve home directory",
        )
    })?;
    std::fs::create_dir_all(&dir)?;

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

    let mut reader = MmapReader::open()?;
    let mut last_idx = reader.write_idx();
    let mut entries: Vec<MonitorEntry> = Vec::new();
    let mut recent_updates: Vec<(String, String)> = Vec::new();

    // Populate with existing entries in the ring buffer (up to write_idx).
    let populated = last_idx.min(RING_CAPACITY as u64) as usize;
    if populated > 0 {
        let start_slot = if last_idx > RING_CAPACITY as u64 {
            (last_idx as usize) % RING_CAPACITY
        } else {
            0
        };
        for i in 0..populated {
            let slot = (start_slot + i) % RING_CAPACITY;
            if let Some(e) = reader.entry(slot)
                && e.delta > 0
            {
                push_recent_update(&mut recent_updates, &e.project, &e.tool_name);
                entries.push(e);
            }
        }
    }

    let mut stdout = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;

    let result = monitor_loop(
        &mut reader,
        &mut entries,
        &mut recent_updates,
        &mut last_idx,
        &mut stdout,
    );

    execute!(stdout, cursor::Show, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;

    let _ = lock_file.unlock();
    let _ = std::fs::remove_file(&lock_path);

    result
}

fn monitor_loop(
    reader: &mut MmapReader,
    entries: &mut Vec<MonitorEntry>,
    recent_updates: &mut Vec<(String, String)>,
    last_idx: &mut u64,
    stdout: &mut std::io::Stdout,
) -> std::io::Result<()> {
    use crossterm::{cursor, event, execute, terminal};
    use std::collections::HashMap;

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
                    entries.clear();
                    recent_updates.clear();
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
        let current_idx = reader.write_idx();
        if current_idx > *last_idx {
            for i in *last_idx..current_idx {
                let slot = (i as usize) % RING_CAPACITY;
                if let Some(e) = reader.entry(slot) {
                    push_recent_update(recent_updates, &e.project, &e.tool_name);
                    entries.push(e);
                }
            }
            *last_idx = current_idx;
        }

        cost_cache.poll_refresh();
        if cost_cache.is_stale() {
            cost_cache.begin_refresh();
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
        let mut grouped: HashMap<String, HashMap<String, u64>> = HashMap::new();
        for entry in entries.iter() {
            let project = &entry.project;
            let method = &entry.tool_name;
            *grouped
                .entry(project.clone())
                .or_default()
                .entry(method.clone())
                .or_default() += entry.delta;
        }

        let mut projects: Vec<String> = grouped
            .keys()
            .filter(|p| !is_temp_dir_name(p) && !p.is_empty())
            .cloned()
            .collect();
        projects.sort();

        // Each line carries an optional ANSI color prefix; padding is computed
        // from the plain text length so escape bytes don't affect alignment.
        let mut all_lines: Vec<(&'static str, String)> = Vec::new();
        let mut grand_total: u64 = 0;

        for project in &projects {
            let Some(methods) = grouped.get(project) else {
                continue;
            };
            let mut method_lines: Vec<String> = methods.keys().cloned().collect();
            method_lines.sort();

            let project_total: u64 = methods.values().sum::<u64>();
            grand_total += project_total;

            all_lines.push((
                "",
                format!("{} ({})", project, format_number(project_total)),
            ));
            for method in &method_lines {
                let delta = *methods.get(method).unwrap_or(&0);
                let color = update_color_for(recent_updates, project, method);
                all_lines.push((color, format!("  {}  {}", method, format_number(delta))));
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
        let total_saved: u64 = entries.iter().map(|e| e.delta).sum();
        let total_str = format_number(total_saved);
        let label = "TraceDecay Monitor";
        let suffix = "saved tokens";
        let footer_content = format!("{label}  {total_str} {suffix}");
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
    use super::{push_recent_update, update_color_for};

    #[test]
    fn push_recent_update_keeps_three_most_recent() {
        let mut recent: Vec<(String, String)> = Vec::new();
        push_recent_update(&mut recent, "proj", "tool_a");
        push_recent_update(&mut recent, "proj", "tool_b");
        push_recent_update(&mut recent, "proj", "tool_c");
        push_recent_update(&mut recent, "proj", "tool_d");
        assert_eq!(recent.len(), 3);
        // Most recent first.
        assert_eq!(recent[0].1, "tool_d");
        assert_eq!(recent[1].1, "tool_c");
        assert_eq!(recent[2].1, "tool_b");
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
