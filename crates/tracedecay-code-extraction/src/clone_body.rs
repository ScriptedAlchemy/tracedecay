use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{NodeKind, SourceSpan};
use tree_sitter::{Node as TreeSitterNode, Point, Tree, TreeCursor};

use crate::ExtractionArtifactV1;

mod rename;

pub const CONSERVATIVE_CLONE_NORMALIZATION_REVISION_V1: u16 = 1;
pub const RENAME_CLONE_NORMALIZATION_REVISION_V1: u16 = 1;
pub const MIN_AUTOMATIC_CLONE_BODY_TOKENS_V1: u32 = 30;
/// Source-byte guard checked before tokenization. Token count cannot bound one
/// giant literal token, while the text artifact still serializes its bytes.
pub const MAX_AUTOMATIC_CLONE_BODY_BYTES_V1: u64 = 64 * 1024;
/// Bodies with more non-trivia tokens than this are not clone candidates and
/// keep no token stream. A clone body is persisted as one serialized record
/// inside a 4 MiB text-artifact page; a 14k-token function (a generated
/// argument extractor, a fixture-heavy test) serializes its conservative and
/// rename streams to ~4.8 MB together, and a single such record parked a whole
/// project's text projection on a deterministic contract violation with no
/// way to converge. 4096 tokens is roughly a thousand lines, keeps both
/// streams under 1.5 MB, and is far past anything clone detection can act on.
pub const MAX_AUTOMATIC_CLONE_BODY_TOKENS_V1: u32 = 4096;

/// A clone-token syntax kind.
///
/// Every kind an extractor emits is a grammar-owned `&'static str`, drawn from
/// a vocabulary of a few hundred names, while one repository pass emits tens of
/// millions of tokens. Owning the name per token made the grammar table's
/// static strings the largest single allocation source of the pre-progress
/// extraction window; borrowing it keeps the wire shape and pays nothing.
/// Decoded pages resolve names back to the grammar through
/// [`crate::ts_provider::grammar_str`].
pub type CloneSyntaxKindV1 = Cow<'static, str>;

/// A clone-token text. Keywords and punctuation, whose text is their kind,
/// borrow the grammar's name the same way; identifiers and literals own theirs.
pub type CloneTokenTextV1 = Cow<'static, str>;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConservativeCloneTokenV1 {
    StructureStart {
        syntax_kind: CloneSyntaxKindV1,
    },
    Syntax {
        syntax_kind: CloneSyntaxKindV1,
        text: CloneTokenTextV1,
    },
    StructureEnd {
        syntax_kind: CloneSyntaxKindV1,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloneBodyEligibilityV1 {
    Eligible,
    ExcludedIncompleteTokenization,
    ExcludedTooSmall {
        minimum_tokens: u32,
    },
    ExcludedTooLarge {
        maximum_tokens: u32,
        maximum_bytes: u64,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CloneBodyTokenizationStatusV1 {
    Complete,
    Partial,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CloneBodyTokenizationIssueV1 {
    BodyBoundaryUnavailable,
    BodyExceedsSizeBound,
    InvalidSourceRange,
    ParseError,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CloneBodyRenameStatusV1 {
    Complete,
    Partial,
    UnsupportedLanguage,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CloneBodyRenameIssueV1 {
    DynamicBinding,
    UnsupportedBindingSyntax,
}

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExtractedCloneBodyV1 {
    pub logical_path: String,
    pub language: String,
    pub symbol_kind: NodeKind,
    pub symbol_occurrence_id: String,
    pub body_span: SourceSpan,
    pub normalization_revision: u16,
    pub non_trivia_token_count: u32,
    pub eligibility: CloneBodyEligibilityV1,
    pub tokenization_status: CloneBodyTokenizationStatusV1,
    pub tokenization_issues: Vec<CloneBodyTokenizationIssueV1>,
    /// Shared with every payload built from this body: a token stream is
    /// read, hashed, and persisted, never edited, and copying it per body
    /// (one `String` per token) was the dominant allocation of the index
    /// workers.
    pub conservative_tokens: Arc<[ConservativeCloneTokenV1]>,
    pub rename_normalization_revision: Option<u16>,
    pub rename_status: CloneBodyRenameStatusV1,
    pub rename_issues: Vec<CloneBodyRenameIssueV1>,
    pub rename_tokens: Option<Arc<[ConservativeCloneTokenV1]>>,
}

impl ExtractedCloneBodyV1 {
    pub fn complete_rename_tokens(&self) -> Option<&[ConservativeCloneTokenV1]> {
        if self.tokenization_status != CloneBodyTokenizationStatusV1::Complete
            || self.rename_status != CloneBodyRenameStatusV1::Complete
        {
            return None;
        }
        self.rename_tokens.as_deref()
    }
}

pub(crate) fn canonicalize_clone_body_order(rows: &mut [ExtractedCloneBodyV1]) {
    rows.sort_by(|left, right| {
        left.logical_path
            .cmp(&right.logical_path)
            .then_with(|| left.body_span.cmp(&right.body_span))
            .then_with(|| left.symbol_occurrence_id.cmp(&right.symbol_occurrence_id))
    });
}

#[derive(Clone)]
struct CallableOccurrence {
    symbol_kind: NodeKind,
    symbol_occurrence_id: String,
    syntax_span: SyntaxSpan,
}

type SyntaxSpan = (usize, usize, usize, usize);

pub(crate) fn attach_conservative_clone_bodies(
    artifact: &mut ExtractionArtifactV1,
    tree: &Tree,
    source: &str,
    language: &str,
    logical_path: &str,
) {
    let mut callables = Vec::new();
    for node in artifact
        .result
        .nodes
        .iter()
        .filter(|node| node.kind.is_callable_kind())
    {
        callables.push(CallableOccurrence {
            symbol_kind: node.kind.clone(),
            symbol_occurrence_id: node.id.clone(),
            syntax_span: (
                node.start_line as usize,
                node.start_column as usize,
                node.end_line as usize,
                node.end_column as usize,
            ),
        });
    }

    let mut clone_bodies = Vec::with_capacity(callables.len());
    let root = tree.root_node();
    for occurrence in &callables {
        let Some(owner) = syntax_owner(root, occurrence.syntax_span) else {
            continue;
        };
        let Some(syntax) = callable_syntax(owner) else {
            continue;
        };
        clone_bodies.push(extract_clone_body(
            occurrence,
            syntax,
            source,
            language,
            logical_path,
        ));
    }
    canonicalize_clone_body_order(&mut clone_bodies);
    artifact.clone_bodies = clone_bodies;
}

fn extract_clone_body(
    occurrence: &CallableOccurrence,
    syntax: CallableSyntax<'_>,
    source: &str,
    language: &str,
    logical_path: &str,
) -> ExtractedCloneBodyV1 {
    let body_bytes = syntax
        .body
        .end_byte()
        .saturating_sub(syntax.body.start_byte()) as u64;
    let (conservative, rename) = if body_bytes > MAX_AUTOMATIC_CLONE_BODY_BYTES_V1 {
        (
            ConservativeFields {
                tokens: Arc::from([]),
                issues: vec![CloneBodyTokenizationIssueV1::BodyExceedsSizeBound],
                token_count: 0,
                status: CloneBodyTokenizationStatusV1::Partial,
                eligibility: oversized_clone_body(),
            },
            oversized_rename_fields(),
        )
    } else {
        tokenize_clone_body(syntax, source, language)
    };
    ExtractedCloneBodyV1 {
        logical_path: logical_path.to_owned(),
        language: language.to_owned(),
        symbol_kind: occurrence.symbol_kind.clone(),
        symbol_occurrence_id: occurrence.symbol_occurrence_id.clone(),
        body_span: SourceSpan {
            start_byte: syntax.body.start_byte() as u64,
            end_byte: syntax.body.end_byte() as u64,
        },
        normalization_revision: CONSERVATIVE_CLONE_NORMALIZATION_REVISION_V1,
        non_trivia_token_count: conservative.token_count,
        eligibility: conservative.eligibility,
        tokenization_status: conservative.status,
        tokenization_issues: conservative.issues,
        conservative_tokens: conservative.tokens,
        rename_normalization_revision: rename.revision,
        rename_status: rename.status,
        rename_issues: rename.issues,
        rename_tokens: rename.tokens,
    }
}

struct ConservativeFields {
    tokens: Arc<[ConservativeCloneTokenV1]>,
    issues: Vec<CloneBodyTokenizationIssueV1>,
    token_count: u32,
    status: CloneBodyTokenizationStatusV1,
    eligibility: CloneBodyEligibilityV1,
}

/// Emit both normalization streams from one traversal.
///
/// The two streams are the same walk of the same body: whether a token is
/// emitted at all is decided by the source text, never by a replacement, so
/// they agree position for position and differ only in the `text` of a
/// renamed identifier. Over this repository that is 778k of 17.25M tokens, so
/// walking the tree a second time to rebuild the other 96% was the larger
/// half of the rename cost. A body whose normalization renames nothing shares
/// the conservative stream outright.
fn tokenize_clone_body(
    syntax: CallableSyntax<'_>,
    source: &str,
    language: &str,
) -> (ConservativeFields, RenameFields) {
    let normalization = rename::normalize(syntax, source, language);
    let mut emitter = TokenEmitter {
        source: source.as_bytes(),
        language,
        replacements: normalization
            .replacements
            .as_ref()
            .filter(|replacements| !replacements.is_empty()),
        tokens: Vec::new(),
        renamed: Vec::new(),
        issues: Vec::new(),
        token_count: 0,
    };
    // Resolved once for the body. Every comma below it used to re-walk its
    // own ancestor chain to ask the same question, and each step of that
    // chain is a `Node::parent` that re-descends from the root.
    emitter.emit(syntax.body, has_ancestor_kind(syntax.body, "token_tree"));
    if !syntax.body_boundary_complete {
        emitter
            .issues
            .push(CloneBodyTokenizationIssueV1::BodyBoundaryUnavailable);
    }
    if syntax.body.has_error() {
        emitter
            .issues
            .push(CloneBodyTokenizationIssueV1::ParseError);
    }
    emitter.issues.sort();
    emitter.issues.dedup();
    let tokenization_status = if emitter.issues.is_empty() {
        CloneBodyTokenizationStatusV1::Complete
    } else {
        CloneBodyTokenizationStatusV1::Partial
    };
    let eligibility = if tokenization_status == CloneBodyTokenizationStatusV1::Partial {
        CloneBodyEligibilityV1::ExcludedIncompleteTokenization
    } else if emitter.token_count < MIN_AUTOMATIC_CLONE_BODY_TOKENS_V1 {
        CloneBodyEligibilityV1::ExcludedTooSmall {
            minimum_tokens: MIN_AUTOMATIC_CLONE_BODY_TOKENS_V1,
        }
    } else if emitter.token_count > MAX_AUTOMATIC_CLONE_BODY_TOKENS_V1 {
        oversized_clone_body()
    } else {
        CloneBodyEligibilityV1::Eligible
    };
    // An oversized body keeps its count and its typed exclusion but no
    // stream: the streams are what would not fit a page, and rename
    // normalization has nothing to normalize for.
    let oversized = matches!(eligibility, CloneBodyEligibilityV1::ExcludedTooLarge { .. });
    let tokens: Arc<[ConservativeCloneTokenV1]> = if oversized {
        Arc::from([])
    } else {
        emitter.tokens.into()
    };
    let rename = if oversized {
        oversized_rename_fields()
    } else {
        RenameFields {
            revision: (normalization.status != CloneBodyRenameStatusV1::UnsupportedLanguage)
                .then_some(RENAME_CLONE_NORMALIZATION_REVISION_V1),
            status: normalization.status,
            issues: normalization.issues,
            tokens: normalization.replacements.is_some().then(|| {
                if emitter.replacements.is_some() {
                    emitter.renamed.into()
                } else {
                    Arc::clone(&tokens)
                }
            }),
        }
    };
    (
        ConservativeFields {
            tokens,
            issues: emitter.issues,
            token_count: emitter.token_count,
            status: tokenization_status,
            eligibility,
        },
        rename,
    )
}

const fn oversized_clone_body() -> CloneBodyEligibilityV1 {
    CloneBodyEligibilityV1::ExcludedTooLarge {
        maximum_tokens: MAX_AUTOMATIC_CLONE_BODY_TOKENS_V1,
        maximum_bytes: MAX_AUTOMATIC_CLONE_BODY_BYTES_V1,
    }
}

struct RenameFields {
    revision: Option<u16>,
    status: CloneBodyRenameStatusV1,
    issues: Vec<CloneBodyRenameIssueV1>,
    tokens: Option<Arc<[ConservativeCloneTokenV1]>>,
}

fn oversized_rename_fields() -> RenameFields {
    RenameFields {
        revision: None,
        status: CloneBodyRenameStatusV1::Partial,
        issues: Vec::new(),
        tokens: None,
    }
}

struct TokenEmitter<'a> {
    source: &'a [u8],
    language: &'a str,
    /// Present only when normalization renames at least one identifier. When
    /// it is absent `renamed` stays empty and the caller shares the
    /// conservative stream.
    replacements: Option<&'a HashMap<(usize, usize), String>>,
    tokens: Vec<ConservativeCloneTokenV1>,
    renamed: Vec<ConservativeCloneTokenV1>,
    issues: Vec<CloneBodyTokenizationIssueV1>,
    token_count: u32,
}

impl<'a> TokenEmitter<'a> {
    fn emit(&mut self, node: TreeSitterNode<'_>, in_token_tree: bool) {
        // Asking a node for its kind is a `strlen` over the grammar table plus
        // a UTF-8 check, so the walk reads it once and passes it around.
        let kind = node.kind();
        if is_comment(kind) {
            return;
        }
        if node.child_count() == 0 {
            self.emit_leaf(node, kind, in_token_tree);
            return;
        }

        if node.is_named() {
            self.push(ConservativeCloneTokenV1::StructureStart {
                syntax_kind: Cow::Borrowed(kind),
            });
        }
        let inside_token_tree = in_token_tree || kind == "token_tree";
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                self.emit(cursor.node(), inside_token_tree);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        if node.is_named() {
            self.push(ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: Cow::Borrowed(kind),
            });
        }
    }

    fn push(&mut self, token: ConservativeCloneTokenV1) {
        if self.replacements.is_some() {
            self.renamed.push(token.clone());
        }
        self.tokens.push(token);
    }

    fn emit_leaf(&mut self, node: TreeSitterNode<'_>, kind: &'static str, in_token_tree: bool) {
        let Ok(text) = node.utf8_text(self.source) else {
            self.issues
                .push(CloneBodyTokenizationIssueV1::InvalidSourceRange);
            return;
        };
        if text.trim().is_empty()
            || is_ignorable_trailing_comma(node, kind, self.source, in_token_tree)
            || (kind == ";" && matches!(self.language, "javascript" | "typescript" | "tsx"))
        {
            return;
        }
        let syntax_kind = Cow::Borrowed(kind);
        let text = if text == kind {
            Cow::Borrowed(kind)
        } else {
            Cow::Owned(text.to_owned())
        };
        if let Some(replacements) = self.replacements {
            let replacement = replacements.get(&(node.start_byte(), node.end_byte()));
            self.renamed.push(ConservativeCloneTokenV1::Syntax {
                syntax_kind: syntax_kind.clone(),
                text: replacement
                    .map_or_else(|| text.clone(), |renamed| Cow::Owned(renamed.clone())),
            });
        }
        self.tokens
            .push(ConservativeCloneTokenV1::Syntax { syntax_kind, text });
        self.token_count = self.token_count.saturating_add(1);
    }
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "comment" | "comments")
        || kind.ends_with("_comment")
        || kind.starts_with("comment_")
}

fn is_ignorable_trailing_comma(
    node: TreeSitterNode<'_>,
    kind: &str,
    source: &[u8],
    in_token_tree: bool,
) -> bool {
    if in_token_tree || kind != "," {
        return false;
    }
    let mut next = node.next_sibling();
    while let Some(sibling) = next {
        if is_comment(sibling.kind()) {
            next = sibling.next_sibling();
            continue;
        }
        return sibling
            .utf8_text(source)
            .is_ok_and(|text| matches!(text, ")" | "]" | "}"));
    }
    false
}

fn has_ancestor_kind(node: TreeSitterNode<'_>, kind: &str) -> bool {
    let mut parent = node.parent();
    while let Some(candidate) = parent {
        if candidate.kind() == kind {
            return true;
        }
        parent = candidate.parent();
    }
    false
}

#[derive(Clone, Copy)]
pub(super) struct CallableSyntax<'tree> {
    owner: TreeSitterNode<'tree>,
    body: TreeSitterNode<'tree>,
    body_boundary_complete: bool,
}

fn callable_syntax(owner: TreeSitterNode<'_>) -> Option<CallableSyntax<'_>> {
    owner
        .child_by_field_name("body")
        .map(|body| CallableSyntax {
            owner,
            body,
            body_boundary_complete: true,
        })
        .or_else(|| {
            SyntaxPreorder::new(owner).skip(1).find_map(|candidate| {
                candidate
                    .child_by_field_name("body")
                    .map(|body| CallableSyntax {
                        owner: candidate,
                        body,
                        body_boundary_complete: true,
                    })
            })
        })
        .or(Some(CallableSyntax {
            owner,
            body: owner,
            body_boundary_complete: false,
        }))
}

fn syntax_owner(
    root: TreeSitterNode<'_>,
    (start_row, start_column, end_row, end_column): SyntaxSpan,
) -> Option<TreeSitterNode<'_>> {
    let mut candidate = root.descendant_for_point_range(
        Point::new(start_row, start_column),
        Point::new(end_row, end_column),
    )?;
    loop {
        let start = candidate.start_position();
        let end = candidate.end_position();
        if (start.row, start.column, end.row, end.column)
            == (start_row, start_column, end_row, end_column)
        {
            return Some(candidate);
        }
        candidate = candidate.parent()?;
    }
}

pub(super) struct SyntaxPreorder<'tree> {
    cursor: TreeCursor<'tree>,
    started: bool,
    finished: bool,
}

impl<'tree> SyntaxPreorder<'tree> {
    pub(super) fn new(node: TreeSitterNode<'tree>) -> Self {
        Self {
            cursor: node.walk(),
            started: false,
            finished: false,
        }
    }

    /// The field the node just yielded occupies in its parent.
    ///
    /// The walk already holds this. Recovering it afterwards costs a
    /// `Node::parent`, which tree-sitter answers by re-descending from the
    /// root, and then a scan of that parent's children per field tried.
    pub(super) fn field_name(&self) -> Option<&'static str> {
        self.cursor.field_name()
    }
}

impl<'tree> Iterator for SyntaxPreorder<'tree> {
    type Item = TreeSitterNode<'tree>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        if !self.started {
            self.started = true;
            return Some(self.cursor.node());
        }
        if self.cursor.goto_first_child() {
            return Some(self.cursor.node());
        }
        while !self.cursor.goto_next_sibling() {
            if !self.cursor.goto_parent() {
                self.finished = true;
                return None;
            }
        }
        Some(self.cursor.node())
    }
}
