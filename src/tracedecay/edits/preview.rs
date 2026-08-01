//! Dry-run preview rendering shared by every edit primitive: a bounded
//! single-hunk diff of the changed region, the success-message wrapper that
//! marks dry runs, the leading-doc/attr heuristic `replace_symbol` uses to
//! decide whether to warn about dropped documentation, and the single line
//! classifier that heuristic shares with `move_symbol`'s header scanner and
//! its inner-doc skip loop.

/// What a single source line looks like when scanning a file's leading
/// region. One classifier shared by every place that used to hand-roll its
/// own prefix checks: [`leading_doc_or_attr`] (is the first non-blank line
/// documentation?), `move_symbol`'s import-insertion header scanner (which
/// leading lines belong to the header vs. the first item), and its `//!`
/// skip loop (never let a moved span start on an inner module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tracedecay) enum LeadingKind {
    /// Empty (after trimming leading whitespace).
    Blank,
    /// Inner doc comment (`//!`) — documents the *enclosing* item/module.
    InnerDoc,
    /// Outer doc comment (`///` or `/**`) — documents the *next* item.
    OuterDoc,
    /// Plain line comment (`//`, but not `///`/`//!`).
    LineComment,
    /// Block comment (`/*`, but not `/**`).
    BlockComment,
    /// Attribute (`#[` or `#!`).
    Attribute,
    /// `use`, `pub use`, or `extern crate`.
    UseImport,
    /// Anything else (real code).
    Code,
}

/// Classifies `line` (leading-whitespace-insensitive) into the kind of
/// leading-region content it looks like.
pub(in crate::tracedecay) fn classify_leading_line(line: &str) -> LeadingKind {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        LeadingKind::Blank
    } else if trimmed.starts_with("//!") {
        LeadingKind::InnerDoc
    } else if trimmed.starts_with("///") || trimmed.starts_with("/**") {
        LeadingKind::OuterDoc
    } else if trimmed.starts_with("//") {
        LeadingKind::LineComment
    } else if trimmed.starts_with("/*") {
        LeadingKind::BlockComment
    } else if trimmed.starts_with("#[") || trimmed.starts_with("#!") {
        LeadingKind::Attribute
    } else if trimmed.starts_with("use ")
        || trimmed.starts_with("pub use ")
        || trimmed.starts_with("extern crate")
    {
        LeadingKind::UseImport
    } else {
        LeadingKind::Code
    }
}

/// Cheap heuristic: does `source`'s first non-blank line look like a leading
/// doc-comment (`//`, `///`, `//!`), block comment (`/*`), or attribute
/// (`#[`, `#!`)? Used only to decide whether a `replace_symbol` note should
/// warn that the replacement text may have dropped the item's docs/attrs.
pub(in crate::tracedecay) fn leading_doc_or_attr(source: &str) -> bool {
    source
        .lines()
        .map(classify_leading_line)
        .find(|kind| *kind != LeadingKind::Blank)
        .is_some_and(|kind| {
            matches!(
                kind,
                LeadingKind::InnerDoc
                    | LeadingKind::OuterDoc
                    | LeadingKind::LineComment
                    | LeadingKind::BlockComment
                    | LeadingKind::Attribute
            )
        })
}

/// Unchanged context lines shown on each side of the changed region in a
/// dry-run preview diff.
pub(in crate::tracedecay) const PREVIEW_DIFF_CONTEXT: usize = 3;

/// Hard cap on the number of lines emitted in a dry-run preview diff. Keeps the
/// preview bounded even when an edit rewrites a large span; the remainder is
/// noted as truncated.
pub(in crate::tracedecay) const MAX_PREVIEW_DIFF_LINES: usize = 200;

/// Success-path message wrapper: on a real edit returns `base` verbatim; on a
/// dry run wraps it to make clear that nothing was written and only a preview
/// was produced.
pub(in crate::tracedecay) fn edit_success_message(dry_run: bool, base: &str) -> String {
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
pub(in crate::tracedecay) fn bounded_region_diff(
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

#[cfg(test)]
mod tests {
    use super::{
        LeadingKind, bounded_region_diff, classify_leading_line, edit_success_message,
        leading_doc_or_attr,
    };

    #[test]
    fn classify_leading_line_covers_every_kind() {
        assert_eq!(classify_leading_line(""), LeadingKind::Blank);
        assert_eq!(classify_leading_line("   \t"), LeadingKind::Blank);
        assert_eq!(classify_leading_line("//! inner"), LeadingKind::InnerDoc);
        assert_eq!(classify_leading_line("/// outer"), LeadingKind::OuterDoc);
        assert_eq!(
            classify_leading_line("/** block doc */"),
            LeadingKind::OuterDoc
        );
        assert_eq!(classify_leading_line("// plain"), LeadingKind::LineComment);
        assert_eq!(
            classify_leading_line("/* block */"),
            LeadingKind::BlockComment
        );
        assert_eq!(classify_leading_line("#[inline]"), LeadingKind::Attribute);
        assert_eq!(
            classify_leading_line("#![allow(dead_code)]"),
            LeadingKind::Attribute
        );
        assert_eq!(
            classify_leading_line("use crate::x;"),
            LeadingKind::UseImport
        );
        assert_eq!(
            classify_leading_line("pub use crate::x;"),
            LeadingKind::UseImport
        );
        assert_eq!(
            classify_leading_line("extern crate foo;"),
            LeadingKind::UseImport
        );
        assert_eq!(classify_leading_line("fn f() {}"), LeadingKind::Code);
        // Leading whitespace never changes the classification.
        assert_eq!(classify_leading_line("   //! inner"), LeadingKind::InnerDoc);
    }

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
