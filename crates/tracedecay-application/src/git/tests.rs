use tracedecay_domain::GitIndexTransactionOperationV1;
use tracedecay_tool_catalog::EffectClass;

use super::git_index_effect_class;

#[test]
fn each_index_mutation_keeps_its_own_effect_class() {
    assert_eq!(
        git_index_effect_class(GitIndexTransactionOperationV1::StageHunks),
        EffectClass::GitIndexStage
    );
    assert_eq!(
        git_index_effect_class(GitIndexTransactionOperationV1::UnstageHunks),
        EffectClass::GitIndexUnstage
    );
    assert_eq!(
        git_index_effect_class(GitIndexTransactionOperationV1::CommitIndex),
        EffectClass::GitIndexCommit
    );
}
