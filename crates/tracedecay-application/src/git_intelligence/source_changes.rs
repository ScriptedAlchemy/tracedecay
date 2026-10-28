//! Shared declaration comparison from exact selected source, without graph state.

use std::collections::BTreeMap;
use std::ops::Range;

use tracedecay_code_extraction::LanguageRegistry;
use tracedecay_contracts::retrieval::{GitContextSourceSideV1, GitDeclarationChangeKindV1};
use tracedecay_domain::{GitChangeKindV1, Node, NodeKind};

use super::{GitComparedSource, GitDiffFileMetadata};

#[derive(Clone)]
pub struct GitSourceDeclaration {
    pub node: Node,
    pub content: String,
    pub span: Range<usize>,
    pub path: String,
}

pub struct GitChangedDeclaration {
    pub change: GitDeclarationChangeKindV1,
    pub before: Option<GitSourceDeclaration>,
    pub after: Option<GitSourceDeclaration>,
}

pub struct GitSourceAnalysis {
    pub declarations: Vec<GitChangedDeclaration>,
    /// A zero-based source line containing changed text outside declaration
    /// spans; the source side is retained for deletion-only changes.
    pub outside: Option<(GitContextSourceSideV1, u32)>,
    pub ambiguous_groups: usize,
}

/// Both sides use the head's logical path when extracting names, so a path
/// rename cannot manufacture a deletion and addition of every declaration.
fn declarations(
    registry: &LanguageRegistry,
    source: &str,
    path: &str,
    logical_path: &str,
) -> std::result::Result<Vec<GitSourceDeclaration>, String> {
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
            Ok(GitSourceDeclaration {
                node,
                content,
                span,
                path: path.to_owned(),
            })
        })
        .collect()
}

fn group_declarations(
    declarations: Vec<GitSourceDeclaration>,
) -> BTreeMap<(String, String), Vec<GitSourceDeclaration>> {
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
    for group in grouped.values_mut() {
        group.sort_by_key(|declaration| declaration.span.start);
    }
    grouped
}

fn declaration_unchanged(old: &GitSourceDeclaration, new: &GitSourceDeclaration) -> bool {
    old.content == new.content
        && old.node.signature == new.node.signature
        && old.node.visibility == new.node.visibility
}

/// Duplicate definitions and overloads can depend on source order even when
/// their individual bodies are unchanged. Preserve reordered exact matches as
/// an ambiguous group instead of treating the declarations as a set.
fn exact_matches_reordered(
    before: &[GitSourceDeclaration],
    after: &[GitSourceDeclaration],
) -> bool {
    if before.len() < 2 || after.len() < 2 {
        return false;
    }
    let mut matched = vec![false; before.len()];
    let mut previous = None;
    for new in after {
        if let Some(position) = before
            .iter()
            .enumerate()
            .position(|(index, old)| !matched[index] && declaration_unchanged(old, new))
        {
            if previous.is_some_and(|previous| position < previous) {
                return true;
            }
            matched[position] = true;
            previous = Some(position);
        }
    }
    false
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
    declarations: &[GitSourceDeclaration],
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

pub fn analyze_source_changes(
    registry: &LanguageRegistry,
    file: &GitDiffFileMetadata,
    source: &GitComparedSource,
) -> Result<GitSourceAnalysis, String> {
    let old_path = file.original_path.as_deref().unwrap_or(&file.path);
    let before = if file.old_mode.is_none() {
        Vec::new()
    } else {
        declarations(registry, &source.before, old_path, &file.path)?
    };
    let after = if file.new_mode.is_none() {
        Vec::new()
    } else {
        declarations(registry, &source.after, &file.path, &file.path)?
    };
    let before_outside = outside_text(&before, &source.before)?;
    let after_outside = outside_text(&after, &source.after)?;
    let outside = (before_outside.parts != after_outside.parts)
        .then(|| {
            source.changed_lines.iter().find_map(|hunk| {
                first_outside_changed_line(&hunk.after, &after_outside.lines)
                    .map(|line| (GitContextSourceSideV1::After, line))
                    .or_else(|| {
                        first_outside_changed_line(&hunk.before, &before_outside.lines)
                            .map(|line| (GitContextSourceSideV1::Before, line))
                    })
            })
        })
        .flatten();
    let mut analysis = GitSourceAnalysis {
        declarations: Vec::new(),
        outside,
        ambiguous_groups: 0,
    };
    let mut before = group_declarations(before);
    for (key, new_group) in group_declarations(after) {
        let mut old_group = before.remove(&key).unwrap_or_default();
        let reordered = exact_matches_reordered(&old_group, &new_group);
        let mut pending = Vec::new();
        for new in new_group {
            if !reordered
                && let Some(position) = old_group
                    .iter()
                    .position(|old| declaration_unchanged(old, &new))
            {
                let old = old_group.remove(position);
                if file.change == GitChangeKindV1::Renamed {
                    analysis.declarations.push(GitChangedDeclaration {
                        change: GitDeclarationChangeKindV1::Renamed,
                        before: Some(old),
                        after: Some(new),
                    });
                }
            } else {
                pending.push(new);
            }
        }
        if pending.len() > 1 || old_group.len() > 1 {
            analysis.ambiguous_groups += 1;
            for new in pending {
                analysis.declarations.push(GitChangedDeclaration {
                    change: GitDeclarationChangeKindV1::AmbiguousOverload,
                    before: None,
                    after: Some(new),
                });
            }
            for old in old_group {
                analysis.declarations.push(GitChangedDeclaration {
                    change: GitDeclarationChangeKindV1::AmbiguousOverload,
                    before: Some(old),
                    after: None,
                });
            }
        } else {
            let old = old_group.pop();
            let new = pending.pop();
            let change = match (&old, &new) {
                (None, None) => continue,
                (None, Some(_)) => GitDeclarationChangeKindV1::Added,
                (Some(_), None) => GitDeclarationChangeKindV1::Removed,
                (Some(old), Some(new)) if old.node.visibility != new.node.visibility => {
                    GitDeclarationChangeKindV1::VisibilityChanged
                }
                (Some(old), Some(new)) if old.node.signature != new.node.signature => {
                    GitDeclarationChangeKindV1::SignatureChanged
                }
                _ => GitDeclarationChangeKindV1::Modified,
            };
            analysis.declarations.push(GitChangedDeclaration {
                change,
                before: old,
                after: new,
            });
        }
    }
    for old in before.into_values().flatten() {
        analysis.declarations.push(GitChangedDeclaration {
            change: GitDeclarationChangeKindV1::Removed,
            before: Some(old),
            after: None,
        });
    }
    Ok(analysis)
}
