//! Format-preserving edits of JSON and JSONC host configs.
//!
//! Installers compute the config value they want; [`edit_json_text`] moves
//! the operator's text to that value by splicing only the members that
//! differ. Comments, key order, indentation, trailing commas and line endings
//! outside those members stay byte-for-byte. Every splice has an exact
//! inverse: removing a member deletes precisely the text appending it added,
//! so install followed by uninstall restores the original bytes without any
//! copy of them being kept.

use jsonc_parser::ast::{Object, Value as Node};
use jsonc_parser::common::{Range, Ranged};
use jsonc_parser::tokens::{Token, TokenAndRange};
use jsonc_parser::{CollectOptions, CommentCollectionStrategy, ParseOptions, parse_to_ast};
use serde_json::Value;

/// Parse options that accept exactly the host config dialect: standard JSON,
/// optionally with comments and trailing commas.
pub(super) fn parse_options(comments_and_trailing_commas: bool) -> ParseOptions {
    ParseOptions {
        allow_comments: comments_and_trailing_commas,
        allow_trailing_commas: comments_and_trailing_commas,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    }
}

pub(super) fn parse_json_text(text: &str, options: &ParseOptions) -> Result<Value, String> {
    parse_to_ast(text, &CollectOptions::default(), options)
        .map_err(|error| error.to_string())?
        .value
        .map(Value::from)
        .ok_or_else(|| "no JSON value".to_string())
}

/// Rewrite `text` so it parses to `target`, touching only differing members.
pub(super) fn edit_json_text(
    text: &str,
    target: &Value,
    options: &ParseOptions,
) -> Result<String, String> {
    let mut text = text.to_string();
    let mut remaining_edits = node_count(&parse_json_text(&text, options)?) + node_count(target);
    loop {
        let splices = {
            let parsed = parse_to_ast(
                &text,
                &CollectOptions {
                    comments: CommentCollectionStrategy::AsTokens,
                    tokens: true,
                },
                options,
            )
            .map_err(|error| error.to_string())?;
            let root = parsed.value.ok_or_else(|| "no JSON value".to_string())?;
            let tokens = parsed
                .tokens
                .ok_or_else(|| "parser returned no tokens".to_string())?;
            let doc = Doc::new(&text, &tokens);
            match doc.diff(&root, target) {
                Some(splices) => splices,
                None if Value::from(root) == *target => return Ok(text),
                None => return Err("edited config does not match the intended value".into()),
            }
        };
        if remaining_edits == 0 {
            return Err("config edit did not converge".into());
        }
        remaining_edits -= 1;
        text = apply(text, splices);
    }
}

fn node_count(value: &Value) -> usize {
    1 + match value {
        Value::Array(items) => items.iter().map(node_count).sum(),
        Value::Object(members) => members.values().map(node_count).sum(),
        _ => 0,
    }
}

/// Replace `text[start..end]` with the string; non-overlapping by construction.
type Splice = (usize, usize, String);

fn apply(mut text: String, mut splices: Vec<Splice>) -> String {
    splices.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, replacement) in splices {
        text.replace_range(start..end, &replacement);
    }
    text
}

/// One comma-separated container: an object's properties or an array's
/// elements, by source range.
struct Container {
    open: usize,
    close: usize,
    items: Vec<Range>,
}

struct Doc<'a> {
    text: &'a str,
    tokens: &'a [TokenAndRange<'a>],
    newline: &'static str,
    unit: String,
}

impl<'a> Doc<'a> {
    fn new(text: &'a str, tokens: &'a [TokenAndRange<'a>]) -> Self {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let unit = text
            .lines()
            .map(|line| &line[..line.len() - line.trim_start().len()])
            .find(|indent| !indent.is_empty())
            .unwrap_or("  ")
            .to_string();
        Self {
            text,
            tokens,
            newline,
            unit,
        }
    }

    /// The first splice set that moves `node` toward `target`, or `None`
    /// when they already agree.
    fn diff(&self, node: &Node, target: &Value) -> Option<Vec<Splice>> {
        if Value::from(node.clone()) == *target {
            return None;
        }
        match (node, target) {
            (Node::Object(object), Value::Object(members)) => self.diff_object(object, members),
            (Node::Array(array), Value::Array(items)) => {
                let container = Container {
                    open: array.range.start,
                    close: array.range.end - 1,
                    items: array.elements.iter().map(Ranged::range).collect(),
                };
                let current: Vec<Value> = array.elements.iter().cloned().map(Value::from).collect();
                match first_array_change(&current, items)? {
                    ArrayChange::Edit(old, new) => self.diff(&array.elements[old], &items[new]),
                    ArrayChange::Remove(old) => Some(self.remove_item(&container, old)),
                    ArrayChange::Insert(at, new) => {
                        Some(self.insert_item(&container, at, None, &items[new]))
                    }
                }
            }
            _ => {
                let range = node.range();
                let indent = self.line_indent(range.start);
                let multiline = self.text.contains('\n');
                Some(vec![(
                    range.start,
                    range.end,
                    self.render(target, &indent, multiline),
                )])
            }
        }
    }

    fn diff_object(
        &self,
        object: &Object,
        members: &serde_json::Map<String, Value>,
    ) -> Option<Vec<Splice>> {
        let container = Container {
            open: object.range.start,
            close: object.range.end - 1,
            items: object.properties.iter().map(|prop| prop.range).collect(),
        };
        if let Some(index) = object
            .properties
            .iter()
            .position(|prop| !members.contains_key(prop.name.as_str()))
        {
            return Some(self.remove_item(&container, index));
        }
        if let Some(splices) = object
            .properties
            .iter()
            .find_map(|prop| self.diff(&prop.value, &members[prop.name.as_str()]))
        {
            return Some(splices);
        }
        let (key, value) = members.iter().find(|(key, _)| {
            !object
                .properties
                .iter()
                .any(|prop| prop.name.as_str() == key.as_str())
        })?;
        let colon = object
            .properties
            .first()
            .map(|prop| &self.text[prop.name.end()..prop.value.start()])
            .filter(|colon| colon.trim() == ":")
            .unwrap_or(": ");
        Some(self.insert_item(&container, container.items.len(), Some((key, colon)), value))
    }

    /// End of the last token (comments included) that ends at or before `pos`.
    fn prev_token_end(&self, pos: usize) -> usize {
        self.tokens
            .iter()
            .rev()
            .find(|token| token.range.end <= pos)
            .map_or(0, |token| token.range.end)
    }

    /// Position of the comma that directly follows the item ending at `end`.
    fn comma_after(&self, end: usize) -> Option<usize> {
        self.tokens
            .iter()
            .filter(|token| token.range.start >= end)
            .find(|token| !matches!(token.token, Token::CommentLine(_) | Token::CommentBlock(_)))
            .filter(|token| token.token == Token::Comma)
            .map(|token| token.range.start)
    }

    /// Leading whitespace of the line containing `pos`.
    fn line_indent(&self, pos: usize) -> String {
        let line_start = self.text[..pos].rfind('\n').map_or(0, |index| index + 1);
        let line = &self.text[line_start..];
        line[..line.len() - line.trim_start_matches([' ', '\t']).len()].to_string()
    }

    /// Indentation for items of `container`: the existing items' own line
    /// indentation when they start their lines, else one unit deeper than the
    /// line that opens the container.
    fn item_indent(&self, container: &Container, reference: Option<Range>) -> String {
        reference
            .map(|item| (self.line_indent(item.start), item.start))
            .filter(|(indent, start)| {
                let line_start = self.text[..*start].rfind('\n').map_or(0, |index| index + 1);
                line_start + indent.len() == *start
            })
            .map_or_else(
                || format!("{}{}", self.line_indent(container.open), self.unit),
                |(indent, _)| indent,
            )
    }

    fn remove_item(&self, container: &Container, index: usize) -> Vec<Splice> {
        let item = container.items[index];
        let start = self.prev_token_end(item.start);
        if container.items.len() == 1 {
            let end = self
                .comma_after(item.end)
                .map_or(item.end, |comma| comma + 1);
            let untouched_is_blank = self.text[container.open + 1..start].trim().is_empty()
                && self.text[end..container.close].trim().is_empty();
            return if untouched_is_blank {
                vec![(container.open + 1, container.close, String::new())]
            } else {
                vec![(start, end, String::new())]
            };
        }
        match self.comma_after(item.end) {
            Some(comma) => vec![(start, comma + 1, String::new())],
            None => {
                let previous = container.items[index - 1];
                let mut splices = vec![(start, item.end, String::new())];
                if let Some(comma) = self.comma_after(previous.end) {
                    splices.push((comma, comma + 1, String::new()));
                }
                splices
            }
        }
    }

    fn insert_item(
        &self,
        container: &Container,
        at: usize,
        key: Option<(&str, &str)>,
        value: &Value,
    ) -> Vec<Splice> {
        let multiline = self.text[container.open..container.close].contains('\n');
        let render = |indent: &str, multiline: bool| {
            let value = self.render(value, indent, multiline);
            match key {
                Some((key, colon)) => format!("{}{colon}{value}", Value::from(key)),
                None => value,
            }
        };
        let items = &container.items;
        if items.is_empty() {
            if key.is_none() && !value.is_object() && !value.is_array() {
                return vec![(container.open + 1, container.open + 1, render("", false))];
            }
            let outer = self.line_indent(container.open);
            let indent = format!("{outer}{}", self.unit);
            let inner = &self.text[container.open + 1..container.close];
            let closing = if inner.contains('\n') {
                String::new()
            } else {
                format!("{}{outer}", self.newline)
            };
            let body = format!("{}{indent}{}{closing}", self.newline, render(&indent, true));
            return vec![(container.open + 1, container.open + 1, body)];
        }
        let reference = items[at.min(items.len() - 1)];
        let indent = self.item_indent(container, Some(reference));
        let separator = if multiline {
            format!("{}{indent}", self.newline)
        } else {
            let gap = match items.get(1) {
                Some(second) => self
                    .comma_after(items[0].end)
                    .map_or("", |comma| &self.text[comma + 1..second.start]),
                None => &self.text[container.open + 1..items[0].start],
            };
            if gap.trim().is_empty() { gap } else { " " }.to_string()
        };
        let body = render(&indent, multiline);
        if at < items.len() {
            let start = self.prev_token_end(items[at].start);
            return vec![(start, start, format!("{separator}{body},"))];
        }
        let last = items[items.len() - 1];
        let end = self.prev_token_end(container.close);
        match self.comma_after(last.end) {
            Some(_) => vec![(end, end, format!("{separator}{body},"))],
            None if end == last.end => vec![(end, end, format!(",{separator}{body}"))],
            None => vec![
                (last.end, last.end, ",".to_string()),
                (end, end, format!("{separator}{body}")),
            ],
        }
    }

    /// Serialize `value` for insertion at a line indented by `indent`.
    fn render(&self, value: &Value, indent: &str, multiline: bool) -> String {
        let nested = value.as_object().is_some_and(|members| !members.is_empty())
            || value.as_array().is_some_and(|items| !items.is_empty());
        if !multiline || !nested {
            return value.to_string();
        }
        // `{:#}` indents by two spaces per level and escapes every newline
        // inside strings, so each line's leading spaces are pure nesting.
        format!("{value:#}")
            .lines()
            .map(|line| {
                let depth = (line.len() - line.trim_start_matches(' ').len()) / 2;
                format!(
                    "{}{}",
                    self.unit.repeat(depth),
                    line.trim_start_matches(' ')
                )
            })
            .collect::<Vec<_>>()
            .join(&format!("{}{indent}", self.newline))
    }
}

enum ArrayChange {
    Edit(usize, usize),
    Remove(usize),
    Insert(usize, usize),
}

/// First change of a longest-common-subsequence alignment of `old` to `new`:
/// unmatched elements are removed or inserted, and an unmatched pair in the
/// same gap is edited in place so its formatting survives.
fn first_array_change(old: &[Value], new: &[Value]) -> Option<ArrayChange> {
    let mut lcs = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() && old[i] == new[j] && lcs[i][j] == lcs[i + 1][j + 1] + 1 {
        i += 1;
        j += 1;
    }
    let old_unmatched = i < old.len() && lcs[i][j] == lcs[i + 1][j];
    let new_unmatched = j < new.len() && lcs[i][j] == lcs[i][j + 1];
    match (old_unmatched, new_unmatched) {
        (true, true) => Some(ArrayChange::Edit(i, j)),
        (true, false) => Some(ArrayChange::Remove(i)),
        (false, true) => Some(ArrayChange::Insert(i, j)),
        (false, false) => None,
    }
}
