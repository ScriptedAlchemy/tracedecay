use std::borrow::Cow;
use std::collections::BTreeSet;

use tracedecay_domain::{
    CodeSearchChunkV1, ExactFieldV1, ExactTechnicalTermKindV1, ExactTechnicalTermV1,
};

use super::ProjectedChunkV1;
use crate::retrieval::exact::{ExactLaneRequest, ExactLiteralV1};

pub(super) fn exact_matches(
    row: &ProjectedChunkV1,
    chunk: &CodeSearchChunkV1,
    request: &ExactLaneRequest,
) -> (Vec<ExactLiteralV1>, Vec<ExactTechnicalTermKindV1>) {
    let mut matched_literals = Vec::new();
    let mut matched_kinds = BTreeSet::new();
    for literal in &request.literals {
        let mut matched = false;
        if matches!(
            literal.field,
            ExactFieldV1::QuotedPhrase
                | ExactFieldV1::DiagnosticText
                | ExactFieldV1::CompilerOrRuntimeError
        ) {
            matched = contains_bytes(
                chunk.sanitized_text.as_str().as_bytes(),
                &literal.original_bytes,
            );
        }
        if literal.field == ExactFieldV1::Path
            && row.logical_path.as_bytes() == literal.canonical_bytes.as_slice()
        {
            matched = true;
            matched_kinds.insert(ExactTechnicalTermKindV1::Path);
        }
        for term in &chunk.exact_terms {
            if exact_field_for_kind(term.kind()) == literal.field
                && canonical_projected_exact_term(term).as_ref()
                    == literal.canonical_bytes.as_slice()
            {
                matched = true;
                matched_kinds.insert(term.kind());
            }
        }
        if matched {
            matched_literals.push(literal.clone());
        }
    }
    (matched_literals, matched_kinds.into_iter().collect())
}

pub(super) fn exact_field_for_kind(kind: ExactTechnicalTermKindV1) -> ExactFieldV1 {
    match kind {
        ExactTechnicalTermKindV1::WholeSymbol => ExactFieldV1::Identifier,
        ExactTechnicalTermKindV1::QualifiedName => ExactFieldV1::QualifiedName,
        ExactTechnicalTermKindV1::Path => ExactFieldV1::Path,
        ExactTechnicalTermKindV1::CompilerErrorCode
        | ExactTechnicalTermKindV1::RuntimeErrorCode => ExactFieldV1::DiagnosticCode,
        ExactTechnicalTermKindV1::CompilerErrorText
        | ExactTechnicalTermKindV1::RuntimeErrorText => ExactFieldV1::CompilerOrRuntimeError,
        ExactTechnicalTermKindV1::CliFlag => ExactFieldV1::CliFlag,
        ExactTechnicalTermKindV1::ToolName => ExactFieldV1::ToolName,
        ExactTechnicalTermKindV1::ConfigurationKey => ExactFieldV1::ConfigurationKey,
        ExactTechnicalTermKindV1::CommitIdentifier => ExactFieldV1::CommitIdentifier,
    }
}

pub(super) fn canonical_projected_exact_term(term: &ExactTechnicalTermV1) -> Cow<'_, [u8]> {
    let bytes = term.canonical_bytes();
    let Ok(value) = std::str::from_utf8(bytes) else {
        return Cow::Borrowed(bytes);
    };
    let canonical = match term.kind() {
        ExactTechnicalTermKindV1::CommitIdentifier => value
            .strip_prefix("commit:")
            .unwrap_or(value)
            .to_ascii_lowercase(),
        ExactTechnicalTermKindV1::CompilerErrorCode
        | ExactTechnicalTermKindV1::RuntimeErrorCode => value.to_ascii_uppercase(),
        ExactTechnicalTermKindV1::CliFlag
        | ExactTechnicalTermKindV1::ToolName
        | ExactTechnicalTermKindV1::ConfigurationKey => value.to_ascii_lowercase(),
        _ => return Cow::Borrowed(bytes),
    };
    if canonical.as_bytes() == bytes {
        Cow::Borrowed(bytes)
    } else {
        Cow::Owned(canonical.into_bytes())
    }
}

pub(super) fn collect_term_kinds(
    chunk: &CodeSearchChunkV1,
    normalized_term: &str,
    kinds: &mut BTreeSet<ExactTechnicalTermKindV1>,
) {
    for term in &chunk.exact_terms {
        if std::str::from_utf8(term.canonical_bytes())
            .is_ok_and(|value| super::normalize_lexical(value) == normalized_term)
        {
            kinds.insert(term.kind());
        }
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
