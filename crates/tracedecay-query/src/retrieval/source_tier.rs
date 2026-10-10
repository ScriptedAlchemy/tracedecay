//! Source-role ranking: production definitions outrank test-file references.

use tracedecay_code_index::is_test_file;
use tracedecay_domain::{CodeSearchChunkGrainV1, RetrievalSourceRoleV1};

use super::lexical::LexicalFieldV1;

/// Classify a hit from the existing test-path authority plus whether the
/// matched evidence is a name or signature definition.
pub(crate) fn classify_source_role(path: &str, definition_match: bool) -> RetrievalSourceRoleV1 {
    if is_test_file(path) {
        RetrievalSourceRoleV1::TestReference
    } else if definition_match {
        RetrievalSourceRoleV1::ProductionDefinition
    } else {
        RetrievalSourceRoleV1::ProductionOther
    }
}

pub(crate) fn is_definition_field(field: LexicalFieldV1) -> bool {
    matches!(
        field,
        LexicalFieldV1::SymbolName | LexicalFieldV1::QualifiedName | LexicalFieldV1::Signature
    )
}

pub(crate) fn exact_definition_grain(grain: CodeSearchChunkGrainV1) -> bool {
    matches!(
        grain,
        CodeSearchChunkGrainV1::SymbolSignature
            | CodeSearchChunkGrainV1::SymbolBody
            | CodeSearchChunkGrainV1::SymbolMember
    )
}

/// True when an exact hit names this row's own symbol, not a body reference
/// to another symbol. Exact signature chunks never answer — the body carries
/// those occurrences — so a defining body's `WholeSymbol`/`QualifiedName`
/// match is the definition evidence.
pub(crate) fn exact_definition_match(
    grain: CodeSearchChunkGrainV1,
    symbol_simple_name: Option<&str>,
    matched_symbol_names: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    let Some(name) = symbol_simple_name.filter(|value| !value.is_empty()) else {
        return false;
    };
    if !exact_definition_grain(grain) {
        return false;
    }
    matched_symbol_names
        .into_iter()
        .any(|matched| identifier_names_symbol(name, matched.as_ref()))
}

fn identifier_names_symbol(symbol: &str, matched: &str) -> bool {
    matched == symbol
        || matched
            .rsplit([':', '.'])
            .find(|part| !part.is_empty())
            .is_some_and(|tail| tail == symbol)
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::RetrievalSourceRoleV1;

    use super::{
        classify_source_role, exact_definition_grain, exact_definition_match, is_definition_field,
    };
    use crate::retrieval::lexical::LexicalFieldV1;
    use tracedecay_domain::CodeSearchChunkGrainV1;

    #[test]
    fn production_definitions_are_not_every_non_test_path() {
        assert_eq!(
            classify_source_role("src/internal/client/ensure-daemon.ts", true),
            RetrievalSourceRoleV1::ProductionDefinition
        );
        assert_eq!(
            classify_source_role("src/internal/client/ensure-daemon.ts", false),
            RetrievalSourceRoleV1::ProductionOther
        );
        assert_eq!(
            classify_source_role("src/internal/client/ensure-daemon.test.ts", true),
            RetrievalSourceRoleV1::TestReference
        );
        assert_eq!(
            RetrievalSourceRoleV1::ProductionDefinition.admission_rank(),
            0
        );
        assert_eq!(RetrievalSourceRoleV1::ProductionOther.admission_rank(), 1);
        assert_eq!(RetrievalSourceRoleV1::TestReference.admission_rank(), 1);
        assert!(is_definition_field(LexicalFieldV1::SymbolName));
        assert!(!is_definition_field(LexicalFieldV1::BodyText));
        assert!(exact_definition_grain(
            CodeSearchChunkGrainV1::SymbolSignature
        ));
        assert!(!exact_definition_grain(CodeSearchChunkGrainV1::FileWindow));
        assert!(exact_definition_match(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("ensureDaemonRunning"),
            ["ensureDaemonRunning"],
        ));
        assert!(exact_definition_match(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("ensureDaemonRunning"),
            ["Client.ensureDaemonRunning"],
        ));
        assert!(!exact_definition_match(
            CodeSearchChunkGrainV1::SymbolBody,
            Some("unrelated"),
            ["ensureDaemonRunning"],
        ));
        assert!(!exact_definition_match(
            CodeSearchChunkGrainV1::FileWindow,
            Some("ensureDaemonRunning"),
            ["ensureDaemonRunning"],
        ));
    }
}
