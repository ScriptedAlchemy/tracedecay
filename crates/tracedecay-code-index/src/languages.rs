//! Versioned language registry port (Plan 25, "Deterministic extraction").
//!
//! One versioned `LanguageDescriptorV1` per language is shared by
//! extraction, structural search, outline, rewrite, analyzer routing, and
//! host LSP projection. Duplicate language tables and parser acquisition
//! paths are forbidden; descriptors, not extractors, select grammars and
//! capabilities.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tracedecay_domain::{
    DomainError, ExpandoBehaviorV1, ExtractorRevision, GrammarRevision, LanguageCapabilitySetV1,
    LanguageDescriptorRevision, LanguageDescriptorV1, LanguageId, LanguageRegistryRevision,
    canonical_sha256,
};

/// The versioned language registry contract. Grammar, aliases, extensions,
/// expando behavior, and extractor revision are selected through this one
/// registry (Plan 25).
pub trait LanguageRegistry {
    /// The revision of the whole registry, recorded on every generation.
    fn registry_revision(&self) -> LanguageRegistryRevision;

    /// Resolve a descriptor by canonical language identity.
    fn descriptor(&self, language: &LanguageId) -> Option<&LanguageDescriptorV1>;

    /// Resolve a descriptor by lowercase file extension (no leading dot).
    fn descriptor_for_extension(&self, extension: &str) -> Option<&LanguageDescriptorV1>;

    /// Resolve a descriptor by alias or host language identifier.
    fn descriptor_for_alias(&self, alias: &str) -> Option<&LanguageDescriptorV1>;

    /// Every registered descriptor, in canonical language-identity order.
    fn descriptors(&self) -> Vec<&LanguageDescriptorV1>;

    /// The descriptor revision recorded for one language.
    fn descriptor_revision(&self, language: &LanguageId) -> Option<LanguageDescriptorRevision>;
}

/// Map a human-readable extractor language name to its canonical language
/// identity (lowercase, punctuation-free). Shared by the registry and the
/// extractor adapter so descriptor and parser selection can never disagree
/// about canonical identity.
pub(crate) fn canonical_language_id(language_name: &str) -> String {
    match language_name {
        "C#" => "csharp".to_owned(),
        "C++" => "cpp".to_owned(),
        "F#" => "fsharp".to_owned(),
        "Objective-C" => "objc".to_owned(),
        "VB.NET" => "vbnet".to_owned(),
        "GW-BASIC" => "gwbasic".to_owned(),
        "MS BASIC 2.0" => "msbasic2".to_owned(),
        other => other.to_lowercase(),
    }
}

/// Languages whose extractors identify stable member spans, enabling
/// `SymbolMember` child chunks (Plan 25).
///
/// Markdown qualifies: a heading owns its whole section, and nested headings
/// are stable child spans. That is what keeps a chunk from splitting mid
/// section — an oversized section splits at its sub-heading boundaries via
/// `structural_segments` instead of at an arbitrary byte window, and a section
/// that fits the budget stays one chunk.
fn has_stable_member_spans(language: &str) -> bool {
    matches!(
        language,
        "markdown"
            | "rust"
            | "go"
            | "java"
            | "scala"
            | "typescript"
            | "python"
            | "c"
            | "cpp"
            | "csharp"
            | "kotlin"
            | "swift"
            | "svelte"
            | "astro"
            | "dart"
            | "php"
            | "ruby"
            | "lua"
            | "zig"
            | "objc"
            | "perl"
            | "haskell"
            | "ocaml"
            | "clojure"
            | "erlang"
            | "elixir"
            | "fsharp"
            | "julia"
            | "r"
            | "pascal"
            | "vbnet"
    )
}

/// Root markers used by analyzer routing and host LSP projection.
fn root_markers(language: &str) -> Vec<String> {
    let markers: &[&str] = match language {
        "rust" => &["Cargo.toml"],
        "go" => &["go.mod"],
        "java" => &["pom.xml", "build.gradle"],
        "scala" => &["build.sbt"],
        "kotlin" => &["build.gradle.kts", "settings.gradle.kts"],
        "typescript" => &["package.json", "tsconfig.json"],
        "python" => &["pyproject.toml", "setup.py"],
        "c" => &["Makefile"],
        "cpp" => &["CMakeLists.txt"],
        "php" => &["composer.json"],
        "ruby" => &["Gemfile"],
        "dart" => &["pubspec.yaml"],
        "elixir" => &["mix.exs"],
        "erlang" => &["rebar.config"],
        "haskell" => &["stack.yaml", "cabal.project"],
        "ocaml" => &["dune-project"],
        "clojure" => &["deps.edn", "project.clj"],
        "julia" => &["Project.toml"],
        "nix" => &["flake.nix"],
        "zig" => &["build.zig"],
        "swift" => &["Package.swift"],
        "perl" => &["Makefile.PL", "cpanfile"],
        "r" => &["DESCRIPTION"],
        _ => &[],
    };
    let mut markers: Vec<String> = markers.iter().map(|marker| (*marker).to_owned()).collect();
    markers.sort();
    markers.dedup();
    markers
}

/// Additional host language identifiers beyond the lowercase extractor name
/// and the canonical identity.
fn extra_aliases(language: &str) -> Vec<&'static str> {
    match language {
        "csharp" => vec!["c#"],
        "cpp" => vec!["c++"],
        "fsharp" => vec!["f#"],
        "objc" => vec!["objective-c"],
        "typescript" => vec!["javascript"],
        "vbnet" => vec!["vb.net"],
        "gwbasic" => vec!["gw-basic"],
        _ => vec![],
    }
}

/// The static language registry: one versioned `LanguageDescriptorV1` per
/// language whose extractor is compiled into this build. Descriptor language
/// facts are owned here; parser acquisition stays in
/// `tracedecay_code_extraction::LanguageRegistry`, which this registry enumerates so
/// the descriptor set always covers exactly the compiled extractor set.
pub struct StaticLanguageRegistry {
    revision: LanguageRegistryRevision,
    /// Descriptors in canonical language-identity order.
    descriptors: Vec<LanguageDescriptorV1>,
    by_language: HashMap<String, usize>,
    by_extension: HashMap<String, usize>,
    by_alias: HashMap<String, usize>,
}

impl StaticLanguageRegistry {
    /// Build the registry from the compiled-in extraction registry. Every
    /// extension the extraction registry dispatches on is attributed to the
    /// extractor that would actually parse it, so descriptor extension
    /// admission and parser selection share one dispatch order.
    pub fn new() -> Self {
        Self::from_extraction_registry(&tracedecay_code_extraction::LanguageRegistry::new())
    }

    /// Build the registry from an existing extraction registry.
    pub fn from_extraction_registry(
        extractors: &tracedecay_code_extraction::LanguageRegistry,
    ) -> Self {
        // Group each dispatched extension by the extractor that claims it,
        // keyed by canonical language identity for deterministic ordering.
        let mut by_language: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
        for extension in extractors.supported_extensions() {
            let probe = format!("probe.{extension}");
            let extractor = extractors
                .extractor_for_file(&probe)
                .expect("every supported extension resolves to an extractor");
            let language = canonical_language_id(extractor.language_name());
            let entry = by_language
                .entry(language)
                .or_insert_with(|| (extractor.language_name().to_owned(), BTreeSet::new()));
            entry.1.insert(extension.to_lowercase());
        }

        let mut descriptors = Vec::with_capacity(by_language.len());
        for (language, (language_name, extensions)) in by_language {
            let mut aliases: BTreeSet<String> = BTreeSet::new();
            aliases.insert(language_name.to_lowercase());
            aliases.insert(language.clone());
            for alias in extra_aliases(&language) {
                aliases.insert(alias.to_owned());
            }
            let descriptor = LanguageDescriptorV1 {
                language: LanguageId::new(language.clone())
                    .expect("canonical language identity is valid"),
                descriptor_revision: LanguageDescriptorRevision::new(format!(
                    "descriptor.{language}.v1"
                ))
                .expect("descriptor revision is canonical"),
                grammar_revision: GrammarRevision::new(format!(
                    "grammar.tree-sitter.{language}.v1"
                ))
                .expect("grammar revision is canonical"),
                extractor_revision: ExtractorRevision::new(format!("extractor.{language}.v2"))
                    .expect("extractor revision is canonical"),
                aliases: aliases.into_iter().collect(),
                extensions: extensions.into_iter().collect(),
                root_markers: root_markers(&language),
                expando: ExpandoBehaviorV1::MarkGenerated,
                stable_member_spans: has_stable_member_spans(&language),
                capabilities: LanguageCapabilitySetV1 {
                    extraction: true,
                    structural_search: true,
                    outline: true,
                    rewrite: true,
                    analyzer_routing: false,
                    lsp_projection: false,
                },
            };
            descriptor
                .validate()
                .expect("static language descriptors are canonical");
            descriptors.push(descriptor);
        }
        Self::try_from_descriptors(descriptors)
            .expect("compiled extraction descriptors must be unique")
    }

    /// Build a registry from explicit descriptors (test seams and pinned
    /// custom registries). Invalid duplicate lookup keys panic here to
    /// preserve the infallible constructor; use [`Self::try_from_descriptors`]
    /// to receive the typed domain rejection instead.
    pub fn from_descriptors(descriptors: Vec<LanguageDescriptorV1>) -> Self {
        Self::try_from_descriptors(descriptors)
            .expect("language descriptors must have unique language, alias, and extension keys")
    }

    /// Try to build a registry from explicit canonical descriptors.
    pub fn try_from_descriptors(
        mut descriptors: Vec<LanguageDescriptorV1>,
    ) -> Result<Self, DomainError> {
        descriptors.sort_by(|left, right| {
            left.language.as_str().cmp(right.language.as_str()).then(
                left.descriptor_revision
                    .as_str()
                    .cmp(right.descriptor_revision.as_str()),
            )
        });
        validate_descriptors(&descriptors)?;

        let mut by_language = HashMap::new();
        let mut by_extension = HashMap::new();
        let mut by_alias = HashMap::new();
        for (index, descriptor) in descriptors.iter().enumerate() {
            by_language.insert(descriptor.language.as_str().to_owned(), index);
            for extension in &descriptor.extensions {
                by_extension.insert(extension.clone(), index);
            }
            for alias in &descriptor.aliases {
                by_alias.insert(alias.to_lowercase(), index);
            }
        }

        let digest = canonical_sha256(&("tracedecay.language-registry.v1", &descriptors))
            .expect("language registry revision payload serializes canonically");
        let short: String = digest
            .as_str()
            .trim_start_matches("sha256:")
            .chars()
            .take(16)
            .collect();
        let revision = LanguageRegistryRevision::new(format!("registry.v1.{short}"))
            .expect("registry revision is canonical");

        Ok(Self {
            revision,
            descriptors,
            by_language,
            by_extension,
            by_alias,
        })
    }
}

impl Default for StaticLanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageRegistry for StaticLanguageRegistry {
    fn registry_revision(&self) -> LanguageRegistryRevision {
        self.revision.clone()
    }

    fn descriptor(&self, language: &LanguageId) -> Option<&LanguageDescriptorV1> {
        self.by_language
            .get(language.as_str())
            .map(|index| &self.descriptors[*index])
    }

    fn descriptor_for_extension(&self, extension: &str) -> Option<&LanguageDescriptorV1> {
        self.by_extension
            .get(extension)
            .map(|index| &self.descriptors[*index])
    }

    fn descriptor_for_alias(&self, alias: &str) -> Option<&LanguageDescriptorV1> {
        self.by_alias
            .get(&alias.to_lowercase())
            .map(|index| &self.descriptors[*index])
    }

    fn descriptors(&self) -> Vec<&LanguageDescriptorV1> {
        self.descriptors.iter().collect()
    }

    fn descriptor_revision(&self, language: &LanguageId) -> Option<LanguageDescriptorRevision> {
        self.descriptor(language)
            .map(|descriptor| descriptor.descriptor_revision.clone())
    }
}

/// Validate a full descriptor set, including every cross-descriptor lookup
/// key, before constructing maps that would otherwise silently overwrite.
pub(crate) fn validate_descriptors(
    descriptors: &[LanguageDescriptorV1],
) -> Result<(), DomainError> {
    let mut languages = BTreeSet::new();
    for descriptor in descriptors {
        descriptor.validate()?;
        let normalized = descriptor.language.as_str().to_lowercase();
        if normalized != descriptor.language.as_str() {
            return Err(DomainError::NonCanonical {
                field: "language registry language identity",
            });
        }
        if !languages.insert(normalized) {
            return Err(DomainError::DuplicateId {
                field: "language registry language",
            });
        }
    }

    let mut aliases = BTreeSet::new();
    let mut extensions = BTreeSet::new();
    for descriptor in descriptors {
        for alias in &descriptor.aliases {
            let normalized = alias.to_lowercase();
            if !aliases.insert(normalized.clone())
                || (languages.contains(&normalized) && normalized != descriptor.language.as_str())
            {
                return Err(DomainError::DuplicateId {
                    field: "language registry alias",
                });
            }
        }
        for extension in &descriptor.extensions {
            if !extensions.insert(extension.clone()) {
                return Err(DomainError::DuplicateId {
                    field: "language registry extension",
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn language(value: &str) -> LanguageId {
        LanguageId::new(value).expect("valid language id")
    }

    #[test]
    fn registry_covers_every_compiled_extractor_extension() {
        let extractors = tracedecay_code_extraction::LanguageRegistry::new();
        let registry = StaticLanguageRegistry::from_extraction_registry(&extractors);
        assert!(!registry.descriptors().is_empty());
        for extension in extractors.supported_extensions() {
            let descriptor = registry
                .descriptor_for_extension(&extension.to_lowercase())
                .unwrap_or_else(|| panic!("descriptor for extension {extension}"));
            // The descriptor's language must be the language of the extractor
            // that would actually parse the file.
            let probe = format!("probe.{extension}");
            let extractor = extractors
                .extractor_for_file(&probe)
                .expect("extractor resolves");
            assert_eq!(
                descriptor.language.as_str(),
                canonical_language_id(extractor.language_name()),
                "extension {extension} dispatched to a different language"
            );
        }
    }

    #[test]
    fn descriptor_lookups_are_canonical_and_deterministic() {
        let registry = StaticLanguageRegistry::new();
        let again = StaticLanguageRegistry::new();
        assert_eq!(registry.registry_revision(), again.registry_revision());

        let rust = registry
            .descriptor(&language("rust"))
            .expect("rust descriptor");
        assert_eq!(rust.extensions, vec!["rs".to_owned()]);
        assert!(rust.stable_member_spans);
        assert!(rust.capabilities.extraction);
        assert_eq!(rust.root_markers, vec!["Cargo.toml".to_owned()]);

        assert_eq!(
            registry
                .descriptor_for_extension("rs")
                .map(|d| d.language.as_str()),
            Some("rust")
        );
        assert_eq!(
            registry
                .descriptor_for_alias("rust")
                .map(|d| d.language.as_str()),
            Some("rust")
        );
        assert_eq!(
            registry
                .descriptor_for_alias("javascript")
                .map(|d| d.language.as_str()),
            Some("typescript")
        );
        assert!(registry.descriptor(&language("cobol-nope")).is_none());
        assert!(registry.descriptor_for_extension("nope").is_none());
        assert_eq!(
            registry.descriptor_revision(&language("rust")),
            Some(rust.descriptor_revision.clone())
        );

        // Canonical language-identity order.
        let ids: Vec<&str> = registry
            .descriptors()
            .iter()
            .map(|d| d.language.as_str())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn every_descriptor_passes_domain_validation() {
        let registry = StaticLanguageRegistry::new();
        for descriptor in registry.descriptors() {
            descriptor.validate().expect("descriptor validates");
            assert!(!descriptor.aliases.is_empty());
            assert!(!descriptor.extensions.is_empty());
        }
    }

    #[test]
    fn descriptor_set_rejects_duplicate_languages_aliases_and_extensions() {
        let rust = StaticLanguageRegistry::new()
            .descriptor(&language("rust"))
            .expect("rust")
            .clone();

        assert!(
            StaticLanguageRegistry::try_from_descriptors(vec![rust.clone(), rust.clone()]).is_err(),
            "duplicate language identities cannot be silently deduplicated"
        );

        let mut duplicate_alias = rust.clone();
        duplicate_alias.language = language("alternaterust");
        duplicate_alias.aliases = vec!["RUST".to_owned()];
        duplicate_alias.extensions = vec!["alternate".to_owned()];
        duplicate_alias
            .validate()
            .expect("individually canonical descriptor");
        assert!(
            StaticLanguageRegistry::try_from_descriptors(vec![rust.clone(), duplicate_alias])
                .is_err(),
            "aliases must resolve to exactly one descriptor"
        );

        let mut duplicate_extension = rust.clone();
        duplicate_extension.language = language("alternaterustextension");
        duplicate_extension.aliases = vec!["alternaterustextension".to_owned()];
        duplicate_extension.extensions = vec!["rs".to_owned()];
        duplicate_extension
            .validate()
            .expect("individually canonical descriptor");
        assert!(
            StaticLanguageRegistry::try_from_descriptors(vec![rust, duplicate_extension]).is_err(),
            "extensions must resolve to exactly one descriptor"
        );
    }

    #[test]
    fn descriptor_set_rejects_language_case_collisions_in_either_order() {
        let rust = StaticLanguageRegistry::new()
            .descriptor(&language("rust"))
            .expect("rust")
            .clone();
        let mut uppercase = rust.clone();
        uppercase.language = language("Rust");
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

    #[test]
    fn from_descriptors_sorts_valid_descriptors() {
        let rust = StaticLanguageRegistry::new()
            .descriptor(&language("rust"))
            .expect("rust")
            .clone();
        let mut renamed = rust.clone();
        renamed.language = language("aaa-test-language");
        renamed.aliases = vec!["aaa-test-language".to_owned()];
        renamed.extensions = vec!["aaa".to_owned()];
        renamed
            .validate()
            .expect("renamed descriptor remains canonical");
        let registry = StaticLanguageRegistry::from_descriptors(vec![rust, renamed]);
        let ids: Vec<&str> = registry
            .descriptors()
            .iter()
            .map(|d| d.language.as_str())
            .collect();
        assert_eq!(ids, vec!["aaa-test-language", "rust"]);
    }

    #[test]
    #[cfg(feature = "lang-markdown")]
    fn markdown_is_admitted_with_stable_section_spans() {
        let registry = StaticLanguageRegistry::new();
        let markdown = registry
            .descriptor(&language("markdown"))
            .expect("markdown is a lite-tier language");
        assert!(markdown.stable_member_spans);
        assert!(markdown.capabilities.extraction);
        assert!(markdown.capabilities.outline);
        assert_eq!(
            markdown.extensions,
            vec!["markdown".to_owned(), "md".to_owned()]
        );
        assert_eq!(
            registry
                .descriptor_for_extension("md")
                .map(|descriptor| descriptor.language.as_str()),
            Some("markdown")
        );
        assert_eq!(
            registry
                .descriptor_for_extension("markdown")
                .map(|descriptor| descriptor.language.as_str()),
            Some("markdown")
        );
    }
}
