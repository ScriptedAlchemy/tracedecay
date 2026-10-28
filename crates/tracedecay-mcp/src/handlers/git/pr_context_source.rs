//! PR presentation over the shared exact Git selection and source comparison.

use std::collections::{BTreeMap, HashSet};

use tracedecay_application::git_intelligence::{
    GitChangeSelection, GitChangedDeclaration, GitSourceDeclaration as Declaration, GitSourceError,
    NativeGitIntelligence, analyze_source_changes,
};
use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_contracts::retrieval::{
    GitContextSourceSideV1, GitDeclarationChangeKindV1, PrContextAreaV1, PrContextEntryPointV1,
    PrContextExcerptV1, PrContextFileV1, PrContextLocationV1, PrContextQualityV1,
    PrContextSourceStateV1, PrContextSourceV1,
};
use tracedecay_domain::{GitChangeKindV1, GitDiffScopeV1, GitOidV1, IndexPathPolicyV1};

use super::shell::classify_file_role;

pub(super) struct SourceBrief {
    pub changes: Vec<PrContextFileV1>,
    pub areas: Vec<PrContextAreaV1>,
    pub entries: Vec<PrContextEntryPointV1>,
    pub quality: Vec<PrContextQualityV1>,
    pub source: PrContextSourceV1,
    pub additions: u64,
    pub deletions: u64,
    pub clean_at_head: bool,
}

fn entry(
    declaration: &Declaration,
    previous: Option<&Declaration>,
    path: &str,
    revision: &str,
    previous_revision: &str,
    change: &str,
) -> PrContextEntryPointV1 {
    let node = &declaration.node;
    let first_change = previous.map_or(0, |old| {
        old.content
            .lines()
            .zip(declaration.content.lines())
            .take_while(|(before, after)| before == after)
            .count()
    });
    let preview_start = first_change.saturating_sub(1);
    let (before_signature, before_signature_truncated) =
        signature_excerpt(if change == "removed" {
            node.signature.as_ref()
        } else {
            previous.and_then(|old| old.node.signature.as_ref())
        });
    let (after_signature, after_signature_truncated) = signature_excerpt(if change == "removed" {
        None
    } else {
        node.signature.as_ref()
    });
    PrContextEntryPointV1 {
        location: PrContextLocationV1 {
            file: path.to_owned(),
            line: node.start_line + 1,
            end_line: node.end_line + 1,
        },
        revision: revision.to_owned(),
        symbol: node.qualified_name.clone(),
        node_id: None,
        change: change.to_owned(),
        before_signature,
        before_signature_truncated,
        after_signature,
        after_signature_truncated,
        before_excerpt: if change == "removed" {
            Some(excerpt(declaration, revision, 0))
        } else {
            previous.map(|old| excerpt(old, previous_revision, preview_start))
        },
        after_excerpt: if change == "removed" {
            None
        } else {
            Some(excerpt(declaration, revision, preview_start))
        },
        reason: match change {
            "signature_changed" => {
                "Declaration signature changed; inspect the contract and its callers."
            }
            "visibility_changed" => {
                "Declaration export or visibility changed; inspect its public contract and consumers."
            }
            "removed" => {
                "Declaration removed; read the merge-base version and check remaining consumers."
            }
            "added" => "New declaration; start here to understand the added behavior.",
            "renamed" => "Declaration moved with its file; content is unchanged.",
            _ => "Declaration body changed; compare the exact before and after revisions.",
        }
        .to_owned(),
    }
}

/// Some language extractors use a complete initializer as a signature. Keep
/// an exact bounded prefix rather than copying that body into the briefing.
fn signature_excerpt(signature: Option<&String>) -> (Option<String>, bool) {
    let Some(signature) = signature else {
        return (None, false);
    };
    let mut end = signature.len().min(600);
    while !signature.is_char_boundary(end) {
        end -= 1;
    }
    (Some(signature[..end].to_owned()), end < signature.len())
}

fn excerpt(
    declaration: &Declaration,
    revision: &str,
    requested_start: usize,
) -> PrContextExcerptV1 {
    let lines = declaration.content.lines().collect::<Vec<_>>();
    let start = requested_start.min(lines.len().saturating_sub(1));
    let selected = lines
        .iter()
        .skip(start)
        .take(6)
        .copied()
        .collect::<Vec<_>>();
    let mut text = selected.join("\n");
    let mut end = text.len().min(600);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let bytes_truncated = end < text.len();
    text.truncate(end);
    // Both counts fit u32: the canonical historical-blob reader admits at
    // most 8 MiB, and this preview contains at most 600 bytes.
    let line = declaration
        .node
        .attrs_start_line
        .min(declaration.node.start_line)
        .saturating_add(start as u32)
        .saturating_add(1);
    let line_count = text.lines().count() as u32;
    PrContextExcerptV1 {
        revision: revision.to_owned(),
        location: PrContextLocationV1 {
            file: declaration.path.clone(),
            line,
            end_line: line.saturating_add(line_count.saturating_sub(1)),
        },
        text,
        truncated: start > 0 || selected.len() < lines.len() || bytes_truncated,
    }
}

fn checkpoint(cancelled: &dyn Fn() -> bool) -> std::result::Result<(), String> {
    if cancelled() {
        Err("PR source comparison cancelled or expired".to_owned())
    } else {
        Ok(())
    }
}

fn file_entry(path: &str, revision: &str, line: u32, reason: String) -> PrContextEntryPointV1 {
    PrContextEntryPointV1 {
        location: PrContextLocationV1 {
            file: path.to_owned(),
            line,
            end_line: line,
        },
        revision: revision.to_owned(),
        symbol: path.to_owned(),
        node_id: None,
        change: "file_change".to_owned(),
        before_signature: None,
        before_signature_truncated: false,
        after_signature: None,
        after_signature_truncated: false,
        before_excerpt: None,
        after_excerpt: None,
        reason,
    }
}

fn project_declaration(
    brief: &mut SourceBrief,
    changed: &GitChangedDeclaration,
    base: &str,
    head: &str,
) {
    let name = match changed.change {
        GitDeclarationChangeKindV1::Added => "added",
        GitDeclarationChangeKindV1::Removed => "removed",
        GitDeclarationChangeKindV1::Modified => "modified",
        GitDeclarationChangeKindV1::SignatureChanged => "signature_changed",
        GitDeclarationChangeKindV1::VisibilityChanged => "visibility_changed",
        GitDeclarationChangeKindV1::Renamed => "renamed",
        GitDeclarationChangeKindV1::AmbiguousOverload => "changed_overload",
    };
    let mut point = match (&changed.before, &changed.after) {
        (old, Some(new)) => entry(new, old.as_ref(), &new.path, head, base, name),
        (Some(old), None) => entry(old, None, &old.path, base, base, "removed"),
        (None, None) => return,
    };
    if changed.change == GitDeclarationChangeKindV1::AmbiguousOverload {
        name.clone_into(&mut point.change);
        "Overload group changed; before/after pairing is ambiguous. Inspect this declaration and the file diff.".clone_into(&mut point.reason);
    }
    if let (Some(old), Some(new)) = (&changed.before, &changed.after) {
        if changed.change == GitDeclarationChangeKindV1::VisibilityChanged {
            point.reason = format!(
                "Declaration visibility changed from {} to {}; inspect its public contract and consumers.",
                old.node.visibility.as_str(),
                new.node.visibility.as_str()
            );
        }
        if old.node.kind.is_callable_kind()
            && old.node.complexity_analysis.is_complete()
            && new.node.complexity_analysis.is_complete()
            && old.node.branches != new.node.branches
        {
            brief.quality.push(PrContextQualityV1 {
                signal: "branch_count_changed".to_owned(),
                observation: format!(
                    "{}: {} → {} branches (static count, not a correctness verdict).",
                    new.node.qualified_name, old.node.branches, new.node.branches
                ),
            });
        }
    }
    brief.entries.push(point);
}

pub(super) fn compare_source(
    git: &NativeGitIntelligence,
    base: &str,
    head: &str,
    policy: &IndexPathPolicyV1,
    cancelled: &dyn Fn() -> bool,
) -> Result<SourceBrief, String> {
    checkpoint(cancelled)?;
    let base = GitOidV1::new(base).map_err(|error| error.to_string())?;
    let head = GitOidV1::new(head).map_err(|error| error.to_string())?;
    let comparison = git
        .compare_changes(&GitChangeSelection::Diff(GitDiffScopeV1::CommitRange {
            base: base.clone(),
            head: head.clone(),
        }))
        .map_err(|error| error.to_string())?;
    let mut brief = SourceBrief {
        additions: 0, deletions: 0, changes: Vec::new(), areas: Vec::new(), entries: Vec::new(), quality: Vec::new(),
        source: PrContextSourceV1 { state: PrContextSourceStateV1::Ready, detail: "Exact merge-base and head blobs; declaration contracts compared. Line totals use the native Myers diff without materializing a full PR patch.".to_owned() },
        clean_at_head: false,
    };
    let registry = LanguageRegistry::new();
    let mut areas = BTreeMap::<String, PrContextAreaV1>::new();
    let (mut gaps, mut unreadable, mut excluded, mut ambiguous) = (0usize, 0usize, 0usize, 0usize);
    for file in &comparison.files {
        checkpoint(cancelled)?;
        let old_path = file.original_path.as_deref().unwrap_or(&file.path);
        let removed = file.change == GitChangeKindV1::Deleted;
        let (mut additions, mut deletions) = (None, None);
        match git.compare_file(&comparison, file, policy) {
            Ok(source) => {
                additions = Some(source.additions);
                deletions = Some(source.deletions);
                checkpoint(cancelled)?;
                match analyze_source_changes(&registry, file, &source) {
                    Ok(analysis) => {
                        ambiguous += analysis.ambiguous_groups;
                        for declaration in &analysis.declarations {
                            project_declaration(
                                &mut brief,
                                declaration,
                                base.as_str(),
                                head.as_str(),
                            );
                        }
                        if let Some((side, line)) = analysis.outside {
                            let before = side == GitContextSourceSideV1::Before;
                            brief.entries.push(file_entry(if before { old_path } else { &file.path }, if before { base.as_str() } else { head.as_str() }, line + 1,
                                "A changed line contains text outside retained declarations, possibly sharing their line; inspect its imports and module wiring. The line diff does not isolate which portion changed.".to_owned()));
                        } else if source.changed_lines.is_empty() && file.old_mode != file.new_mode
                        {
                            brief.entries.push(file_entry(if removed { old_path } else { &file.path }, if removed { base.as_str() } else { head.as_str() }, 1,
                                "Git file mode changed; inspect its existence, executable or file-type contract.".to_owned()));
                        }
                    }
                    Err(reason) => {
                        gaps += 1;
                        let (before, line) =
                            source.changed_lines.first().map_or((removed, 1), |hunk| {
                                if hunk.after.is_empty() {
                                    (true, hunk.before.start + 1)
                                } else {
                                    (false, hunk.after.start + 1)
                                }
                            });
                        brief.entries.push(file_entry(
                            if before { old_path } else { &file.path },
                            if before { base.as_str() } else { head.as_str() },
                            line,
                            format!("File-level entrypoint: {reason}."),
                        ));
                    }
                }
            }
            Err(error) => {
                if error.is_terminal() {
                    return Err(error.to_string());
                }
                gaps += 1;
                unreadable += 1;
                if matches!(error, GitSourceError::Excluded) {
                    excluded += 1;
                } else if !error.is_opaque() {
                    brief.entries.push(file_entry(
                        if removed { old_path } else { &file.path },
                        if removed {
                            base.as_str()
                        } else {
                            head.as_str()
                        },
                        1,
                        format!("File-level entrypoint: {error}; line totals unavailable."),
                    ));
                }
            }
        }
        brief.additions += u64::from(additions.unwrap_or(0));
        brief.deletions += u64::from(deletions.unwrap_or(0));
        let area = area_name(&file.path);
        let area = areas.entry(area.clone()).or_insert(PrContextAreaV1 {
            area,
            files: 0,
            additions: 0,
            deletions: 0,
        });
        area.files += 1;
        area.additions += u64::from(additions.unwrap_or(0));
        area.deletions += u64::from(deletions.unwrap_or(0));
        brief.changes.push(PrContextFileV1 {
            role: classify_file_role(&file.path, &HashSet::new()),
            file: file.path.clone(),
            previous_file: file.original_path.clone(),
            change: change_name(file.change).to_owned(),
            additions,
            deletions,
        });
    }
    brief.areas = areas.into_values().collect();
    brief
        .areas
        .sort_by_key(|area| std::cmp::Reverse(area.additions + area.deletions));
    if gaps > 0 || ambiguous > 0 {
        brief.source.state = PrContextSourceStateV1::Partial;
        brief.source.detail = format!(
            "Exact Git comparison; {gaps} files have file-level analysis only, {ambiguous} overload groups cannot be paired. Native Myers line totals exclude {unreadable} opaque or unreadable files, including {excluded} files excluded by configured source policy; their file totals are null."
        );
    }
    checkpoint(cancelled)?;
    match git.status() {
        Ok(status) => {
            brief.clean_at_head = status.head.commit().is_some_and(|oid| oid == &head)
                && !status.coverage.leaves_state_unread()
                && status.staged_count() == 0
                && status.unstaged_count() == 0
                && status.conflicted_count() == 0
                && status.untracked_count() == 0;
            if !brief.clean_at_head {
                brief.quality.push(PrContextQualityV1 { signal: "checkout_differs".to_owned(), observation: "The checkout is dirty or is not the compared head. Source entrypoints refer to committed Git blobs.".to_owned() });
            }
        }
        Err(error) => {
            git.checkpoint().map_err(|error| error.to_string())?;
            brief.quality.push(PrContextQualityV1 { signal: "checkout_unverified".to_owned(), observation: format!("Worktree status unavailable: {error}. Graph enrichment cannot prove correspondence to the compared head.") });
        }
    }
    Ok(brief)
}

fn area_name(path: &str) -> String {
    let mut parts = path.split('/');
    let first = parts.next().unwrap_or(path);
    match (first, parts.next()) {
        ("crates" | "packages" | "apps", Some(package)) => format!("{first}/{package}"),
        (_, Some(_)) => first.to_owned(),
        _ => "repository root".to_owned(),
    }
}

fn change_name(change: GitChangeKindV1) -> &'static str {
    match change {
        GitChangeKindV1::Unmodified => "unmodified",
        GitChangeKindV1::Modified => "modified",
        GitChangeKindV1::Added => "added",
        GitChangeKindV1::Deleted => "deleted",
        GitChangeKindV1::Renamed => "renamed",
        GitChangeKindV1::Copied => "copied",
        GitChangeKindV1::TypeChanged => "type_changed",
        GitChangeKindV1::Unmerged => "unmerged",
    }
}
