//! Text rendering for `move_symbol`: collapsing the blank-line separator a
//! removed span leaves behind, assembling the destination file's content
//! (imports plus the moved block), inserting imports after a file's header
//! region, de-duplicating imports against what the destination already has,
//! and the combined dry-run diff of both sides of the move.

use std::collections::HashSet;

use tracedecay_code_extraction::ts_provider::parse_extractor_source;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tree_sitter::Tree;

use crate::edits::{MAX_PREVIEW_DIFF_LINES, PREVIEW_DIFF_CONTEXT, bounded_region_diff};

use super::use_parsing::{destination_import_is_unconditional, use_bindings};

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
    dest_file: &str,
    dest_original: &str,
    imports: &[String],
    moved_text: &str,
) -> Result<String> {
    let moved_block = moved_text.trim_end_matches('\n');
    let mut content = if dest_file.ends_with(".rs") {
        // Even a dependency-free move must not append inside malformed syntax
        // or adopt dangling outer attributes from the destination.
        insert_imports(dest_original, imports)?
    } else if imports.is_empty() {
        dest_original.to_owned()
    } else {
        return Err(destination_syntax_error(
            "automatic dependency imports require a Rust destination",
        ));
    };
    let newline = line_ending(dest_original);
    if !content.is_empty() {
        if !content.ends_with('\n') {
            content.push_str(newline);
        }
        if !content.ends_with(&format!("{newline}{newline}")) {
            content.push_str(newline);
        }
    }
    content.push_str(moved_block);
    content.push('\n');
    Ok(content)
}

/// Inserts imports between complete top-level Rust items, preserving all
/// existing bytes and the attachments of outer docs and attributes.
pub(super) fn insert_imports(dest_source: &str, imports: &[String]) -> Result<String> {
    let offset = rust_import_offset(dest_source)?;
    if imports.is_empty() {
        return Ok(dest_source.to_string());
    }
    let newline = line_ending(dest_source);
    let (before, after) = dest_source.split_at(offset);
    let mut content = before.to_owned();
    if !before.is_empty() && !before.ends_with('\n') {
        content.push_str(newline);
    }
    content.push_str(&imports.join(newline));
    content.push_str(newline);
    content.push_str(after);
    Ok(content)
}

fn line_ending(source: &str) -> &'static str {
    if source
        .find('\n')
        .is_some_and(|offset| offset > 0 && source.as_bytes()[offset - 1] == b'\r')
    {
        "\r\n"
    } else {
        "\n"
    }
}

fn destination_syntax_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("source-edit-destination-syntax-unsupported", false, message)
}

fn parse_rust_destination(source: &str) -> Result<Tree> {
    let tree = parse_extractor_source("rust", "Rust", source).map_err(|error| {
        destination_syntax_error(format!("cannot parse Rust destination: {error}"))
    })?;
    if tree.root_node().has_error() {
        return Err(destination_syntax_error(
            "Rust destination contains incomplete or unsupported syntax",
        ));
    }
    Ok(tree)
}

fn rust_import_offset(source: &str) -> Result<usize> {
    let tree = parse_rust_destination(source)?;
    let root = tree.root_node();
    let mut offset = None;
    let mut seen_item = false;
    let mut pending_outer = false;
    for node in root.named_children(&mut root.walk()) {
        let comment = matches!(node.kind(), "line_comment" | "block_comment");
        if node.kind() == "inner_attribute_item"
            || (comment && node.child_by_field_name("inner").is_some())
        {
            if seen_item || pending_outer {
                return Err(destination_syntax_error(
                    "Rust destination has an inner attribute or doc after an item or outer attribute",
                ));
            }
        } else if node.kind() == "attribute_item"
            || (comment && node.child_by_field_name("outer").is_some())
        {
            // Outer attachments belong to the following item, including when
            // that item is itself an import. Never split this group.
            offset.get_or_insert(node.start_byte());
            pending_outer = true;
        } else if !comment && node.kind() != "shebang" {
            if !matches!(node.kind(), "use_declaration" | "extern_crate_declaration") {
                offset.get_or_insert(node.start_byte());
            }
            seen_item = true;
            pending_outer = false;
        }
    }
    if pending_outer {
        return Err(destination_syntax_error(
            "Rust destination ends with an outer attribute or doc without its item",
        ));
    }
    Ok(offset.unwrap_or(source.len()))
}

/// Drops matching unconditional module imports and de-duplicates within the
/// batch. Text in comments or nested scopes cannot supply a module binding.
/// Without name resolution, declared names and unknown wildcard bindings
/// conservatively prevent insertion that could change existing uses.
pub(super) fn dedup_preserve(
    imports: &[String],
    dest_original: &str,
    dest_module: Option<&str>,
) -> Result<Vec<String>> {
    if imports.is_empty() {
        return Ok(Vec::new());
    }
    let tree = parse_rust_destination(dest_original)?;
    let mut existing = HashSet::new();
    let mut bound_names = HashSet::new();
    let mut declared_names = HashSet::new();
    let mut attributed = HashSet::new();
    let mut has_wildcard = false;
    for node in tree.root_node().named_children(&mut tree.walk()) {
        if node.kind() != "use_declaration" {
            if let Some(name) = node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"))
                .and_then(|name| name.utf8_text(dest_original.as_bytes()).ok())
            {
                declared_names.insert(name.to_owned());
            }
            continue;
        }
        let bindings = node
            .child_by_field_name("argument")
            .and_then(|argument| use_bindings(argument, dest_original.as_bytes()))
            .ok_or_else(|| {
                destination_syntax_error("cannot identify a complete destination import")
            })?;
        has_wildcard |= bindings.wildcard;
        let unconditional = destination_import_is_unconditional(node, dest_original.as_bytes());
        for leaf in bindings.leaves {
            if unconditional {
                bound_names.insert(leaf.binding.clone());
                existing.insert(leaf);
            } else {
                attributed.insert(leaf.binding);
            }
        }
    }
    let import_source = imports.join("\n");
    let import_tree = parse_rust_destination(&import_source)?;
    let mut cursor = import_tree.walk();
    let mut declarations = import_tree
        .root_node()
        .named_children(&mut cursor)
        .filter(|node| node.kind() == "use_declaration");
    let mut out = Vec::new();
    for imp in imports {
        let declaration = declarations.next().ok_or_else(|| {
            destination_syntax_error("cannot identify an automatic dependency import")
        })?;
        let bindings = declaration
            .child_by_field_name("argument")
            .and_then(|argument| use_bindings(argument, import_source.as_bytes()))
            .ok_or_else(|| {
                destination_syntax_error("cannot identify an automatic dependency binding")
            })?;
        let [leaf] = bindings.leaves.as_slice() else {
            return Err(destination_syntax_error(
                "automatic dependency import must provide exactly one named binding",
            ));
        };
        if bindings.wildcard
            || bindings.anonymous
            || declaration.utf8_text(import_source.as_bytes()).ok() != Some(imp.trim())
        {
            return Err(destination_syntax_error(
                "automatic dependency import must be one complete named declaration",
            ));
        }
        if existing.contains(leaf) {
            continue;
        }
        if attributed.contains(&leaf.binding) {
            // A conditional import may or may not bind the name. Neither
            // dropping the dependency nor adding an unconditional duplicate
            // is safe without resolving that configuration.
            return Err(destination_syntax_error(format!(
                "automatic import conflicts with an attributed destination binding: {}",
                leaf.binding
            )));
        }
        if bound_names.contains(&leaf.binding) {
            return Err(destination_syntax_error(format!(
                "automatic import conflicts with a different destination binding: {}",
                leaf.binding
            )));
        }
        // An import of an item the destination module itself declares
        // (`use crate::SweepType;` inserted into `src/lib.rs`) would be a
        // self-import: the name is already in scope at the destination.
        if let Some(dest_module) = dest_module
            && let Some(path) = &leaf.canonical_path
            && path
                .strip_prefix(dest_module)
                .and_then(|tail| tail.strip_prefix("::"))
                .is_some_and(|name| !name.contains("::") && name == leaf.binding)
        {
            continue;
        }
        if declared_names.contains(&leaf.binding) {
            return Err(destination_syntax_error(format!(
                "automatic import overlaps a destination item name: {}",
                leaf.binding
            )));
        }
        if has_wildcard {
            // An explicit binding can silently replace a name supplied by a
            // glob. Without resolving it, the existing uses cannot be proven
            // to retain their meaning. Anonymous aliases do not bind names.
            return Err(destination_syntax_error(format!(
                "automatic import may shadow a destination wildcard binding: {}",
                leaf.binding
            )));
        }
        bound_names.insert(leaf.binding.clone());
        existing.insert(leaf.clone());
        out.push(imp.clone());
    }
    if declarations.next().is_some() {
        return Err(destination_syntax_error(
            "automatic dependency import contains extra declarations",
        ));
    }
    Ok(out)
}

/// Removes each orphaned single-leaf `use` line (only the moved symbol needed
/// it) from the residual source so the move does not leave dead imports.
pub(super) fn strip_orphaned_imports(source_modified: &str, orphaned: &[String]) -> String {
    if orphaned.is_empty() {
        return source_modified.to_string();
    }
    let ends_with_newline = source_modified.ends_with('\n');
    let mut remaining: Vec<&str> = orphaned.iter().map(String::as_str).collect();
    let lines: Vec<&str> = source_modified
        .lines()
        .filter(
            |line| match remaining.iter().position(|import| line.trim() == *import) {
                Some(index) => {
                    remaining.swap_remove(index);
                    false
                }
                None => true,
            },
        )
        .collect();
    let mut out = lines.join("\n");
    if ends_with_newline && !out.is_empty() {
        out.push('\n');
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
            "src/relocated.rs",
            "",
            &["use crate::pricing::LineItem;".to_string()],
            "/// doc\nfn f() {}\n",
        )
        .unwrap();
        assert_eq!(
            content,
            "use crate::pricing::LineItem;\n\n/// doc\nfn f() {}\n"
        );
    }

    #[test]
    fn build_dest_content_existing_file_appends_after_blank() {
        let content = build_dest_content(
            "src/relocated.rs",
            "//! mod\n\nfn existing() {}\n",
            &[],
            "fn f() {}",
        )
        .unwrap();
        assert_eq!(content, "//! mod\n\nfn existing() {}\n\nfn f() {}\n");
    }

    #[test]
    fn insert_imports_after_header_block() {
        let out = insert_imports(
            "//! module doc\n\nfn a() {}\n",
            &["use crate::X;".to_string()],
        )
        .unwrap();
        assert_eq!(out, "//! module doc\n\nuse crate::X;\nfn a() {}\n");
    }

    #[test]
    fn insert_imports_keeps_outer_doc_attached_to_first_item() {
        // An outer doc-comment (`///`) documents the first item; the `use` must
        // NOT be wedged between the doc and its `pub fn`.
        let out = insert_imports(
            "/// Docs for other.\npub fn other() {}\n",
            &["use crate::X;".to_string()],
        )
        .unwrap();
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
    fn insert_imports_preserves_comments_and_complete_same_line_items() {
        for (original, expected) in [
            (
                "/* License. */\nuse a::{B, C}; /* Import note. */\nfn marker() {}",
                "/* License. */\nuse a::{B, C}; /* Import note. */\nuse crate::X;\nfn marker() {}",
            ),
            (
                "use a::B; fn marker() {}",
                "use a::B; \nuse crate::X;\nfn marker() {}",
            ),
            ("// License.", "// License.\nuse crate::X;\n"),
            ("/* License. */", "/* License. */\nuse crate::X;\n"),
            ("//! Module.", "//! Module.\nuse crate::X;\n"),
        ] {
            assert_eq!(
                insert_imports(original, &["use crate::X;".to_owned()]).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn destination_validation_rejects_incomplete_or_misplaced_attachments() {
        for original in [
            "use a::{B, C",
            "/// Detached.\n",
            "#[allow(unused)]\n// Detached.\n",
            "use a::B;\n#![allow(unused)]\n",
            "fn keep() {}\n//! Misplaced module doc.\n",
        ] {
            assert!(
                build_dest_content("src/relocated.rs", original, &[], "fn moved() {}").is_err(),
                "must refuse without needing new imports: {original}"
            );
        }
    }

    #[test]
    fn non_rust_destination_without_imports_keeps_generic_append_support() {
        let original = "export const keep = 1;\n \t";
        assert_eq!(
            build_dest_content("src/relocated.ts", original, &[], "export const moved = 2;")
                .unwrap(),
            "export const keep = 1;\n \t\n\nexport const moved = 2;\n"
        );
        assert!(
            build_dest_content(
                "src/relocated.ts",
                original,
                &["use crate::X;".to_owned()],
                "export const moved = 2;"
            )
            .is_err()
        );
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
            None,
        )
        .unwrap();
        assert_eq!(out, vec!["use a::B;".to_string()]);
    }

    #[test]
    fn strip_orphaned_imports_removes_only_the_orphaned_use_lines() {
        let out = super::strip_orphaned_imports(
            "use crate::SweepType;\nuse std::fmt::Debug;\npub fn keep() {}\n",
            &["use crate::SweepType;".to_string()],
        );
        assert_eq!(out, "use std::fmt::Debug;\npub fn keep() {}\n");
    }

    #[test]
    fn dedup_preserve_skips_destination_self_imports() {
        let out = dedup_preserve(
            &[
                "use crate::SweepType;".to_string(),
                "use crate::nested::Other;".to_string(),
            ],
            "pub struct SweepType { pub value: i32 }\n",
            Some("crate"),
        )
        .unwrap();
        assert_eq!(out, vec!["use crate::nested::Other;".to_string()]);
    }
}
