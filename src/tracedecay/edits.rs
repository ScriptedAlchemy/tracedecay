//! Anchored source-editing primitives (str-replace, insert, symbol
//! replacement, ast-grep rewrites) plus the single-file re-index they
//! trigger.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
use crate::sync;
use crate::types::*;

use super::indexing::{accumulate_symbol_scope, safe_extract};
use super::{TraceDecay, current_timestamp};

impl TraceDecay {
    /// Resolves a path to a relative path string.
    /// If the path is already relative, validates that it stays in the project.
    /// If absolute, strips the `project_root` prefix.
    fn resolve_path(&self, path: &str) -> Option<String> {
        crate::storage::ProjectPath::resolve(&self.project_root, Path::new(path))
            .ok()
            .map(|path| path.relative_path_string())
    }

    /// Gets the absolute path for a relative path.
    fn absolute_path(&self, relative_path: &str) -> PathBuf {
        self.project_root.join(relative_path)
    }

    /// Re-indexes a single file after an edit.
    pub(super) async fn reindex_file(&self, file_path: &str) -> Result<()> {
        let abs_path = self.absolute_path(file_path);
        let source = std::fs::read_to_string(&abs_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read file {file_path}: {e}"),
        })?;

        let Some(extractor) = self.registry.extractor_for_file(file_path) else {
            return Ok(());
        };

        let mut result =
            safe_extract(extractor, file_path, &source).ok_or_else(|| TraceDecayError::Config {
                message: format!("extraction panicked for {file_path}"),
            })?;
        result.sanitize();

        let hash = sync::content_hash(&source);
        let size = source.len() as u64;
        let mtime = sync::file_stat(&abs_path).map_or_else(current_timestamp, |(m, _)| m);

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
    async fn commit_or_preview_edit(
        &self,
        rel_path: &str,
        abs_path: &Path,
        original: &str,
        modified: &str,
        dry_run: bool,
    ) -> Result<Option<String>> {
        if dry_run {
            return Ok(Some(bounded_region_diff(
                original,
                modified,
                PREVIEW_DIFF_CONTEXT,
                MAX_PREVIEW_DIFF_LINES,
            )));
        }
        tokio::fs::write(abs_path, modified)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to write {rel_path}: {e}"),
            })?;
        self.reindex_file(rel_path).await?;
        Ok(None)
    }

    /// Performs a single string replacement.
    /// Fails if `old_str` is not found or matches more than once.
    pub async fn str_replace(
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

        let abs_path = self.absolute_path(&rel_path);
        let source = std::fs::read_to_string(&abs_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read {path}: {e}"),
        })?;

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
            .commit_or_preview_edit(&rel_path, &abs_path, &source, &modified, dry_run)
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
    pub async fn multi_str_replace(
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

        let abs_path = self.absolute_path(&rel_path);
        let source = std::fs::read_to_string(&abs_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read {path}: {e}"),
        })?;

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
            .commit_or_preview_edit(&rel_path, &abs_path, &source, &modified, dry_run)
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
    pub async fn insert_at(
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

        let abs_path = self.absolute_path(&rel_path);
        let source = std::fs::read_to_string(&abs_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read {path}: {e}"),
        })?;

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
            .commit_or_preview_edit(&rel_path, &abs_path, &source, &modified, dry_run)
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
    pub async fn replace_symbol(
        &self,
        symbol: &str,
        new_source: &str,
        dry_run: bool,
    ) -> Result<EditResult> {
        let target = resolve_symbol_for_edit(self, symbol).await?;
        let project_path =
            crate::storage::ProjectPath::resolve(&self.project_root, Path::new(&target.file_path))?;
        let rel_path = target.file_path.clone();
        let abs_path = project_path.absolute_path();
        let source = std::fs::read_to_string(&abs_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read {rel_path}: {e}"),
        })?;
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
            .commit_or_preview_edit(&rel_path, &abs_path, &source, &modified, dry_run)
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
    pub async fn insert_at_symbol(
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
        let project_path =
            crate::storage::ProjectPath::resolve(&self.project_root, Path::new(&target.file_path))?;
        let rel_path = target.file_path.clone();
        let abs_path = project_path.absolute_path();
        let source = std::fs::read_to_string(&abs_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read {rel_path}: {e}"),
        })?;
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
            .commit_or_preview_edit(&rel_path, &abs_path, &source, &modified, dry_run)
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

    /// Performs structural rewrite using ast-grep CLI.
    pub async fn ast_grep_rewrite(
        &self,
        path: &str,
        pattern: &str,
        rewrite: &str,
        dry_run: bool,
    ) -> Result<AstGrepResult> {
        let rel_path = self
            .resolve_path(path)
            .ok_or_else(|| TraceDecayError::Config {
                message: "path is not within the project".to_string(),
            })?;

        let abs_path = self.absolute_path(&rel_path);

        let check_output = crate::external_tools::ast_grep_command()
            .args(["--version"])
            .output();

        if check_output.is_err() {
            if can_use_literal_rewrite_fallback(pattern) {
                let source = std::fs::read_to_string(&abs_path).map_err(TraceDecayError::Io)?;
                if !source.contains(pattern) {
                    return Ok(AstGrepResult {
                        success: false,
                        file_path: rel_path.clone(),
                        pattern: pattern.to_string(),
                        rewrite: rewrite.to_string(),
                        dry_run,
                        diff: None,
                        message: "pattern not found (built-in literal fallback)".to_string(),
                    });
                }
                let modified = source.replace(pattern, rewrite);
                let diff = self
                    .commit_or_preview_edit(&rel_path, &abs_path, &source, &modified, dry_run)
                    .await?;
                return Ok(AstGrepResult {
                    success: true,
                    file_path: rel_path,
                    pattern: pattern.to_string(),
                    rewrite: rewrite.to_string(),
                    dry_run,
                    diff,
                    message: edit_success_message(
                        dry_run,
                        "literal rewrite completed using built-in fallback",
                    ),
                });
            }
            return Ok(AstGrepResult {
                success: false,
                file_path: rel_path.clone(),
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                dry_run,
                diff: None,
                message: "ast-grep is not installed and this pattern needs SGPattern matching. Simple literal rewrites are handled by the built-in fallback.".to_string(),
            });
        }

        // On a dry run, omit `-U` (`--update-all`): ast-grep then prints the
        // planned changes to stdout and leaves the file untouched, which is
        // exactly the preview we want. A real run keeps `-U` to apply in place.
        let abs_path_arg = abs_path.to_string_lossy();
        let mut ast_grep_args: Vec<&str> = vec!["run", "-p", pattern, "-r", rewrite];
        if !dry_run {
            ast_grep_args.push("-U");
        }
        ast_grep_args.push(abs_path_arg.as_ref());
        let output = crate::external_tools::ast_grep_command()
            .args(&ast_grep_args)
            .output()
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to run ast-grep: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr_trim = stderr.trim();
            let stdout_trim = stdout.trim();
            let exit = output
                .status
                .code()
                .map_or_else(|| "killed by signal".to_string(), |c| c.to_string());
            let message = if !stderr_trim.is_empty() {
                format!("ast-grep failed (exit {exit}): {stderr_trim}")
            } else if !stdout_trim.is_empty() {
                format!("ast-grep failed (exit {exit}). stdout: {stdout_trim}")
            } else {
                format!(
                    "ast-grep failed (exit {exit}) with no output. Likely causes: \
                     pattern matched 0 nodes, language not inferred from file extension \
                     (e.g. .txt has no parser), or invalid pattern syntax. \
                     File: {rel_path}, pattern: {pattern:?}"
                )
            };
            return Ok(AstGrepResult {
                success: false,
                file_path: rel_path.clone(),
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                dry_run,
                diff: None,
                message,
            });
        }

        if dry_run {
            // ast-grep printed the would-be changes to stdout; surface them as
            // the preview and leave the file (and index) untouched.
            let preview = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return Ok(AstGrepResult {
                success: true,
                file_path: rel_path,
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                dry_run: true,
                diff: (!preview.is_empty()).then_some(preview),
                message: edit_success_message(true, "ast-grep rewrite completed"),
            });
        }

        self.reindex_file(&rel_path).await?;

        Ok(AstGrepResult {
            success: true,
            file_path: rel_path,
            pattern: pattern.to_string(),
            rewrite: rewrite.to_string(),
            dry_run: false,
            diff: None,
            message: "ast-grep rewrite completed".to_string(),
        })
    }
}

/// Cheap heuristic: does `source`'s first non-blank line look like a leading
/// doc-comment (`//`, `///`, `//!`), block comment (`/*`), or attribute
/// (`#[`, `#!`)? Used only to decide whether a `replace_symbol` note should
/// warn that the replacement text may have dropped the item's docs/attrs.
fn leading_doc_or_attr(source: &str) -> bool {
    source
        .lines()
        .map(str::trim_start)
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            line.starts_with("//")
                || line.starts_with("/*")
                || line.starts_with("#[")
                || line.starts_with("#!")
        })
}

fn can_use_literal_rewrite_fallback(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    !trimmed.is_empty()
        && trimmed == pattern
        && !pattern.contains('$')
        && !pattern.contains('\n')
        && !pattern.contains('\r')
}

/// Unchanged context lines shown on each side of the changed region in a
/// dry-run preview diff.
pub(super) const PREVIEW_DIFF_CONTEXT: usize = 3;

/// Hard cap on the number of lines emitted in a dry-run preview diff. Keeps the
/// preview bounded even when an edit rewrites a large span; the remainder is
/// noted as truncated.
pub(super) const MAX_PREVIEW_DIFF_LINES: usize = 200;

/// Success-path message wrapper: on a real edit returns `base` verbatim; on a
/// dry run wraps it to make clear that nothing was written and only a preview
/// was produced.
pub(super) fn edit_success_message(dry_run: bool, base: &str) -> String {
    if dry_run {
        format!("dry run — nothing written; preview only ({base})")
    } else {
        base.to_string()
    }
}

/// Builds a bounded, unified-style diff of the single changed region between
/// `original` and `modified`. The two texts are compared line-by-line: the
/// common leading and trailing lines are trimmed and only the differing middle
/// band — plus `context` unchanged lines on each side — is rendered, capped at
/// `max_lines` (excess is noted as truncated). This is a cheap single-hunk
/// preview for a localized edit, not a minimal multi-hunk LCS diff; a widely
/// scattered set of changes collapses into one hunk spanning them.
pub(super) fn bounded_region_diff(
    original: &str,
    modified: &str,
    context: usize,
    max_lines: usize,
) -> String {
    if original == modified {
        return "(no changes)".to_string();
    }
    let old: Vec<&str> = original.lines().collect();
    let new: Vec<&str> = modified.lines().collect();

    // Longest common line prefix.
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix += 1;
    }
    // Longest common line suffix that does not overlap the prefix.
    let mut suffix = 0;
    while suffix < old.len().saturating_sub(prefix)
        && suffix < new.len().saturating_sub(prefix)
        && old[old.len() - 1 - suffix] == new[new.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_change_end = old.len() - suffix; // exclusive
    let new_change_end = new.len() - suffix; // exclusive
    let ctx_start = prefix.saturating_sub(context);
    let old_ctx_end = (old_change_end + context).min(old.len());
    let new_ctx_end = (new_change_end + context).min(new.len());

    let mut out: Vec<String> = Vec::new();
    out.push(format!(
        "@@ -{},{} +{},{} @@",
        ctx_start + 1,
        old_ctx_end - ctx_start,
        ctx_start + 1,
        new_ctx_end - ctx_start
    ));
    for line in &old[ctx_start..prefix] {
        out.push(format!(" {line}"));
    }
    for line in &old[prefix..old_change_end] {
        out.push(format!("-{line}"));
    }
    for line in &new[prefix..new_change_end] {
        out.push(format!("+{line}"));
    }
    for line in &new[new_change_end..new_ctx_end] {
        out.push(format!(" {line}"));
    }

    if out.len() > max_lines {
        let omitted = out.len() - max_lines;
        out.truncate(max_lines);
        out.push(format!("... diff truncated ({omitted} more line(s))"));
    }
    out.join("\n")
}

/// Resolves a symbol name to a single node suitable for symbol-aware editing.
///
/// Exact-qualified-name match wins; on ambiguity the resolver narrows to
/// callable kinds (function/method/etc.). If still more than one candidate
/// remains the edit is refused — silently picking the wrong site is far
/// worse than asking the caller to disambiguate.
pub(super) async fn resolve_symbol_for_edit(cg: &TraceDecay, symbol: &str) -> Result<Node> {
    let nodes = cg.get_nodes_by_qualified_name(symbol).await?;
    let mut iter = nodes.into_iter();
    let Some(first) = iter.next() else {
        return Err(TraceDecayError::Config {
            message: format!("symbol '{symbol}' not found"),
        });
    };
    let rest: Vec<Node> = iter.collect();
    if rest.is_empty() {
        return Ok(first);
    }
    let total = rest.len() + 1;
    let mut callables: Vec<Node> = std::iter::once(first)
        .chain(rest)
        .filter(|n| {
            matches!(
                n.kind,
                NodeKind::Function
                    | NodeKind::Method
                    | NodeKind::StructMethod
                    | NodeKind::Constructor
                    | NodeKind::AbstractMethod
                    | NodeKind::ArrowFunction
                    | NodeKind::Procedure
            )
        })
        .collect();
    if callables.len() == 1 {
        return Ok(callables.remove(0));
    }
    Err(TraceDecayError::Config {
        message: format!(
            "symbol '{symbol}' is ambiguous ({total} matches); pass a fully qualified name"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::leading_doc_or_attr;

    #[test]
    fn leading_doc_or_attr_detects_doc_comment() {
        assert!(leading_doc_or_attr("/// docs\nfn f() {}"));
        assert!(leading_doc_or_attr("//! module doc\nfn f() {}"));
        assert!(leading_doc_or_attr("// plain\nfn f() {}"));
    }

    #[test]
    fn leading_doc_or_attr_detects_attribute_and_block_comment() {
        assert!(leading_doc_or_attr("#[inline]\nfn f() {}"));
        assert!(leading_doc_or_attr("#![allow(dead_code)]"));
        assert!(leading_doc_or_attr("/* block */\nfn f() {}"));
    }

    #[test]
    fn leading_doc_or_attr_skips_leading_blank_lines() {
        assert!(leading_doc_or_attr("\n\n   /// docs\nfn f() {}"));
        assert!(!leading_doc_or_attr("\n\nfn f() {}"));
    }

    #[test]
    fn leading_doc_or_attr_false_for_bare_code() {
        assert!(!leading_doc_or_attr("fn f() {}"));
        assert!(!leading_doc_or_attr(""));
        assert!(!leading_doc_or_attr("pub struct S;"));
    }

    use super::{bounded_region_diff, edit_success_message};

    #[test]
    fn bounded_region_diff_reports_no_changes_when_identical() {
        assert_eq!(
            bounded_region_diff("a\nb\n", "a\nb\n", 3, 200),
            "(no changes)"
        );
    }

    #[test]
    fn bounded_region_diff_shows_changed_line_with_context() {
        let original = "one\ntwo\nthree\nfour\nfive\n";
        let modified = "one\ntwo\nTHREE\nfour\nfive\n";
        let diff = bounded_region_diff(original, modified, 1, 200);
        assert!(diff.contains("-three"), "diff should mark removal: {diff}");
        assert!(diff.contains("+THREE"), "diff should mark addition: {diff}");
        // One line of context on each side, but not the far-away lines.
        assert!(
            diff.contains(" two"),
            "diff should include leading context: {diff}"
        );
        assert!(
            diff.contains(" four"),
            "diff should include trailing context: {diff}"
        );
        assert!(
            !diff.contains("one"),
            "distant lines should be trimmed: {diff}"
        );
        assert!(
            diff.starts_with("@@"),
            "diff should carry a hunk header: {diff}"
        );
    }

    #[test]
    fn bounded_region_diff_handles_pure_insertion() {
        let diff = bounded_region_diff("a\nb\n", "a\nNEW\nb\n", 3, 200);
        assert!(diff.contains("+NEW"), "insertion should appear: {diff}");
        assert!(
            !diff.lines().any(|line| line.starts_with('-')),
            "pure insertion has no removals: {diff}"
        );
    }

    #[test]
    fn bounded_region_diff_truncates_past_the_cap() {
        let original = "keep\n";
        let modified: String = std::iter::once("keep")
            .chain((0..500).map(|_| "x"))
            .collect::<Vec<_>>()
            .join("\n");
        let diff = bounded_region_diff(original, &modified, 3, 50);
        assert!(
            diff.contains("diff truncated"),
            "large diff should truncate: {diff}"
        );
    }

    #[test]
    fn edit_success_message_marks_dry_runs() {
        assert_eq!(edit_success_message(false, "done"), "done");
        let dry = edit_success_message(true, "done");
        assert!(
            dry.contains("dry run"),
            "dry-run message should say so: {dry}"
        );
        assert!(
            dry.contains("done"),
            "dry-run message should keep the base: {dry}"
        );
    }
}
