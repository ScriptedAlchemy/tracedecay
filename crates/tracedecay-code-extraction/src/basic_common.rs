//! Helpers shared by the line-number BASIC extractors (GW-BASIC and
//! MS BASIC 2.0), which synthesize Function nodes from REM ... RETURN blocks.
//!
//! Bodies are moved here unchanged from the per-language copies so extraction
//! output stays byte-identical.

use tree_sitter::Node as TsNode;

use crate::common::ExtractionState;
use crate::traversal::find_direct_child_by_kind;

/// Represents a collected line from the BASIC program for subroutine synthesis.
pub(crate) struct BasicLine<'a> {
    /// The `line` AST node.
    pub(crate) node: TsNode<'a>,
    /// The line number (e.g. 10, 20, 100).
    pub(crate) line_number: u32,
    /// The kind of the first statement on this line.
    pub(crate) statement_kind: String,
    /// The text of the REM comment, if this line is a REM.
    pub(crate) comment_text: Option<String>,
}

/// Parse a single `line` node into a `BasicLine` struct.
pub(crate) fn parse_line<'a>(state: &ExtractionState, node: TsNode<'a>) -> Option<BasicLine<'a>> {
    let line_number_node = find_direct_child_by_kind(node, "line_number")?;
    let line_number_text = state.node_text(line_number_node);
    let line_number: u32 = line_number_text.trim().parse().unwrap_or(0);

    // Navigate: line -> statement_list -> statement -> specific_kind
    let statement_list = find_direct_child_by_kind(node, "statement_list")?;
    let statement = find_direct_child_by_kind(statement_list, "statement")?;

    let mut stmt_cursor = statement.walk();
    let mut statement_kind = String::new();
    let mut comment_text = None;
    if stmt_cursor.goto_first_child() {
        let child = stmt_cursor.node();
        statement_kind = child.kind().to_string();
        if child.kind() == "comment" {
            let text = state.node_text(child);
            // Strip a leading "REM" keyword (case-insensitive) when present.
            // Content-checked so non-ASCII text never lands the byte cut
            // inside a multi-byte character.
            let stripped = text
                .get(..3)
                .filter(|p| p.eq_ignore_ascii_case("REM"))
                .map_or(text, |_| &text[3..])
                .trim()
                .to_string();
            comment_text = Some(stripped);
        }
    }

    Some(BasicLine {
        node,
        line_number,
        statement_kind,
        comment_text,
    })
}

/// Find ranges of lines that belong to subroutines (REM ... RETURN blocks).
pub(crate) fn find_subroutine_ranges(lines: &[BasicLine<'_>]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].statement_kind == "comment" {
            let rem_start = i;
            while i < lines.len() && lines[i].statement_kind == "comment" {
                i += 1;
            }
            let mut body_end = i;
            let mut has_return = false;
            while body_end < lines.len() {
                if lines[body_end].statement_kind == "return_statement" {
                    has_return = true;
                    body_end += 1;
                    break;
                }
                if lines[body_end].statement_kind == "comment" {
                    break;
                }
                body_end += 1;
            }
            if has_return {
                ranges.push((rem_start, body_end));
            }
            i = body_end;
        } else {
            i += 1;
        }
    }
    ranges
}

/// Visit each line that is not part of a subroutine (REM ... RETURN block).
pub(crate) fn for_each_top_level_line<'l, 'tree>(
    lines: &'l [BasicLine<'tree>],
    mut visit: impl FnMut(&'l BasicLine<'tree>),
) {
    let subroutine_ranges = find_subroutine_ranges(lines);
    for (idx, line) in lines.iter().enumerate() {
        // Skip lines that are inside subroutines.
        if subroutine_ranges
            .iter()
            .any(|(start, end)| idx >= *start && idx < *end)
        {
            continue;
        }
        visit(line);
    }
}

/// Derive a function name from REM comment text.
///
/// Takes the first REM line text and converts it into a snake_case-like
/// identifier. For example, "VALIDATE CONFIGURATION" becomes "`VALIDATE_CONFIGURATION`".
pub(crate) fn derive_function_name(rem_comments: &[String]) -> String {
    if rem_comments.is_empty() {
        return "UNNAMED_SUB".to_string();
    }
    let first = &rem_comments[0];
    // Replace spaces with underscores and keep alphanumeric + underscore.
    let name: String = first
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Collapse multiple underscores and trim.
    let mut collapsed = String::new();
    let mut prev_underscore = false;
    for c in name.chars() {
        if c == '_' {
            if !prev_underscore && !collapsed.is_empty() {
                collapsed.push('_');
            }
            prev_underscore = true;
        } else {
            collapsed.push(c);
            prev_underscore = false;
        }
    }
    let trimmed = collapsed.trim_end_matches('_').to_string();
    if trimmed.is_empty() {
        "UNNAMED_SUB".to_string()
    } else {
        trimmed
    }
}
