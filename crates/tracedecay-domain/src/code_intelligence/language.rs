//! Versioned language descriptor contracts (Plan 25, "Deterministic
//! extraction"). One versioned `LanguageDescriptorV1` per language is shared
//! by extraction, structural search, outline, rewrite, analyzer routing, and
//! host LSP projection. Descriptors — not extractors — select grammars and
//! capabilities.
//!
//! These are pure values: no parser acquisition, no host `ast-grep` binary,
//! no configuration-owned executable commands or settings (Plan 20 owns
//! those).

use serde::{Deserialize, Serialize};

use crate::research::DomainError;

use super::identity::{ExtractorRevision, GrammarRevision, LanguageDescriptorRevision, LanguageId};

/// One versioned language descriptor (Plan 25). The same canonical record
/// supplies extension, language-ID, root-marker, and capability facts for
/// analyzer routing and host LSP projection; it does not absorb
/// configuration-owned executable commands or settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageDescriptorV1 {
    pub language: LanguageId,
    pub descriptor_revision: LanguageDescriptorRevision,
    pub grammar_revision: GrammarRevision,
    pub extractor_revision: ExtractorRevision,
    /// Canonical alternative names and host language identifiers.
    pub aliases: Vec<String>,
    /// Lowercase file extensions without the leading dot, canonical order.
    pub extensions: Vec<String>,
    /// Root markers used by analyzer routing and host LSP projection.
    pub root_markers: Vec<String>,
    /// Expando (generated/derived file) handling for this language.
    pub expando: ExpandoBehaviorV1,
    /// Whether the descriptor identifies stable member spans, enabling
    /// `SymbolMember` child chunks (Plan 25).
    pub stable_member_spans: bool,
    /// Declared extraction/navigation capabilities.
    pub capabilities: LanguageCapabilitySetV1,
}

impl LanguageDescriptorV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.language.validate()?;
        self.descriptor_revision.validate()?;
        self.grammar_revision.validate()?;
        self.extractor_revision.validate()?;
        if self.extensions.is_empty() && self.aliases.is_empty() {
            return Err(DomainError::Empty {
                field: "language descriptor extensions and aliases",
            });
        }
        validate_sorted_unique_strings(&self.aliases, "language descriptor alias order")?;
        validate_sorted_unique_strings(&self.extensions, "language descriptor extension order")?;
        validate_sorted_unique_strings(
            &self.root_markers,
            "language descriptor root marker order",
        )?;
        if self.extensions.iter().any(|extension| {
            extension.is_empty()
                || extension.starts_with('.')
                || extension.chars().any(char::is_uppercase)
        }) {
            return Err(DomainError::NonCanonical {
                field: "language descriptor extension form",
            });
        }
        Ok(())
    }
}

fn validate_sorted_unique_strings(
    values: &[String],
    field: &'static str,
) -> Result<(), DomainError> {
    if values.iter().any(|value| {
        value.is_empty() || value.trim() != value || value.chars().any(char::is_control)
    }) || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// How a descriptor treats generated/derived (expando) files.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExpandoBehaviorV1 {
    /// Expando files are indexed like handwritten files.
    Include,
    /// Expando files are indexed but marked as generated evidence.
    MarkGenerated,
    /// Expando files are excluded as explicit unsupported ranges.
    Exclude,
}

/// Declared capability facts shared by extraction, structural search,
/// analyzer routing, and host LSP projection.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguageCapabilitySetV1 {
    /// Tree-sitter extraction is available for this language.
    pub extraction: bool,
    /// In-process structural match/outline/rewrite is available.
    pub structural_search: bool,
    /// Symbol outline production is available.
    pub outline: bool,
    /// Structural rewrite is available.
    pub rewrite: bool,
    /// Analyzer routing facts are declared for this language.
    pub analyzer_routing: bool,
    /// Host LSP projection facts are declared for this language.
    pub lsp_projection: bool,
}

/// Edge-authority classes recorded on every extracted relationship
/// (Plan 25: `syntax_exact | name_resolved | compiler_or_lsp_resolved |
/// dynamic_observed | heuristic_candidate | unknown_unsupported`). Every
/// graph path preserves its weakest edge authority.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EdgeAuthorityV1 {
    SyntaxExact,
    NameResolved,
    CompilerOrLspResolved,
    DynamicObserved,
    HeuristicCandidate,
    UnknownUnsupported,
}

impl EdgeAuthorityV1 {
    /// The weaker of two authority classes, used when composing graph paths
    /// (Plan 25: a path preserves its weakest edge authority).
    pub const fn weakest(self, other: Self) -> Self {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::SyntaxExact => 6,
            Self::NameResolved => 5,
            Self::CompilerOrLspResolved => 4,
            Self::DynamicObserved => 3,
            Self::HeuristicCandidate => 2,
            Self::UnknownUnsupported => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid fixture identity")
    }

    fn descriptor() -> LanguageDescriptorV1 {
        LanguageDescriptorV1 {
            language: id("rust"),
            descriptor_revision: id("descriptor.rust.v1"),
            grammar_revision: id("grammar.rust.v1"),
            extractor_revision: id("extractor.rust.v1"),
            aliases: vec!["rs".to_owned(), "rust".to_owned()],
            extensions: vec!["rlib".to_owned(), "rs".to_owned()],
            root_markers: vec!["Cargo.lock".to_owned(), "Cargo.toml".to_owned()],
            expando: ExpandoBehaviorV1::MarkGenerated,
            stable_member_spans: true,
            capabilities: LanguageCapabilitySetV1 {
                extraction: true,
                structural_search: true,
                outline: true,
                rewrite: true,
                analyzer_routing: true,
                lsp_projection: true,
            },
        }
    }

    #[test]
    fn descriptor_requires_sorted_unique_aliases_extensions_and_root_markers() {
        descriptor().validate().expect("canonical descriptor");

        let mut duplicate_alias = descriptor();
        duplicate_alias.aliases.push("rust".to_owned());
        assert!(duplicate_alias.validate().is_err());

        let mut duplicate_extension = descriptor();
        duplicate_extension.extensions.push("rs".to_owned());
        assert!(duplicate_extension.validate().is_err());

        let mut reordered_roots = descriptor();
        reordered_roots.root_markers.reverse();
        assert!(reordered_roots.validate().is_err());
    }
}
