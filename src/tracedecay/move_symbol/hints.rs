//! Non-caller impact hints for `move_symbol`: the collected dependency
//! findings, a `#[cfg(...)]`-gate warning when the moved span carries one,
//! and a heuristic warning when moving a symbol risks forming a two-way
//! module dependency with the destination.

use crate::types::MoveHint;

use super::use_parsing::parse_use_statements;

/// Collected dependency findings for a move.
#[derive(Default)]
pub(super) struct DependencyAnalysis {
    /// `use` lines to auto-insert at the destination (unambiguous, visible).
    pub(super) auto_imports: Vec<String>,
    /// Single-leaf source `use` lines only the moved body needed; the move
    /// removes them from the source so it does not leave dead imports behind.
    pub(super) orphaned_source_imports: Vec<String>,
    /// Findings that need caller attention.
    pub(super) hints: Vec<MoveHint>,
}

/// Hints when the moved span carries a `#[cfg(...)]` gate the destination might
/// not satisfy.
pub(super) fn cfg_context_hints(moved_text: &str, dest_rel: &str) -> Vec<MoveHint> {
    let mut out = Vec::new();
    for (idx, raw) in moved_text.lines().enumerate() {
        let t = raw.trim();
        if t.starts_with("#[cfg(") || t.starts_with("#[cfg_attr(") {
            // The gate's offset is within the moved snippet, not a real line in
            // the destination file — reporting it as `line` (which every other
            // hint uses for a concrete file site) points at nothing. Leave `line`
            // unset and describe the moved-span offset in the detail instead.
            out.push(MoveHint {
                kind: "cfg_context".to_string(),
                file: dest_rel.to_string(),
                line: None,
                detail: format!(
                    "moved item is gated by `{t}` (at line {} of the moved span)",
                    idx + 1
                ),
                suggestion: Some(
                    "confirm the destination module builds under the same cfg".to_string(),
                ),
            });
            break;
        }
    }
    out
}

/// File-level cycle-risk heuristic: if the destination already imports from the
/// source module, moving a symbol here — while the source module's former call
/// sites will now import it back from the destination — can form a two-way
/// module dependency. Evidence is the destination's own `use` lines. Skipped
/// when the source lives at the crate root (`crate`), where the signal is noise.
pub(super) fn cycle_risk_hints(
    dest_original: &str,
    dest_rel: &str,
    src_module: Option<&str>,
) -> Vec<MoveHint> {
    let Some(src_module) = src_module else {
        return Vec::new();
    };
    if src_module == "crate" {
        return Vec::new();
    }
    for stmt in parse_use_statements(dest_original) {
        if use_targets_module(&stmt.text, src_module) {
            return vec![MoveHint {
                kind: "cycle_risk".to_string(),
                file: dest_rel.to_string(),
                line: Some(stmt.line),
                detail: format!(
                    "destination already imports from `{src_module}` (`{}`); moving a symbol here while the source module imports it back can form a module dependency cycle",
                    stmt.text.trim()
                ),
                suggestion: Some(format!(
                    "verify `{src_module}` and this module do not end up importing each other; consider a shared leaf module"
                )),
            }];
        }
    }
    Vec::new()
}

/// True when a `use` statement's path begins at `module` (exact module or a
/// `module::…` descendant), respecting `::` boundaries.
fn use_targets_module(use_stmt: &str, module: &str) -> bool {
    let line = use_stmt.trim();
    let Some(after) = line
        .strip_prefix("pub use ")
        .or_else(|| line.strip_prefix("use "))
        .map(str::trim)
    else {
        return false;
    };
    let after = after.strip_suffix(';').unwrap_or(after).trim_start();
    match after.strip_prefix(module) {
        Some(rest) => rest.is_empty() || rest.trim_start().starts_with("::"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{cfg_context_hints, cycle_risk_hints};

    #[test]
    fn cfg_context_hint_reports_moved_span_offset_not_dest_line() {
        let moved = "#[cfg(feature = \"x\")]\npub fn gated() {}\n";
        let hints = cfg_context_hints(moved, "src/dest.rs");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].kind, "cfg_context");
        // `line` must be unset — the offset is within the snippet, not a real
        // destination line.
        assert_eq!(hints[0].line, None, "hint: {:?}", hints[0]);
        assert!(
            hints[0].detail.contains("moved span"),
            "detail should describe the moved-span offset: {}",
            hints[0].detail
        );
    }

    #[test]
    fn cycle_risk_flags_dest_importing_source_module() {
        let dest = "use crate::pricing::LineItem;\n\nfn helper() {}\n";
        let hints = cycle_risk_hints(dest, "src/grand_total.rs", Some("crate::pricing"));
        assert_eq!(hints.len(), 1, "one cycle_risk hint");
        assert_eq!(hints[0].kind, "cycle_risk");
        assert_eq!(hints[0].line, Some(1));
        // No false positive on a sibling module with a shared prefix.
        assert!(
            cycle_risk_hints(
                "use crate::pricing_utils::X;\n",
                "src/grand_total.rs",
                Some("crate::pricing")
            )
            .is_empty()
        );
        // Crate-root source is treated as noise and skipped.
        assert!(cycle_risk_hints("use crate::pricing::X;\n", "src/g.rs", Some("crate")).is_empty());
    }
}
