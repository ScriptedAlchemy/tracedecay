//! Per-file and overall index-staleness banner logic.

use crate::path_tree::format_compact_annotated_path_list;

/// Render a duration in seconds as a compact phrase: `"5s ago"`,
/// `"3m ago"`, `"2h ago"`, `"4d ago"`. Used in the staleness banner so
/// the agent can judge how stale "still stale" actually is.
pub(crate) fn humanize_age(secs: i64) -> String {
    if secs < 60 {
        format!("{}s ago", secs.max(0))
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Build the per-file staleness banner inserted at the top of any tool
/// response that referenced files newer than the sealed graph generation.
///
/// The shape mimics codegraph's #428 banner: name each pending file with
/// its edit age (how long since the on-disk mtime), and direct the agent
/// to `Read` those specific files. The rest of the response is treated
/// as authoritative — distinct from the previous binary "STALE INDEX"
/// warning that asked the agent to distrust the whole answer.
pub(crate) fn format_per_file_staleness_banner(
    project_root: &std::path::Path,
    stale_files: &[String],
) -> String {
    let now_secs = crate::tracedecay::current_timestamp();

    let mut lines = Vec::with_capacity(stale_files.len() + 2);
    lines.push(format!(
        "WARNING: {} file(s) referenced below were edited after the last sync. \
         Read these directly; the rest of this response reflects the current index:",
        stale_files.len()
    ));
    let annotated_paths = stale_files
        .iter()
        .map(|path| {
            let age = file_mtime_secs(project_root, path).map_or(0, |m| now_secs.saturating_sub(m));
            (path.as_str(), format!(" (edited {})", humanize_age(age)))
        })
        .collect::<Vec<_>>();
    let path_list = format_compact_annotated_path_list(annotated_paths, "  - ", "  ");
    if !path_list.is_empty() {
        lines.push(path_list);
    }
    lines.push("Run `tracedecay sync` to refresh the index.".to_string());
    lines.join("\n")
}

/// Read the on-disk mtime (UNIX seconds) for `relative_path` joined onto
/// `project_root`. Returns `None` when the file is missing or stat fails.
fn file_mtime_secs(project_root: &std::path::Path, relative_path: &str) -> Option<i64> {
    let abs = project_root.join(relative_path);
    let meta = std::fs::metadata(&abs).ok()?;
    let modified = meta.modified().ok()?;
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(secs)
}

/// Inputs to the overall-staleness banner decision, factored out so the
/// branch logic is unit-testable without a live server.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StalenessBannerInputs {
    pub(crate) age_secs: i64,
    /// Whether canonical edit-event watching is enabled.
    pub(crate) auto_sync_on: bool,
    /// Serving a read-only fallback/ancestor store (`fallback_warning().is_some()`).
    pub(crate) fallback_store: bool,
}

/// Format the index-age phrase used by the overall-staleness banner,
/// preserving the pre-existing `"Xd Yh"` / `"Xh Ym"` shape. `age_secs` is
/// assumed `> 3600` (the banner's guard); shorter ages still format sensibly.
pub(crate) fn format_index_age_phrase(age_secs: i64) -> String {
    let hours = age_secs / 3600;
    let mins = (age_secs % 3600) / 60;
    if hours >= 24 {
        format!("{}d {}h", hours / 24, hours % 24)
    } else {
        format!("{hours}h {mins}m")
    }
}

/// Decide the overall-staleness banner. The age guard (`> 3600s`) is applied
/// by the caller.
///
/// Rules:
/// - Canonical edit watching on and not a fallback store: explain the sealed
///   generation without claiming an unobserved refresh was scheduled.
/// - Auto-repair impossible (fallback store, or auto-sync fully disabled):
///   fall back to the manual `tracedecay sync` instruction.
pub(crate) fn staleness_banner(inputs: StalenessBannerInputs) -> String {
    let age_phrase = format_index_age_phrase(inputs.age_secs);
    if inputs.auto_sync_on && !inputs.fallback_store {
        format!(
            "Note: index last synced {age_phrase} ago; automatic edit tracking is enabled, \
             but very recent edits may not appear in this sealed generation."
        )
    } else {
        format!(
            "WARNING: Index last synced {age_phrase} ago. \
             Run `tracedecay sync` to update."
        )
    }
}
