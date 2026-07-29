use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_domain::LanguageId;

use crate::support::id;

#[test]
fn registry_is_total_canonical_and_alias_backed() {
    let extraction = tracedecay_code_index::extraction::LanguageRegistry::new();
    let registry = StaticLanguageRegistry::from_extraction_registry(&extraction);

    for extension in extraction.supported_extensions() {
        let descriptor = registry
            .descriptor_for_extension(&extension.to_lowercase())
            .unwrap_or_else(|| panic!("missing descriptor for {extension}"));
        descriptor.validate().expect("descriptor is canonical");
    }

    let languages: Vec<&str> = registry
        .descriptors()
        .iter()
        .map(|descriptor| descriptor.language.as_str())
        .collect();
    let mut sorted_languages = languages.clone();
    sorted_languages.sort_unstable();
    assert_eq!(languages, sorted_languages);

    for descriptor in registry.descriptors() {
        let mut root_markers = descriptor.root_markers.clone();
        root_markers.sort();
        root_markers.dedup();
        assert_eq!(descriptor.root_markers, root_markers);
    }

    let rust = registry
        .descriptor(&id::<LanguageId>("rust"))
        .expect("rust descriptor");
    assert_eq!(rust.extensions, ["rs"]);
    assert_eq!(
        registry
            .descriptor_for_alias("javascript")
            .map(|descriptor| descriptor.language.as_str()),
        Some("typescript")
    );
    assert_eq!(
        registry
            .descriptor_for_alias("JavaScript")
            .map(|descriptor| descriptor.language.as_str()),
        Some("typescript")
    );
    assert_eq!(
        registry
            .descriptor_for_alias("C++")
            .map(|descriptor| descriptor.language.as_str()),
        Some("cpp")
    );
}

#[test]
fn registry_revision_is_stable_for_identical_compiled_extractors() {
    let first = StaticLanguageRegistry::new();
    let second = StaticLanguageRegistry::new();

    assert_eq!(first.registry_revision(), second.registry_revision());
    assert_eq!(first.descriptors(), second.descriptors());
}

#[test]
fn registry_revision_covers_every_descriptor_fact() {
    let descriptor = StaticLanguageRegistry::new()
        .descriptor(&id::<LanguageId>("rust"))
        .expect("rust descriptor")
        .clone();
    let baseline = StaticLanguageRegistry::from_descriptors(vec![descriptor.clone()]);

    let mut changed = descriptor;
    changed.aliases.push("rust-lang".to_owned());
    changed.aliases.sort();
    changed.aliases.dedup();
    changed.validate().expect("changed descriptor is canonical");
    let changed = StaticLanguageRegistry::from_descriptors(vec![changed]);

    assert_ne!(baseline.registry_revision(), changed.registry_revision());
}

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
