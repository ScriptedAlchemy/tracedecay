use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_domain::{DomainError, LanguageId};

use crate::support::id;

#[test]
fn core_profile_admits_its_languages_and_omits_optional_grammars() {
    let registry = StaticLanguageRegistry::new();
    let languages = registry
        .descriptors()
        .into_iter()
        .map(|descriptor| descriptor.language.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        languages,
        [
            "astro",
            "bash",
            "c",
            "cpp",
            "csharp",
            "dart",
            "go",
            "java",
            "json",
            "kotlin",
            "markdown",
            "nix",
            "pascal",
            "php",
            "powershell",
            "protobuf",
            "python",
            "ruby",
            "rust",
            "scala",
            "svelte",
            "swift",
            "toml",
            "typescript",
            "vbnet",
        ],
        "this suite must exercise the shipped grammar set, without dev-only extras"
    );

    let extractors = tracedecay_code_extraction::LanguageRegistry::new();
    for extension in ["cob", "f90", "lean", "lua"] {
        assert!(registry.descriptor_for_extension(extension).is_none());
        assert!(
            extractors
                .extractor_for_file(&format!("optional.{extension}"))
                .is_none(),
            "an omitted language must not dispatch to an extractor"
        );
    }
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
