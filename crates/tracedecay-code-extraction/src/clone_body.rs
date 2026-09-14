use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{NodeKind, SourceSpan};
use tree_sitter::{Node as TreeSitterNode, Tree, TreeCursor};

use crate::ExtractionArtifactV1;

pub const CONSERVATIVE_CLONE_NORMALIZATION_REVISION_V1: u16 = 1;
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
    InvalidSourceRange,
    ParseError,
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
}

type SyntaxSpan = (usize, usize, usize, usize);

pub(crate) fn attach_conservative_clone_bodies(
    artifact: &mut ExtractionArtifactV1,
    tree: &Tree,
    source: &str,
    language: &str,
    logical_path: &str,
) {
    let mut callables = HashMap::<SyntaxSpan, Vec<CallableOccurrence>>::new();
    for node in artifact
        .result
        .nodes
        .iter()
        .filter(|node| node.kind.is_callable_kind())
    {
        callables
            .entry((
                node.start_line as usize,
                node.start_column as usize,
                node.end_line as usize,
                node.end_column as usize,
            ))
            .or_default()
            .push(CallableOccurrence {
                symbol_kind: node.kind.clone(),
                symbol_occurrence_id: node.id.clone(),
            });
    }

    let mut clone_bodies = Vec::with_capacity(callables.len());
    let mut emitted = HashSet::with_capacity(callables.len());
    for owner in SyntaxPreorder::new(tree.root_node()) {
        let key = (
            owner.start_position().row,
            owner.start_position().column,
            owner.end_position().row,
            owner.end_position().column,
        );
        let Some(occurrences) = callables.get(&key) else {
            continue;
        };
        let Some(body) = callable_body(owner) else {
            continue;
        };
        for occurrence in occurrences {
            if !emitted.insert(occurrence.symbol_occurrence_id.as_str()) {
                continue;
            }
            clone_bodies.push(extract_clone_body(
                occurrence,
                body,
                source,
                language,
                logical_path,
            ));
        }
    }
    canonicalize_clone_body_order(&mut clone_bodies);
    artifact.clone_bodies = clone_bodies;
}

fn extract_clone_body(
    occurrence: &CallableOccurrence,
    body: TreeSitterNode<'_>,
    source: &str,
    language: &str,
    logical_path: &str,
) -> ExtractedCloneBodyV1 {
    let mut emitter = TokenEmitter {
        source: source.as_bytes(),
        language,
        tokens: Vec::new(),
        issues: Vec::new(),
        token_count: 0,
    };
    emitter.emit(body);
    if body.has_error() {
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
    let eligibility = if emitter.token_count < MIN_AUTOMATIC_CLONE_BODY_TOKENS_V1 {
        CloneBodyEligibilityV1::ExcludedTooSmall {
            minimum_tokens: MIN_AUTOMATIC_CLONE_BODY_TOKENS_V1,
        }
    } else {
        CloneBodyEligibilityV1::Eligible
    };

    ExtractedCloneBodyV1 {
        logical_path: logical_path.to_owned(),
        language: language.to_owned(),
        symbol_kind: occurrence.symbol_kind.clone(),
        symbol_occurrence_id: occurrence.symbol_occurrence_id.clone(),
        body_span: SourceSpan {
            start_byte: body.start_byte() as u64,
            end_byte: body.end_byte() as u64,
        },
        normalization_revision: CONSERVATIVE_CLONE_NORMALIZATION_REVISION_V1,
        non_trivia_token_count: emitter.token_count,
        eligibility,
        tokenization_status,
        tokenization_issues: emitter.issues,
        conservative_tokens: emitter.tokens,
    }
}

struct TokenEmitter<'a> {
    source: &'a [u8],
    language: &'a str,
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
            text: text.to_owned(),
        });
        self.token_count = self.token_count.saturating_add(1);
    }
}

fn is_comment(kind: &str) -> bool {
    kind == "comment" || kind.ends_with("_comment") || kind.starts_with("comment_")
}

fn is_ignorable_trailing_comma(node: TreeSitterNode<'_>, source: &[u8]) -> bool {
    if node.kind() != "," {
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

fn callable_body(owner: TreeSitterNode<'_>) -> Option<TreeSitterNode<'_>> {
    owner.child_by_field_name("body").or_else(|| {
        SyntaxPreorder::new(owner)
            .skip(1)
            .find_map(|node| node.child_by_field_name("body"))
    })
}

struct SyntaxPreorder<'tree> {
    cursor: TreeCursor<'tree>,
    started: bool,
    finished: bool,
}

impl<'tree> SyntaxPreorder<'tree> {
    fn new(node: TreeSitterNode<'tree>) -> Self {
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
