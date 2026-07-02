// Rust guideline compliant 2025-10-17
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::branch;
use crate::branch_meta;
use crate::config::TraceDecayConfig;
use crate::context::ContextBuilder;
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::extraction::LanguageRegistry;
use crate::global_db::GlobalDb;
use crate::graph::{GraphQueryManager, GraphTraverser};
use crate::memory::encoding::HolographicEncoder;
use crate::memory::retrieval::FactRetriever;
use crate::memory::store::MemoryStore;
use crate::memory::trust::{DEFAULT_MIN_TRUST, DEFAULT_TRUST};
use crate::memory::types::{
    AddFactOutcome, AddFactRequest, ContradictionResult, FactRecord, FactSearchResult,
    FeedbackRequest, FeedbackResult, MemoryCategory, MemoryRepairStats, MemoryStatus,
    SearchFactsRequest, TrustHistoryEntry, UpdateFactRequest,
};
use crate::storage::{self, StoreLayout};
use crate::sync;
use crate::types::*;

mod indexing;
mod lifecycle;
mod locking;
mod scan;

#[doc(hidden)]
pub use locking::{try_acquire_sync_lock, SyncLockGuard};

use indexing::{accumulate_symbol_scope, safe_extract};

/// Central orchestrator that coordinates all subsystems of the code graph.
///
/// Provides a high-level API for initializing, indexing, querying, and
/// syncing a Rust codebase's semantic knowledge graph.
pub struct TraceDecay {
    db: Database,
    config: TraceDecayConfig,
    project_root: PathBuf,
    store_layout: StoreLayout,
    open_options: TraceDecayOpenOptions,
    registry: LanguageRegistry,
    /// The active git branch (None if detached HEAD or not a git repo).
    active_branch: Option<String>,
    /// The branch whose DB is actually being served (may differ from `active_branch` on fallback).
    serving_branch: Option<String>,
    /// Set when serving from a fallback (ancestor) DB instead of the exact branch.
    fallback_warning: Option<String>,
    read_only: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TraceDecayOpenOptions {
    pub profile_root: Option<PathBuf>,
    pub global_db_path: Option<PathBuf>,
}

impl TraceDecayOpenOptions {
    fn resolved_profile_root(&self) -> Result<PathBuf> {
        if let Some(profile_root) = &self.profile_root {
            return Ok(profile_root.clone());
        }
        if let Some(parent) = self
            .global_db_path
            .as_deref()
            .and_then(std::path::Path::parent)
        {
            return Ok(parent.to_path_buf());
        }
        storage::default_profile_root()
    }

    async fn open_global_db(&self) -> Option<GlobalDb> {
        match self.global_db_path.as_deref() {
            Some(path) => GlobalDb::open_at(path).await,
            None => GlobalDb::open().await,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackedBranchDiagnostic {
    pub name: String,
    pub db_file: String,
    pub db_path: PathBuf,
    pub db_exists: bool,
    pub size_bytes: u64,
    pub parent: Option<String>,
    pub parent_db_path: Option<PathBuf>,
    pub parent_db_exists: Option<bool>,
    pub created_at: String,
    pub last_synced_at: String,
    pub is_default: bool,
    pub is_current: bool,
    pub is_open_active: bool,
    pub is_serving: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BranchDiagnostics {
    pub tracking_enabled: bool,
    pub default_branch: Option<String>,
    pub current_branch: Option<String>,
    pub open_active_branch: Option<String>,
    pub serving_branch: Option<String>,
    pub serving_db_path: PathBuf,
    pub serving_db_exists: bool,
    pub branch_drifted: bool,
    pub branch_resolution: String,
    pub is_fallback: bool,
    pub fallback_target: Option<String>,
    pub fallback_warning: Option<String>,
    pub live_branch_tracked: bool,
    pub live_branch_db_path: Option<PathBuf>,
    pub live_branch_db_exists: Option<bool>,
    pub nearest_tracked_ancestor: Option<String>,
    pub nearest_tracked_ancestor_db_path: Option<PathBuf>,
    pub nearest_tracked_ancestor_db_exists: Option<bool>,
    pub tracked_branch_count: usize,
    pub branches: Vec<TrackedBranchDiagnostic>,
    pub warnings: Vec<String>,
}

/// Result of a full indexing operation.
pub struct IndexResult {
    /// Number of files scanned and indexed.
    pub file_count: usize,
    /// Total number of nodes extracted.
    pub node_count: usize,
    /// Total number of edges (extracted + resolved).
    pub edge_count: usize,
    /// Time taken in milliseconds.
    pub duration_ms: u64,
}

/// Result of an incremental sync operation.
#[derive(Debug)]
pub struct SyncResult {
    /// Number of newly added files.
    pub files_added: usize,
    /// Number of modified (re-indexed) files.
    pub files_modified: usize,
    /// Number of removed files.
    pub files_removed: usize,
    /// Time taken in milliseconds.
    pub duration_ms: u64,
    /// Paths of added files (populated only when doctor mode is requested).
    pub added_paths: Vec<String>,
    /// Paths of modified files (populated only when doctor mode is requested).
    pub modified_paths: Vec<String>,
    /// Paths of removed files (populated only when doctor mode is requested).
    pub removed_paths: Vec<String>,
    /// Files that were found on disk but could not be read (path, error message).
    pub skipped_paths: Vec<(String, String)>,
}

/// Returns the current UNIX timestamp in seconds.
pub fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Query delegation
// ---------------------------------------------------------------------------

impl TraceDecay {
    /// Searches for nodes matching the given query string.
    ///
    /// Over-fetches from the FTS layer and re-ranks results so that symbol
    /// definitions (functions, structs, traits, etc.) sort above mere
    /// references (`use`, `module`, annotation usages) that happen to share
    /// the same name. BM25 alone does not distinguish kinds, so a `use foo`
    /// statement could outrank the actual `pub fn foo()` definition.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let overfetch = limit.saturating_mul(3).max(30);
        let trimmed_query = query.trim();
        let mut raw = self.db.search_nodes(query, overfetch).await?;

        // FTS/BM25 can bury exact symbol definitions below many short import
        // rows. On Sonium, `LinearOperator` had dozens of `use ...LinearOperator`
        // rows in the top FTS window while the actual trait definition was
        // outside `overfetch`, so the kind tier below never saw it. Seed the
        // candidate set with exact `name = query` hits first, then dedup.
        if !trimmed_query.is_empty() {
            let mut exact_names = vec![trimmed_query.to_string()];
            if let Some(short) = trimmed_query.rsplit("::").next() {
                if short != trimmed_query && !short.is_empty() {
                    exact_names.push(short.to_string());
                }
            }
            let exact = self
                .db
                .search_nodes_by_exact_name(&exact_names, overfetch)
                .await?;
            raw.extend(
                exact
                    .into_iter()
                    .map(|node| SearchResult { node, score: 0.0 }),
            );
        }

        let mut seen = HashSet::new();
        let mut ranked: Vec<SearchResult> = raw
            .into_iter()
            .filter(|r| seen.insert(r.node.id.clone()))
            .map(|mut r| {
                r.score += kind_rank_bonus(&r.node.kind);
                // Exact-name match boost: when the node's `name` equals the
                // query verbatim, surface it ahead of partial / qualified-name
                // matches. Without this, searching for a trait like
                // `LinearOperator` could be outranked by a `Method` whose
                // qualified name happens to contain `LinearOperator` (e.g.
                // a method declared inside the trait body), or by a `Field`
                // that shares the same simple name.
                if !trimmed_query.is_empty() && r.node.name == trimmed_query {
                    r.score += 10.0;
                }
                r
            })
            .collect();
        // Sort by kind tier first (definitions > references), then score
        // descending. Tier-first avoids any chance that a `use` re-export
        // (kind tier = `Use`) outscores a real definition because BM25
        // happened to weight the short re-export row highly. Score is the
        // secondary key so within a tier we still respect BM25.
        ranked.sort_by(|a, b| {
            kind_tier(&a.node.kind)
                .cmp(&kind_tier(&b.node.kind))
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        ranked.truncate(limit);
        Ok(ranked)
    }

    /// Returns aggregate statistics about the code graph.
    pub async fn get_stats(&self) -> Result<GraphStats> {
        self.db.get_stats().await
    }

    /// Retrieves a single node by its unique ID.
    pub async fn get_node(&self, id: &str) -> Result<Option<Node>> {
        self.db.get_node_by_id(id).await
    }

    /// Returns all nodes that transitively call the given node, up to `max_depth`.
    pub async fn get_callers(&self, node_id: &str, max_depth: usize) -> Result<Vec<(Node, Edge)>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_callers(node_id, max_depth).await
    }

    /// Returns all nodes that the given node transitively calls, up to `max_depth`.
    pub async fn get_callees(&self, node_id: &str, max_depth: usize) -> Result<Vec<(Node, Edge)>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_callees(node_id, max_depth).await
    }

    /// Computes the impact radius: all nodes that directly or indirectly
    /// depend on the given node, up to `max_depth`.
    pub async fn get_impact_radius(&self, node_id: &str, max_depth: usize) -> Result<Subgraph> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_impact_radius(node_id, max_depth).await
    }

    /// Same as `get_impact_radius` but multi-source: takes many seed node
    /// IDs and walks the union of their impact radii with a single shared
    /// `visited` set, so each downstream node is traversed at most once.
    pub async fn get_impact_radius_multi(
        &self,
        seed_ids: &[String],
        max_depth: usize,
    ) -> Result<Vec<Node>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_impact_radius_multi(seed_ids, max_depth).await
    }

    /// Finds the shortest directed call chain from `from_id` to `to_id`,
    /// following only outgoing `Calls` edges. Returns `None` if no chain
    /// exists within `max_depth` hops.
    pub async fn get_call_chain(
        &self,
        from_id: &str,
        to_id: &str,
        max_depth: usize,
    ) -> Result<Option<crate::graph::traversal::GraphPath>> {
        let traverser = GraphTraverser::new(&self.db);
        traverser
            .find_path_directed(from_id, to_id, &[crate::types::EdgeKind::Calls], max_depth)
            .await
    }

    /// Builds a bidirectional call graph around a node.
    pub async fn get_call_graph(&self, node_id: &str, depth: usize) -> Result<Subgraph> {
        let traverser = GraphTraverser::new(&self.db);
        traverser.get_call_graph(node_id, depth).await
    }

    /// Finds potentially dead code (nodes with no incoming edges).
    ///
    /// When `include_public` is `false` (the default), `pub` items are
    /// excluded — they may be referenced by code outside the indexed
    /// scope. Pass `true` to also surface pub items with zero indexed
    /// callers (useful for workspace-internal audits).
    pub async fn find_dead_code(
        &self,
        kinds: &[NodeKind],
        include_public: bool,
    ) -> Result<Vec<Node>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.find_dead_code(kinds, include_public).await
    }

    /// Returns all nodes for a given file, ordered by start line.
    pub async fn get_nodes_by_file(&self, file_path: &str) -> Result<Vec<Node>> {
        self.db.get_nodes_by_file(file_path).await
    }

    /// Returns every node in the database.
    pub async fn get_all_nodes(&self) -> Result<Vec<Node>> {
        self.db.get_all_nodes().await
    }

    /// Returns incoming edges to a target node.
    pub async fn get_incoming_edges(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.db.get_incoming_edges(node_id, &[]).await
    }

    /// Returns the subset of `candidate_ids` that have a `#[test]` annotation.
    pub async fn get_test_annotated_node_ids(
        &self,
        candidate_ids: &[String],
    ) -> Result<HashSet<String>> {
        self.db.get_test_annotated_node_ids(candidate_ids).await
    }

    /// Returns all file paths containing at least one `#[test]`-annotated function.
    pub async fn get_files_with_test_annotations(&self) -> Result<HashSet<String>> {
        self.db.get_files_with_test_annotations().await
    }

    /// Returns all node IDs marked with `/// skip-test-coverage`.
    pub async fn get_skip_test_coverage_node_ids(&self) -> Result<HashSet<String>> {
        self.db.get_skip_test_coverage_node_ids().await
    }

    /// Returns incoming edges for many target nodes in one round-trip.
    /// Empty `kinds` matches every edge kind.
    pub async fn get_incoming_edges_bulk(
        &self,
        target_ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        self.db.get_incoming_edges_bulk(target_ids, kinds).await
    }

    /// Returns all nodes whose `qualified_name` matches `qname`.
    /// Cross-run lookup independent of the content-hash node IDs.
    pub async fn get_nodes_by_qualified_name(&self, qname: &str) -> Result<Vec<Node>> {
        self.db.get_nodes_by_qualified_name(qname).await
    }

    /// Exact bare-name lookup using `idx_nodes_name`. No relevance scoring,
    /// no fuzzy matching — for that, use [`search`](Self::search).
    pub async fn get_nodes_by_name(&self, name: &str) -> Result<Vec<Node>> {
        self.db.get_nodes_by_name(name).await
    }

    /// Returns outgoing edges from a source node.
    pub async fn get_outgoing_edges(&self, node_id: &str) -> Result<Vec<Edge>> {
        self.db.get_outgoing_edges(node_id, &[]).await
    }

    /// Returns every edge in the database.
    pub async fn get_all_edges(&self) -> Result<Vec<Edge>> {
        self.db.get_all_edges().await
    }

    /// Returns nodes ranked by edge count for a given edge kind and direction,
    /// optionally filtered by node kind.
    pub async fn get_ranked_nodes_by_edge_kind(
        &self,
        edge_kind: &EdgeKind,
        node_kind: Option<&NodeKind>,
        incoming: bool,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64)>> {
        self.db
            .get_ranked_nodes_by_edge_kind(edge_kind, node_kind, incoming, path_prefix, limit)
            .await
    }

    /// Returns nodes ranked by line span, optionally filtered by node kind and path.
    pub async fn get_largest_nodes(
        &self,
        node_kind: Option<&NodeKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u32)>> {
        self.db
            .get_largest_nodes(node_kind, path_prefix, limit)
            .await
    }

    /// Returns files ranked by coupling (fan-in or fan-out).
    pub async fn get_file_coupling(
        &self,
        fan_in: bool,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, u64)>> {
        self.db.get_file_coupling(fan_in, path_prefix, limit).await
    }

    /// Returns classes/interfaces ranked by inheritance depth via extends chains.
    pub async fn get_inheritance_depth(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64)>> {
        self.db.get_inheritance_depth(path_prefix, limit).await
    }

    /// Returns node kind distribution, optionally filtered by path prefix.
    pub async fn get_node_distribution(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, String, u64)>> {
        self.db.get_node_distribution(path_prefix).await
    }

    /// Returns calls edges as (`source_id`, `target_id`) pairs for cycle detection.
    pub async fn get_call_edges(&self, path_prefix: Option<&str>) -> Result<Vec<(String, String)>> {
        self.db.get_call_edges(path_prefix).await
    }

    /// Returns calls edges as (`source_id`, `target_id`, `line`) tuples.
    pub async fn get_call_edges_with_lines(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<Vec<(String, String, Option<u32>)>> {
        self.db.get_call_edges_with_lines(path_prefix).await
    }

    /// Returns functions/methods ranked by composite complexity score.
    pub async fn get_complexity_ranked(
        &self,
        node_kind: Option<&NodeKind>,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u32, u64, u64, u64)>> {
        self.db
            .get_complexity_ranked(node_kind, path_prefix, limit)
            .await
    }

    /// Returns public symbols missing docstrings.
    pub async fn get_undocumented_public_symbols(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Node>> {
        self.db
            .get_undocumented_public_symbols(path_prefix, limit)
            .await
    }

    /// Returns classes ranked by member count (methods + fields).
    pub async fn get_god_classes(
        &self,
        path_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(Node, u64, u64, u64)>> {
        self.db.get_god_classes(path_prefix, limit).await
    }

    /// Detects circular dependencies at the file level.
    pub async fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.find_circular_dependencies().await
    }

    /// Builds an AI-ready context for a given task description.
    pub async fn build_context(
        &self,
        task: &str,
        options: &BuildContextOptions,
    ) -> Result<TaskContext> {
        let builder = ContextBuilder::new(&self.db, &self.project_root);
        builder.build_context(task, options).await
    }

    /// Returns all indexed file records.
    pub async fn get_all_files(&self) -> Result<Vec<FileRecord>> {
        self.db.get_all_files().await
    }

    /// Returns the `#[derive(...)]` names attached to the given node.
    ///
    /// The graph's `DerivesMacro` edges are unreliable here: the resolver
    /// fuzzy-binds std-trait names like `Debug` to nonsense nodes (a `Debug`
    /// enum variant in an unrelated test fixture) and the resulting unique
    /// constraint on `(source, target, kind, line)` collapses multiple
    /// distinct derives on the same type onto a single edge — so a struct
    /// that derives `Debug, Clone, PartialEq, Eq, Hash` may surface only one
    /// of them. Instead we re-read the lines between `attrs_start_line` and
    /// `start_line` of the node, which the extractor already promises to
    /// cover the leading attribute block, and parse `#[derive(...)]`
    /// attributes directly. Bounded file I/O — one read per call.
    pub async fn get_derives_for_node(&self, node_id: &str) -> Result<Vec<String>> {
        let Some(node) = self.db.get_node_by_id(node_id).await? else {
            return Ok(Vec::new());
        };
        let file_path = self.project_root().join(&node.file_path);
        let Ok(content) = std::fs::read_to_string(&file_path) else {
            return Ok(Vec::new());
        };
        Ok(parse_derives_in_attr_block(
            &content,
            node.attrs_start_line,
            node.start_line,
        ))
    }

    /// Finds the most specific (smallest-span) node whose source range
    /// contains the given `(file, line)` location.
    ///
    /// Returns `None` when no indexed node covers the location — typically
    /// because the file isn't indexed, or the line is in a region the
    /// extractor didn't capture (e.g. inside a `use` block or top-of-file
    /// comment). Lines are 1-based to match `rustc` / `clippy` output;
    /// `Node.start_line` / `end_line` are 0-based internally so we subtract
    /// before comparing.
    ///
    /// Implementation loads every node in the file (cached at the index
    /// layer) and picks the smallest containing span. At the typical ~50
    /// nodes per file this is faster than a custom range-query and stays
    /// honest about overlap (impl blocks contain methods, etc.).
    pub async fn node_at_location(&self, file: &str, line_1based: u32) -> Result<Option<Node>> {
        if line_1based == 0 {
            return Ok(None);
        }
        let zero_based = line_1based - 1;
        let normalized = normalize_lookup_path(self.project_root(), file);
        let mut nodes = self.db.get_nodes_by_file(&normalized).await?;
        nodes.retain(|n| n.start_line <= zero_based && n.end_line >= zero_based);
        // Prefer the smallest containing span — that's the most specific
        // owner of the source location.
        nodes.sort_by_key(|n| (n.end_line - n.start_line, n.start_line));
        Ok(nodes.into_iter().next())
    }

    /// Returns the indexed size in bytes for a file path, or `0` if unknown.
    /// Used to estimate the token cost of expanding a file in responses.
    pub async fn get_file_size_bytes(&self, path: &str) -> u64 {
        match self.db.get_file(path).await {
            Ok(Some(rec)) => rec.size,
            _ => 0,
        }
    }

    /// Returns `impl` blocks matching the given trait and/or implementing type.
    ///
    /// Both filters are optional:
    /// - With only `trait_name`: every impl of that trait, regardless of the
    ///   implementing type.
    /// - With only `type_name`: every impl block for that type (trait impls
    ///   and inherent impls).
    /// - With both: the intersection.
    /// - With neither: every `impl` node in the graph (use sparingly).
    ///
    /// Each result carries the impl node plus, when available, the resolved
    /// trait node it implements. Matching uses substring containment on the
    /// trait/type names so callers can pass either short or qualified names.
    pub async fn get_impls(
        &self,
        trait_name: Option<&str>,
        type_name: Option<&str>,
    ) -> Result<Vec<(Node, Option<Node>)>> {
        use crate::types::EdgeKind;

        // Candidate impl blocks.
        let mut impls = self.db.get_nodes_by_kind(NodeKind::Impl).await?;

        // Filter by implementing type if requested. The impl node's `name`
        // field holds the type identifier (e.g. "MyType" for `impl Foo for MyType`).
        if let Some(type_q) = type_name {
            impls.retain(|n| node_name_matches(n, type_q));
        }

        // Gather Implements edges per impl, then batch-fetch every trait node
        // in one `get_nodes_by_ids` call to avoid an N+1 across impl blocks.
        let mut per_impl_trait_id: Vec<Option<String>> = Vec::with_capacity(impls.len());
        let mut trait_target_ids: Vec<String> = Vec::new();
        for impl_node in &impls {
            let edges = self
                .db
                .get_outgoing_edges(&impl_node.id, &[EdgeKind::Implements])
                .await
                .unwrap_or_default();
            let target = edges.into_iter().next().map(|e| e.target);
            if let Some(ref t) = target {
                trait_target_ids.push(t.clone());
            }
            per_impl_trait_id.push(target);
        }
        let trait_nodes = if trait_target_ids.is_empty() {
            Vec::new()
        } else {
            self.db.get_nodes_by_ids(&trait_target_ids).await?
        };
        let trait_map: std::collections::HashMap<String, Node> =
            trait_nodes.into_iter().map(|n| (n.id.clone(), n)).collect();

        let mut out: Vec<(Node, Option<Node>)> = Vec::with_capacity(impls.len());
        for (impl_node, trait_id) in impls.into_iter().zip(per_impl_trait_id) {
            let trait_node = trait_id.and_then(|id| trait_map.get(&id).cloned());

            // Trait filter: drop inherent impls when a trait was requested.
            if let Some(trait_q) = trait_name {
                let matched = trait_node
                    .as_ref()
                    .is_some_and(|t| node_name_matches(t, trait_q));
                if !matched {
                    continue;
                }
            }

            out.push((impl_node, trait_node));
        }
        Ok(out)
    }

    /// Resolves a trait method node to the concrete method nodes that satisfy
    /// it across every `impl` block of the enclosing trait.
    ///
    /// Returns an empty vec when the input is not a method whose parent (via
    /// `Contains`) is a trait. Used by `tracedecay_callees` to surface concrete
    /// dispatch targets in addition to the trait method itself.
    pub async fn get_trait_dispatch_targets(&self, method: &Node) -> Result<Vec<Node>> {
        use crate::types::EdgeKind;

        // Only method-kind nodes can be trait methods.
        if !matches!(method.kind, NodeKind::Method | NodeKind::Function) {
            return Ok(Vec::new());
        }

        // Find the trait that contains this method. parent_id points at
        // the enclosing scope after v9; verify it's actually a Trait.
        let Some(parent_id) = method.parent_id.as_deref() else {
            return Ok(Vec::new());
        };
        let Some(trait_node) = self.db.get_node_by_id(parent_id).await? else {
            return Ok(Vec::new());
        };
        if trait_node.kind != NodeKind::Trait {
            return Ok(Vec::new());
        }

        // Find every impl block of that trait.
        let impl_edges = self
            .db
            .get_incoming_edges(&trait_node.id, &[EdgeKind::Implements])
            .await?;
        let impl_ids: Vec<String> = impl_edges.into_iter().map(|e| e.source).collect();
        if impl_ids.is_empty() {
            return Ok(Vec::new());
        }

        // For each impl block, surface the method whose name matches the
        // trait method. Multiple impls may share names with unrelated nodes,
        // so we filter by both kind and name.
        let mut targets = Vec::new();
        for impl_id in impl_ids {
            let candidates = self.db.get_children_of(&impl_id).await?;
            for n in candidates {
                if matches!(n.kind, NodeKind::Method | NodeKind::Function) && n.name == method.name
                {
                    targets.push(n);
                }
            }
        }
        Ok(targets)
    }

    /// Returns file paths that depend on the given file.
    pub async fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        let qm = GraphQueryManager::new(&self.db);
        qm.get_file_dependents(file_path).await
    }

    /// Returns a map of file path to approximate token count (size / 4).
    pub async fn get_file_token_map(&self) -> Result<HashMap<String, u64>> {
        let files = self.db.get_all_files().await?;
        Ok(files.into_iter().map(|f| (f.path, f.size / 4)).collect())
    }

    /// Returns the persisted tokens-saved counter.
    pub async fn get_tokens_saved(&self) -> Result<u64> {
        match self.db.get_metadata("tokens_saved").await? {
            Some(v) => Ok(v.parse::<u64>().unwrap_or(0)),
            None => Ok(0),
        }
    }

    /// Persists the tokens-saved counter to the database.
    pub async fn set_tokens_saved(&self, value: u64) -> Result<()> {
        self.db
            .set_metadata("tokens_saved", &value.to_string())
            .await
    }

    /// Returns the resettable project-local token counter.
    ///
    /// This is separate from the main `tokens_saved` counter and can be
    /// independently reset via [`Self::reset_local_counter`].
    pub async fn get_local_counter(&self) -> Result<u64> {
        match self.db.get_metadata("local_counter").await? {
            Some(v) => Ok(v.parse::<u64>().unwrap_or(0)),
            None => Ok(0),
        }
    }

    /// Resets the project-local token counter to zero.
    pub async fn reset_local_counter(&self) -> Result<()> {
        self.db.set_metadata("local_counter", "0").await
    }

    /// Increments the project-local token counter by the given amount.
    pub async fn add_local_counter(&self, delta: u64) -> Result<()> {
        let current = self.get_local_counter().await?;
        self.db
            .set_metadata("local_counter", &(current + delta).to_string())
            .await
    }

    /// Returns all nodes under a directory prefix filtered by kinds.
    pub async fn get_nodes_by_dir(&self, dir: &str, kinds: &[NodeKind]) -> Result<Vec<Node>> {
        self.db.get_nodes_by_dir(dir, kinds).await
    }

    /// Returns edges where both source and target are in the given node ID set.
    pub async fn get_internal_edges(&self, node_ids: &[String]) -> Result<Vec<Edge>> {
        self.db.get_internal_edges(node_ids).await
    }

    /// Checkpoints the WAL and closes the database connection.
    pub async fn checkpoint(&self) -> Result<()> {
        self.db.checkpoint().await
    }

    /// Consumes the code graph and closes the database connection.
    pub fn close(self) {
        self.db.close();
    }

    /// Runs VACUUM and ANALYZE to reclaim disk space and update planner stats.
    pub async fn optimize(&self) -> Result<()> {
        self.db.optimize().await
    }

    /// Returns a reference to the current configuration.
    pub fn get_config(&self) -> &TraceDecayConfig {
        &self.config
    }

    /// Returns the project root path.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Raw connection to the project database for crate-internal read layers
    /// (the dashboard HTTP server). Honors whatever branch DB `open` selected.
    pub(crate) fn dashboard_connection(&self) -> libsql::Connection {
        self.db.conn().clone()
    }

    /// Filesystem path of the project's tracedecay directory, for display in
    /// dashboard payloads (mirrors the `path` field of the Hermes plugin API).
    pub(crate) fn dashboard_db_path(&self) -> std::path::PathBuf {
        self.db_path()
    }

    fn ensure_branch_writable(&self, operation: &str) -> Result<()> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: format!("cannot {operation}: active TraceDecay store is open read-only"),
            });
        }

        if self.is_fallback() {
            let active = self.active_branch.as_deref().unwrap_or("detached HEAD");
            let serving = self.serving_branch.as_deref().unwrap_or("default branch");
            let hint = self.active_branch.as_deref().map_or_else(
                || " Check out a tracked branch before writing.".to_string(),
                |branch| format!(" Run `tracedecay branch add {branch}` before writing."),
            );

            return Err(TraceDecayError::Config {
                message: format!(
                    "cannot {operation}: active branch '{active}' is served from fallback branch \
                     '{serving}'.{hint}"
                ),
            });
        }

        // Branch-drift guard. A long-running MCP server resolves its branch DB
        // once at open time and caches `serving_branch`. If the working tree
        // switched branches since then, this instance still holds the *old*
        // branch's DB — a write would persist the new branch's files into the
        // wrong DB. Re-read the live branch and refuse when it no longer matches
        // the branch we serve. Single-DB mode (no branch metadata) leaves
        // `serving_branch == None` and is exempt: there is only one DB (#2).
        if let Some(serving) = self.serving_branch.as_deref() {
            let live = branch::current_branch(&self.project_root);
            if live.as_deref() != Some(serving) {
                let live_name = live.as_deref().unwrap_or("detached HEAD");
                return Err(TraceDecayError::Config {
                    message: format!(
                        "cannot {operation}: index is open for branch '{serving}' but the working \
                         tree is now on '{live_name}'. Reopen tracedecay so it serves '{live_name}' \
                         (e.g. restart the MCP server)."
                    ),
                });
            }
        }

        Ok(())
    }

    /// Returns `true` when the live git branch differs from the branch this
    /// instance resolved at open time (and branch tracking is active).
    ///
    /// A long-running MCP server resolves its branch DB once at open time; a
    /// mid-session `git checkout` makes the cached DB stale. Callers detect
    /// that here and reopen the correct branch DB via
    /// [`reopen_for_current_branch`](Self::reopen_for_current_branch) before
    /// serving reads or writes. The comparison is against the open-time branch
    /// (`active_branch`), so reopening clears the drift even when the new
    /// branch is untracked and legitimately falls back to an ancestor DB —
    /// avoiding a reopen loop. Returns `false` in single-DB mode (no branch
    /// metadata), where every branch maps to the same DB.
    pub fn branch_drifted(&self) -> bool {
        if self.serving_branch.is_none() {
            return false;
        }
        branch::current_branch(&self.project_root).as_deref() != self.active_branch.as_deref()
    }

    /// Reopens this project for the live git branch, returning a fresh instance
    /// bound to the correct branch DB. Use after [`branch_drifted`](Self::branch_drifted)
    /// reports drift so subsequent reads and writes target the right DB.
    pub async fn reopen_for_current_branch(&self) -> Result<Self> {
        Self::open_with_options(&self.project_root, self.open_options.clone()).await
    }

    /// Recompute the on-disk path to the `SQLite` DB this instance is
    /// serving. Useful for diagnostics (e.g. WAL/SHM size sampling) —
    /// returns the same path that `Database::open` was called with.
    pub fn db_path(&self) -> PathBuf {
        let (path, _, _) = Self::resolve_db_for_branch(
            &self.project_root,
            &self.store_layout.data_root,
            self.serving_branch.as_deref(),
        );
        path
    }

    pub fn store_layout(&self) -> &StoreLayout {
        &self.store_layout
    }

    pub(crate) fn open_options(&self) -> TraceDecayOpenOptions {
        self.open_options.clone()
    }

    pub async fn open_project_store_db(&self) -> Result<Database> {
        if self.read_only {
            return Err(TraceDecayError::Config {
                message: "cannot open project store for writing: active TraceDecay store is open read-only"
                    .to_string(),
            });
        }
        let (db, _) = Database::open(&self.store_layout.graph_db_path).await?;
        Ok(db)
    }

    pub async fn open_project_store_db_read_only(&self) -> Result<Database> {
        let (db, _) = Database::open_read_only(&self.store_layout.graph_db_path).await?;
        Ok(db)
    }

    fn build_branch_diagnostics(
        project_root: &Path,
        data_root: &Path,
        open_active_branch: Option<String>,
        serving_branch: Option<String>,
        fallback_warning: Option<String>,
        serving_db_path: PathBuf,
    ) -> BranchDiagnostics {
        let meta = branch_meta::load_branch_meta(data_root);
        let current_branch = branch::current_branch(project_root);
        let tracking_enabled = meta.as_ref().is_some_and(|m| !m.branches.is_empty());
        let branch_drifted =
            tracking_enabled && current_branch.as_deref() != open_active_branch.as_deref();
        let is_fallback = fallback_warning.is_some();
        let fallback_target = if is_fallback {
            serving_branch.clone()
        } else {
            None
        };
        let serving_db_exists = serving_db_path.exists();

        let (
            live_branch_tracked,
            live_branch_db_path,
            live_branch_db_exists,
            nearest_tracked_ancestor,
            nearest_tracked_ancestor_db_path,
            nearest_tracked_ancestor_db_exists,
        ) = if let (Some(meta), Some(current)) = (meta.as_ref(), current_branch.as_deref()) {
            let live_branch_tracked = meta.is_tracked(current);
            let live_branch_db_path = if live_branch_tracked {
                branch::resolve_branch_db_path(data_root, current, meta)
            } else {
                None
            };
            let live_branch_db_exists = live_branch_db_path.as_ref().map(|path| path.exists());
            let nearest_tracked_ancestor = if live_branch_tracked {
                None
            } else {
                branch::find_nearest_tracked_ancestor(project_root, current, meta)
            };
            let nearest_tracked_ancestor_db_path = nearest_tracked_ancestor
                .as_deref()
                .and_then(|ancestor| branch::resolve_branch_db_path(data_root, ancestor, meta));
            let nearest_tracked_ancestor_db_exists = nearest_tracked_ancestor_db_path
                .as_ref()
                .map(|path| path.exists());
            (
                live_branch_tracked,
                live_branch_db_path,
                live_branch_db_exists,
                nearest_tracked_ancestor,
                nearest_tracked_ancestor_db_path,
                nearest_tracked_ancestor_db_exists,
            )
        } else {
            (false, None, None, None, None, None)
        };

        let mut warnings = Vec::new();
        if branch_drifted {
            warnings.push(format!(
                "branch drift detected: working tree is on '{}' but this instance opened on '{}' and is still serving '{}'. Reopen the index so reads and writes target the live branch.",
                current_branch.as_deref().unwrap_or("detached HEAD"),
                open_active_branch.as_deref().unwrap_or("detached HEAD"),
                serving_branch.as_deref().unwrap_or("default branch"),
            ));
        }
        if !serving_db_exists {
            warnings.push(format!(
                "serving branch '{}' points at a missing DB: {}",
                serving_branch.as_deref().unwrap_or("default branch"),
                serving_db_path.display(),
            ));
        }
        if let (Some(current), Some(false), Some(path)) = (
            current_branch.as_deref(),
            live_branch_db_exists,
            live_branch_db_path.as_ref(),
        ) {
            warnings.push(format!(
                "tracked branch '{}' is listed in branch metadata but its DB is missing at '{}'; serving '{}' instead.",
                current,
                path.display(),
                serving_branch.as_deref().unwrap_or("default branch"),
            ));
        } else if is_fallback {
            match (
                current_branch.as_deref(),
                nearest_tracked_ancestor.as_deref(),
                fallback_target.as_deref(),
            ) {
                (Some(current), Some(ancestor), Some(target)) => warnings.push(format!(
                    "branch '{current}' is not tracked; nearest indexed ancestor is '{ancestor}' and tracedecay is serving '{target}' instead."
                )),
                (Some(current), None, Some(target)) => warnings.push(format!(
                    "branch '{current}' is not tracked and no indexed ancestor DB was available; tracedecay is serving '{target}' instead."
                )),
                _ => {}
            }
        }

        let branch_resolution = if !tracking_enabled {
            "single_db".to_string()
        } else if branch_drifted {
            "stale_serving_branch".to_string()
        } else if current_branch.is_none() {
            "detached_default".to_string()
        } else if is_fallback {
            match (
                nearest_tracked_ancestor.as_deref(),
                fallback_target.as_deref(),
            ) {
                (Some(ancestor), Some(target)) if ancestor == target => {
                    "fallback_ancestor".to_string()
                }
                _ => "fallback_default".to_string(),
            }
        } else {
            "exact".to_string()
        };

        let mut branches = Vec::new();
        if let Some(meta) = meta.as_ref() {
            let mut names: Vec<_> = meta.branches.keys().cloned().collect();
            names.sort();
            for name in names {
                let entry = &meta.branches[&name];
                let db_path = data_root.join(&entry.db_file);
                let db_exists = db_path.exists();
                let size_bytes = db_path.metadata().map_or(0, |metadata| metadata.len());
                let parent_db_path = entry
                    .parent
                    .as_deref()
                    .and_then(|parent| branch::resolve_branch_db_path(data_root, parent, meta));
                let parent_db_exists = parent_db_path.as_ref().map(|path| path.exists());
                let mut branch_warnings = Vec::new();
                if !db_exists {
                    branch_warnings.push(format!("missing DB at '{}'", db_path.display()));
                }
                if entry.parent.is_some() && parent_db_exists == Some(false) {
                    branch_warnings.push("parent DB is missing".to_string());
                }
                branches.push(TrackedBranchDiagnostic {
                    name: name.clone(),
                    db_file: entry.db_file.clone(),
                    db_path,
                    db_exists,
                    size_bytes,
                    parent: entry.parent.clone(),
                    parent_db_path,
                    parent_db_exists,
                    created_at: entry.created_at.clone(),
                    last_synced_at: entry.last_synced_at.clone(),
                    is_default: name == meta.default_branch,
                    is_current: current_branch.as_deref() == Some(name.as_str()),
                    is_open_active: open_active_branch.as_deref() == Some(name.as_str()),
                    is_serving: serving_branch.as_deref() == Some(name.as_str()),
                    warnings: branch_warnings,
                });
            }
        }

        BranchDiagnostics {
            tracking_enabled,
            default_branch: meta.as_ref().map(|m| m.default_branch.clone()),
            current_branch,
            open_active_branch,
            serving_branch,
            serving_db_path,
            serving_db_exists,
            branch_drifted,
            branch_resolution,
            is_fallback,
            fallback_target,
            fallback_warning,
            live_branch_tracked,
            live_branch_db_path,
            live_branch_db_exists,
            nearest_tracked_ancestor,
            nearest_tracked_ancestor_db_path,
            nearest_tracked_ancestor_db_exists,
            tracked_branch_count: branches.len(),
            branches,
            warnings,
        }
    }

    pub fn project_branch_diagnostics(project_root: &Path) -> BranchDiagnostics {
        let store_layout = storage::resolve_layout_for_current_profile(project_root)
            .unwrap_or_else(|_| {
                let profile_root = storage::default_profile_root()
                    .unwrap_or_else(|_| std::path::PathBuf::from(crate::config::TRACEDECAY_DIR));
                storage::default_profile_sharded_layout(project_root, &profile_root).unwrap_or_else(
                    |_| {
                        storage::profile_sharded_layout(
                            project_root,
                            &profile_root,
                            &storage::EnrollmentMarker {
                                project_id: storage::default_profile_project_id(project_root),
                                storage_mode: storage::StorageMode::ProfileSharded,
                            },
                        )
                        .unwrap_or_else(|err| {
                            panic!("default profile project id must be valid: {err}")
                        })
                    },
                )
            });
        let current_branch = branch::current_branch(project_root);
        let (serving_db_path, serving_branch, fallback_warning) = Self::resolve_db_for_branch(
            project_root,
            &store_layout.data_root,
            current_branch.as_deref(),
        );
        Self::build_branch_diagnostics(
            project_root,
            &store_layout.data_root,
            current_branch,
            serving_branch,
            fallback_warning,
            serving_db_path,
        )
    }

    pub fn branch_diagnostics(&self) -> BranchDiagnostics {
        Self::build_branch_diagnostics(
            &self.project_root,
            &self.store_layout.data_root,
            self.active_branch.clone(),
            self.serving_branch.clone(),
            self.fallback_warning.clone(),
            self.db_path(),
        )
    }

    /// Returns the active git branch, if any.
    pub fn active_branch(&self) -> Option<&str> {
        self.active_branch.as_deref()
    }

    /// Returns the branch whose DB is actually being served.
    pub fn serving_branch(&self) -> Option<&str> {
        self.serving_branch.as_deref()
    }

    /// Returns a fallback warning if serving from an ancestor branch DB.
    pub fn fallback_warning(&self) -> Option<&str> {
        self.fallback_warning.as_deref()
    }

    /// Returns true if serving from a fallback (ancestor) DB.
    pub fn is_fallback(&self) -> bool {
        self.fallback_warning.is_some()
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
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

// ---------------------------------------------------------------------------
// Session memory
// ---------------------------------------------------------------------------

const MAX_FACT_LIMIT: usize = 200;
const DEFAULT_FACT_LIMIT: usize = 20;

fn memory_database_error(operation: &str, message: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!("{operation} failed: {message}"),
        operation: operation.to_string(),
    }
}

fn fact_result_ids(results: &[FactSearchResult]) -> Vec<i64> {
    results.iter().map(|result| result.fact.fact_id).collect()
}

fn fact_ids(facts: &[FactRecord]) -> Vec<i64> {
    facts.iter().map(|fact| fact.fact_id).collect()
}

impl TraceDecay {
    /// Add a fact to the holographic memory store. The outcome carries the
    /// stored (or pre-existing) fact plus a write-time diff report
    /// (near-duplicate / possible-conflict / secret rejection).
    pub async fn add_fact(&self, request: AddFactRequest) -> Result<AddFactOutcome> {
        MemoryStore::new(self.db.conn())
            .add_fact(request, DEFAULT_TRUST)
            .await
    }

    /// Search facts by lexical overlap, entity metadata, category, and trust.
    pub async fn search_facts(&self, request: SearchFactsRequest) -> Result<Vec<FactSearchResult>> {
        let mut results = FactRetriever::new(self.db.conn())
            .search(
                &request.query,
                request.category,
                request.min_trust,
                request.limit.unwrap_or(DEFAULT_FACT_LIMIT),
            )
            .await?;
        if !request.include_why {
            for result in &mut results {
                result.why = None;
            }
        }
        MemoryStore::new(self.db.conn())
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    /// Search facts without updating recall/access counters. This is for
    /// background enrichment surfaces such as `tracedecay_context`, where a
    /// memory match is supporting context rather than an explicit recall.
    pub async fn search_facts_untracked(
        &self,
        request: SearchFactsRequest,
    ) -> Result<Vec<FactSearchResult>> {
        let db = self.open_project_store_db().await?;
        let mut results = FactRetriever::new(db.conn())
            .search_untracked(
                &request.query,
                request.category,
                request.min_trust,
                request.limit.unwrap_or(DEFAULT_FACT_LIMIT),
            )
            .await?;
        if !request.include_why {
            for result in &mut results {
                result.why = None;
            }
        }
        Ok(results)
    }

    pub async fn probe_entity(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let results = FactRetriever::new(self.db.conn())
            .probe(entity, category, min_trust, limit)
            .await?;
        MemoryStore::new(self.db.conn())
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    pub async fn related_facts(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let retriever = FactRetriever::new(self.db.conn());
        let related_entities = retriever.related(entity, limit).await?;
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();
        for related in related_entities {
            for result in retriever
                .probe(&related.name, category, min_trust, limit.saturating_mul(2))
                .await?
            {
                if seen.insert(result.fact.fact_id) {
                    results.push(result);
                    if results.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                        break;
                    }
                }
            }
            if results.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                break;
            }
        }
        MemoryStore::new(self.db.conn())
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    pub async fn reason_facts(
        &self,
        entities: &[String],
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let results = FactRetriever::new(self.db.conn())
            .reason(entities, category, min_trust, limit)
            .await?;
        MemoryStore::new(self.db.conn())
            .increment_retrieval_counts(&fact_result_ids(&results))
            .await?;
        Ok(results)
    }

    pub async fn contradict_facts(
        &self,
        category: Option<MemoryCategory>,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<ContradictionResult>> {
        let retriever = FactRetriever::new(self.db.conn());
        if let Some(category) = category {
            return retriever.contradict(category, threshold, limit).await;
        }

        let mut out = Vec::new();
        for category in [
            MemoryCategory::General,
            MemoryCategory::UserPref,
            MemoryCategory::Project,
            MemoryCategory::Tool,
            MemoryCategory::Decision,
            MemoryCategory::CodeArea,
        ] {
            out.extend(retriever.contradict(category, threshold, limit).await?);
            if out.len() >= limit.clamp(1, MAX_FACT_LIMIT) {
                out.truncate(limit.clamp(1, MAX_FACT_LIMIT));
                break;
            }
        }
        Ok(out)
    }

    pub async fn update_fact(&self, request: UpdateFactRequest) -> Result<FactRecord> {
        MemoryStore::new(self.db.conn()).update_fact(request).await
    }

    pub async fn remove_fact(&self, fact_id: i64) -> Result<bool> {
        MemoryStore::new(self.db.conn()).remove_fact(fact_id).await
    }

    pub async fn list_facts(
        &self,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactRecord>> {
        let facts = MemoryStore::new(self.db.conn())
            .list_facts(category, min_trust, limit)
            .await?;
        MemoryStore::new(self.db.conn())
            .increment_retrieval_counts(&fact_ids(&facts))
            .await?;
        Ok(facts)
    }

    pub async fn get_fact(&self, fact_id: i64) -> Result<Option<FactRecord>> {
        MemoryStore::new(self.db.conn()).get_fact(fact_id).await
    }

    pub async fn record_fact_feedback(&self, request: FeedbackRequest) -> Result<FeedbackResult> {
        MemoryStore::new(self.db.conn())
            .record_feedback_event(request)
            .await
    }

    pub async fn fact_trust_history(&self, fact_id: i64) -> Result<Vec<TrustHistoryEntry>> {
        MemoryStore::new(self.db.conn())
            .fact_trust_history(fact_id)
            .await
    }

    async fn repair_derived_memory_for_conn(
        conn: &libsql::Connection,
    ) -> Result<MemoryRepairStats> {
        let store = MemoryStore::new(conn);
        let mut missing_vectors_repaired = 0;
        loop {
            let repaired = store.compute_missing_vectors(500).await?;
            if repaired == 0 {
                break;
            }
            missing_vectors_repaired += repaired;
        }

        let banks_rebuilt = store.rebuild_dirty_banks().await?;

        Ok(MemoryRepairStats {
            missing_vectors_repaired,
            banks_rebuilt,
        })
    }

    pub(crate) async fn memory_status_for_conn(conn: &libsql::Connection) -> Result<MemoryStatus> {
        let operation = "memory_status";
        let repair = Self::repair_derived_memory_for_conn(conn).await?;
        let hrr_dim = HolographicEncoder::DIMENSIONS;
        let mut fact_rows = conn
            .query("SELECT trust_score FROM memory_facts", ())
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let row_err = |e: libsql::Error| memory_database_error(operation, e);
        let mut trust_0_025_count = 0_usize;
        let mut trust_025_050_count = 0_usize;
        let mut trust_050_075_count = 0_usize;
        let mut trust_075_100_count = 0_usize;
        let mut below_default_recall_threshold_count = 0_usize;
        let mut fact_count = 0_usize;
        while let Some(row) = fact_rows.next().await.map_err(row_err)? {
            fact_count += 1;
            let trust_score = row.get::<f64>(0).map_err(row_err)?;
            if trust_score < DEFAULT_MIN_TRUST {
                below_default_recall_threshold_count += 1;
            }
            if trust_score < 0.25 {
                trust_0_025_count += 1;
            } else if trust_score < 0.50 {
                trust_025_050_count += 1;
            } else if trust_score < 0.75 {
                trust_050_075_count += 1;
            } else {
                trust_075_100_count += 1;
            }
        }
        let mut entity_rows = conn
            .query("SELECT COUNT(*) FROM memory_entities", ())
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let entity_count = entity_rows
            .next()
            .await
            .map_err(row_err)?
            .map_or(Ok(0_i64), |row| row.get(0).map_err(row_err))?;
        let mut bank_rows = conn
            .query("SELECT COUNT(*) FROM memory_banks", ())
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let bank_count = bank_rows
            .next()
            .await
            .map_err(row_err)?
            .map_or(Ok(0_i64), |row| row.get(0).map_err(row_err))?;
        let mut aggregate_rows = conn
            .query(
                "SELECT COALESCE(SUM(helpful_count), 0),
                        COALESCE(SUM(unhelpful_count), 0),
                        COALESCE(SUM(CASE
                            WHEN hrr_vector IS NULL
                              OR hrr_algebra != 'amari_fhrr'
                              OR hrr_dim != ?1
                            THEN 1 ELSE 0 END), 0)
                 FROM memory_facts",
                libsql::params![hrr_dim as i64],
            )
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let Some(aggregate_row) = aggregate_rows.next().await.map_err(row_err)? else {
            return Err(memory_database_error(
                operation,
                "memory aggregate query returned no rows",
            ));
        };
        let helpful_count = aggregate_row.get::<i64>(0).map_err(row_err)?;
        let unhelpful_count = aggregate_row.get::<i64>(1).map_err(row_err)?;
        let missing_vector_count = aggregate_row.get::<i64>(2).map_err(row_err)?;
        let mut backfill_rows = conn
            .query(
                "SELECT COUNT(*) FROM memory_facts
                 WHERE json_extract(metadata, '$.holographic_memory_backfill_v1') = 1",
                (),
            )
            .await
            .map_err(|e| memory_database_error(operation, e))?;
        let backfilled_count = backfill_rows
            .next()
            .await
            .map_err(row_err)?
            .map_or(Ok(0_i64), |row| row.get(0).map_err(row_err))?;
        let estimated_capacity = (hrr_dim as f64 / (hrr_dim as f64).ln()).round() as usize;
        Ok(MemoryStatus {
            fact_count,
            entity_count: entity_count as usize,
            bank_count: bank_count as usize,
            algebra_name: "amari_fhrr".to_string(),
            hrr_dim,
            estimated_capacity,
            trust_0_025_count,
            trust_025_050_count,
            trust_050_075_count,
            trust_075_100_count,
            below_default_recall_threshold_count,
            helpful_count: helpful_count as usize,
            unhelpful_count: unhelpful_count as usize,
            missing_vector_count: missing_vector_count as usize,
            legacy_backfill_complete: backfilled_count > 0,
            repair,
        })
    }

    pub async fn memory_status(&self) -> Result<MemoryStatus> {
        Self::memory_status_for_conn(self.db.conn()).await
    }

    pub async fn project_memory_status(&self) -> Result<MemoryStatus> {
        let db = self.open_project_store_db().await?;
        Self::memory_status_for_conn(db.conn()).await
    }
}

// ---------------------------------------------------------------------------
// Shared utilities
// ---------------------------------------------------------------------------

/// Search-result rank bonus applied per node kind, so symbol *definitions*
/// outrank mere *references* (use statements, annotation usages, modules)
/// that BM25 may otherwise score equally. Tuned so a definition with a
/// slightly worse BM25 score still surfaces above its imports.
///
/// Exhaustive match by design: when a new `NodeKind` variant is added the
/// compiler will force a re-tune here rather than silently defaulting it to
/// `0.0`, matching the project rule "crash hard if there is an unknown
/// value".
/// Coarse ranking tier used as the primary sort key in `search`. Lower
/// numbers sort first. The tiers separate "real definitions" (functions,
/// types, traits, …) from "references" (`use`, `module`, annotation usage)
/// so a re-export can never beat the thing it re-exports, no matter what
/// BM25 produces for the row.
fn kind_tier(kind: &NodeKind) -> u8 {
    match kind {
        // Tier 0: callable definitions and type definitions — the
        // "what is this?" answers a user usually wants when searching by
        // symbol name.
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::Constructor
        | NodeKind::AbstractMethod
        | NodeKind::ArrowFunction
        | NodeKind::Procedure
        | NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::Interface
        | NodeKind::InterfaceType
        | NodeKind::Record
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass
        | NodeKind::TypeAlias
        | NodeKind::Union
        | NodeKind::Typedef
        | NodeKind::Mixin
        | NodeKind::Extension
        | NodeKind::Delegate
        | NodeKind::Template
        | NodeKind::PascalRecord
        | NodeKind::ScalaObject
        | NodeKind::KotlinObject
        | NodeKind::CompanionObject
        | NodeKind::Annotation
        | NodeKind::Event => 0,
        // Proto definitions (feature-gated)
        #[cfg(feature = "lang-protobuf")]
        NodeKind::ProtoMessage | NodeKind::ProtoService | NodeKind::ProtoRpc => 0,
        // Tier 1: impl blocks — between definitions and references.
        NodeKind::Impl => 1,
        // Tier 2: values, macros, members of types.
        NodeKind::Const
        | NodeKind::Static
        | NodeKind::Macro
        | NodeKind::PreprocessorDef
        | NodeKind::EnumVariant
        | NodeKind::Field
        | NodeKind::ValField
        | NodeKind::VarField
        | NodeKind::Property
        | NodeKind::CSharpProperty
        | NodeKind::StructTag
        | NodeKind::InitBlock
        | NodeKind::Export => 2,
        // Tier 3: containers (module, namespace, …) — usually not the
        // answer to "find symbol".
        NodeKind::Module
        | NodeKind::Package
        | NodeKind::Namespace
        | NodeKind::ScalaPackage
        | NodeKind::GoPackage
        | NodeKind::KotlinPackage
        | NodeKind::PascalUnit
        | NodeKind::Library
        | NodeKind::File
        | NodeKind::GenericParam
        | NodeKind::PascalProgram => 3,
        // Tier 4: pure references / annotations — always rank last.
        NodeKind::Use | NodeKind::Include | NodeKind::AnnotationUsage | NodeKind::Decorator => 4,
    }
}

fn kind_rank_bonus(kind: &NodeKind) -> f64 {
    match kind {
        // Callable definitions
        NodeKind::Function
        | NodeKind::Method
        | NodeKind::StructMethod
        | NodeKind::Constructor
        | NodeKind::AbstractMethod
        | NodeKind::ArrowFunction
        | NodeKind::Procedure => 3.0,
        // Type definitions
        NodeKind::Struct
        | NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Class
        | NodeKind::InnerClass
        | NodeKind::Interface
        | NodeKind::InterfaceType
        | NodeKind::Record
        | NodeKind::CaseClass
        | NodeKind::DataClass
        | NodeKind::SealedClass
        | NodeKind::TypeAlias
        | NodeKind::Union
        | NodeKind::Typedef
        | NodeKind::Mixin
        | NodeKind::Extension
        | NodeKind::Delegate
        | NodeKind::Template
        | NodeKind::PascalRecord
        | NodeKind::ScalaObject
        | NodeKind::KotlinObject
        | NodeKind::CompanionObject
        | NodeKind::Annotation
        | NodeKind::Event => 2.5,
        // Proto definitions
        #[cfg(feature = "lang-protobuf")]
        NodeKind::ProtoMessage | NodeKind::ProtoService | NodeKind::ProtoRpc => 2.5,
        // Impl blocks (between defs and refs)
        NodeKind::Impl => 2.0,
        // Values, macros, preprocessor defs
        NodeKind::Const
        | NodeKind::Static
        | NodeKind::Macro
        | NodeKind::PreprocessorDef
        | NodeKind::EnumVariant => 1.0,
        // Members of types
        NodeKind::Field
        | NodeKind::ValField
        | NodeKind::VarField
        | NodeKind::Property
        | NodeKind::CSharpProperty
        | NodeKind::StructTag
        | NodeKind::InitBlock
        | NodeKind::Export => 0.5,
        // File / generic-parameter — neutral
        NodeKind::File | NodeKind::GenericParam | NodeKind::PascalProgram => 0.0,
        // References & containers — push below definitions
        NodeKind::Use | NodeKind::Include => -3.0,
        NodeKind::AnnotationUsage | NodeKind::Decorator => -2.0,
        NodeKind::Module
        | NodeKind::Package
        | NodeKind::Namespace
        | NodeKind::ScalaPackage
        | NodeKind::GoPackage
        | NodeKind::KotlinPackage
        | NodeKind::PascalUnit
        | NodeKind::Library => -1.5,
    }
}

/// Parses every `#[derive(A, B, C)]` attribute appearing in `content`
/// between (0-based, inclusive) `start_line` and `end_line`. Multiple
/// derive attributes stack — `#[derive(Debug)]` and `#[derive(Clone)]` on
/// the same item both contribute. The returned list is de-duplicated and
/// preserves source order (Debug before Clone if that's how they're
/// written).
fn parse_derives_in_attr_block(content: &str, start_line: u32, end_line: u32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let lines: Vec<&str> = content.lines().collect();
    let start = start_line as usize;
    let end = (end_line as usize).min(lines.len().saturating_sub(1));
    if start >= lines.len() {
        return out;
    }
    // Join the attribute block into a single string so multi-line
    // `#[derive(\n  Debug,\n  Clone,\n)]` (rustfmt's split form for long
    // derive lists) is handled uniformly with the single-line variant.
    let block = lines[start..=end].join("\n");
    let mut search_from = 0usize;
    while let Some(start_idx) = block[search_from..].find("#[derive(") {
        let abs_start = search_from + start_idx + "#[derive(".len();
        let Some(close_offset) = block[abs_start..].find(')') else {
            break;
        };
        let inner = &block[abs_start..abs_start + close_offset];
        for name in inner.split(',') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            // Strip the path prefix on fully-qualified derives so callers
            // see `Serialize` not `serde::Serialize`. Matches the convention
            // the static derive table uses.
            let short = name.rsplit("::").next().unwrap_or(name).to_string();
            if seen.insert(short.clone()) {
                out.push(short);
            }
        }
        search_from = abs_start + close_offset + 1;
    }
    out
}

/// Normalises an external file path (typically from a `cargo check` /
/// `cargo clippy` diagnostic span) into the project-relative,
/// forward-slash form the index stores. Handles three real-world shapes:
///
/// - Absolute paths (cargo emits them when `--manifest-path` points at a
///   project root that differs from `cwd`): strip the `project_root`
///   prefix so `/abs/path/to/project/src/lib.rs` becomes `src/lib.rs`.
/// - Backslash paths (Windows cargo): convert `\` → `/`.
/// - Already-relative forward-slash paths: pass through unchanged.
///
/// Falls back to returning the input verbatim if no transformation
/// applies — `get_nodes_by_file` will then handle "no such file" the
/// same way it always does.
fn normalize_lookup_path(project_root: &std::path::Path, raw: &str) -> String {
    let forward = raw.replace('\\', "/");
    let path = std::path::Path::new(&forward);
    if path.is_absolute() {
        // Try canonicalising both sides; canonicalisation handles
        // symlinks, `..` segments, and trailing slashes uniformly. If
        // either fails (file doesn't exist on disk, project root
        // moved), fall back to a raw prefix strip.
        if let (Ok(abs), Ok(root)) = (path.canonicalize(), project_root.canonicalize()) {
            if let Ok(rel) = abs.strip_prefix(&root) {
                return rel.to_string_lossy().replace('\\', "/");
            }
        }
        let root_str = project_root.to_string_lossy();
        if let Some(rel) = forward.strip_prefix(root_str.as_ref()) {
            return rel.trim_start_matches('/').to_string();
        }
    }
    forward
}

/// True when the user-supplied query matches either the node's short `name`
/// or its `qualified_name`. Matching is exact on the short name and substring
/// on the qualified name, so callers can pass either form for the impl/trait
/// filter on `tracedecay_impls`.
fn node_name_matches(node: &Node, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    node.name == query || node.qualified_name == query || node.qualified_name.contains(query)
}

/// Returns `true` if the file path looks like a test file.
pub fn is_test_file(path: &str) -> bool {
    let test_segments = [
        "test/",
        "tests/",
        "__tests__/",
        "spec/",
        "e2e/",
        ".test.",
        ".spec.",
        "_test.",
        "_spec.",
    ];
    let lower = path.to_ascii_lowercase();
    test_segments.iter().any(|s| lower.contains(s))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod derive_parse_tests {
    use super::parse_derives_in_attr_block;

    #[test]
    fn parses_single_derive_block() {
        let src = "\
#[derive(Debug, Clone, PartialEq)]
pub struct Foo;
";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Debug", "Clone", "PartialEq"]);
    }

    #[test]
    fn stacks_multiple_derive_attributes() {
        let src = "\
#[derive(Debug)]
#[derive(Clone, Hash)]
pub enum K {}
";
        let derives = parse_derives_in_attr_block(src, 0, 2);
        assert_eq!(derives, vec!["Debug", "Clone", "Hash"]);
    }

    #[test]
    fn strips_path_prefix_on_qualified_derive() {
        let src = "#[derive(serde::Serialize, Debug)]\npub struct S;\n";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Serialize", "Debug"]);
    }

    #[test]
    fn ignores_non_derive_attributes() {
        let src = "\
#[cfg(feature = \"foo\")]
#[serde(rename = \"x\")]
#[derive(Debug)]
pub struct S;
";
        let derives = parse_derives_in_attr_block(src, 0, 3);
        assert_eq!(derives, vec!["Debug"]);
    }

    #[test]
    fn deduplicates_repeated_derives() {
        let src = "#[derive(Debug, Debug, Clone)]\npub struct S;\n";
        let derives = parse_derives_in_attr_block(src, 0, 1);
        assert_eq!(derives, vec!["Debug", "Clone"]);
    }

    /// Regression: rustfmt splits long derive lists across lines:
    ///   `#[derive(\n    Debug,\n    Clone,\n    PartialEq,\n)]`
    /// The previous line-bounded parser dropped all of these because it
    /// only matched `#[derive(...)]` when the closing `)` was on the
    /// same line. Production codebases with realistic-sized derive
    /// lists were getting empty `derives` output.
    #[test]
    fn parses_multiline_derive_attribute() {
        let src = "\
#[derive(
    Debug,
    Clone,
    PartialEq,
)]
pub struct Wide;
";
        let derives = parse_derives_in_attr_block(src, 0, 5);
        assert_eq!(derives, vec!["Debug", "Clone", "PartialEq"]);
    }

    #[test]
    fn parses_multiline_derive_mixed_with_single_line() {
        let src = "\
#[derive(Debug)]
#[derive(
    Clone,
    Hash,
)]
pub struct M;
";
        let derives = parse_derives_in_attr_block(src, 0, 5);
        assert_eq!(derives, vec!["Debug", "Clone", "Hash"]);
    }
}
