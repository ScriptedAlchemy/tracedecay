//! Bounded retained Tree-sitter parsing shared by saved indexing and LSP overlays.
//!
//! The retained tree is an operational optimization only. Callers bind every
//! transition to either exact repository evidence or an opaque, exact LSP
//! session scope. Tree-sitter node identity never leaves this module and never
//! becomes code lineage or persisted generation identity.

use std::{
    ops::ControlFlow,
    time::{Duration, Instant},
};

use thiserror::Error;
use tracedecay_domain::{
    CommitId, ContentDigest, ManifestDigest, ProjectId, RefId, RepositoryDirtyStateV1,
    RepositoryId, TreeId, WorktreeId,
};
use tree_sitter::{InputEdit, ParseOptions, Parser, Point, Tree};

use crate::ts_provider;

/// Default per-document source bound. It matches the LSP overlay hard limit.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
/// A malformed edit stream cannot make range reporting itself unbounded.
pub const DEFAULT_MAX_CHANGED_RANGES: usize = 256;
/// Parsing is synchronous, but every invocation has a cooperative deadline.
pub const DEFAULT_MAX_PARSE_TIME: Duration = Duration::from_millis(250);

/// Exact authority for source retained by one parser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseDocumentIdentity {
    /// Saved or captured repository content with its complete observable Git
    /// identity. `None` commit/tree values truthfully represent an unborn or
    /// otherwise unavailable Git object; they are never fabricated.
    Repository {
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: Option<WorktreeId>,
        reference: Option<RefId>,
        commit: Option<CommitId>,
        tree: Option<TreeId>,
        dirty: RepositoryDirtyStateV1,
        logical_path: String,
    },
    /// An unsaved LSP document. The LSP gateway owns the opaque scope and
    /// document digests plus version/content identity; this leaf neither
    /// widens them into repository identity nor persists them.
    SessionOverlay {
        scope_identity: ManifestDigest,
        document_identity: ManifestDigest,
        version: i64,
        content_digest: ContentDigest,
        logical_path: String,
    },
}

impl ParseDocumentIdentity {
    /// Whether two identities name successive states of the same authorized
    /// document. Revision, dirty state, version, and content may move; project,
    /// checkout/session scope, document, and logical path may not.
    pub fn identifies_same_document(&self, next: &Self) -> bool {
        match (self, next) {
            (
                Self::Repository {
                    project_id,
                    repository_id,
                    worktree_id,
                    logical_path,
                    ..
                },
                Self::Repository {
                    project_id: next_project,
                    repository_id: next_repository,
                    worktree_id: next_worktree,
                    logical_path: next_path,
                    ..
                },
            ) => {
                project_id == next_project
                    && repository_id == next_repository
                    && worktree_id == next_worktree
                    && logical_path == next_path
            }
            (
                Self::SessionOverlay {
                    scope_identity,
                    document_identity,
                    logical_path,
                    ..
                },
                Self::SessionOverlay {
                    scope_identity: next_scope,
                    document_identity: next_document,
                    logical_path: next_path,
                    ..
                },
            ) => {
                scope_identity == next_scope
                    && document_identity == next_document
                    && logical_path == next_path
            }
            _ => false,
        }
    }

    pub fn logical_path(&self) -> &str {
        match self {
            Self::Repository { logical_path, .. } | Self::SessionOverlay { logical_path, .. } => {
                logical_path
            }
        }
    }
}

/// Leaf-owned byte row/column point. Columns are bytes, matching Tree-sitter;
/// LSP UTF-16 conversion remains in the gateway.
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

/// One ordered edit expressed against the document state produced by the
/// preceding edit in the same batch.
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

/// One bounded Tree-sitter changed range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseChangedRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_position: ParsePoint,
    pub end_position: ParsePoint,
}

/// Resource bounds carried by a parser instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLimits {
    pub max_source_bytes: usize,
    pub max_changed_ranges: usize,
    pub max_parse_time: Duration,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_changed_ranges: DEFAULT_MAX_CHANGED_RANGES,
            max_parse_time: DEFAULT_MAX_PARSE_TIME,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseResetReason {
    FullReplacement,
    LanguageChanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseReuse {
    Initial,
    Incremental,
    Noop,
    Reset { reason: ParseResetReason },
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

/// Direct operational instrumentation. Values describe work performed, not
/// product identity; no Tree-sitter node or allocation address is exposed.
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
    #[error("the next parse identity names another document")]
    IdentityMismatch,
    #[error("Tree-sitter rejected the bundled grammar for {language_id}: {detail}")]
    GrammarRejected { language_id: String, detail: String },
    #[error("Tree-sitter parsing exceeded {limit:?}")]
    TimedOut { limit: Duration },
    #[error("Tree-sitter did not produce a syntax tree")]
    ParseFailed,
}

/// One session-local or indexing-owner-local retained parser and tree.
///
/// Updates are atomic: validation and parsing happen against cloned tree
/// state, and a failure leaves the prior source, identity, and tree intact.
pub struct RetainedParseDocument {
    identity: ParseDocumentIdentity,
    language_id: String,
    source: String,
    parser: Parser,
    tree: Tree,
    limits: ParseLimits,
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
        let grammar_key = grammar_key(&language_id);
        let language = ts_provider::try_language(grammar_key).map_err(|_| {
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
        let changed_ranges = if source.is_empty() {
            Vec::new()
        } else {
            vec![ParseChangedRange {
                start_byte: 0,
                end_byte: source.len(),
                start_position: ParsePoint { row: 0, column: 0 },
                end_position: point_at(&source, source.len()),
            }]
        };
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
                language_id,
                source,
                parser,
                tree,
                limits,
            },
            report,
        ))
    }

    pub fn identity(&self) -> &ParseDocumentIdentity {
        &self.identity
    }

    pub fn language_id(&self) -> &str {
        &self.language_id
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn retained_source_bytes(&self) -> usize {
        self.source.len()
    }

    pub fn apply_edits(
        &mut self,
        next_identity: ParseDocumentIdentity,
        edits: &[ParseInputEdit],
        new_source: impl Into<String>,
    ) -> Result<ParseReport, ParseError> {
        if !self.identity.identifies_same_document(&next_identity) {
            return Err(ParseError::IdentityMismatch);
        }
        let new_source = new_source.into();
        ensure_source_bound(&new_source, self.limits)?;
        validate_edits(self.source.len(), new_source.len(), edits)?;
        if edits.is_empty() {
            if self.source != new_source {
                return Err(ParseError::InvalidEdit {
                    detail: "an empty edit batch changed source bytes".to_owned(),
                });
            }
            self.identity = next_identity;
            return Ok(ParseReport {
                reuse: ParseReuse::Noop,
                completeness: completeness_for(&self.tree, None),
                changed_ranges: Vec::new(),
                metrics: ParseMetrics {
                    source_bytes: self.source.len(),
                    input_edit_count: 0,
                    changed_bytes: 0,
                    changed_range_count: 0,
                    returned_changed_range_count: 0,
                    parse_elapsed: Duration::ZERO,
                    reused_prior_tree: true,
                },
            });
        }

        let mut edited_tree = self.tree.clone();
        for edit in edits {
            edited_tree.edit(&(*edit).into());
        }
        let (new_tree, elapsed) = parse_with_deadline(
            &mut self.parser,
            &new_source,
            Some(&edited_tree),
            self.limits,
        )?;
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
            self.limits.max_changed_ranges,
        );
        self.identity = next_identity;
        self.source = new_source;
        self.tree = new_tree;
        Ok(report)
    }

    /// Compute one byte-exact minimal edit and incrementally parse saved source.
    pub fn reparse(
        &mut self,
        next_identity: ParseDocumentIdentity,
        new_source: impl Into<String>,
    ) -> Result<ParseReport, ParseError> {
        let new_source = new_source.into();
        if self.source == new_source {
            return self.apply_edits(next_identity, &[], new_source);
        }
        let edit = minimal_edit(&self.source, &new_source);
        self.apply_edits(next_identity, &[edit], new_source)
    }

    /// Parse a whole-document replacement without consulting the prior tree.
    pub fn replace(
        &mut self,
        next_identity: ParseDocumentIdentity,
        new_source: impl Into<String>,
    ) -> Result<ParseReport, ParseError> {
        if !self.identity.identifies_same_document(&next_identity) {
            return Err(ParseError::IdentityMismatch);
        }
        let new_source = new_source.into();
        ensure_source_bound(&new_source, self.limits)?;
        let (new_tree, elapsed) =
            parse_with_deadline(&mut self.parser, &new_source, None, self.limits)?;
        let ranges = if new_source.is_empty() {
            Vec::new()
        } else {
            vec![ParseChangedRange {
                start_byte: 0,
                end_byte: new_source.len(),
                start_position: ParsePoint { row: 0, column: 0 },
                end_position: point_at(&new_source, new_source.len()),
            }]
        };
        let report = report_for(
            ParseReuse::Reset {
                reason: ParseResetReason::FullReplacement,
            },
            &new_tree,
            ranges,
            new_source.len(),
            0,
            elapsed,
            false,
            self.limits.max_changed_ranges,
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
    let mut timed_out = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if started.elapsed() >= limits.max_parse_time {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let options = ParseOptions::new().progress_callback(&mut progress);
    let bytes = source.as_bytes();
    let tree = parser.parse_with_options(
        &mut |offset, _| match bytes.get(offset..) {
            Some(remaining) => remaining,
            None => &[],
        },
        old_tree,
        Some(options),
    );
    let elapsed = started.elapsed();
    match tree {
        Some(tree) => Ok((tree, elapsed)),
        None if timed_out || elapsed >= limits.max_parse_time => {
            parser.reset();
            Err(ParseError::TimedOut {
                limit: limits.max_parse_time,
            })
        }
        None => {
            parser.reset();
            Err(ParseError::ParseFailed)
        }
    }
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

    let available_before = before.len() - prefix;
    let available_after = after.len() - prefix;
    let mut suffix = before_bytes[prefix..]
        .iter()
        .rev()
        .zip(after_bytes[prefix..].iter().rev())
        .take(available_before.min(available_after))
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
