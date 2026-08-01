//! Core anchored source-editing primitives: string replacement, anchored
//! insertion, and symbol-span replacement. Every primitive resolves its
//! target path or symbol, reads the current bytes through the source-edit
//! file authority, computes the modified text, and hands the before/after
//! pair to [`TraceDecay::commit_or_preview_edit`] — the single write-or-
//! preview gate shared by every primitive (including `ast_grep_rewrite` in
//! the sibling `ast_grep` module).

use std::collections::HashSet;
use std::path::Path;

use same_file::Handle;

use crate::errors::{Result, TraceDecayError};
use crate::sync;
use crate::types::*;

use super::super::indexing::{accumulate_symbol_scope, safe_extract};
use super::super::{TraceDecay, current_timestamp};

use super::file_authority::{SourceEditFileAuthority, normalize_source_edit_relative_path};
use super::plan::{capture_planned_source_edit, validate_planned_source_edit};
use super::preview::{
    MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff, edit_success_message,
    leading_doc_or_attr,
};
use super::symbols::resolve_symbol_for_edit;

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

    /// Re-indexes a single file after an edit.
    pub(super) async fn reindex_file(
        &self,
        file_path: &str,
        source: &str,
        file: &SourceEditFileAuthority,
    ) -> Result<()> {
        let Some(extractor) = self.registry.extractor_for_file(file_path) else {
            return Ok(());
        };

        let mut result =
            safe_extract(extractor, file_path, source).ok_or_else(|| TraceDecayError::Config {
                message: format!("extraction panicked for {file_path}"),
            })?;
        result.sanitize();

        let hash = sync::content_hash(source);
        let size = source.len() as u64;
        let mtime = file
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| {
                modified
                    .into_std()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs() as i64)
            })
            .unwrap_or_else(current_timestamp);

        let transaction = self.db.begin_write_transaction("reindex file").await?;
        self.db
            .delete_nodes_by_file_unguarded(&transaction, file_path)
            .await?;
        self.db
            .insert_nodes_unguarded(&transaction, &result.nodes)
            .await?;
        self.db
            .insert_edges_unguarded(&transaction, &result.edges)
            .await?;
        if !result.unresolved_refs.is_empty() {
            self.db
                .insert_unresolved_refs_unguarded(&transaction, &result.unresolved_refs)
                .await?;
        }

        let file_record = FileRecord {
            path: file_path.to_string(),
            content_hash: hash,
            size,
            modified_at: mtime,
            indexed_at: current_timestamp(),
            node_count: result.nodes.len() as u32,
        };
        self.db
            .upsert_file_unguarded(&transaction, &file_record)
            .await?;
        transaction.commit().await?;
        let mut short = HashSet::new();
        let mut keys = HashSet::new();
        accumulate_symbol_scope(&result.nodes, &mut short, &mut keys);
        self.reresolve_after_reindex(&[file_path.to_string()], &short, &keys)
            .await?;

        Ok(())
    }

    /// Write-or-preview gate shared by every edit primitive. On a real run this
    /// writes `modified` to `abs_path` and reindexes the file, returning `None`.
    /// On a dry run it writes nothing and reindexes nothing, instead returning a
    /// bounded preview diff of the changed region (the would-be change) so
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
        self.reindex_file(rel_path, modified, file).await?;
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

        let matches: Vec<_> = source.match_indices(old_str).collect();
        match matches.len() {
            0 => {
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
            }
            1 => {}
            n => {
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
        }

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
            replaced_span: None,
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
                let count = source.matches(old).count();
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
        let mut modified = new_lines.join("\n");
        if source.ends_with('\n') {
            modified.push('\n');
        }

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
        symbol: &str,
        new_source: &str,
        dry_run: bool,
    ) -> Result<EditResult> {
        let target = resolve_symbol_for_edit(self, symbol).await?;
        let rel_path = target.file_path.clone();
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(&rel_path)?;
        let lines: Vec<&str> = source.lines().collect();
        // Honor the leading doc-comment / attribute block adaptively. The
        // extractor only sets `attrs_start_line` below `start_line` for an
        // item that actually has such a block. When `new_source` carries its
        // own leading docs/attrs, the whole span (docs included) is swapped so
        // documentation is never duplicated; when it does not, the existing
        // block is preserved above the replacement so replacing a symbol's
        // body never silently deletes its documentation.
        let has_leading_block = (target.attrs_start_line as usize) < target.start_line as usize;
        let replacement_brings_block = leading_doc_or_attr(new_source);
        let start = if has_leading_block && replacement_brings_block {
            target.attrs_start_line as usize
        } else {
            target.start_line as usize
        };
        let end_inclusive = (target.end_line as usize).min(lines.len().saturating_sub(1));
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
                    target.end_line,
                    lines.len()
                ),
            });
        }
        let replaced_span = lines[start..=end_inclusive].join("\n");
        let trailing_newline = source.ends_with('\n');
        let mut rebuilt: Vec<String> = Vec::with_capacity(lines.len());
        rebuilt.extend(lines[..start].iter().map(|s| (*s).to_string()));
        rebuilt.push(new_source.trim_end_matches('\n').to_string());
        rebuilt.extend(lines[end_inclusive + 1..].iter().map(|s| (*s).to_string()));
        let mut modified = rebuilt.join("\n");
        if trailing_newline {
            modified.push('\n');
        }
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
        // If the old span carried leading docs/attrs but the replacement text
        // does not appear to, surface a note so the caller can recover them
        // from `replaced_span` rather than silently losing documentation.
        let base = format!(
            "replaced {}:{}-{}",
            target.file_path,
            start + 1,
            target.end_line + 1
        );
        let mut message = edit_success_message(dry_run, &base);
        if has_leading_block && !replacement_brings_block {
            message.push_str(
                "; note: the item's leading docs/attrs were preserved above the \
                 replacement — include a leading doc/attr block in new_source to \
                 replace them",
            );
        }
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
        let target = resolve_symbol_for_edit(self, symbol).await?;
        let rel_path = target.file_path.clone();
        let file = SourceEditFileAuthority::open(&self.project_root, Path::new(&rel_path))?;
        let (source, source_identity) = file.read_to_string(&rel_path)?;
        let lines: Vec<&str> = source.lines().collect();
        // `before` inserts above the item's leading doc-comment / attribute
        // block (when the extractor recorded one) so new content lands above the
        // docs rather than splitting them from their item; `after` is unaffected.
        let anchor_line = if before {
            // Anchor above the item's leading doc-comment / attribute block so
            // "before" never splits docs from the item they document. For items
            // with no leading block, attrs_start_line == start_line and this is
            // the item line itself. The min() guards against inconsistent rows.
            target.attrs_start_line.min(target.start_line) as usize
        } else {
            (target.end_line as usize).saturating_add(1)
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
        let trailing_newline = source.ends_with('\n');
        let mut rebuilt: Vec<String> = Vec::with_capacity(lines.len() + 1);
        rebuilt.extend(lines[..anchor_line].iter().map(|s| (*s).to_string()));
        rebuilt.push(content.trim_end_matches('\n').to_string());
        rebuilt.extend(lines[anchor_line..].iter().map(|s| (*s).to_string()));
        let mut modified = rebuilt.join("\n");
        if trailing_newline {
            modified.push('\n');
        }
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
