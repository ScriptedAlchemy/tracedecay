//! Bounded retained Tree-sitter parsing for ephemeral document owners.
//!
//! The retained tree is operational state only. Callers supply an exact
//! session/document generation identity, and neither Tree-sitter nodes nor
//! parser state can become durable code-generation identity.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};

use thiserror::Error;
use tracedecay_domain::{ContentDigest, ManifestDigest};
use tree_sitter::{InputEdit, ParseOptions, Parser, Point, Tree};

use crate::ts_provider;

/// Canonical maximum source retained for one parsed LSP document.
pub const MAX_RETAINED_PARSE_SOURCE_BYTES: usize = 2 * 1024 * 1024;
/// Canonical maximum synchronous Tree-sitter work for one parse attempt.
pub const MAX_RETAINED_PARSE_TIME: Duration = Duration::from_millis(250);
/// Changed ranges are evidence for bounded downstream extraction, so malformed
/// or adversarial syntax cannot make their reporting unbounded.
pub const MAX_RETAINED_PARSE_CHANGED_RANGES: usize = 256;

/// Exact session-local identity for one document incarnation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseDocumentIdentity {
    scope_identity: ManifestDigest,
    document_identity: ManifestDigest,
    document_generation: u64,
    version: i64,
    content_digest: ContentDigest,
    logical_path: String,
}

impl ParseDocumentIdentity {
    #[must_use]
    pub fn new(
        scope_identity: ManifestDigest,
        document_identity: ManifestDigest,
        document_generation: u64,
        version: i64,
        content_digest: ContentDigest,
        logical_path: String,
    ) -> Self {
        Self {
            scope_identity,
            document_identity,
            document_generation,
            version,
            content_digest,
            logical_path,
        }
    }

    #[must_use]
    pub const fn document_generation(&self) -> u64 {
        self.document_generation
    }

    fn identifies_same_document(&self, next: &Self) -> bool {
        self.scope_identity == next.scope_identity
            && self.document_identity == next.document_identity
            && self.logical_path == next.logical_path
    }

    fn identifies_same_generation(&self, next: &Self) -> bool {
        self.identifies_same_document(next) && self.document_generation == next.document_generation
    }
}

/// Tree-sitter row/column point. Columns are bytes, while LSP UTF-16
/// conversion remains owned by the gateway.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParsePoint {
    pub row: usize,
    pub column: usize,
}

impl From<ParsePoint> for Point {
    fn from(point: ParsePoint) -> Self {
        Self::new(point.row, point.column)
    }
}

impl From<Point> for ParsePoint {
    fn from(point: Point) -> Self {
        Self {
            row: point.row,
            column: point.column,
        }
    }
}

/// One edit expressed against the state produced by the preceding edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseInputEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_position: ParsePoint,
    pub old_end_position: ParsePoint,
    pub new_end_position: ParsePoint,
}

impl From<ParseInputEdit> for InputEdit {
    fn from(edit: ParseInputEdit) -> Self {
        Self {
            start_byte: edit.start_byte,
            old_end_byte: edit.old_end_byte,
            new_end_byte: edit.new_end_byte,
            start_position: edit.start_position.into(),
            old_end_position: edit.old_end_position.into(),
            new_end_position: edit.new_end_position.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseChangedRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_position: ParsePoint,
    pub end_position: ParsePoint,
}

/// Resource policy applied independently to every parse attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLimits {
    pub max_source_bytes: usize,
    pub max_changed_ranges: usize,
    pub max_parse_time: Duration,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: MAX_RETAINED_PARSE_SOURCE_BYTES,
            max_changed_ranges: MAX_RETAINED_PARSE_CHANGED_RANGES,
            max_parse_time: MAX_RETAINED_PARSE_TIME,
        }
    }
}

impl ParseLimits {
    /// True at and beyond the exact parse budget boundary.
    #[must_use]
    pub fn parse_deadline_expired(self, elapsed: Duration) -> bool {
        elapsed >= self.max_parse_time
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseReuse {
    Initial,
    Incremental,
    Noop,
    Reset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsePartialReason {
    SyntaxErrors,
    ChangedRangesTruncated { returned: usize, total: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseCompleteness {
    Complete,
    Partial { reasons: Vec<ParsePartialReason> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseMetrics {
    pub source_bytes: usize,
    pub input_edit_count: usize,
    pub changed_bytes: usize,
    pub changed_range_count: usize,
    pub returned_changed_range_count: usize,
    pub parse_elapsed: Duration,
    pub reused_prior_tree: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseReport {
    pub reuse: ParseReuse,
    pub completeness: ParseCompleteness,
    pub changed_ranges: Vec<ParseChangedRange>,
    pub metrics: ParseMetrics,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ParseError {
    #[error("no bundled Tree-sitter grammar is available for language {language_id}")]
    UnsupportedLanguage { language_id: String },
    #[error("source is {size} bytes, exceeding the retained parser limit of {limit}")]
    SourceTooLarge { size: usize, limit: usize },
    #[error("the edit batch does not describe the supplied source: {detail}")]
    InvalidEdit { detail: String },
    #[error("the next parse identity names another document generation")]
    IdentityMismatch,
    #[error(
        "whole-document replacement must advance generation beyond {current}, received {received}"
    )]
    DocumentGenerationNotAdvanced { current: u64, received: u64 },
    #[error("Tree-sitter rejected the bundled grammar for {language_id}: {detail}")]
    GrammarRejected { language_id: String, detail: String },
    #[error("Tree-sitter parsing exceeded {limit:?}")]
    TimedOut { limit: Duration },
    #[error("Tree-sitter did not produce a syntax tree")]
    ParseFailed,
}

/// One owner-local retained parser. Failed updates leave its identity, source,
/// and tree unchanged.
pub struct RetainedParseDocument {
    identity: ParseDocumentIdentity,
    source: String,
    parser: Parser,
    tree: Tree,
}

impl RetainedParseDocument {
    pub fn open(
        identity: ParseDocumentIdentity,
        language_id: impl Into<String>,
        source: impl Into<String>,
        limits: ParseLimits,
    ) -> Result<(Self, ParseReport), ParseError> {
        let language_id = language_id.into();
        let source = source.into();
        ensure_source_bound(&source, limits)?;
        let language = ts_provider::try_language(grammar_key(&language_id)).map_err(|_| {
            ParseError::UnsupportedLanguage {
                language_id: language_id.clone(),
            }
        })?;
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .map_err(|error| ParseError::GrammarRejected {
                language_id: language_id.clone(),
                detail: error.to_string(),
            })?;
        let (tree, elapsed) = parse_with_deadline(&mut parser, &source, None, limits)?;
        let changed_ranges = whole_source_range(&source);
        let report = report_for(
            ParseReuse::Initial,
            &tree,
            changed_ranges,
            source.len(),
            0,
            elapsed,
            false,
            limits.max_changed_ranges,
        );
        Ok((
            Self {
                identity,
                source,
                parser,
                tree,
            },
            report,
        ))
    }

    #[must_use]
    pub fn identity(&self) -> &ParseDocumentIdentity {
        &self.identity
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn apply_edits(
        &mut self,
        next_identity: ParseDocumentIdentity,
        edits: &[ParseInputEdit],
        new_source: impl Into<String>,
        limits: ParseLimits,
    ) -> Result<ParseReport, ParseError> {
        if !self.identity.identifies_same_generation(&next_identity) {
            return Err(ParseError::IdentityMismatch);
        }
        let new_source = new_source.into();
        ensure_source_bound(&new_source, limits)?;
        validate_edits(self.source.len(), new_source.len(), edits)?;
        if edits.is_empty() {
            if self.source != new_source {
                return Err(ParseError::InvalidEdit {
                    detail: "an empty edit batch changed source bytes".to_owned(),
                });
            }
            self.identity = next_identity;
            return Ok(noop_report(&self.tree, self.source.len()));
        }

        let mut edited_tree = self.tree.clone();
        for edit in edits {
            edited_tree.edit(&(*edit).into());
        }
        let (new_tree, elapsed) =
            parse_with_deadline(&mut self.parser, &new_source, Some(&edited_tree), limits)?;
        let changed_ranges = edited_tree
            .changed_ranges(&new_tree)
            .map(|range| ParseChangedRange {
                start_byte: range.start_byte,
                end_byte: range.end_byte,
                start_position: range.start_point.into(),
                end_position: range.end_point.into(),
            })
            .collect::<Vec<_>>();
        let report = report_for(
            ParseReuse::Incremental,
            &new_tree,
            changed_ranges,
            new_source.len(),
            edits.len(),
            elapsed,
            true,
            limits.max_changed_ranges,
        );
        self.identity = next_identity;
        self.source = new_source;
        self.tree = new_tree;
        Ok(report)
    }

    pub fn reparse(
        &mut self,
        next_identity: ParseDocumentIdentity,
        new_source: impl Into<String>,
        limits: ParseLimits,
    ) -> Result<ParseReport, ParseError> {
        let new_source = new_source.into();
        if self.source == new_source {
            return self.apply_edits(next_identity, &[], new_source, limits);
        }
        let edit = minimal_edit(&self.source, &new_source);
        self.apply_edits(next_identity, &[edit], new_source, limits)
    }

    pub fn replace(
        &mut self,
        next_identity: ParseDocumentIdentity,
        new_source: impl Into<String>,
        limits: ParseLimits,
    ) -> Result<ParseReport, ParseError> {
        if !self.identity.identifies_same_document(&next_identity) {
            return Err(ParseError::IdentityMismatch);
        }
        if next_identity.document_generation <= self.identity.document_generation {
            return Err(ParseError::DocumentGenerationNotAdvanced {
                current: self.identity.document_generation,
                received: next_identity.document_generation,
            });
        }
        let new_source = new_source.into();
        ensure_source_bound(&new_source, limits)?;
        let (new_tree, elapsed) = parse_with_deadline(&mut self.parser, &new_source, None, limits)?;
        let report = report_for(
            ParseReuse::Reset,
            &new_tree,
            whole_source_range(&new_source),
            new_source.len(),
            0,
            elapsed,
            false,
            limits.max_changed_ranges,
        );
        self.identity = next_identity;
        self.source = new_source;
        self.tree = new_tree;
        Ok(report)
    }
}

fn ensure_source_bound(source: &str, limits: ParseLimits) -> Result<(), ParseError> {
    if source.len() > limits.max_source_bytes {
        return Err(ParseError::SourceTooLarge {
            size: source.len(),
            limit: limits.max_source_bytes,
        });
    }
    Ok(())
}

fn parse_with_deadline(
    parser: &mut Parser,
    source: &str,
    old_tree: Option<&Tree>,
    limits: ParseLimits,
) -> Result<(Tree, Duration), ParseError> {
    if limits.max_parse_time.is_zero() {
        return Err(ParseError::TimedOut {
            limit: limits.max_parse_time,
        });
    }
    let started = Instant::now();
    let mut interrupted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if limits.parse_deadline_expired(started.elapsed()) {
            interrupted = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let bytes = source.as_bytes();
    let parsed = parser.parse_with_options(
        &mut |offset, _| match bytes.get(offset..) {
            Some(remaining) => remaining,
            None => &[],
        },
        old_tree,
        Some(options),
    );
    let elapsed = started.elapsed();
    if interrupted || limits.parse_deadline_expired(elapsed) {
        parser.reset();
        return Err(ParseError::TimedOut {
            limit: limits.max_parse_time,
        });
    }
    parsed.map(|tree| (tree, elapsed)).ok_or_else(|| {
        parser.reset();
        ParseError::ParseFailed
    })
}

fn validate_edits(
    initial_len: usize,
    expected_len: usize,
    edits: &[ParseInputEdit],
) -> Result<(), ParseError> {
    let mut current_len = initial_len;
    for (index, edit) in edits.iter().enumerate() {
        if edit.start_byte > edit.old_end_byte
            || edit.old_end_byte > current_len
            || edit.new_end_byte < edit.start_byte
        {
            return Err(ParseError::InvalidEdit {
                detail: format!("edit {index} has byte bounds outside the evolving source"),
            });
        }
        let removed = edit.old_end_byte - edit.start_byte;
        let inserted = edit.new_end_byte - edit.start_byte;
        current_len = current_len
            .checked_sub(removed)
            .and_then(|value| value.checked_add(inserted))
            .ok_or_else(|| ParseError::InvalidEdit {
                detail: format!("edit {index} overflows source length"),
            })?;
    }
    if current_len != expected_len {
        return Err(ParseError::InvalidEdit {
            detail: format!(
                "ordered edits produce {current_len} bytes but supplied source has {expected_len}"
            ),
        });
    }
    Ok(())
}

fn minimal_edit(before: &str, after: &str) -> ParseInputEdit {
    let before_bytes = before.as_bytes();
    let after_bytes = after.as_bytes();
    let mut prefix = before_bytes
        .iter()
        .zip(after_bytes)
        .take_while(|(left, right)| left == right)
        .count();
    while !before.is_char_boundary(prefix) || !after.is_char_boundary(prefix) {
        prefix -= 1;
    }

    let mut suffix = before_bytes[prefix..]
        .iter()
        .rev()
        .zip(after_bytes[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    while !before.is_char_boundary(before.len() - suffix)
        || !after.is_char_boundary(after.len() - suffix)
    {
        suffix -= 1;
    }

    let old_end = before.len() - suffix;
    let new_end = after.len() - suffix;
    ParseInputEdit {
        start_byte: prefix,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_at(before, prefix),
        old_end_position: point_at(before, old_end),
        new_end_position: point_at(after, new_end),
    }
}

fn point_at(source: &str, byte: usize) -> ParsePoint {
    let prefix = &source[..byte];
    let row = prefix.bytes().filter(|value| *value == b'\n').count();
    let column = prefix
        .rfind('\n')
        .map_or(prefix.len(), |line_start| prefix.len() - line_start - 1);
    ParsePoint { row, column }
}

fn whole_source_range(source: &str) -> Vec<ParseChangedRange> {
    if source.is_empty() {
        Vec::new()
    } else {
        vec![ParseChangedRange {
            start_byte: 0,
            end_byte: source.len(),
            start_position: ParsePoint { row: 0, column: 0 },
            end_position: point_at(source, source.len()),
        }]
    }
}

fn noop_report(tree: &Tree, source_bytes: usize) -> ParseReport {
    ParseReport {
        reuse: ParseReuse::Noop,
        completeness: completeness_for(tree, None),
        changed_ranges: Vec::new(),
        metrics: ParseMetrics {
            source_bytes,
            input_edit_count: 0,
            changed_bytes: 0,
            changed_range_count: 0,
            returned_changed_range_count: 0,
            parse_elapsed: Duration::ZERO,
            reused_prior_tree: true,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn report_for(
    reuse: ParseReuse,
    tree: &Tree,
    mut ranges: Vec<ParseChangedRange>,
    source_bytes: usize,
    input_edit_count: usize,
    parse_elapsed: Duration,
    reused_prior_tree: bool,
    max_changed_ranges: usize,
) -> ParseReport {
    let total_ranges = ranges.len();
    let truncated = (total_ranges > max_changed_ranges).then_some(total_ranges);
    ranges.truncate(max_changed_ranges);
    let changed_bytes = ranges.iter().fold(0usize, |total, range| {
        total.saturating_add(range.end_byte.saturating_sub(range.start_byte))
    });
    ParseReport {
        reuse,
        completeness: completeness_for(tree, truncated.map(|total| (ranges.len(), total))),
        metrics: ParseMetrics {
            source_bytes,
            input_edit_count,
            changed_bytes,
            changed_range_count: total_ranges,
            returned_changed_range_count: ranges.len(),
            parse_elapsed,
            reused_prior_tree,
        },
        changed_ranges: ranges,
    }
}

fn completeness_for(tree: &Tree, truncated: Option<(usize, usize)>) -> ParseCompleteness {
    let mut reasons = Vec::new();
    if tree.root_node().has_error() {
        reasons.push(ParsePartialReason::SyntaxErrors);
    }
    if let Some((returned, total)) = truncated {
        reasons.push(ParsePartialReason::ChangedRangesTruncated { returned, total });
    }
    if reasons.is_empty() {
        ParseCompleteness::Complete
    } else {
        ParseCompleteness::Partial { reasons }
    }
}

fn grammar_key(language_id: &str) -> &str {
    match language_id {
        "c#" | "csharp" => "c_sharp",
        "c++" => "cpp",
        "f#" => "fsharp",
        "objective-c" => "objc",
        "javascriptreact" | "jsx" => "javascript",
        "typescriptreact" => "tsx",
        _ => language_id,
    }
}
