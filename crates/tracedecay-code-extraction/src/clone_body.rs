use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{NodeKind, SourceSpan};
use tree_sitter::{Node as TreeSitterNode, Point, Tree, TreeCursor};

use crate::ExtractionArtifactV1;

mod rename;

pub const CONSERVATIVE_CLONE_NORMALIZATION_REVISION_V1: u16 = 1;
pub const RENAME_CLONE_NORMALIZATION_REVISION_V1: u16 = 1;
pub const MIN_AUTOMATIC_CLONE_BODY_TOKENS_V1: u32 = 30;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConservativeCloneTokenV1 {
    StructureStart { syntax_kind: String },
    Syntax { syntax_kind: String, text: String },
    StructureEnd { syntax_kind: String },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloneBodyEligibilityV1 {
    Eligible,
    ExcludedIncompleteTokenization,
    ExcludedTooSmall { minimum_tokens: u32 },
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
    pub conservative_tokens: Vec<ConservativeCloneTokenV1>,
    pub rename_normalization_revision: Option<u16>,
    pub rename_status: CloneBodyRenameStatusV1,
    pub rename_issues: Vec<CloneBodyRenameIssueV1>,
    pub rename_tokens: Option<Vec<ConservativeCloneTokenV1>>,
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
    let conservative = conservative_fields(syntax, source, language);
    let rename = rename_fields(syntax, source, language);
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
    tokens: Vec<ConservativeCloneTokenV1>,
    issues: Vec<CloneBodyTokenizationIssueV1>,
    token_count: u32,
    status: CloneBodyTokenizationStatusV1,
    eligibility: CloneBodyEligibilityV1,
}

fn conservative_fields(
    syntax: CallableSyntax<'_>,
    source: &str,
    language: &str,
) -> ConservativeFields {
    let mut emitter = TokenEmitter {
        source: source.as_bytes(),
        language,
        replacements: None,
        tokens: Vec::new(),
        issues: Vec::new(),
        token_count: 0,
    };
    emitter.emit(syntax.body);
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
    } else {
        CloneBodyEligibilityV1::Eligible
    };
    ConservativeFields {
        tokens: emitter.tokens,
        issues: emitter.issues,
        token_count: emitter.token_count,
        status: tokenization_status,
        eligibility,
    }
}

struct RenameFields {
    revision: Option<u16>,
    status: CloneBodyRenameStatusV1,
    issues: Vec<CloneBodyRenameIssueV1>,
    tokens: Option<Vec<ConservativeCloneTokenV1>>,
}

fn rename_fields(syntax: CallableSyntax<'_>, source: &str, language: &str) -> RenameFields {
    let normalization = rename::normalize(syntax, source, language);
    let tokens = normalization
        .replacements
        .as_ref()
        .map(|replacements| rename_token_stream(syntax.body, source, language, replacements));
    RenameFields {
        revision: (normalization.status != CloneBodyRenameStatusV1::UnsupportedLanguage)
            .then_some(RENAME_CLONE_NORMALIZATION_REVISION_V1),
        status: normalization.status,
        issues: normalization.issues,
        tokens,
    }
}

fn rename_token_stream(
    body: TreeSitterNode<'_>,
    source: &str,
    language: &str,
    replacements: &HashMap<(usize, usize), String>,
) -> Vec<ConservativeCloneTokenV1> {
    let mut emitter = TokenEmitter {
        source: source.as_bytes(),
        language,
        replacements: Some(replacements),
        tokens: Vec::new(),
        issues: Vec::new(),
        token_count: 0,
    };
    emitter.emit(body);
    emitter.tokens
}

struct TokenEmitter<'a> {
    source: &'a [u8],
    language: &'a str,
    replacements: Option<&'a HashMap<(usize, usize), String>>,
    tokens: Vec<ConservativeCloneTokenV1>,
    issues: Vec<CloneBodyTokenizationIssueV1>,
    token_count: u32,
}

impl<'a> TokenEmitter<'a> {
    fn emit(&mut self, node: TreeSitterNode<'_>) {
        if is_comment(node.kind()) {
            return;
        }
        if node.child_count() == 0 {
            self.emit_leaf(node);
            return;
        }

        if node.is_named() {
            self.tokens.push(ConservativeCloneTokenV1::StructureStart {
                syntax_kind: node.kind().to_owned(),
            });
        }
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                self.emit(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        if node.is_named() {
            self.tokens.push(ConservativeCloneTokenV1::StructureEnd {
                syntax_kind: node.kind().to_owned(),
            });
        }
    }

    fn emit_leaf(&mut self, node: TreeSitterNode<'_>) {
        let Ok(text) = node.utf8_text(self.source) else {
            self.issues
                .push(CloneBodyTokenizationIssueV1::InvalidSourceRange);
            return;
        };
        if text.trim().is_empty()
            || is_ignorable_trailing_comma(node, self.source)
            || (node.kind() == ";" && matches!(self.language, "javascript" | "typescript" | "tsx"))
        {
            return;
        }
        self.tokens.push(ConservativeCloneTokenV1::Syntax {
            syntax_kind: node.kind().to_owned(),
            text: self
                .replacements
                .and_then(|replacements| replacements.get(&(node.start_byte(), node.end_byte())))
                .map_or_else(|| text.to_owned(), Clone::clone),
        });
        self.token_count = self.token_count.saturating_add(1);
    }
}

fn is_comment(kind: &str) -> bool {
    matches!(kind, "comment" | "comments")
        || kind.ends_with("_comment")
        || kind.starts_with("comment_")
}

fn is_ignorable_trailing_comma(node: TreeSitterNode<'_>, source: &[u8]) -> bool {
    if node.kind() != "," || has_ancestor_kind(node, "token_tree") {
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
