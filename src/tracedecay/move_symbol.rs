//! `move_symbol`: relocate a function (Rust-first, provider-agnostic shape)
//! from its file to a destination file. The centerpiece is the post-move
//! **impact report** — every reference, dependency, visibility, or module
//! concern the move raises, surfaced as evidence-based, actionable hints
//! derived from the code graph (callers/callees) and parse-level facts
//! (identifiers, `use` lines, module declarations). Never regex-only guessing.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use tree_sitter::{Node as TsNode, Parser};

use crate::errors::{Result, TraceDecayError};
use crate::types::{MoveHint, MoveResult, Node, NodeKind, Visibility};
use tracedecay_code_extraction::source_mask::{MaskOptions, masked_rust_source_with};

use super::TraceDecay;
use super::edits::{
    MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff, capture_planned_source_edit,
    edit_success_message, publish_planned_source_edit, resolve_symbol_for_edit,
    validate_planned_source_edit,
};

impl TraceDecay {
    /// Moves a resolved symbol from its current file to `dest_file`.
    ///
    /// `dry_run` (the default at the tool layer) computes the removal span, the
    /// destination shape, the combined preview diff, and the full impact report
    /// while writing nothing. `dry_run = false` performs the move (remove the
    /// span — docs/attrs included — from the source, insert it at the
    /// destination with a blank-line separator, and auto-insert unambiguous
    /// needed imports), then returns the same impact report of everything that
    /// still needs manual attention (callers, module declaration, visibility).
    ///
    /// `update_references` is reserved for a future version; in v1 caller
    /// references are never auto-edited — the exact change rides in the hints.
    pub(crate) async fn move_symbol(
        &self,
        symbol: &str,
        dest_file: &str,
        dry_run: bool,
        _update_references: bool,
    ) -> Result<MoveResult> {
        let target = resolve_symbol_for_edit(self, symbol).await?;
        let symbol_label = format!("{} ({})", target.name, target.kind.as_str());
        let source_rel = target.file_path.clone();
        let dest_rel = self.resolve_dest_rel(dest_file)?;

        let fail = |message: String, impact: Vec<MoveHint>| MoveResult {
            success: false,
            symbol: symbol_label.clone(),
            source_file: source_rel.clone(),
            dest_file: dest_rel.clone(),
            moved_span: None,
            dry_run,
            diff: None,
            applied_imports: Vec::new(),
            impact,
            message,
        };

        if dest_rel == source_rel {
            return Ok(fail(
                format!("destination is the symbol's own file ({source_rel}); nothing to move"),
                Vec::new(),
            ));
        }

        let source_abs = self.project_root.join(&source_rel);
        let dest_abs = self.project_root.join(&dest_rel);
        validate_write_containment(&self.project_root, &source_abs, "source")?;
        validate_write_containment(&self.project_root, &dest_abs, "destination")?;
        if same_existing_file(&source_abs, &dest_abs) {
            return Ok(fail(
                format!(
                    "destination resolves to the symbol's own file ({source_rel}); nothing to move"
                ),
                Vec::new(),
            ));
        }
        // The source write now goes through the project-root-scoped source edit
        // authority, so only the resolver's validation is still needed here (it
        // errors when the path cannot be inspected). The destination path is
        // still used directly by the rollback below.
        write_path_preserving_final_symlink(&source_abs, "source")?;
        let dest_write_abs = write_path_preserving_final_symlink(&dest_abs, "destination")?;
        let source = std::fs::read_to_string(&source_abs).map_err(|e| TraceDecayError::Config {
            message: format!("failed to read {source_rel}: {e}"),
        })?;
        let src_lines: Vec<&str> = source.lines().collect();

        // Mirror replace_symbol's span semantics but ALWAYS include the leading
        // doc-comment / attribute block: a moved item carries its own docs.
        let mut start = target.attrs_start_line as usize;
        let end_inclusive = (target.end_line as usize).min(src_lines.len().saturating_sub(1));
        if start >= src_lines.len() || start > end_inclusive {
            return Ok(fail(
                format!(
                    "symbol span [{}..={}] out of bounds for {}-line file",
                    start,
                    target.end_line,
                    src_lines.len()
                ),
                Vec::new(),
            ));
        }
        // A contiguous leading `//!` inner module-doc line (no blank line before
        // the item) can never belong to the moved item — inner docs attach to the
        // enclosing module, not the following item. If `attrs_start_line` picked
        // up such a line, advance past it so the source keeps its module doc and
        // the destination doesn't receive a stray `//!` mid-file (a hard E0753).
        while start < end_inclusive && src_lines[start].trim_start().starts_with("//!") {
            start += 1;
        }
        let moved_text = src_lines[start..=end_inclusive].join("\n");

        // Destination collision: refuse rather than clobber.
        let dest_nodes = self.get_nodes_by_file(&dest_rel).await.unwrap_or_default();
        if let Some(clash) = dest_nodes
            .iter()
            .find(|n| n.name == target.name && is_importable_item(&n.kind))
        {
            let hint = MoveHint {
                kind: "collision".to_string(),
                file: dest_rel.clone(),
                line: Some(clash.start_line + 1),
                detail: format!(
                    "destination already defines `{}` ({})",
                    clash.name,
                    clash.kind.as_str()
                ),
                suggestion: Some(
                    "rename one of them or choose a different destination".to_string(),
                ),
            };
            return Ok(fail(
                format!("destination {dest_rel} already defines `{}`", target.name),
                vec![hint],
            ));
        }

        // Build the source with the span removed (with blank-line cleanup).
        let trailing_newline = source.ends_with('\n');
        let residual = remove_span_with_cleanup(&src_lines, start, end_inclusive);
        let mut source_modified = residual.join("\n");
        if trailing_newline && !source_modified.is_empty() {
            source_modified.push('\n');
        }

        // Dependency + import analysis: what the moved body needs at the
        // destination, and which of those we can auto-insert unambiguously.
        // Read the destination, distinguishing "does not exist yet" (a fresh
        // destination file) from "exists but unreadable" (e.g. non-UTF8). Only
        // the former is treated as empty; an unreadable existing file must refuse
        // rather than be silently clobbered.
        let (dest_original, dest_existed) = match std::fs::read_to_string(&dest_abs) {
            Ok(text) => (text, true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
            Err(e) => {
                return Ok(fail(
                    format!("failed to read destination {dest_rel}: {e}"),
                    Vec::new(),
                ));
            }
        };
        let dest_module = rust_module_path(&dest_rel);
        let src_module = rust_module_path(&source_rel);
        let analysis = self
            .analyze_dependencies(
                &target,
                &source_rel,
                &dest_rel,
                &source,
                &source_modified,
                &moved_text,
            )
            .await?;

        // Assemble the destination content.
        let applied_imports = dedup_preserve(&analysis.auto_imports, &dest_original);
        let dest_modified = build_dest_content(&dest_original, &applied_imports, &moved_text);

        // Impact report.
        let mut impact = analysis.hints;
        impact.extend(
            self.caller_hints(
                &target,
                &source_rel,
                &dest_rel,
                src_module.as_deref(),
                dest_module.as_deref(),
            )
            .await?,
        );
        if let Some(hint) = self.module_missing_hint(&dest_rel).await {
            impact.push(hint);
        }
        impact.extend(cfg_context_hints(&moved_text, &dest_rel));
        impact.extend(cycle_risk_hints(
            &dest_original,
            &dest_rel,
            src_module.as_deref(),
        ));

        if dry_run {
            capture_planned_source_edit(
                &source_rel,
                Some(source.as_str()),
                Some(source_modified.as_str()),
            );
            capture_planned_source_edit(
                &dest_rel,
                dest_existed.then_some(dest_original.as_str()),
                Some(dest_modified.as_str()),
            );
            let diff = combined_diff(
                &source_rel,
                &source,
                &source_modified,
                &dest_rel,
                &dest_original,
                &dest_modified,
            );
            return Ok(MoveResult {
                success: true,
                symbol: symbol_label,
                source_file: source_rel,
                dest_file: dest_rel,
                moved_span: Some(moved_text),
                dry_run: true,
                diff: Some(diff),
                applied_imports,
                impact,
                message: edit_success_message(true, "move previewed"),
            });
        }

        // Apply only if both files still match the snapshots used to build the
        // move. Dependency analysis can take long enough for another agent or
        // editor to change either file; blindly writing those stale snapshots
        // would discard unrelated work.
        validate_planned_source_edit(
            &source_rel,
            Some(source.as_str()),
            Some(source_modified.as_str()),
        )?;
        validate_planned_source_edit(
            &dest_rel,
            dest_existed.then_some(dest_original.as_str()),
            Some(dest_modified.as_str()),
        )?;
        ensure_text_unchanged(&source_abs, Some(&source), &source_rel)?;
        ensure_text_unchanged(
            &dest_abs,
            dest_existed.then_some(dest_original.as_str()),
            &dest_rel,
        )?;

        // Write each file through an atomic sibling rename. Destination first
        // deliberately leaves a duplicate symbol (recoverable) rather than a
        // missing one if the process stops between the two commits.
        publish_planned_source_edit(
            &self.project_root,
            &dest_rel,
            dest_existed.then_some(dest_original.as_str()),
            &dest_modified,
        )?;
        if let Err(e) = ensure_text_unchanged(&source_abs, Some(&source), &source_rel)
            .and_then(|()| ensure_text_unchanged(&dest_abs, Some(&dest_modified), &dest_rel))
            .and_then(|()| {
                publish_planned_source_edit(
                    &self.project_root,
                    &source_rel,
                    Some(source.as_str()),
                    &source_modified,
                )
            })
        {
            // Roll back so a half-applied move leaves no trace. If the
            // destination existed before, restore its original bytes; if we
            // created it, delete it (and any now-empty parent dirs we created)
            // rather than leaving an empty file behind. Never overwrite a
            // third party's destination edit while rolling back.
            let rollback = if std::fs::read_to_string(&dest_abs).ok().as_deref()
                == Some(dest_modified.as_str())
            {
                if dest_existed {
                    crate::agents::safe_write_text_file(&dest_write_abs, &dest_original, None)
                } else {
                    std::fs::remove_file(&dest_write_abs).map_err(|remove_error| {
                        TraceDecayError::Config {
                            message: format!(
                                "failed to remove newly-created destination {dest_rel}: {remove_error}"
                            ),
                        }
                    })
                }
            } else {
                Err(TraceDecayError::Config {
                    message: format!(
                        "destination {dest_rel} changed concurrently; refusing to overwrite it during rollback"
                    ),
                })
            };
            if !dest_existed && rollback.is_ok() {
                let mut dir = dest_abs.parent().map(Path::to_path_buf);
                while let Some(d) = dir {
                    if d == self.project_root {
                        break;
                    }
                    // `remove_dir` removes only empty dirs and errors otherwise,
                    // so this naturally stops at the first non-empty ancestor.
                    if std::fs::remove_dir(&d).is_err() {
                        break;
                    }
                    dir = d.parent().map(Path::to_path_buf);
                }
            }
            return Err(TraceDecayError::Config {
                message: match rollback {
                    Ok(()) => format!(
                        "move aborted before writing {source_rel}: {e}; destination rolled back"
                    ),
                    Err(rollback_error) => format!(
                        "move aborted before writing {source_rel}: {e}; rollback incomplete: {rollback_error}"
                    ),
                },
            });
        }
        // A move changes symbol identity because the destination path is part
        // of the node ID. Reindexing the two files independently can drop
        // unchanged callers: the destination is indexed while the old source
        // symbol still exists, then deleting that old node removes its incoming
        // call edges without re-extracting the caller. Run the daemon-owned
        // project sync once over the completed two-file move so both files are
        // indexed in one generation and affected references are re-resolved
        // before a follow-up refactor starts.
        self.sync().await?;

        Ok(MoveResult {
            success: true,
            symbol: symbol_label,
            source_file: source_rel,
            dest_file: dest_rel,
            moved_span: Some(moved_text),
            dry_run: false,
            diff: None,
            applied_imports,
            impact,
            message: "move applied".to_string(),
        })
    }

    /// Normalizes `dest_file` (absolute or project-relative) to a
    /// project-relative, forward-slash path, rejecting escapes.
    fn resolve_dest_rel(&self, dest_file: &str) -> Result<String> {
        let raw = Path::new(dest_file);
        let rel: PathBuf = if raw.is_absolute() {
            raw.strip_prefix(&self.project_root)
                .map_err(|_| TraceDecayError::Config {
                    message: "destination is not within the project".to_string(),
                })?
                .to_path_buf()
        } else {
            PathBuf::from(dest_file)
        };
        // Canonicalize the relative path BEFORE the equality guard and collision
        // check compare it: reject `..` escapes and drop `.` (CurDir) components,
        // rebuilding from `Component::Normal` parts only. Without this a
        // `./src/pricing.rs` destination compares unequal to the graph's
        // normalized `src/pricing.rs`, slipping past the same-file guard and the
        // collision check — the apply would then write and truncate the very same
        // inode, silently deleting the symbol.
        let mut normalized = PathBuf::new();
        for comp in rel.components() {
            match comp {
                Component::Normal(part) => normalized.push(part),
                Component::ParentDir => {
                    return Err(TraceDecayError::Config {
                        message: "destination path must not contain '..'".to_string(),
                    });
                }
                // Drop `.` (CurDir) so `./x` canonicalizes to `x`, and drop any
                // root/prefix components (they cannot survive `strip_prefix`
                // above) rather than embed them in a relative path.
                Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            }
        }
        let s = normalized.to_string_lossy().replace('\\', "/");
        if s.is_empty() {
            return Err(TraceDecayError::Config {
                message: "destination path is empty".to_string(),
            });
        }
        Ok(s)
    }

    /// Dependency analysis for the moved body: same-file symbols and source
    /// `use`-imports the body references that will no longer resolve at the
    /// destination. Produces auto-insertable imports plus hints for the rest.
    async fn analyze_dependencies(
        &self,
        target: &Node,
        source_rel: &str,
        dest_rel: &str,
        source: &str,
        source_modified: &str,
        moved_text: &str,
    ) -> Result<DependencyAnalysis> {
        let src_module = rust_module_path(source_rel);
        let mut out = DependencyAnalysis::default();
        // Scan only real code: a doc-comment mention (e.g. ``[`compute_total`]``)
        // and identifiers that appear only inside string literals are not
        // dependencies, so comments and strings are masked before tokenizing.
        // Implicit format captures (`format!("{helper}")`) survive the mask and
        // remain counted as real uses.
        let code_only = masked_rust_source_with(moved_text, MaskOptions::UNUSED_IMPORTS);
        let idents = body_identifiers(&code_only);
        let source_code_only =
            masked_rust_source_with(source_modified, MaskOptions::UNUSED_IMPORTS);

        // 1. Same-file item dependencies (structs, enums, helpers, consts, …).
        let src_nodes = self.get_nodes_by_file(source_rel).await.unwrap_or_default();
        let mut handled: HashSet<String> = HashSet::new();
        for node in &src_nodes {
            if node.id == target.id || node.name == target.name || !is_importable_item(&node.kind) {
                continue;
            }
            if node.name.is_empty() || !idents.contains(&node.name) {
                continue;
            }
            if !handled.insert(node.name.clone()) {
                continue;
            }
            let module = src_module.as_deref().unwrap_or("crate");
            let use_line = format!("use {module}::{};", node.name);
            let visible = matches!(node.visibility, Visibility::Pub | Visibility::PubCrate);
            if visible {
                out.auto_imports.push(use_line);
            } else {
                out.hints.push(MoveHint {
                    kind: "dependency_broken".to_string(),
                    file: dest_rel.to_string(),
                    line: None,
                    detail: format!(
                        "moved body depends on `{}` ({}), which is {} in {source_rel}",
                        node.name,
                        node.kind.as_str(),
                        visibility_word(&node.visibility)
                    ),
                    suggestion: Some(format!(
                        "add `{use_line}` at the destination and escalate `{}` to `pub(crate)`",
                        node.name
                    )),
                });
                out.hints.push(MoveHint {
                    kind: "visibility_required".to_string(),
                    file: source_rel.to_string(),
                    line: Some(node.start_line + 1),
                    detail: format!(
                        "`{}` is {} but is now referenced from another module",
                        node.name,
                        visibility_word(&node.visibility)
                    ),
                    suggestion: Some(format!("change `{}` to at least `pub(crate)`", node.name)),
                });
            }
        }

        // 2. Source `use`-import dependencies the moved body relies on.
        for stmt in parse_use_statements(source) {
            let matched: Vec<&UseLeaf> = stmt
                .leaves
                .iter()
                .filter(|leaf| idents.contains(&leaf.binding))
                .collect();
            if matched.is_empty() {
                continue;
            }
            if stmt.leaves.len() == 1 && !stmt.glob {
                if let Some(import) = portable_dependency_import(&stmt.text) {
                    out.auto_imports.push(import);
                } else {
                    out.hints.push(MoveHint {
                        kind: "import_needed".to_string(),
                        file: dest_rel.to_string(),
                        line: None,
                        detail: format!(
                            "moved body uses a source-relative import from {source_rel}: `{}`",
                            stmt.text.trim()
                        ),
                        suggestion: Some(format!(
                            "resolve `{}` against the source module and add a destination-stable import",
                            stmt.text.trim()
                        )),
                    });
                }
                // Orphaned-import: source no longer needs it after the move.
                let leaf = &stmt.leaves[0].binding;
                if !word_present(&source_code_only, leaf) {
                    out.hints.push(MoveHint {
                        kind: "orphaned_import".to_string(),
                        file: source_rel.to_string(),
                        line: Some(stmt.line),
                        detail: format!(
                            "`{}` is only used by the moved symbol and is now unused in {source_rel}",
                            stmt.text.trim()
                        ),
                        suggestion: Some(format!("remove `{}` from {source_rel}", stmt.text.trim())),
                    });
                }
            } else {
                let names: Vec<&str> = matched.iter().map(|l| l.binding.as_str()).collect();
                out.hints.push(MoveHint {
                    kind: "import_needed".to_string(),
                    file: dest_rel.to_string(),
                    line: None,
                    detail: format!(
                        "moved body uses {names:?} from a grouped/glob import in {source_rel}"
                    ),
                    suggestion: Some(format!(
                        "add an import for {names:?} at the destination (from `{}`)",
                        stmt.text.trim()
                    )),
                });
            }
        }

        Ok(out)
    }

    /// Caller hints: every graph call edge into the moved symbol, classified by
    /// whether the caller shared the source module (unqualified call — needs a
    /// `use` for the new module) or referenced it via another module (path/use
    /// now points at the old location).
    async fn caller_hints(
        &self,
        target: &Node,
        source_rel: &str,
        dest_rel: &str,
        src_module: Option<&str>,
        dest_module: Option<&str>,
    ) -> Result<Vec<MoveHint>> {
        let mut hints = Vec::new();
        let callers = self.get_callers(&target.id, 1).await.unwrap_or_default();
        let dest_mod = dest_module.unwrap_or("crate");
        let src_mod = src_module.unwrap_or("crate");
        for (caller, edge) in callers {
            let same_module = caller.file_path == source_rel;
            let detail;
            let suggestion;
            if same_module {
                detail = format!(
                    "`{}` called `{}` unqualified from the same module; it now lives in {dest_rel}",
                    caller.name, target.name
                );
                suggestion = Some(format!(
                    "add `use {dest_mod}::{};` to {}",
                    target.name, caller.file_path
                ));
            } else {
                detail = format!(
                    "`{}` in {} references `{}` via `{src_mod}`; the path is now `{dest_mod}`",
                    caller.name, caller.file_path, target.name
                );
                suggestion = Some(format!(
                    "retarget the reference from `{src_mod}::{}` to `{dest_mod}::{}`",
                    target.name, target.name
                ));
            }
            hints.push(MoveHint {
                kind: "caller_reference".to_string(),
                file: caller.file_path.clone(),
                line: edge.line.map(|l| l + 1),
                detail,
                suggestion,
            });

            // The moved fn's own visibility may be too narrow for a caller that
            // is now in a different module tree.
            if matches!(
                target.visibility,
                Visibility::Private | Visibility::PubSuper
            ) && caller.file_path != dest_rel
            {
                hints.push(MoveHint {
                    kind: "visibility_required".to_string(),
                    file: dest_rel.to_string(),
                    line: None,
                    detail: format!(
                        "`{}` is {} but has a caller in another module ({})",
                        target.name,
                        visibility_word(&target.visibility),
                        caller.file_path
                    ),
                    suggestion: Some(format!(
                        "escalate `{}` to at least `pub(crate)` at the destination",
                        target.name
                    )),
                });
            }
        }
        Ok(hints)
    }

    /// Hint when the destination file's module is not declared anywhere in the
    /// crate. Existing-but-unlinked files need the same hint as fresh files.
    async fn module_missing_hint(&self, dest_rel: &str) -> Option<MoveHint> {
        let stem = module_stem(dest_rel)?;
        if self.module_declared(dest_rel, &stem) {
            return None;
        }
        let parent = self.declaring_parent_file(dest_rel);
        Some(MoveHint {
            kind: "module_missing".to_string(),
            file: parent.clone(),
            line: None,
            detail: format!("module `{stem}` for {dest_rel} is not declared in the crate"),
            suggestion: Some(format!("add `mod {stem};` to {parent}")),
        })
    }

    /// The nearest existing file that could declare `dest_rel`'s module,
    /// falling back to the exact parent-module path that must be created.
    fn declaring_parent_file(&self, dest_rel: &str) -> String {
        let candidates = parent_module_candidates(dest_rel);
        candidates
            .iter()
            .find(|candidate| self.project_root.join(candidate).is_file())
            .cloned()
            .or_else(|| candidates.into_iter().next())
            .unwrap_or_else(|| crate_root_file(dest_rel))
    }

    /// True when some file that could be the parent module of `dest_rel`
    /// already contains a `mod <stem>;` declaration.
    fn module_declared(&self, dest_rel: &str, stem: &str) -> bool {
        for candidate in parent_module_candidates(dest_rel) {
            if let Ok(text) = std::fs::read_to_string(self.project_root.join(&candidate))
                && source_declares_external_module(&text, stem)
            {
                return true;
            }
        }
        false
    }
}

/// Reject a destination whose existing file or nearest existing parent
/// resolves outside the canonical project root. This covers both a symlinked
/// destination file and a symlinked directory component while still allowing
/// symlinks that stay inside the checkout.
fn validate_write_containment(project_root: &Path, path: &Path, label: &str) -> Result<()> {
    let canonical_root = project_root
        .canonicalize()
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to canonicalize project root '{}': {e}",
                project_root.display()
            ),
        })?;
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or_else(|| TraceDecayError::Config {
                    message: format!("{label} '{}' has no existing parent", path.display()),
                })?;
            }
            Err(e) => {
                return Err(TraceDecayError::Config {
                    message: format!("failed to inspect {label} '{}': {e}", path.display()),
                });
            }
        }
    }
    let canonical_existing = existing
        .canonicalize()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to resolve {label} '{}': {e}", path.display()),
        })?;
    if !canonical_existing.starts_with(&canonical_root) {
        return Err(TraceDecayError::Config {
            message: format!(
                "{label} '{}' escapes project root through '{}'",
                path.display(),
                existing.display()
            ),
        });
    }
    Ok(())
}

fn same_existing_file(source: &Path, destination: &Path) -> bool {
    same_file::is_same_file(source, destination).unwrap_or(false)
}

fn write_path_preserving_final_symlink(path: &Path, label: &str) -> Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            path.canonicalize().map_err(|e| TraceDecayError::Config {
                message: format!("failed to resolve {label} '{}': {e}", path.display()),
            })
        }
        Ok(_) => Ok(path.to_path_buf()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(e) => Err(TraceDecayError::Config {
            message: format!("failed to inspect {label} '{}': {e}", path.display()),
        }),
    }
}

fn ensure_text_unchanged(path: &Path, expected: Option<&str>, label: &str) -> Result<()> {
    match expected {
        Some(expected) => match std::fs::read_to_string(path) {
            Ok(current) if current == expected => Ok(()),
            Ok(_) => Err(TraceDecayError::Config {
                message: format!("{label} changed while the move was being prepared; retry"),
            }),
            Err(e) => Err(TraceDecayError::Config {
                message: format!("failed to re-read {label} before applying move: {e}"),
            }),
        },
        None => match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(TraceDecayError::Config {
                message: format!("{label} was created while the move was being prepared; retry"),
            }),
            Err(e) => Err(TraceDecayError::Config {
                message: format!("failed to re-check {label} before applying move: {e}"),
            }),
        },
    }
}

/// Collected dependency findings for a move.
#[derive(Default)]
struct DependencyAnalysis {
    /// `use` lines to auto-insert at the destination (unambiguous, visible).
    auto_imports: Vec<String>,
    /// Findings that need caller attention.
    hints: Vec<MoveHint>,
}

/// A single binding brought into scope by a `use` statement.
struct UseLeaf {
    /// The name as referenced in code (the alias when `as` is present).
    binding: String,
}

/// A parsed `use` statement from a source file.
struct UseStatement {
    /// The full statement text (single physical line).
    text: String,
    /// 1-based line of the statement.
    line: u32,
    /// Whether the statement is a glob import (`use a::*;`).
    glob: bool,
    leaves: Vec<UseLeaf>,
}

/// Item kinds that a `use` statement can bring into scope — the ones a moved
/// body could depend on across a module boundary.
fn is_importable_item(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Trait
            | NodeKind::Function
            | NodeKind::Const
            | NodeKind::Static
            | NodeKind::TypeAlias
            | NodeKind::Macro
            | NodeKind::Union
            | NodeKind::Typedef
            | NodeKind::Record
    )
}

fn visibility_word(v: &Visibility) -> &'static str {
    match v {
        Visibility::Pub => "pub",
        Visibility::PubCrate => "pub(crate)",
        Visibility::PubSuper => "pub(super)",
        Visibility::Private => "private",
    }
}

/// Derives a Rust module path (`crate::a::b`) from a project-relative `.rs`
/// file path under a `src/` root. Returns `None` for non-Rust files.
fn rust_module_path(rel: &str) -> Option<String> {
    let stem = rel.strip_suffix(".rs")?;
    // Normalize to components after an optional `src/` (or `.../src/`) segment.
    let parts: Vec<&str> = stem.split('/').collect();
    let src_idx = parts.iter().rposition(|p| *p == "src");
    let tail: &[&str] = match src_idx {
        Some(i) => &parts[i + 1..],
        None => &parts[..],
    };
    let mut segs: Vec<&str> = tail.to_vec();
    // Crate roots and module files contribute no path segment.
    if let Some("lib" | "main" | "mod") = segs.last().copied() {
        segs.pop();
    }
    let mut path = String::from("crate");
    for seg in segs {
        if seg.is_empty() {
            continue;
        }
        path.push_str("::");
        path.push_str(seg);
    }
    Some(path)
}

/// The module stem for a destination file (`src/foo/bar.rs` -> `bar`,
/// `src/foo/mod.rs` -> `foo`).
fn module_stem(rel: &str) -> Option<String> {
    let stem = rel.strip_suffix(".rs")?;
    let parts: Vec<&str> = stem.split('/').filter(|p| !p.is_empty()).collect();
    let last = parts.last().copied()?;
    if last == "mod" {
        return parts
            .get(parts.len().wrapping_sub(2))
            .map(|s| (*s).to_string());
    }
    if last == "lib" || last == "main" {
        return None;
    }
    Some(last.to_string())
}

/// The likely crate-root file for a destination, used to suggest where a
/// `mod` statement belongs.
fn crate_root_file(dest_rel: &str) -> String {
    let src_prefix = dest_rel.rfind("src/").map(|i| &dest_rel[..i + 4]);
    match src_prefix {
        Some(prefix) => format!("{prefix}lib.rs"),
        None => "src/lib.rs".to_string(),
    }
}

/// Files that could declare `dest_rel`'s module with a `mod` statement.
fn parent_module_candidates(dest_rel: &str) -> Vec<String> {
    let Some(stem) = dest_rel.strip_suffix(".rs") else {
        return Vec::new();
    };
    let parts: Vec<&str> = stem.split('/').filter(|part| !part.is_empty()).collect();
    let src_idx = parts.iter().rposition(|part| *part == "src");
    let (prefix, tail) = match src_idx {
        Some(index) => (
            format!("{}/", parts[..=index].join("/")),
            &parts[index + 1..],
        ),
        None => (String::new(), parts.as_slice()),
    };
    let Some(file_stem) = tail.last() else {
        return Vec::new();
    };
    let parent_segments = if *file_stem == "mod" {
        &tail[..tail.len().saturating_sub(2)]
    } else {
        &tail[..tail.len() - 1]
    };
    if parent_segments.is_empty() {
        let root = if prefix.is_empty() {
            "src/".to_string()
        } else {
            prefix
        };
        return vec![format!("{root}lib.rs"), format!("{root}main.rs")];
    }
    let parent = format!("{prefix}{}", parent_segments.join("/"));
    vec![format!("{parent}.rs"), format!("{parent}/mod.rs")]
}

/// Parse-level check for an external `mod name;` declaration. Text matches
/// alone are unsafe here: comments, strings, and inline `mod name { ... }`
/// blocks do not connect `name.rs` to the module tree.
fn source_declares_external_module(source: &str, expected: &str) -> bool {
    let Ok(language) = tracedecay_code_extraction::ts_provider::try_language("rust") else {
        return false;
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return false;
    }
    let Some(tree) = parser.parse(source, None) else {
        return false;
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        return false;
    }
    loop {
        let node = cursor.node();
        if node.kind() == "mod_item"
            && node.child_by_field_name("body").is_none()
            && node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source.as_bytes()).ok())
                == Some(expected)
        {
            return true;
        }
        if !cursor.goto_next_sibling() {
            return false;
        }
    }
}

/// Collects identifier tokens from source text (Rust identifier rules). Used to
/// test whether the moved body references a given symbol name.
fn body_identifiers(text: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch == '_' || ch.is_ascii_alphanumeric() {
            cur.push(ch);
        } else if is_identifier(&cur) {
            out.insert(std::mem::take(&mut cur));
        } else {
            cur.clear();
        }
    }
    if is_identifier(&cur) {
        out.insert(cur);
    }
    out
}

/// True when `s` is a non-empty Rust-style identifier (does not start with a
/// digit).
fn is_identifier(s: &str) -> bool {
    matches!(s.chars().next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
}

/// Whether `name` appears as a standalone identifier in `text`.
fn word_present(text: &str, name: &str) -> bool {
    body_identifiers(text).contains(name)
}

/// Parses `use` statements from Rust source into their brought-in bindings.
/// Handles `use a::B;`, `use a::B as C;`, grouped `use a::{B, C};`, and
/// multi-line grouped imports. Uses a tree-sitter walk so grouped imports that
/// span several physical lines are captured whole and `use` tokens inside
/// comments or strings are never matched; falls back to a line scan only when
/// the Rust grammar is unavailable.
fn parse_use_statements(source: &str) -> Vec<UseStatement> {
    match parse_use_statements_ts(source) {
        Some(stmts) => stmts,
        None => parse_use_statements_linescan(source),
    }
}

/// Return a destination-stable dependency import.
///
/// `self::` and `super::` are relative to the source module and would silently
/// change meaning after a move, so they require an explicit hint instead of
/// auto-insertion. A source `pub use` is reduced to a private `use`: the moved
/// body needs the binding, not a new public re-export from the destination.
fn portable_dependency_import(statement: &str) -> Option<String> {
    let trimmed = statement.trim();
    let (path, was_public) = trimmed
        .strip_prefix("pub use ")
        .map(|path| (path, true))
        .or_else(|| trimmed.strip_prefix("use ").map(|path| (path, false)))?;
    let path = path.trim_start();
    if path == "self;"
        || path.starts_with("self::")
        || path == "super;"
        || path.starts_with("super::")
    {
        return None;
    }
    Some(if was_public {
        format!("use {path}")
    } else {
        trimmed.to_string()
    })
}

/// Tree-sitter path: collect every `use_declaration` node (top-level and inside
/// `mod` blocks) and parse its full byte-range text. Returns `None` when the
/// grammar cannot be loaded so the caller can fall back to the line scan.
fn parse_use_statements_ts(source: &str) -> Option<Vec<UseStatement>> {
    let mut parser = Parser::new();
    let language = tracedecay_code_extraction::ts_provider::try_language("rust").ok()?;
    parser.set_language(&language).ok()?;
    let tree = parser.parse(source, None)?;
    let mut out = Vec::new();
    collect_use_declarations(tree.root_node(), source.as_bytes(), &mut out);
    Some(out)
}

/// Recursive pre-order walk collecting `use_declaration` nodes into parsed
/// [`UseStatement`]s.
fn collect_use_declarations(node: TsNode<'_>, src: &[u8], out: &mut Vec<UseStatement>) {
    if node.kind() == "use_declaration" {
        if let Ok(text) = node.utf8_text(src)
            && let Some((glob, leaves)) = parse_use_bindings(text)
        {
            out.push(UseStatement {
                text: text.to_string(),
                line: node.start_position().row as u32 + 1,
                glob,
                leaves,
            });
        }
        // `use_declaration` nodes do not nest; no need to descend further.
        return;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            collect_use_declarations(cursor.node(), src, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Fallback line scan (grammar unavailable): one physical line per statement.
/// Misses multi-line grouped imports but never fails the move.
fn parse_use_statements_linescan(source: &str) -> Vec<UseStatement> {
    let mut out = Vec::new();
    for (idx, raw) in source.lines().enumerate() {
        if let Some((glob, leaves)) = parse_use_bindings(raw) {
            out.push(UseStatement {
                text: raw.to_string(),
                line: (idx + 1) as u32,
                glob,
                leaves,
            });
        }
    }
    out
}

/// Parses the bindings out of a single `use` statement's text (possibly
/// multi-line). Accepts `use ...;` and `pub use ...;`. Returns `None` for lines
/// that are not a complete `use` statement or that bring nothing into scope.
fn parse_use_bindings(stmt_text: &str) -> Option<(bool, Vec<UseLeaf>)> {
    let line = stmt_text.trim();
    let after_use = line
        .strip_prefix("pub use ")
        .or_else(|| line.strip_prefix("use "))?
        .trim();
    let body = after_use.strip_suffix(';')?;
    let glob = body.contains('*');
    let leaves: Vec<UseLeaf> = if let Some(open) = body.find('{') {
        let inner = body[open + 1..].trim_end().trim_end_matches('}');
        inner
            .split(',')
            .filter_map(|item| leaf_binding(item.trim()))
            .map(|binding| UseLeaf { binding })
            .collect()
    } else {
        leaf_binding(body)
            .map(|binding| vec![UseLeaf { binding }])
            .unwrap_or_default()
    };
    if leaves.is_empty() && !glob {
        return None;
    }
    Some((glob, leaves))
}

/// The in-code binding for a single `use` path segment (`a::b::C` -> `C`,
/// `a::C as D` -> `D`). Returns `None` for globs and empty items.
fn leaf_binding(item: &str) -> Option<String> {
    let item = item.trim();
    if item.is_empty() || item == "*" || item.ends_with("::*") {
        return None;
    }
    if let Some((_, alias)) = item.rsplit_once(" as ") {
        let alias = alias.trim();
        return (!alias.is_empty()).then(|| alias.to_string());
    }
    let last = item.rsplit("::").next()?.trim();
    (!last.is_empty() && last != "self").then(|| last.to_string())
}

/// Removes `lines[start..=end]` and collapses the blank-line separator the
/// removed item left behind so the source stays tidy.
fn remove_span_with_cleanup(lines: &[&str], start: usize, end: usize) -> Vec<String> {
    let mut out: Vec<String> = lines[..start].iter().map(|s| (*s).to_string()).collect();
    let before_blank = start > 0 && lines[start - 1].trim().is_empty();
    let tail_start = end + 1;
    if tail_start < lines.len() {
        let after_blank = lines[tail_start].trim().is_empty();
        let ts = if before_blank && after_blank {
            tail_start + 1
        } else {
            tail_start
        };
        out.extend(lines[ts..].iter().map(|s| (*s).to_string()));
    } else if before_blank {
        out.pop();
    }
    out
}

/// Builds the destination file content: leading imports (into an existing
/// import region or at the top of a fresh file), then the moved block after a
/// blank-line separator.
fn build_dest_content(dest_original: &str, imports: &[String], moved_text: &str) -> String {
    let moved_block = moved_text.trim_end_matches('\n');
    if dest_original.trim().is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if !imports.is_empty() {
            parts.push(imports.join("\n"));
        }
        parts.push(moved_block.to_string());
        let mut content = parts.join("\n\n");
        content.push('\n');
        content
    } else {
        let with_imports = insert_imports(dest_original, imports);
        format!("{}\n\n{}\n", with_imports.trim_end(), moved_block)
    }
}

/// Inserts `imports` into an existing Rust file after its leading module-doc /
/// comment / existing-`use` region.
fn insert_imports(dest_source: &str, imports: &[String]) -> String {
    if imports.is_empty() {
        return dest_source.to_string();
    }
    let lines: Vec<&str> = dest_source.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        let t = lines[idx].trim();
        // Stop before an OUTER doc-comment block (`///` or `/**`): it documents
        // the first item, and inserting a `use` between the doc and its item
        // detaches the doc. Inner docs (`//!`) and plain comments (`//`) stay in
        // the header region. Check `///`/`/**` first since `///` also matches the
        // generic `//` prefix below.
        if t.starts_with("///") || t.starts_with("/**") {
            break;
        }
        let header = t.is_empty()
            || t.starts_with("//!")
            || t.starts_with("//")
            || t.starts_with("use ")
            || t.starts_with("pub use ")
            || t.starts_with("extern crate");
        if header {
            idx += 1;
        } else {
            break;
        }
    }
    let mut rebuilt: Vec<String> = lines[..idx].iter().map(|s| (*s).to_string()).collect();
    for imp in imports {
        rebuilt.push(imp.clone());
    }
    rebuilt.extend(lines[idx..].iter().map(|s| (*s).to_string()));
    let mut out = rebuilt.join("\n");
    if dest_source.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Drops imports already present verbatim in the destination and de-duplicates
/// within the batch, preserving order.
fn dedup_preserve(imports: &[String], dest_original: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for imp in imports {
        let trimmed = imp.trim();
        if dest_original.lines().any(|l| l.trim() == trimmed) {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(imp.clone());
        }
    }
    out
}

/// Hints when the moved span carries a `#[cfg(...)]` gate the destination might
/// not satisfy.
fn cfg_context_hints(moved_text: &str, dest_rel: &str) -> Vec<MoveHint> {
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
fn cycle_risk_hints(
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

/// Builds a combined dry-run diff of the source removal and destination
/// insertion.
fn combined_diff(
    source_rel: &str,
    source: &str,
    source_modified: &str,
    dest_rel: &str,
    dest_original: &str,
    dest_modified: &str,
) -> String {
    let src_diff = bounded_region_diff(
        source,
        source_modified,
        PREVIEW_DIFF_CONTEXT,
        MAX_PREVIEW_DIFF_LINES,
    );
    let dest_diff = bounded_region_diff(
        dest_original,
        dest_modified,
        PREVIEW_DIFF_CONTEXT,
        MAX_PREVIEW_DIFF_LINES,
    );
    format!(
        "--- {source_rel} (source, remove)\n{src_diff}\n\n+++ {dest_rel} (destination, insert)\n{dest_diff}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_existing_file_detects_hard_link() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.rs");
        let alias = dir.path().join("alias.rs");
        std::fs::write(&source, "fn source() {}\n").unwrap();
        std::fs::hard_link(&source, &alias).unwrap();
        assert!(same_existing_file(&source, &alias));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_target_preserves_final_symlink() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.rs");
        let alias = dir.path().join("alias.rs");
        std::fs::write(&target, "fn old() {}\n").unwrap();
        unix_fs::symlink(&target, &alias).unwrap();

        let write_path = write_path_preserving_final_symlink(&alias, "test").unwrap();
        crate::agents::safe_write_text_file(&write_path, "fn new() {}\n", None).unwrap();

        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn new() {}\n");
    }

    #[test]
    fn optimistic_write_guard_rejects_changed_or_created_files() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing.rs");
        let created = dir.path().join("created.rs");
        std::fs::write(&existing, "fn before() {}\n").unwrap();
        std::fs::write(&existing, "fn concurrent() {}\n").unwrap();
        std::fs::write(&created, "fn appeared() {}\n").unwrap();

        let changed = ensure_text_unchanged(&existing, Some("fn before() {}\n"), "source")
            .unwrap_err()
            .to_string();
        assert!(changed.contains("changed while the move was being prepared"));
        let appeared = ensure_text_unchanged(&created, None, "destination")
            .unwrap_err()
            .to_string();
        assert!(appeared.contains("was created while the move was being prepared"));
    }

    #[tokio::test]
    async fn move_symbol_apply_refreshes_caller_graph() {
        let _profile = crate::config::PinnedUserDataDir::new();
        let profile_root = crate::storage::default_profile_root().expect("test profile root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&profile_root, std::fs::Permissions::from_mode(0o700))
                .expect("secure test profile root");
        }
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join("src/lib.rs"),
            "pub mod source;\npub mod caller;\npub mod destination;\n",
        )
        .unwrap();
        std::fs::write(
            project.join("src/source.rs"),
            "pub fn moved() -> u32 { 1 }\n",
        )
        .unwrap();
        std::fs::write(
            project.join("src/caller.rs"),
            "use crate::source::moved;\npub fn caller() -> u32 { moved() }\n",
        )
        .unwrap();
        std::fs::write(
            project.join("src/destination.rs"),
            "pub fn existing() -> u32 { 0 }\n",
        )
        .unwrap();

        let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
            &profile_root,
            "move symbol fixture initialization",
        )
        .expect("acquire fixture lifecycle authority");
        let _database_scope = crate::db::enter_maintenance_database_scope(
            &lifecycle,
            &profile_root,
            "move symbol fixture initialization",
        )
        .expect("enter fixture maintenance database scope");
        let cg = TraceDecay::init_with_exclusive_maintenance(
            project,
            crate::tracedecay::TraceDecayOpenOptions {
                profile_root: Some(profile_root),
                global_db_path: None,
            },
            &lifecycle,
        )
        .await
        .unwrap();
        cg.index_all().await.unwrap();
        let result = cg
            .move_symbol("moved", "src/destination.rs", false, false)
            .await
            .unwrap();
        assert!(result.success, "move result: {result:?}");

        let moved = cg
            .get_nodes_by_qualified_name("moved")
            .await
            .unwrap()
            .into_iter()
            .find(|node| node.file_path == "src/destination.rs")
            .expect("moved symbol should be indexed at the destination");
        let callers = cg.get_callers(&moved.id, 1).await.unwrap();
        assert!(
            callers.iter().any(|(caller, _)| caller.name == "caller"),
            "caller graph must be fresh for the next refactor step: {callers:?}"
        );
    }

    #[test]
    fn rust_module_path_maps_src_layout() {
        assert_eq!(
            rust_module_path("src/pricing.rs").as_deref(),
            Some("crate::pricing")
        );
        assert_eq!(
            rust_module_path("src/foo/bar.rs").as_deref(),
            Some("crate::foo::bar")
        );
        assert_eq!(
            rust_module_path("src/foo/mod.rs").as_deref(),
            Some("crate::foo")
        );
        assert_eq!(rust_module_path("src/lib.rs").as_deref(), Some("crate"));
        assert_eq!(rust_module_path("src/main.rs").as_deref(), Some("crate"));
        assert_eq!(
            rust_module_path("evals/fixture/src/pricing.rs").as_deref(),
            Some("crate::pricing")
        );
        assert_eq!(rust_module_path("README.md"), None);
    }

    #[test]
    fn module_stem_and_root() {
        assert_eq!(
            module_stem("src/grand_total.rs").as_deref(),
            Some("grand_total")
        );
        assert_eq!(module_stem("src/foo/mod.rs").as_deref(), Some("foo"));
        assert_eq!(module_stem("src/lib.rs"), None);
        assert_eq!(
            crate_root_file("evals/fixture/src/grand_total.rs"),
            "evals/fixture/src/lib.rs"
        );
        assert_eq!(crate_root_file("src/a.rs"), "src/lib.rs");
    }

    #[test]
    fn parent_module_candidates_follow_rust_file_layout() {
        assert_eq!(
            parent_module_candidates("src/foo.rs"),
            vec!["src/lib.rs", "src/main.rs"]
        );
        assert_eq!(
            parent_module_candidates("src/foo/bar.rs"),
            vec!["src/foo.rs", "src/foo/mod.rs"]
        );
        assert_eq!(
            parent_module_candidates("src/foo/mod.rs"),
            vec!["src/lib.rs", "src/main.rs"]
        );
        assert_eq!(
            parent_module_candidates("src/foo/bar/mod.rs"),
            vec!["src/foo.rs", "src/foo/mod.rs"]
        );
        assert_eq!(
            parent_module_candidates("evals/fixture/src/foo/bar.rs"),
            vec!["evals/fixture/src/foo.rs", "evals/fixture/src/foo/mod.rs"]
        );
    }

    #[test]
    fn external_module_detection_ignores_comments_strings_and_inline_modules() {
        assert!(source_declares_external_module(
            "#[cfg(feature = \"x\")]\npub mod child;\n",
            "child"
        ));
        assert!(!source_declares_external_module(
            "// mod child;\nconst NOTE: &str = \"mod child;\";\n",
            "child"
        ));
        assert!(!source_declares_external_module(
            "mod child { pub fn inline() {} }\n",
            "child"
        ));
    }

    #[test]
    fn body_identifiers_collects_names_not_numbers() {
        let ids = body_identifiers("let x = LineItem { unit_price: 3u64 };");
        assert!(ids.contains("LineItem"));
        assert!(ids.contains("unit_price"));
        assert!(ids.contains("let"));
        assert!(!ids.contains("3u64"));
        assert!(!ids.contains("3"));
    }

    #[test]
    fn parse_use_statements_handles_forms() {
        let src = "use std::collections::HashMap;\npub use crate::a::B as C;\nuse crate::x::{Y, Z};\nuse crate::glob::*;\n";
        let stmts = parse_use_statements(src);
        assert_eq!(stmts.len(), 4);
        assert_eq!(stmts[0].leaves[0].binding, "HashMap");
        assert!(!stmts[0].glob);
        assert_eq!(stmts[1].leaves[0].binding, "C");
        assert_eq!(stmts[2].leaves.len(), 2);
        assert_eq!(stmts[2].leaves[0].binding, "Y");
        assert_eq!(stmts[2].leaves[1].binding, "Z");
        assert!(stmts[3].glob);
    }

    #[test]
    fn portable_dependency_import_rejects_source_relative_paths() {
        assert_eq!(
            portable_dependency_import("use crate::shared::Thing;").as_deref(),
            Some("use crate::shared::Thing;")
        );
        assert_eq!(
            portable_dependency_import("use external_crate::Thing;").as_deref(),
            Some("use external_crate::Thing;")
        );
        assert_eq!(portable_dependency_import("use self::Thing;"), None);
        assert_eq!(portable_dependency_import("use super::Thing;"), None);
        assert_eq!(
            portable_dependency_import("use super::parent::Thing;"),
            None
        );
    }

    #[test]
    fn portable_dependency_import_does_not_create_a_new_reexport() {
        assert_eq!(
            portable_dependency_import("pub use crate::shared::Thing;").as_deref(),
            Some("use crate::shared::Thing;")
        );
    }

    #[test]
    fn remove_span_collapses_trailing_blank_at_eof() {
        let lines = vec!["fn a() {}", "", "/// doc", "fn b() {}"];
        let out = remove_span_with_cleanup(&lines, 2, 3);
        assert_eq!(out, vec!["fn a() {}".to_string()]);
    }

    #[test]
    fn remove_span_collapses_interior_blank() {
        let lines = vec!["a", "", "b", "", "c"];
        // remove "b" (index 2) with blanks on both sides -> one blank collapses
        let out = remove_span_with_cleanup(&lines, 2, 2);
        assert_eq!(out, vec!["a".to_string(), String::new(), "c".to_string()]);
    }

    #[test]
    fn build_dest_content_new_file_prepends_imports() {
        let content = build_dest_content(
            "",
            &["use crate::pricing::LineItem;".to_string()],
            "/// doc\nfn f() {}\n",
        );
        assert_eq!(
            content,
            "use crate::pricing::LineItem;\n\n/// doc\nfn f() {}\n"
        );
    }

    #[test]
    fn build_dest_content_existing_file_appends_after_blank() {
        let content = build_dest_content("//! mod\n\nfn existing() {}\n", &[], "fn f() {}");
        assert_eq!(content, "//! mod\n\nfn existing() {}\n\nfn f() {}\n");
    }

    #[test]
    fn insert_imports_after_header_block() {
        let out = insert_imports(
            "//! module doc\n\nfn a() {}\n",
            &["use crate::X;".to_string()],
        );
        assert_eq!(out, "//! module doc\n\nuse crate::X;\nfn a() {}\n");
    }

    #[test]
    fn insert_imports_keeps_outer_doc_attached_to_first_item() {
        // An outer doc-comment (`///`) documents the first item; the `use` must
        // NOT be wedged between the doc and its `pub fn`.
        let out = insert_imports(
            "/// Docs for other.\npub fn other() {}\n",
            &["use crate::X;".to_string()],
        );
        assert_eq!(
            out,
            "use crate::X;\n/// Docs for other.\npub fn other() {}\n"
        );
        // The doc line stays immediately above the item.
        let lines: Vec<&str> = out.lines().collect();
        let doc = lines
            .iter()
            .position(|l| l.trim() == "/// Docs for other.")
            .unwrap();
        assert_eq!(lines[doc + 1].trim(), "pub fn other() {}");
    }

    #[test]
    fn insert_imports_stops_before_outer_block_doc() {
        let out = insert_imports(
            "/** Block doc. */\npub fn other() {}\n",
            &["use crate::X;".to_string()],
        );
        assert_eq!(out, "use crate::X;\n/** Block doc. */\npub fn other() {}\n");
    }

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

    /// Mirror `analyze_dependencies`' scan: mask comments/strings (preserving
    /// format captures) then collect identifier tokens.
    fn masked_idents(src: &str) -> HashSet<String> {
        body_identifiers(&masked_rust_source_with(src, MaskOptions::UNUSED_IMPORTS))
    }

    #[test]
    fn masked_scan_drops_doc_and_comment_mentions() {
        let src = "/// see [`compute_total`]\npub fn f(x: LineItem) -> u64 {\n    x.v // compute_total again\n}\n";
        let ids = masked_idents(src);
        assert!(ids.contains("LineItem"), "code idents kept: {ids:?}");
        assert!(
            !ids.contains("compute_total"),
            "doc/comment mention dropped: {ids:?}"
        );
    }

    #[test]
    fn masked_scan_handles_block_comments() {
        let ids = masked_idents("let a = 1; /* Foo bar */ let b = Baz;");
        assert!(ids.contains("Baz"));
        assert!(!ids.contains("Foo"));
    }

    #[test]
    fn masked_scan_drops_string_literal_identifiers() {
        // An identifier that appears only inside a string literal is not a
        // dependency and must not be counted.
        let ids = masked_idents(r#"fn f() { let s = "Widget in a string"; let x = Gadget; }"#);
        assert!(ids.contains("Gadget"), "real code ident kept: {ids:?}");
        assert!(
            !ids.contains("Widget"),
            "string-literal ident dropped: {ids:?}"
        );
    }

    #[test]
    fn masked_scan_keeps_format_captures() {
        // `format!("{helper_result}")`-style implicit captures ARE real uses.
        let ids = masked_idents(r#"fn f() -> String { format!("{helper_result}") }"#);
        assert!(
            ids.contains("helper_result"),
            "implicit format capture counted: {ids:?}"
        );
    }

    #[test]
    fn parse_use_statements_handles_multiline_grouped_import() {
        let src = "use crate::x::{\n    Alpha,\n    Beta,\n    Gamma,\n};\nfn f() {}\n";
        let stmts = parse_use_statements(src);
        assert_eq!(stmts.len(), 1, "one grouped statement: {}", stmts.len());
        let bindings: Vec<&str> = stmts[0].leaves.iter().map(|l| l.binding.as_str()).collect();
        assert_eq!(bindings, vec!["Alpha", "Beta", "Gamma"]);
        assert_eq!(stmts[0].line, 1);
        assert!(stmts[0].text.contains("Gamma"), "text is whole statement");
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

    #[test]
    fn dedup_preserve_skips_existing() {
        let out = dedup_preserve(
            &[
                "use a::B;".to_string(),
                "use a::B;".to_string(),
                "use c::D;".to_string(),
            ],
            "use c::D;\n",
        );
        assert_eq!(out, vec!["use a::B;".to_string()]);
    }
}
