//! Declaration changes from exact Git blobs, independent of index readiness.

use std::collections::{BTreeMap, HashSet};

use tracedecay_application::git_intelligence::NativeGitIntelligence;
use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_contracts::git::{GIT_HISTORICAL_BLOB_MAX_BYTES, GitHistoricalBlobRequestV1};
use tracedecay_contracts::retrieval::{
    PrContextAreaV1, PrContextEntryPointV1, PrContextExcerptV1, PrContextFileV1,
    PrContextLocationV1, PrContextQualityV1, PrContextSourceStateV1, PrContextSourceV1,
};
use tracedecay_domain::{GitChangeKindV1, GitDiffScopeV1, GitOidV1, Node, NodeKind};

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
    revision: String,
    path: String,
}

/// Both sides use the head's logical path when extracting names, so a path
/// rename cannot manufacture a deletion and addition of every declaration.
fn declarations(
    git: &NativeGitIntelligence,
    registry: &LanguageRegistry,
    revision: &GitOidV1,
    path: &str,
    logical_path: &str,
) -> std::result::Result<Vec<Declaration>, String> {
    let extractor = registry
        .extractor_for_file(logical_path)
        .ok_or_else(|| "unsupported language".to_owned())?;
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
    let source = String::from_utf8(bytes).map_err(|_| "non-UTF-8 source".to_owned())?;
    let artifact = extractor.extract_artifact(logical_path, &source);
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
            let content = start
                .zip(end)
                .and_then(|(start, end)| source.get(start..end))
                .ok_or_else(|| format!("invalid source span for {}", node.qualified_name))?
                .to_owned();
            Ok(Declaration {
                node,
                content,
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
    PrContextEntryPointV1 {
        location: PrContextLocationV1 {
            file: path.to_owned(),
            line: node.start_line + 1,
            end_line: node.end_line + 1,
        },
        revision: revision.to_owned(),
        symbol: node.qualified_name.clone(),
        change: change.to_owned(),
        before_signature: if change == "removed" {
            node.signature.clone()
        } else {
            previous.and_then(|old| old.node.signature.clone())
        },
        after_signature: if change == "removed" {
            None
        } else {
            node.signature.clone()
        },
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

pub(super) fn compare_source(
    git: &NativeGitIntelligence,
    base: &str,
    head: &str,
    cancelled: &dyn Fn() -> bool,
) -> std::result::Result<SourceBrief, String> {
    checkpoint(cancelled)?;
    let base = GitOidV1::new(base).map_err(|error| error.to_string())?;
    let head = GitOidV1::new(head).map_err(|error| error.to_string())?;
    let diff = git
        .diff(&GitDiffScopeV1::CommitRange {
            base: base.clone(),
            head: head.clone(),
        })
        .map_err(|error| error.to_string())?;
    let mut brief = SourceBrief {
        additions: diff.insertions(),
        deletions: diff.deletions(),
        changes: Vec::new(),
        areas: Vec::new(),
        entries: Vec::new(),
        quality: Vec::new(),
        source: PrContextSourceV1 {
            state: PrContextSourceStateV1::Ready,
            detail: "Exact merge-base and head blobs; declaration content and signatures compared."
                .to_owned(),
        },
        clean_at_head: false,
    };
    let registry = LanguageRegistry::new();
    let mut areas = BTreeMap::<String, PrContextAreaV1>::new();
    let mut gaps = 0usize;
    let mut ambiguous = 0usize;
    for file in diff.files {
        checkpoint(cancelled)?;
        let old_path = file.original_path.as_deref().unwrap_or(&file.path);
        let role = classify_file_role(&file.path, &HashSet::new());
        let area = area_name(&file.path);
        let area = areas.entry(area.clone()).or_insert(PrContextAreaV1 {
            area,
            files: 0,
            additions: 0,
            deletions: 0,
        });
        area.files += 1;
        area.additions += u64::from(file.insertions.unwrap_or(0));
        area.deletions += u64::from(file.deletions.unwrap_or(0));
        let mut changed_names = Vec::new();
        let before_count = brief.entries.len();
        let read = if file.binary || file.submodule {
            Err("binary or submodule change; inspect its file-level diff".to_owned())
        } else {
            let before = if file.change == GitChangeKindV1::Added {
                Ok(Vec::new())
            } else {
                declarations(git, &registry, &base, old_path, &file.path)
            };
            checkpoint(cancelled)?;
            let after = if file.change == GitChangeKindV1::Deleted {
                Ok(Vec::new())
            } else {
                declarations(git, &registry, &head, &file.path, &file.path)
            };
            before.and_then(|before| after.map(|after| (before, after)))
        };
        match read {
            Ok((before, after)) => {
                let mut before = group_declarations(before);
                for (key, mut new_group) in group_declarations(after) {
                    let mut old_group = before.remove(&key).unwrap_or_default();
                    // Names alone do not identify overloads. Cancel identical
                    // declarations first; disclose ambiguous residual matches.
                    new_group.retain(|new| {
                        if let Some(position) =
                            old_group.iter().position(|old| old.content == new.content)
                        {
                            let old = old_group.remove(position);
                            if file.change == GitChangeKindV1::Renamed {
                                brief.entries.push(entry(
                                    new,
                                    Some(&old),
                                    &file.path,
                                    head.as_str(),
                                    "renamed",
                                ));
                            }
                            false
                        } else {
                            true
                        }
                    });
                    if new_group.len() > 1 || old_group.len() > 1 {
                        ambiguous += 1;
                        for new in &new_group {
                            let mut point =
                                entry(new, None, &file.path, head.as_str(), "changed_overload");
                            point.reason = "Overload group changed; before/after pairing is ambiguous. Inspect this declaration and the file diff.".to_owned();
                            brief.entries.push(point);
                        }
                        for old in &old_group {
                            let mut point =
                                entry(old, None, old_path, base.as_str(), "changed_overload");
                            point.before_signature = old.node.signature.clone();
                            point.after_signature = None;
                            point.before_excerpt = point.after_excerpt.take();
                            point.reason =
                                "Before-side declaration in an ambiguous changed overload group."
                                    .to_owned();
                            brief.entries.push(point);
                        }
                    } else if let Some(new) = new_group.first() {
                        let old = old_group.first();
                        let change = match old {
                            None => "added",
                            Some(old) if old.node.signature != new.node.signature => {
                                "signature_changed"
                            }
                            Some(_) => "modified",
                        };
                        brief
                            .entries
                            .push(entry(new, old, &file.path, head.as_str(), change));
                        if let Some(old) = old
                            && old.node.kind.is_callable_kind()
                            && old.node.complexity_analysis.is_complete()
                            && new.node.complexity_analysis.is_complete()
                            && old.node.branches != new.node.branches
                        {
                            brief.quality.push(PrContextQualityV1 {
                                signal: "branch_count_changed".to_owned(),
                                observation: format!("{}: {} → {} branches (static count, not a correctness verdict).", new.node.qualified_name, old.node.branches, new.node.branches),
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
            }
            Err(reason) => {
                gaps += 1;
                if !file.binary && !file.submodule {
                    let removed = file.change == GitChangeKindV1::Deleted;
                    let line = file.hunks.first().map_or(1, |hunk| {
                        if removed {
                            hunk.old_start.max(1)
                        } else {
                            hunk.new_start.max(1)
                        }
                    });
                    brief.entries.push(PrContextEntryPointV1 {
                        location: PrContextLocationV1 {
                            file: if removed {
                                old_path.to_owned()
                            } else {
                                file.path.clone()
                            },
                            line,
                            end_line: line,
                        },
                        revision: if removed {
                            base.as_str()
                        } else {
                            head.as_str()
                        }
                        .to_owned(),
                        symbol: file.path.clone(),
                        change: "file_change".to_owned(),
                        before_signature: None,
                        after_signature: None,
                        before_excerpt: None,
                        after_excerpt: None,
                        reason: format!("File-level entrypoint: {reason}."),
                    });
                }
            }
        }
        if brief.entries.len() == before_count
            && !file.binary
            && !file.submodule
            && !file.hunks.is_empty()
        {
            // Imports, module declarations, and prose between declarations
            // still deserve a reading location when no retained symbol owns
            // the changed text.
            let removed = file.change == GitChangeKindV1::Deleted;
            let hunk = &file.hunks[0];
            let line = if removed {
                hunk.old_start.max(1)
            } else {
                hunk.new_start.max(1)
            };
            brief.entries.push(PrContextEntryPointV1 {
                location: PrContextLocationV1 {
                    file: if removed {
                        old_path.to_owned()
                    } else {
                        file.path.clone()
                    },
                    line,
                    end_line: line,
                },
                revision: if removed {
                    base.as_str()
                } else {
                    head.as_str()
                }
                .to_owned(),
                symbol: file.path.clone(),
                change: "file_change".to_owned(),
                before_signature: None,
                after_signature: None,
                before_excerpt: None,
                after_excerpt: None,
                reason: "Changed text outside retained declaration bodies; inspect this file diff."
                    .to_owned(),
            });
        }
        changed_names.extend(
            brief.entries[before_count..]
                .iter()
                .filter(|entry| entry.change != "file_change")
                .map(|entry| entry.symbol.clone()),
        );
        changed_names.sort();
        changed_names.dedup();
        brief.changes.push(PrContextFileV1 {
            file: file.path,
            previous_file: file.original_path,
            change: change_name(file.change).to_owned(),
            role,
            additions: file.insertions,
            deletions: file.deletions,
            symbols: changed_names,
        });
    }
    brief.areas = areas.into_values().collect();
    brief
        .areas
        .sort_by_key(|area| std::cmp::Reverse(area.additions + area.deletions));
    if gaps > 0 || ambiguous > 0 || !diff.coverage.is_complete() {
        brief.source.state = PrContextSourceStateV1::Partial;
        brief.source.detail = format!(
            "Exact Git comparison; {gaps} files have file-level analysis only, {ambiguous} overload groups cannot be paired, Git coverage complete: {}.",
            diff.coverage.is_complete()
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
