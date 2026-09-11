#![cfg(windows)]

use tracedecay_semantic::SemanticModelLifecycleOwnerV1;
use tracedecay_semantic_contracts::{
    DEFAULT_FASTEMBED_MODEL_ID, MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID,
    SemanticModelLifecycleStateV1,
};

#[test]
fn windows_disables_only_fastembed_semantics() {
    let root = tempfile::tempdir().expect("lifecycle root");
    let owner = SemanticModelLifecycleOwnerV1::open_default(root.path()).expect("lifecycle owner");

    let fastembed = owner
        .select_model(Some(DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("cataloged FastEmbed selection");
    assert!(matches!(
        fastembed.state,
        Some(SemanticModelLifecycleStateV1::Failed {
            ref detail,
            retryable: false,
            ..
        }) if detail == "runtime is incompatible"
    ));
    assert!(!fastembed.remediation.retry);
    assert!(!owner.enqueue_demand_acquisition_if_needed());

    let model2vec = owner
        .select_model(Some(MODEL2VEC_POTION_CODE_16M_V2_MODEL_ID), true)
        .expect("cataloged Model2Vec selection");
    assert!(matches!(
        model2vec.state,
        Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
    ));
}
