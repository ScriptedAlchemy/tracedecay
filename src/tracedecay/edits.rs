//! Anchored source-editing primitives (str-replace, insert, symbol
//! replacement, ast-grep rewrites) plus the single-file re-index they
//! trigger.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
use crate::sync;
use crate::types::*;

use super::indexing::{accumulate_symbol_scope, safe_extract};
use super::{current_timestamp, TraceDecay};

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
    async fn reindex_file(&self, file_path: &str) -> Result<()> {
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

        self.db.delete_nodes_by_file(file_path).await?;
        self.db.insert_nodes(&result.nodes).await?;
        self.db.insert_edges(&result.edges).await?;
        if !result.unresolved_refs.is_empty() {
            self.db
                .insert_unresolved_refs(&result.unresolved_refs)
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
        self.db.upsert_file(&file_record).await?;
        let mut short = HashSet::new();
        let mut keys = HashSet::new();
        accumulate_symbol_scope(&result.nodes, &mut short, &mut keys);
        self.reresolve_after_reindex(&[file_path.to_string()], &short, &keys)
            .await?;

        Ok(())
    }

    /// Performs a single string replacement.
    /// Fails if `old_str` is not found or matches more than once.
    pub async fn str_replace(
        &self,
        path: &str,
        old_str: &str,
        new_str: &str,
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
                    message: format!("old_str not found in {path}"),
                })
            }
            1 => {}
            n => {
                return Ok(EditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    matched_str: old_str.to_string(),
                    new_str: new_str.to_string(),
                    message: format!("old_str matches {n} times, must match exactly once"),
                })
            }
        }

        let modified = source.replacen(old_str, new_str, 1);

        tokio::fs::write(&abs_path, &modified)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to write {path}: {e}"),
            })?;

        self.reindex_file(&rel_path).await?;

        Ok(EditResult {
            success: true,
            file_path: rel_path,
            matched_str: old_str.to_string(),
            new_str: new_str.to_string(),
            message: "replacement successful".to_string(),
        })
    }

    /// Applies multiple string replacements atomically.
    /// Fails if any `old_str` doesn't match exactly once.
    pub async fn multi_str_replace(
        &self,
        path: &str,
        replacements: &[(&str, &str)],
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

        for (old, _) in replacements {
            let count = source.matches(old).count();
            if count != 1 {
                return Ok(MultiEditResult {
                    success: false,
                    file_path: rel_path.clone(),
                    applied_count: 0,
                    message: format!(
                        "replacement '{}' matches {} times, must match exactly once",
                        crate::text::utf8_prefix_at_or_before(old, 20),
                        count
                    ),
                });
            }
        }

        let mut modified = source;
        for (old, new) in replacements {
            modified = modified.replacen(old, new, 1);
        }

        tokio::fs::write(&abs_path, &modified)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to write {path}: {e}"),
            })?;

        self.reindex_file(&rel_path).await?;

        Ok(MultiEditResult {
            success: true,
            file_path: rel_path,
            applied_count: replacements.len(),
            message: format!("applied {} replacements", replacements.len()),
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

        tokio::fs::write(&abs_path, &modified)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to write {path}: {e}"),
            })?;

        self.reindex_file(&rel_path).await?;

        Ok(InsertResult {
            success: true,
            file_path: rel_path,
            anchor_line: (anchor_line + 1) as u32,
            content: content.to_string(),
            before,
            message: format!("inserted at line {}", anchor_line + 1),
        })
    }

    /// Replaces the full source of a named symbol (function, method, struct,
    /// etc.) with `new_source`. Resolves the symbol via exact qualified-name
    /// match — if the name is ambiguous, callable definitions win; if still
    /// ambiguous after that filter, the edit is refused so we don't clobber
    /// the wrong site.
    pub async fn replace_symbol(&self, symbol: &str, new_source: &str) -> Result<EditResult> {
        let target = resolve_symbol_for_edit(self, symbol).await?;
        let project_path =
            crate::storage::ProjectPath::resolve(&self.project_root, Path::new(&target.file_path))?;
        let rel_path = target.file_path.clone();
        let abs_path = project_path.absolute_path();
        let source = std::fs::read_to_string(&abs_path).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read {rel_path}: {e}"),
        })?;
        let lines: Vec<&str> = source.lines().collect();
        let start = target.start_line as usize;
        let end_inclusive = (target.end_line as usize).min(lines.len().saturating_sub(1));
        if start >= lines.len() || start > end_inclusive {
            return Ok(EditResult {
                success: false,
                file_path: rel_path,
                matched_str: symbol.to_string(),
                new_str: String::new(),
                message: format!(
                    "symbol range [{}..={}] out of bounds for {}-line file",
                    target.start_line,
                    target.end_line,
                    lines.len()
                ),
            });
        }
        let trailing_newline = source.ends_with('\n');
        let mut rebuilt: Vec<String> = Vec::with_capacity(lines.len());
        rebuilt.extend(lines[..start].iter().map(|s| (*s).to_string()));
        rebuilt.push(new_source.trim_end_matches('\n').to_string());
        rebuilt.extend(lines[end_inclusive + 1..].iter().map(|s| (*s).to_string()));
        let mut modified = rebuilt.join("\n");
        if trailing_newline {
            modified.push('\n');
        }
        tokio::fs::write(&abs_path, &modified)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to write {rel_path}: {e}"),
            })?;
        self.reindex_file(&rel_path).await?;
        Ok(EditResult {
            success: true,
            file_path: rel_path,
            matched_str: format!("{} ({})", target.name, target.kind.as_str()),
            new_str: new_source.to_string(),
            message: format!(
                "replaced {}:{}-{}",
                target.file_path,
                target.start_line + 1,
                target.end_line + 1
            ),
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
        let anchor_line = if before {
            target.start_line as usize
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
        tokio::fs::write(&abs_path, &modified)
            .await
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to write {rel_path}: {e}"),
            })?;
        self.reindex_file(&rel_path).await?;
        Ok(InsertResult {
            success: true,
            file_path: rel_path,
            anchor_line: (anchor_line + 1) as u32,
            content: content.to_string(),
            before,
            message: format!(
                "inserted {} {} ({}) at line {}",
                position,
                target.name,
                target.kind.as_str(),
                anchor_line + 1
            ),
        })
    }

    /// Performs structural rewrite using ast-grep CLI.
    pub async fn ast_grep_rewrite(
        &self,
        path: &str,
        pattern: &str,
        rewrite: &str,
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
                let mut source = std::fs::read_to_string(&abs_path).map_err(TraceDecayError::Io)?;
                if !source.contains(pattern) {
                    return Ok(AstGrepResult {
                        success: false,
                        file_path: rel_path.clone(),
                        pattern: pattern.to_string(),
                        rewrite: rewrite.to_string(),
                        message: "pattern not found (built-in literal fallback)".to_string(),
                    });
                }
                source = source.replace(pattern, rewrite);
                std::fs::write(&abs_path, source).map_err(TraceDecayError::Io)?;
                self.reindex_file(&rel_path).await?;
                return Ok(AstGrepResult {
                    success: true,
                    file_path: rel_path,
                    pattern: pattern.to_string(),
                    rewrite: rewrite.to_string(),
                    message: "literal rewrite completed using built-in fallback".to_string(),
                });
            }
            return Ok(AstGrepResult {
                success: false,
                file_path: rel_path.clone(),
                pattern: pattern.to_string(),
                rewrite: rewrite.to_string(),
                message: "ast-grep is not installed and this pattern needs SGPattern matching. Simple literal rewrites are handled by the built-in fallback.".to_string(),
            });
        }

        let output = crate::external_tools::ast_grep_command()
            .args([
                "run",
                "-p",
                pattern,
                "-r",
                rewrite,
                "-U",
                abs_path.to_string_lossy().as_ref(),
            ])
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
                message,
            });
        }

        self.reindex_file(&rel_path).await?;

        Ok(AstGrepResult {
            success: true,
            file_path: rel_path,
            pattern: pattern.to_string(),
            rewrite: rewrite.to_string(),
            message: "ast-grep rewrite completed".to_string(),
        })
    }
}

fn can_use_literal_rewrite_fallback(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    !trimmed.is_empty()
        && trimmed == pattern
        && !pattern.contains('$')
        && !pattern.contains('\n')
        && !pattern.contains('\r')
}

/// Resolves a symbol name to a single node suitable for symbol-aware editing.
///
/// Exact-qualified-name match wins; on ambiguity the resolver narrows to
/// callable kinds (function/method/etc.). If still more than one candidate
/// remains the edit is refused — silently picking the wrong site is far
/// worse than asking the caller to disambiguate.
async fn resolve_symbol_for_edit(cg: &TraceDecay, symbol: &str) -> Result<Node> {
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
