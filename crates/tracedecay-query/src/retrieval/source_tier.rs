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

#[cfg(test)]
mod tests {
    use tracedecay_domain::RetrievalSourceRoleV1;

    use super::{classify_source_role, exact_definition_grain, is_definition_field};
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
    }
}
