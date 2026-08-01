//! Text rendering for `move_symbol`: collapsing the blank-line separator a
//! removed span leaves behind, assembling the destination file's content
//! (imports plus the moved block), inserting imports after a file's header
//! region, de-duplicating imports against what the destination already has,
//! and the combined dry-run diff of both sides of the move.

use std::collections::HashSet;

use super::super::edits::{
    LeadingKind, MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff,
    classify_leading_line, splice_lines,
};

/// Removes `lines[start..=end]` and collapses the blank-line separator the
/// removed item left behind so the source stays tidy.
pub(super) fn remove_span_with_cleanup(lines: &[&str], start: usize, end: usize) -> Vec<String> {
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
pub(super) fn build_dest_content(
    dest_original: &str,
    imports: &[String],
    moved_text: &str,
) -> String {
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
pub(super) fn insert_imports(dest_source: &str, imports: &[String]) -> String {
    if imports.is_empty() {
        return dest_source.to_string();
    }
    let lines: Vec<&str> = dest_source.lines().collect();
    let mut idx = 0;
    while idx < lines.len() {
        // Stop before an OUTER doc-comment block (`///` or `/**`): it documents
        // the first item, and inserting a `use` between the doc and its item
        // detaches the doc. Inner docs (`//!`), plain comments (`//`), blank
        // lines, and existing imports stay in the header region.
        match classify_leading_line(lines[idx]) {
            LeadingKind::OuterDoc => break,
            LeadingKind::Blank | LeadingKind::InnerDoc | LeadingKind::LineComment => idx += 1,
            LeadingKind::UseImport => idx += 1,
            LeadingKind::BlockComment | LeadingKind::Attribute | LeadingKind::Code => break,
        }
    }
    let mut rebuilt: Vec<&str> = Vec::with_capacity(lines.len() + imports.len());
    rebuilt.extend_from_slice(&lines[..idx]);
    rebuilt.extend(imports.iter().map(String::as_str));
    rebuilt.extend_from_slice(&lines[idx..]);
    splice_lines(&rebuilt, dest_source.ends_with('\n'))
}

/// Drops imports already present verbatim in the destination and de-duplicates
/// within the batch, preserving order.
pub(super) fn dedup_preserve(imports: &[String], dest_original: &str) -> Vec<String> {
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

/// Builds a combined dry-run diff of the source removal and destination
/// insertion.
pub(super) fn combined_diff(
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
    use super::{build_dest_content, dedup_preserve, insert_imports, remove_span_with_cleanup};

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
