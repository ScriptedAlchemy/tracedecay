use tracedecay::{extraction, types};

#[test]
fn leaf_crates_are_canonical_compatibility_paths() {
    assert_eq!(
        types::NodeKind::File,
        tracedecay_domain::code_intelligence::NodeKind::File
    );
    let _ = std::any::TypeId::of::<types::NodeKind>();
    let _ = std::any::TypeId::of::<tracedecay_domain::code_intelligence::NodeKind>();
    let _ = std::any::TypeId::of::<extraction::RustExtractor>();
    let _ = std::any::TypeId::of::<tracedecay_code_extraction::RustExtractor>();
}
