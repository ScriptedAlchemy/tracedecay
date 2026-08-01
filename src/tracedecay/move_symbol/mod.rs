//! `move_symbol`: relocate a function (Rust-first, provider-agnostic shape)
//! from its file to a destination file. The centerpiece is the post-move
//! **impact report** — every reference, dependency, visibility, or module
//! concern the move raises, surfaced as evidence-based, actionable hints
//! derived from the code graph (callers/callees) and parse-level facts
//! (identifiers, `use` lines, module declarations). Never regex-only guessing.

mod fs_guards;
mod hints;
mod rendering;
mod rust_paths;
mod use_parsing;

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
use crate::types::{MoveHint, MoveResult, Node, Visibility};
use tracedecay_code_extraction::source_mask::{MaskOptions, masked_rust_source_with};

use super::TraceDecay;
use super::edits::{
    LeadingBlock, LeadingKind, capture_planned_source_edit, classify_leading_line,
    edit_success_message, item_line_span, publish_planned_source_edit, resolve_symbol_for_edit,
    splice_lines, validate_planned_source_edit,
};

use fs_guards::{
    ensure_text_unchanged, same_existing_file, validate_write_containment,
    write_path_preserving_final_symlink,
};
use hints::{DependencyAnalysis, cfg_context_hints, cycle_risk_hints};
use rendering::{build_dest_content, combined_diff, dedup_preserve, remove_span_with_cleanup};
use rust_paths::{
    crate_root_file, is_importable_item, module_stem, parent_module_candidates, rust_module_path,
    source_declares_external_module, visibility_word,
};
use use_parsing::{UseLeaf, body_identifiers, parse_use_statements, portable_dependency_import};

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
        let span = item_line_span(&target, src_lines.len(), LeadingBlock::Always);
        let mut start = span.start;
        let end_inclusive = span.end_inclusive;
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
        while start < end_inclusive
            && classify_leading_line(src_lines[start]) == LeadingKind::InnerDoc
        {
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
        let residual = remove_span_with_cleanup(&src_lines, start, end_inclusive);
        let source_modified = splice_lines(&residual, source.ends_with('\n'));

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
        // Hoisted out of the `use`-statement loop below: `source_code_only`
        // never changes across iterations, so recomputing its identifier set
        // on every matching `use` statement was pure waste.
        let source_identifiers = body_identifiers(&source_code_only);

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
                if !source_identifiers.contains(leaf) {
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

#[cfg(test)]
mod tests {
    use super::TraceDecay;

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
}
