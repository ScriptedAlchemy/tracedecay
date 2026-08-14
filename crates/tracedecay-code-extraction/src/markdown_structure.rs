//! Load-bearing structure *inside* a markdown section.
//!
//! The symbol graph stays heading-level: sections are the only markdown
//! symbols, and bullets are never exploded into graph nodes. But the content
//! that makes a plan document a *work ledger* — task-list checkboxes and their
//! checked state, nested bullets, ordered steps, tables, block quotes, fenced
//! code and its language tag — has to survive into retrieval as structure, not
//! as one flat text blob. Otherwise "which checklist items under 'Remaining
//! work' are still unchecked" is unanswerable without re-reading the file.
//!
//! This parser is deliberately line-based and grammar-free: it compiles with
//! no tree-sitter feature enabled, so the retrieval layer can depend on it
//! without pulling a grammar bundle. Fenced code is tracked so that a `- [ ]`
//! or `| a | b |` line inside a code sample is never mistaken for real
//! structure.

use serde::{Deserialize, Serialize};

/// Spaces of indentation per nesting level for list items.
const INDENT_PER_LEVEL: u32 = 2;

/// One `- [ ]` / `- [x]` task-list entry.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MarkdownChecklistItem {
    /// Zero-based line in the enclosing file.
    pub line: u32,
    /// Nesting depth, zero for a top-level item.
    pub depth: u32,
    /// `true` for `[x]` / `[X]`, `false` for `[ ]`.
    pub checked: bool,
    /// Item text with the marker and checkbox removed.
    pub text: String,
}

/// One bullet or ordered list entry that is not a task-list entry.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MarkdownListItem {
    pub line: u32,
    pub depth: u32,
    pub text: String,
}

/// A pipe table, kept as a typed block with its shape rather than its cells.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MarkdownTable {
    pub start_line: u32,
    pub end_line: u32,
    /// Columns in the header row.
    pub columns: u32,
    /// Body rows, excluding the header and the delimiter row.
    pub rows: u32,
}

/// A contiguous run of `>` quoted lines.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MarkdownBlockQuote {
    pub start_line: u32,
    pub end_line: u32,
    pub lines: u32,
}

/// A fenced code block and its info-string language, when tagged.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MarkdownCodeBlock {
    pub start_line: u32,
    pub end_line: u32,
    pub language: Option<String>,
}

/// Everything structural found in one section body.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MarkdownSectionStructure {
    pub checklist: Vec<MarkdownChecklistItem>,
    pub bullets: Vec<MarkdownListItem>,
    pub ordered: Vec<MarkdownListItem>,
    pub tables: Vec<MarkdownTable>,
    pub block_quotes: Vec<MarkdownBlockQuote>,
    pub code_blocks: Vec<MarkdownCodeBlock>,
}

impl MarkdownSectionStructure {
    /// `true` when the section carries no structure worth publishing.
    pub fn is_empty(&self) -> bool {
        self.checklist.is_empty()
            && self.bullets.is_empty()
            && self.ordered.is_empty()
            && self.tables.is_empty()
            && self.block_quotes.is_empty()
            && self.code_blocks.is_empty()
    }

    /// Checklist items still unchecked — the question plan documents are
    /// actually asked.
    pub fn unchecked(&self) -> impl Iterator<Item = &MarkdownChecklistItem> {
        self.checklist.iter().filter(|item| !item.checked)
    }
}

/// Fence state for one `` ``` `` / `~~~` block.
struct OpenFence {
    marker: u8,
    width: usize,
    start_line: u32,
    language: Option<String>,
}

/// Parse the structure of `body`, whose first line is `start_line` in the
/// enclosing file (zero-based). Reported line numbers are absolute.
pub fn parse_section_structure(body: &str, start_line: u32) -> MarkdownSectionStructure {
    let mut structure = MarkdownSectionStructure::default();
    let mut fence: Option<OpenFence> = None;
    let mut quote_run: Option<(u32, u32)> = None;
    let mut table_run: Option<MarkdownTable> = None;

    for (offset, raw_line) in body.lines().enumerate() {
        let line_number = start_line.saturating_add(offset as u32);
        let line = raw_line.trim_end_matches('\r');
        let trimmed = line.trim_start();

        if let Some(open) = &fence {
            if closes_fence(trimmed, open) {
                structure.code_blocks.push(MarkdownCodeBlock {
                    start_line: open.start_line,
                    end_line: line_number,
                    language: open.language.clone(),
                });
                fence = None;
            }
            continue;
        }
        if let Some(open) = open_fence(trimmed, line_number) {
            flush_quote(&mut structure, &mut quote_run);
            flush_table(&mut structure, &mut table_run);
            fence = Some(open);
            continue;
        }

        if trimmed.starts_with('>') {
            flush_table(&mut structure, &mut table_run);
            match &mut quote_run {
                Some((_, end)) => *end = line_number,
                None => quote_run = Some((line_number, line_number)),
            }
            continue;
        }
        flush_quote(&mut structure, &mut quote_run);

        if let Some(cells) = table_row_cells(trimmed) {
            match &mut table_run {
                Some(table) => {
                    if is_table_delimiter(trimmed) {
                        // The delimiter row confirms the pending header; it is
                        // not itself a body row.
                        table.columns = table.columns.max(cells);
                    } else {
                        table.rows += 1;
                    }
                    table.end_line = line_number;
                }
                None => {
                    table_run = Some(MarkdownTable {
                        start_line: line_number,
                        end_line: line_number,
                        columns: cells,
                        rows: 0,
                    });
                }
            }
            continue;
        }
        flush_table(&mut structure, &mut table_run);

        let indent = leading_spaces(line);
        let depth = indent / INDENT_PER_LEVEL;
        if let Some(rest) = bullet_body(trimmed) {
            match checkbox(rest) {
                Some((checked, text)) => structure.checklist.push(MarkdownChecklistItem {
                    line: line_number,
                    depth,
                    checked,
                    text: text.trim().to_owned(),
                }),
                None => structure.bullets.push(MarkdownListItem {
                    line: line_number,
                    depth,
                    text: rest.trim().to_owned(),
                }),
            }
            continue;
        }
        if let Some(rest) = ordered_body(trimmed) {
            match checkbox(rest) {
                Some((checked, text)) => structure.checklist.push(MarkdownChecklistItem {
                    line: line_number,
                    depth,
                    checked,
                    text: text.trim().to_owned(),
                }),
                None => structure.ordered.push(MarkdownListItem {
                    line: line_number,
                    depth,
                    text: rest.trim().to_owned(),
                }),
            }
        }
    }

    // An unterminated fence still describes real content: close it at the end
    // of the section rather than dropping the block.
    if let Some(open) = fence {
        let end = start_line.saturating_add(body.lines().count().saturating_sub(1) as u32);
        structure.code_blocks.push(MarkdownCodeBlock {
            start_line: open.start_line,
            end_line: end.max(open.start_line),
            language: open.language,
        });
    }
    flush_quote(&mut structure, &mut quote_run);
    flush_table(&mut structure, &mut table_run);
    structure
}

fn flush_quote(structure: &mut MarkdownSectionStructure, run: &mut Option<(u32, u32)>) {
    if let Some((start, end)) = run.take() {
        structure.block_quotes.push(MarkdownBlockQuote {
            start_line: start,
            end_line: end,
            lines: end - start + 1,
        });
    }
}

fn flush_table(structure: &mut MarkdownSectionStructure, run: &mut Option<MarkdownTable>) {
    if let Some(table) = run.take() {
        // A single pipe-bearing line is prose, not a table.
        if table.end_line > table.start_line {
            structure.tables.push(table);
        }
    }
}

fn leading_spaces(line: &str) -> u32 {
    let mut spaces = 0u32;
    for ch in line.chars() {
        match ch {
            ' ' => spaces += 1,
            '\t' => spaces += INDENT_PER_LEVEL,
            _ => break,
        }
    }
    spaces
}

fn open_fence(trimmed: &str, line_number: u32) -> Option<OpenFence> {
    let marker = trimmed.as_bytes().first().copied()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let width = trimmed.bytes().take_while(|byte| *byte == marker).count();
    if width < 3 {
        return None;
    }
    let info = trimmed[width..].trim();
    let language = info
        .split_whitespace()
        .next()
        .filter(|language| !language.is_empty())
        .map(str::to_owned);
    Some(OpenFence {
        marker,
        width,
        start_line: line_number,
        language,
    })
}

fn closes_fence(trimmed: &str, open: &OpenFence) -> bool {
    let width = trimmed
        .bytes()
        .take_while(|byte| *byte == open.marker)
        .count();
    width >= open.width && trimmed[width..].trim().is_empty()
}

/// The text after a `-`/`*`/`+` bullet marker, when the line is a bullet.
fn bullet_body(trimmed: &str) -> Option<&str> {
    let mut chars = trimmed.chars();
    let marker = chars.next()?;
    if !matches!(marker, '-' | '*' | '+') {
        return None;
    }
    let rest = chars.as_str();
    // `---` is a setext underline or thematic break, not a list.
    if rest.starts_with(marker) {
        return None;
    }
    match rest.strip_prefix([' ', '\t']) {
        Some(body) => Some(body),
        None => None,
    }
}

/// The text after an `1.` / `1)` marker, when the line is an ordered item.
fn ordered_body(trimmed: &str) -> Option<&str> {
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &trimmed[digits..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    rest.strip_prefix([' ', '\t'])
}

/// `(checked, remaining text)` when a list body opens with a task checkbox.
fn checkbox(body: &str) -> Option<(bool, &str)> {
    let rest = body.strip_prefix('[')?;
    let mut chars = rest.chars();
    let state = chars.next()?;
    let rest = chars.as_str().strip_prefix(']')?;
    match state {
        ' ' => Some((false, rest)),
        'x' | 'X' => Some((true, rest)),
        _ => None,
    }
}

/// Cell count when the line looks like a pipe-table row.
fn table_row_cells(trimmed: &str) -> Option<u32> {
    if !trimmed.contains('|') {
        return None;
    }
    let body = trimmed
        .trim_start_matches('|')
        .trim_end_matches('|')
        .trim_end();
    if body.is_empty() {
        return None;
    }
    Some(body.split('|').count() as u32)
}

fn is_table_delimiter(trimmed: &str) -> bool {
    let body = trimmed.trim_start_matches('|').trim_end_matches('|');
    body.contains('-')
        && body
            .chars()
            .all(|ch| matches!(ch, '-' | ':' | '|' | ' ' | '\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checklists_lists_tables_quotes_and_fences() {
        let body = "\
- [x] done
- [ ] open
  - nested bullet
1. first step

| a | b |
| - | - |
| 1 | 2 |

> quoted

```rust
fn x() {}
```
";
        let structure = parse_section_structure(body, 10);
        assert_eq!(structure.checklist.len(), 2);
        assert!(structure.checklist[0].checked);
        assert!(!structure.checklist[1].checked);
        assert_eq!(structure.checklist[0].line, 10);
        assert_eq!(structure.bullets.len(), 1);
        assert_eq!(structure.bullets[0].depth, 1);
        assert_eq!(structure.ordered.len(), 1);
        assert_eq!(structure.tables.len(), 1);
        assert_eq!(structure.tables[0].columns, 2);
        assert_eq!(structure.tables[0].rows, 1);
        assert_eq!(structure.block_quotes.len(), 1);
        assert_eq!(structure.code_blocks.len(), 1);
        assert_eq!(structure.code_blocks[0].language.as_deref(), Some("rust"));
        assert_eq!(structure.unchecked().count(), 1);
    }

    #[test]
    fn fenced_code_does_not_mint_fake_structure() {
        let body = "\
```
- [ ] not a task
| a | b |
```
";
        let structure = parse_section_structure(body, 0);
        assert!(structure.checklist.is_empty());
        assert!(structure.tables.is_empty());
        assert_eq!(structure.code_blocks.len(), 1);
    }

    #[test]
    fn unterminated_fence_closes_at_section_end() {
        let body = "```python\nprint(1)\nprint(2)\n";
        let structure = parse_section_structure(body, 3);
        assert_eq!(structure.code_blocks.len(), 1);
        assert_eq!(structure.code_blocks[0].start_line, 3);
        assert_eq!(structure.code_blocks[0].end_line, 5);
        assert_eq!(structure.code_blocks[0].language.as_deref(), Some("python"));
    }
}
