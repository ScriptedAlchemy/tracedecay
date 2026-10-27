//! Declaration changes from exact Git blobs, independent of index readiness.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use tracedecay_application::git_intelligence::NativeGitIntelligence;
use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_contracts::git::{GIT_HISTORICAL_BLOB_MAX_BYTES, GitHistoricalBlobRequestV1};
use tracedecay_contracts::retrieval::{
    PrContextAreaV1, PrContextEntryPointV1, PrContextExcerptV1, PrContextFileV1,
    PrContextLocationV1, PrContextQualityV1, PrContextSourceStateV1, PrContextSourceV1,
};
use tracedecay_domain::{GitChangeKindV1, GitOidV1, Node, NodeKind};

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

struct Declaration {
    node: Node,
    content: String,
    span: Range<usize>,
    revision: String,
    path: String,
}

fn read_source(
    git: &NativeGitIntelligence,
    revision: &GitOidV1,
    path: &str,
) -> std::result::Result<String, String> {
    let blob = git
        .historical_blob(&GitHistoricalBlobRequestV1 {
            commit: revision.clone(),
            path: path.to_owned(),
            max_bytes: GIT_HISTORICAL_BLOB_MAX_BYTES,
            include_bytes: true,
        })
        .map_err(|error| error.to_string())?;
    let bytes = blob
        .bytes
        .ok_or_else(|| format!("missing blob at {}:{path}", revision.as_str()))?;
    if bytes.contains(&0) {
        return Err("binary source".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "non-UTF-8 source".to_owned())
}

/// Both sides use the head's logical path when extracting names, so a path
/// rename cannot manufacture a deletion and addition of every declaration.
fn declarations(
    registry: &LanguageRegistry,
    source: &str,
    revision: &GitOidV1,
    path: &str,
    logical_path: &str,
) -> std::result::Result<Vec<Declaration>, String> {
    let extractor = registry
        .extractor_for_file(logical_path)
        .ok_or_else(|| "unsupported language".to_owned())?;
    let artifact = extractor.extract_artifact(logical_path, source);
    if !artifact.result.errors.is_empty() {
        return Err(format!(
            "{} extraction errors",
            artifact.result.errors.len()
        ));
    }
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let mut offsets = Vec::with_capacity(lines.len() + 1);
    offsets.push(0usize);
    for line in &lines {
        offsets.push(offsets.last().copied().unwrap_or_default() + line.len());
    }
    artifact
        .result
        .nodes
        .into_iter()
        .filter(|node| {
            !matches!(
                node.kind,
                NodeKind::File
                    | NodeKind::Module
                    | NodeKind::Impl
                    | NodeKind::Use
                    | NodeKind::Package
                    | NodeKind::GoPackage
                    | NodeKind::ScalaPackage
                    | NodeKind::KotlinPackage
                    | NodeKind::GenericParam
                    | NodeKind::AnnotationUsage
            )
        })
        .map(|node| {
            let start = if node.attrs_start_line < node.start_line {
                offsets.get(node.attrs_start_line as usize).copied()
            } else {
                offsets
                    .get(node.start_line as usize)
                    .and_then(|offset| offset.checked_add(node.start_column as usize))
            };
            let end = offsets
                .get(node.end_line as usize)
                .and_then(|offset| offset.checked_add(node.end_column as usize));
            let (span, content) = start
                .zip(end)
                .and_then(|(start, end)| {
                    source
                        .get(start..end)
                        .map(|text| (start..end, text.to_owned()))
                })
                .ok_or_else(|| format!("invalid source span for {}", node.qualified_name))?;
            Ok(Declaration {
                node,
                content,
                span,
                revision: revision.as_str().to_owned(),
                path: path.to_owned(),
            })
        })
        .collect()
}

fn group_declarations(
    declarations: Vec<Declaration>,
) -> BTreeMap<(String, String), Vec<Declaration>> {
    let mut grouped = BTreeMap::<_, Vec<_>>::new();
    for declaration in declarations {
        grouped
            .entry((
                declaration.node.qualified_name.clone(),
                declaration.node.kind.as_str().to_owned(),
            ))
            .or_default()
            .push(declaration);
    }
    grouped
}

fn entry(
    declaration: &Declaration,
    previous: Option<&Declaration>,
    path: &str,
    revision: &str,
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
            Some(excerpt(declaration, 0))
        } else {
            previous.map(|old| excerpt(old, preview_start))
        },
        after_excerpt: if change == "removed" {
            None
        } else {
            Some(excerpt(declaration, preview_start))
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

fn excerpt(declaration: &Declaration, requested_start: usize) -> PrContextExcerptV1 {
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
        revision: declaration.revision.clone(),
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

struct OutsideText<'a> {
    parts: Vec<&'a str>,
    lines: Vec<Range<u32>>,
}

/// Use the extractor's exact byte spans, including columns. A declaration
/// does not own an import or top-level expression sharing its first/last line.
/// Keep the slices separate so moving outside text across declarations also
/// changes the comparison, while ordinary body edits leave them unchanged.
fn outside_text<'a>(
    declarations: &[Declaration],
    source: &'a str,
) -> std::result::Result<OutsideText<'a>, String> {
    let mut ranges = declarations
        .iter()
        .map(|declaration| declaration.span.clone())
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| (range.start, range.end));
    let mut merged = Vec::<Range<usize>>::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    let mut line_starts = vec![0usize];
    line_starts.extend(source.match_indices('\n').map(|(offset, _)| offset + 1));
    let mut outside = OutsideText {
        parts: Vec::new(),
        lines: Vec::new(),
    };
    let mut offset = 0;
    let end = source.len()..source.len();
    for range in merged.iter().chain(std::iter::once(&end)) {
        let text = source
            .get(offset..range.start)
            .ok_or_else(|| "invalid uncovered source span".to_owned())?;
        outside.parts.push(text);
        if !text.trim().is_empty() {
            let first = offset + text.len() - text.trim_start().len();
            let last = offset + text.trim_end().len() - 1;
            // Historical blobs are bounded at 8 MiB, so line indices fit u32.
            let first_line = line_starts.partition_point(|start| *start <= first) - 1;
            let last_line = line_starts.partition_point(|start| *start <= last) - 1;
            outside.lines.push(first_line as u32..last_line as u32 + 1);
        }
        offset = range.end;
    }
    Ok(outside)
}

fn first_outside_changed_line(changed: &Range<u32>, outside: &[Range<u32>]) -> Option<u32> {
    outside.iter().find_map(|range| {
        let line = changed.start.max(range.start);
        (line < changed.end.min(range.end)).then_some(line)
    })
}

fn compare_declarations(
    brief: &mut SourceBrief,
    before: Vec<Declaration>,
    after: Vec<Declaration>,
    path: &str,
    old_path: &str,
    base: &GitOidV1,
    head: &GitOidV1,
    renamed: bool,
) -> usize {
    let mut ambiguous = 0;
    let mut before = group_declarations(before);
    for (key, mut new_group) in group_declarations(after) {
        let mut old_group = before.remove(&key).unwrap_or_default();
        // Visibility can be represented outside the declaration's own syntax
        // span (for example TypeScript's export wrapper).
        new_group.retain(|new| {
            if let Some(position) = old_group.iter().position(|old| {
                old.content == new.content
                    && old.node.signature == new.node.signature
                    && old.node.visibility == new.node.visibility
            }) {
                let old = old_group.remove(position);
                if renamed {
                    brief
                        .entries
                        .push(entry(new, Some(&old), path, head.as_str(), "renamed"));
                }
                false
            } else {
                true
            }
        });
        if new_group.len() > 1 || old_group.len() > 1 {
            ambiguous += 1;
            for new in &new_group {
                let mut point = entry(new, None, path, head.as_str(), "changed_overload");
                point.reason = "Overload group changed; before/after pairing is ambiguous. Inspect this declaration and the file diff.".to_owned();
                brief.entries.push(point);
            }
            for old in &old_group {
                let mut point = entry(old, None, old_path, base.as_str(), "removed");
                point.change = "changed_overload".to_owned();
                point.reason =
                    "Before-side declaration in an ambiguous changed overload group.".to_owned();
                brief.entries.push(point);
            }
        } else if let Some(new) = new_group.first() {
            let old = old_group.first();
            let change = match old {
                None => "added",
                Some(old) if old.node.visibility != new.node.visibility => "visibility_changed",
                Some(old) if old.node.signature != new.node.signature => "signature_changed",
                Some(_) => "modified",
            };
            let mut point = entry(new, old, path, head.as_str(), change);
            if change == "visibility_changed"
                && let Some(old) = old
            {
                point.reason = format!(
                    "Declaration visibility changed from {} to {}; inspect its public contract and consumers.",
                    old.node.visibility.as_str(),
                    new.node.visibility.as_str()
                );
            }
            brief.entries.push(point);
            if let Some(old) = old
                && old.node.kind.is_callable_kind()
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
        } else if let Some(old) = old_group.first() {
            brief
                .entries
                .push(entry(old, None, old_path, base.as_str(), "removed"));
        }
    }
    for old in before.into_values().flatten() {
        brief
            .entries
            .push(entry(&old, None, old_path, base.as_str(), "removed"));
    }
    ambiguous
}

pub(super) fn compare_source(
    git: &NativeGitIntelligence,
    base: &str,
    head: &str,
    cancelled: &dyn Fn() -> bool,
) -> std::result::Result<SourceBrief, String> {
    checkpoint(cancelled)?;
    let base = GitOidV1::new(base).map_err(|error| error.to_string())?;
    let head = GitOidV1::new(head).map_err(|error| error.to_string())?;
    let files = git
        .diff_file_metadata(&base, &head)
        .map_err(|error| error.to_string())?;
    let mut brief = SourceBrief {
        additions: 0,
        deletions: 0,
        changes: Vec::new(),
        areas: Vec::new(),
        entries: Vec::new(),
        quality: Vec::new(),
        source: PrContextSourceV1 {
            state: PrContextSourceStateV1::Ready,
            detail: "Exact merge-base and head blobs; declaration contracts compared. Line totals use the native Myers diff without materializing a full PR patch.".to_owned(),
        },
        clean_at_head: false,
    };
    let registry = LanguageRegistry::new();
    let mut areas = BTreeMap::<String, PrContextAreaV1>::new();
    let mut gaps = 0usize;
    let mut unreadable = 0usize;
    let mut ambiguous = 0usize;
    for file in files {
        checkpoint(cancelled)?;
        let old_path = file.original_path.as_deref().unwrap_or(&file.path);
        let submodule = file
            .new_mode
            .as_ref()
            .is_some_and(|mode| mode.is_submodule())
            || file
                .old_mode
                .as_ref()
                .is_some_and(|mode| mode.is_submodule());
        let before = if file.change == GitChangeKindV1::Added {
            Ok(String::new())
        } else if submodule {
            Err("submodule change".to_owned())
        } else {
            read_source(git, &base, old_path)
        };
        checkpoint(cancelled)?;
        let after = if file.change == GitChangeKindV1::Deleted {
            Ok(String::new())
        } else if submodule {
            Err("submodule change".to_owned())
        } else {
            read_source(git, &head, &file.path)
        };
        let removed = file.change == GitChangeKindV1::Deleted;
        let mut additions = None;
        let mut deletions = None;
        match before.and_then(|before| after.map(|after| (before, after))) {
            Ok((before_source, after_source)) => {
                let input = gix::diff::blob::InternedInput::new(
                    before_source.as_str(),
                    after_source.as_str(),
                );
                let diff = gix::diff::blob::diff_with_slider_heuristics(
                    gix::diff::blob::Algorithm::Myers,
                    &input,
                );
                additions = Some(diff.count_additions());
                deletions = Some(diff.count_removals());
                let hunks = diff.hunks().collect::<Vec<_>>();
                checkpoint(cancelled)?;
                let before = if file.change == GitChangeKindV1::Added {
                    Ok(Vec::new())
                } else {
                    declarations(&registry, &before_source, &base, old_path, &file.path)
                };
                checkpoint(cancelled)?;
                let after = if removed {
                    Ok(Vec::new())
                } else {
                    declarations(&registry, &after_source, &head, &file.path, &file.path)
                };
                match before.and_then(|before| after.map(|after| (before, after))) {
                    Ok((before, after)) => {
                        let before_outside = outside_text(&before, &before_source)?;
                        let after_outside = outside_text(&after, &after_source)?;
                        let outside = (before_outside.parts != after_outside.parts)
                            .then(|| {
                                hunks.iter().find_map(|hunk| {
                                    first_outside_changed_line(&hunk.after, &after_outside.lines)
                                        .map(|line| (false, line))
                                        .or_else(|| {
                                            first_outside_changed_line(
                                                &hunk.before,
                                                &before_outside.lines,
                                            )
                                            .map(|line| (true, line))
                                        })
                                })
                            })
                            .flatten();
                        ambiguous += compare_declarations(
                            &mut brief,
                            before,
                            after,
                            &file.path,
                            old_path,
                            &base,
                            &head,
                            file.change == GitChangeKindV1::Renamed,
                        );
                        if let Some((before_side, line)) = outside {
                            brief.entries.push(file_entry(
                                if before_side { old_path } else { &file.path },
                                if before_side { base.as_str() } else { head.as_str() },
                                line + 1,
                                "A changed line contains text outside retained declarations, possibly sharing their line; inspect its imports and module wiring. The line diff does not isolate which portion changed.".to_owned(),
                            ));
                        } else if hunks.is_empty() && file.old_mode != file.new_mode {
                            brief.entries.push(file_entry(if removed { old_path } else { &file.path },
                                if removed { base.as_str() } else { head.as_str() }, 1,
                                "Git file mode changed; inspect its existence, executable or file-type contract.".to_owned()));
                        }
                    }
                    Err(reason) => {
                        gaps += 1;
                        let (before_side, line) = hunks.first().map_or((removed, 1), |hunk| {
                            if hunk.after.is_empty() {
                                (true, hunk.before.start + 1)
                            } else {
                                (false, hunk.after.start + 1)
                            }
                        });
                        brief.entries.push(file_entry(
                            if before_side { old_path } else { &file.path },
                            if before_side {
                                base.as_str()
                            } else {
                                head.as_str()
                            },
                            line,
                            format!("File-level entrypoint: {reason}."),
                        ));
                    }
                }
            }
            Err(reason) => {
                gaps += 1;
                unreadable += 1;
                if !submodule && !matches!(reason.as_str(), "binary source" | "non-UTF-8 source") {
                    brief.entries.push(file_entry(
                        if removed { old_path } else { &file.path },
                        if removed {
                            base.as_str()
                        } else {
                            head.as_str()
                        },
                        1,
                        format!("File-level entrypoint: {reason}; line totals unavailable."),
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
            file: file.path,
            previous_file: file.original_path,
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
            "Exact Git comparison; {gaps} files have file-level analysis only, {ambiguous} overload groups cannot be paired. Native Myers line totals exclude {unreadable} binary, submodule, or unreadable files; their file totals are null."
        );
    }
    checkpoint(cancelled)?;
    match git.status() {
        Ok(status) => {
            brief.clean_at_head = status.head.commit().is_some_and(|oid| oid == &head)
                && !status.coverage.leaves_state_unread()
                && status.staged_count() == 0 && status.unstaged_count() == 0
                && status.conflicted_count() == 0 && status.untracked_count() == 0;
            if !brief.clean_at_head {
                brief.quality.push(PrContextQualityV1 { signal: "checkout_differs".to_owned(), observation: "The checkout is dirty or is not the compared head. Source entrypoints refer to committed Git blobs.".to_owned() });
            }
        }
        Err(error) => brief.quality.push(PrContextQualityV1 { signal: "checkout_unverified".to_owned(), observation: format!("Worktree status unavailable: {error}. Graph enrichment cannot prove correspondence to the compared head.") }),
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
