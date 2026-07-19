//! Versioned language registry port (Plan 25, "Deterministic extraction").
//!
//! One versioned `LanguageDescriptorV1` per language is shared by
//! extraction, structural search, outline, rewrite, analyzer routing, and
//! host LSP projection. Duplicate language tables and parser acquisition
//! paths are forbidden; descriptors, not extractors, select grammars and
//! capabilities.

use tracedecay_domain::{
    LanguageDescriptorRevision, LanguageDescriptorV1, LanguageId, LanguageRegistryRevision,
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
