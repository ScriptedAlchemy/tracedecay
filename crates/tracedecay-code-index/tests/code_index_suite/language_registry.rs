use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_domain::{DomainError, LanguageId};

use crate::support::id;

#[test]
fn registry_rejects_language_case_collisions_in_either_order() {
    let rust = StaticLanguageRegistry::new()
        .descriptor(&id::<LanguageId>("rust"))
        .expect("rust descriptor")
        .clone();
    let mut uppercase = rust.clone();
    uppercase.language = id::<LanguageId>("Rust");
    uppercase.aliases = vec!["rust-uppercase".to_owned()];
    uppercase.extensions = vec!["rust-uppercase".to_owned()];
    uppercase
        .validate()
        .expect("mixed-case identity is individually well-formed");

    for (order, descriptors) in [
        ("rust first", vec![rust.clone(), uppercase.clone()]),
        ("uppercase first", vec![uppercase.clone(), rust.clone()]),
    ] {
        let error = StaticLanguageRegistry::try_from_descriptors(descriptors)
            .err()
            .unwrap_or_else(|| panic!("{order}: case collision must be refused"));
        assert!(
            matches!(
                error,
                DomainError::NonCanonical {
                    field: "language registry language identity"
                }
            ),
            "{order}: {error:?}"
        );
    }
    let accepted = StaticLanguageRegistry::try_from_descriptors(vec![rust])
        .expect("the canonical descriptor alone forms a registry");
    assert_eq!(
        accepted
            .descriptor(&id::<LanguageId>("rust"))
            .expect("rust descriptor")
            .language
            .as_str(),
        "rust"
    );
}
