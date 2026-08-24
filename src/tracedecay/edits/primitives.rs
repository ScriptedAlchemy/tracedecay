//! Core anchored source-editing primitives: string replacement, anchored
//! insertion, and symbol-span replacement. Every primitive resolves its
//! target path or symbol, reads the current bytes through the source-edit
//! file authority, computes the modified text, and hands the before/after
//! pair to [`TraceDecay::commit_or_preview_edit`] — the single write-or-
//! preview gate shared by every primitive (including `ast_grep_rewrite` in
//! the sibling `ast_grep` module).

use std::path::Path;

use same_file::Handle;
use tracedecay_usecases::tracedecay::SourceEditGraphReadV1;

use crate::errors::{Result, TraceDecayError};
use crate::types::*;

use super::super::TraceDecay;

use super::file_authority::{SourceEditFileAuthority, normalize_source_edit_relative_path};
use super::plan::{capture_planned_source_edit, validate_planned_source_edit};
use super::preview::{
    MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff, edit_success_message,
};
use super::symbols::resolve_symbol_for_edit;

/// Joins `lines` back into a single string with `\n` separators, re-adding
/// the trailing newline the original source had (skipped when the result is
/// empty, so deleting a file's only content never leaves a lone `\n`
/// behind). The common tail of every anchored-edit / move-symbol splice:
/// build the new line list, then hand it here instead of hand-rolling
/// `join("\n")` plus a conditional trailing push at each call site.
pub(in crate::tracedecay) fn splice_lines<S: AsRef<str>>(
    lines: &[S],
    source_had_trailing_newline: bool,
) -> String {
    let mut joined = lines
        .iter()
        .map(AsRef::as_ref)
        .collect::<Vec<&str>>()
        .join("\n");
    if source_had_trailing_newline && !joined.is_empty() {
        joined.push('\n');
    }
    joined
}

impl TraceDecay {
    /// Resolves a path to a relative path string.
    /// If the path is already relative, validates that it stays in the project.
    /// If absolute, strips the `project_root` prefix.
    pub(super) fn resolve_path(&self, path: &str) -> Option<String> {
        let path = Path::new(path);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.project_root).ok()?
        } else {
            path
        };
        normalize_source_edit_relative_path(relative)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
    }

    /// Write-or-preview gate shared by every edit primitive. On a real run this
    /// publishes `modified` through the descriptor-scoped file authority and
    /// leaves generation refresh to the daemon-owned code-index scheduler. On
    /// a dry run it writes nothing, instead returning a bounded preview diff so
    /// callers can review before committing. Centralizing the write here keeps
    /// the dry-run gate in one place around each primitive's own validation and
    /// span logic.
    pub(super) async fn commit_or_preview_edit(
        &self,
        rel_path: &str,
        file: &SourceEditFileAuthority,
        expected_identity: &Handle,
        original: &str,
        modified: &str,
        dry_run: bool,
    ) -> Result<Option<String>> {
        if dry_run {
            capture_planned_source_edit(rel_path, Some(original), Some(modified));
            return Ok(Some(bounded_region_diff(
                original,
                modified,
                PREVIEW_DIFF_CONTEXT,
                MAX_PREVIEW_DIFF_LINES,
            )));
        }
        validate_planned_source_edit(rel_path, Some(original), Some(modified))?;
        file.publish(
            rel_path,
            Some(original),
            Some(expected_identity),
            modified,
            || {},
        )?;
        Ok(None)
    }

    /// Performs a single string replacement.
    /// Fails if `old_str` is not found or matches more than once.
    pub(crate) async fn str_replace(
        &self,
        path: &str,
        old_str: &str,
        new_str: &str,
        dry_run: bool,
    ) -> Result<EditResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;

        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        let mut matches = source.match_indices(old_str);
        let Some(matched) = matches.next() else {
            return Ok(EditResult {
                success: false,
                file_path: rel_path.clone(),
                matched_str: old_str.to_string(),
                new_str: new_str.to_string(),
                replaced_span: None,
                dry_run,
                diff: None,
                message: format!("old_str not found in {path}"),
            });
        };
        if matches.next().is_some() {
            // Two matches already consumed from `matches`; the remainder it
            // still holds is every match after those, so the total is that
            // count plus the two already seen — no need for a second,
            // redundant full-string `source.matches(old_str).count()` pass.
            let n = 2 + matches.count();
            return Ok(EditResult {
                success: false,
                file_path: rel_path.clone(),
                matched_str: old_str.to_string(),
                new_str: new_str.to_string(),
                replaced_span: None,
                dry_run,
                diff: None,
                message: format!("old_str matches {n} times, must match exactly once"),
            });
        }
        let (_, matched_text) = matched;

        let modified = source.replacen(old_str, new_str, 1);

        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(EditResult {
            success: true,
            file_path: rel_path,
            matched_str: old_str.to_string(),
            new_str: new_str.to_string(),
            replaced_span: Some(matched_text.to_string()),
            dry_run,
            diff,
            message: edit_success_message(dry_run, "replacement successful"),
        })
    }

    /// Applies multiple string replacements atomically.
    /// Fails if any `old_str` doesn't match exactly once.
    pub(crate) async fn multi_str_replace(
        &self,
        path: &str,
        replacements: &[(&str, &str)],
        dry_run: bool,
    ) -> Result<MultiEditResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;

        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        // Resolve every replacement against the ORIGINAL source. Each `old`
        // must match exactly once, and no two matched ranges may overlap.
        // Splicing from the original (instead of applying `replacen`
        // sequentially against progressively-edited text) guarantees a later
        // `old` can never match text an earlier replacement introduced, and no
        // match can land at a shifted offset.
        let mut spans: Vec<(usize, usize, &str, &str)> = Vec::with_capacity(replacements.len());
        for (old, new) in replacements {
            let mut hits = source.match_indices(old);
            let Some((start, matched)) = hits.next() else {
                return Ok(MultiEditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    applied_count: 0,
                    dry_run,
                    diff: None,
                    message: format!(
                        "replacement '{}' matches 0 times, must match exactly once",
                        crate::text::utf8_prefix_at_or_before(old, 20)
                    ),
                });
            };
            if hits.next().is_some() {
                // Two matches already consumed from `hits`; the remainder it
                // still holds is every match after those, so the total is
                // that count plus the two already seen — no need for a
                // redundant full-string `source.matches(old).count()` pass.
                let count = 2 + hits.count();
                return Ok(MultiEditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    applied_count: 0,
                    dry_run,
                    diff: None,
                    message: format!(
                        "replacement '{}' matches {} times, must match exactly once",
                        crate::text::utf8_prefix_at_or_before(old, 20),
                        count
                    ),
                });
            }
            spans.push((start, start + matched.len(), old, new));
        }

        // Order by match start so we can both detect overlaps and splice in one
        // left-to-right pass. Touching ranges are fine; only true overlaps (a
        // later match starting inside an earlier one) are rejected.
        spans.sort_by_key(|&(start, _, _, _)| start);
        for window in spans.windows(2) {
            let (_, prev_end, prev_old, _) = window[0];
            let (next_start, _, next_old, _) = window[1];
            if next_start < prev_end {
                return Ok(MultiEditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    applied_count: 0,
                    dry_run,
                    diff: None,
                    message: format!(
                        "replacements '{}' and '{}' target overlapping ranges; apply them separately",
                        crate::text::utf8_prefix_at_or_before(prev_old, 20),
                        crate::text::utf8_prefix_at_or_before(next_old, 20)
                    ),
                });
            }
        }

        let mut modified = String::with_capacity(source.len());
        let mut cursor = 0usize;
        for &(start, end, _, new) in &spans {
            modified.push_str(&source[cursor..start]);
            modified.push_str(new);
            cursor = end;
        }
        modified.push_str(&source[cursor..]);

        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(MultiEditResult {
            success: true,
            file_path: rel_path,
            applied_count: replacements.len(),
            dry_run,
            diff,
            message: edit_success_message(
                dry_run,
                &format!("applied {} replacements", replacements.len()),
            ),
        })
    }

    /// Inserts content before or after a unique anchor.
    /// Anchor can be a string or 1-indexed line number.
    pub(crate) async fn insert_at(
        &self,
        path: &str,
        anchor: &str,
        content: &str,
        before: bool,
        dry_run: bool,
    ) -> Result<InsertResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;

        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(path)?;

        let lines: Vec<&str> = source.lines().collect();

        let anchor_line = if anchor.chars().all(|c| c.is_ascii_digit()) {
            let line_num: usize = anchor.parse().map_err(|_| TraceDecayError::Config {
                message: format!("invalid line number: {anchor}"),
            })?;
            if line_num == 0 || line_num > lines.len() {
                return Ok(InsertResult {
                    success: false,
                    file_path: rel_path.clone(),
                    anchor_line: line_num as u32,
                    content: content.to_string(),
                    before,
                    dry_run,
                    diff: None,
                    message: format!(
                        "line number {line_num} out of range (file has {} lines)",
                        lines.len()
                    ),
                });
            }
            line_num - 1
        } else {
            let anchor_prefix = crate::text::utf8_prefix_at_or_before(anchor, 100);
            let matching_lines: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.contains(anchor_prefix))
                .map(|(i, _)| i)
                .collect();

            if matching_lines.is_empty() {
                return Ok(InsertResult {
                    success: false,
                    file_path: rel_path.clone(),
                    anchor_line: 0,
                    content: content.to_string(),
                    before,
                    dry_run,
                    diff: None,
                    message: format!("anchor '{anchor}' not found"),
                });
            }
            if matching_lines.len() > 1 {
                return Ok(InsertResult {
                    success: false,
                    file_path: rel_path.clone(),
                    anchor_line: matching_lines.len() as u32,
                    content: content.to_string(),
                    before,
                    dry_run,
                    diff: None,
                    message: format!(
                        "anchor '{anchor}' matches {} lines, must match exactly one",
                        matching_lines.len()
                    ),
                });
            }
            matching_lines[0]
        };

        let insert_idx = if before { anchor_line } else { anchor_line + 1 };
        let mut new_lines: Vec<&str> = lines[..insert_idx].to_vec();
        new_lines.push(content);
        new_lines.extend_from_slice(&lines[insert_idx..]);
        let modified = splice_lines(&new_lines, source.ends_with('\n'));

        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;

        Ok(InsertResult {
            success: true,
            file_path: rel_path,
            anchor_line: (anchor_line + 1) as u32,
            content: content.to_string(),
            before,
            dry_run,
            diff,
            message: edit_success_message(
                dry_run,
                &format!("inserted at line {}", anchor_line + 1),
            ),
        })
    }

    /// Replaces the full source of a named symbol (function, method, struct,
    /// etc.) with `new_source`. Resolves the symbol via exact qualified-name
    /// match — if the name is ambiguous, callable definitions win; if still
    /// ambiguous after that filter, the edit is refused so we don't clobber
    /// the wrong site.
    pub(crate) async fn replace_symbol(
        &self,
        graph: SourceEditGraphReadV1,
        symbol: &str,
        new_source: &str,
        dry_run: bool,
    ) -> Result<EditResult> {
        let target = resolve_symbol_for_edit(&graph, symbol)?;
        let rel_path = target.file_path.clone();
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(&rel_path)?;
        let lines: Vec<&str> = source.lines().collect();
        let (start, end_inclusive) = target.line_bounds(&source)?;
        if start >= lines.len() || start > end_inclusive {
            return Ok(EditResult {
                success: false,
                file_path: rel_path,
                matched_str: symbol.to_string(),
                new_str: String::new(),
                replaced_span: None,
                dry_run,
                diff: None,
                message: format!(
                    "symbol range [{}..={}] out of bounds for {}-line file",
                    start,
                    end_inclusive,
                    lines.len()
                ),
            });
        }
        let replaced_span = lines[start..=end_inclusive].join("\n");
        let mut rebuilt: Vec<&str> = Vec::with_capacity(lines.len());
        rebuilt.extend_from_slice(&lines[..start]);
        rebuilt.push(new_source.trim_end_matches('\n'));
        rebuilt.extend_from_slice(&lines[end_inclusive + 1..]);
        let modified = splice_lines(&rebuilt, source.ends_with('\n'));
        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;
        let base = format!(
            "replaced {}:{}-{}",
            target.file_path,
            start + 1,
            end_inclusive + 1
        );
        let message = edit_success_message(dry_run, &base);
        Ok(EditResult {
            success: true,
            file_path: rel_path,
            matched_str: format!("{} ({})", target.name, target.kind.as_str()),
            new_str: new_source.to_string(),
            replaced_span: Some(replaced_span),
            dry_run,
            diff,
            message,
        })
    }

    /// Inserts `content` immediately before or after a named symbol. `position`
    /// is one of `"before"` or `"after"`. Uses the same resolution logic as
    /// `replace_symbol`.
    pub(crate) async fn insert_at_symbol(
        &self,
        graph: SourceEditGraphReadV1,
        symbol: &str,
        content: &str,
        position: &str,
        dry_run: bool,
    ) -> Result<InsertResult> {
        let before = match position {
            "before" => true,
            "after" => false,
            other => {
                return Err(TraceDecayError::Config {
                    message: format!("position must be \"before\" or \"after\", got {other:?}"),
                });
            }
        };
        let target = resolve_symbol_for_edit(&graph, symbol)?;
        let rel_path = target.file_path.clone();
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(&rel_path)?;
        let lines: Vec<&str> = source.lines().collect();
        let (symbol_start, symbol_end) = target.line_bounds(&source)?;
        let anchor_line = if before {
            symbol_start
        } else {
            symbol_end.saturating_add(1)
        };
        if anchor_line > lines.len() {
            return Ok(InsertResult {
                success: false,
                file_path: rel_path,
                anchor_line: anchor_line as u32,
                content: content.to_string(),
                before,
                dry_run,
                diff: None,
                message: format!("anchor line {anchor_line} past EOF ({})", lines.len()),
            });
        }
        let mut rebuilt: Vec<&str> = Vec::with_capacity(lines.len() + 1);
        rebuilt.extend_from_slice(&lines[..anchor_line]);
        rebuilt.push(content.trim_end_matches('\n'));
        rebuilt.extend_from_slice(&lines[anchor_line..]);
        let modified = splice_lines(&rebuilt, source.ends_with('\n'));
        let diff = self
            .commit_or_preview_edit(
                &rel_path,
                &file,
                &source_identity,
                &source,
                &modified,
                dry_run,
            )
            .await?;
        Ok(InsertResult {
            success: true,
            file_path: rel_path,
            anchor_line: (anchor_line + 1) as u32,
            content: content.to_string(),
            before,
            dry_run,
            diff,
            message: edit_success_message(
                dry_run,
                &format!(
                    "inserted {} {} ({}) at line {}",
                    position,
                    target.name,
                    target.kind.as_str(),
                    anchor_line + 1
                ),
            ),
        })
    }
}
