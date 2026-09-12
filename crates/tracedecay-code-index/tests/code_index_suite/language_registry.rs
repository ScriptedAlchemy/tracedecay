use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_domain::LanguageId;

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

    assert!(
        StaticLanguageRegistry::try_from_descriptors(vec![rust.clone(), uppercase.clone()])
            .is_err()
    );
    assert!(
        StaticLanguageRegistry::try_from_descriptors(vec![uppercase, rust]).is_err(),
        "case-collision rejection must not depend on input order"
    );
}
