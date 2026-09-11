//! Parser-backed evidence emitted alongside the legacy extraction graph.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{ExtractionResult, SourceSpan};

/// Semantic namespace occupied by one imported binding.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImportNamespaceV1 {
    Type,
    Value,
    SideEffect,
}

/// Resolution boundary implied by a source module specifier.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImportModuleKindV1 {
    BareModule,
    ProjectRelative,
}

/// Classify a module specifier using the importing language's syntax.
pub fn import_module_kind(language: &str, module_specifier: &str) -> Option<ImportModuleKindV1> {
    match language {
        "rust" => Some(rust_import_module_kind(module_specifier)),
        "typescript" | "tsx" | "javascript" | "astro" | "svelte" => {
            Some(typescript_import_module_kind(module_specifier))
        }
        _ => None,
    }
}

pub(crate) fn rust_import_module_kind(module_specifier: &str) -> ImportModuleKindV1 {
    let project_relative = matches!(module_specifier, "crate" | "self" | "super")
        || module_specifier.starts_with("crate::")
        || module_specifier.starts_with("self::")
        || module_specifier.starts_with("super::");
    if project_relative {
        ImportModuleKindV1::ProjectRelative
    } else {
        ImportModuleKindV1::BareModule
    }
}

pub(crate) fn typescript_import_module_kind(module_specifier: &str) -> ImportModuleKindV1 {
    if matches!(module_specifier, "." | "..")
        || module_specifier.starts_with("./")
        || module_specifier.starts_with("../")
    {
        ImportModuleKindV1::ProjectRelative
    } else {
        ImportModuleKindV1::BareModule
    }
}

/// One parser-backed import binding or side-effect statement.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq, Hash)]
#[serde(deny_unknown_fields)]
pub struct ExtractedImportEvidenceV1 {
    pub logical_path: String,
    pub module_specifier: String,
    pub imported_name: Option<String>,
    pub local_name: Option<String>,
    pub namespace: ImportNamespaceV1,
    pub module_kind: ImportModuleKindV1,
    pub span: SourceSpan,
    pub start_line: u32,
    pub start_column: u32,
}

impl Ord for ExtractedImportEvidenceV1 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.logical_path
            .cmp(&other.logical_path)
            .then_with(|| self.span.cmp(&other.span))
            .then_with(|| self.module_specifier.cmp(&other.module_specifier))
            .then_with(|| self.imported_name.cmp(&other.imported_name))
            .then_with(|| self.local_name.cmp(&other.local_name))
            .then_with(|| self.namespace.cmp(&other.namespace))
            .then_with(|| self.module_kind.cmp(&other.module_kind))
            .then_with(|| self.start_line.cmp(&other.start_line))
            .then_with(|| self.start_column.cmp(&other.start_column))
    }
}

impl PartialOrd for ExtractedImportEvidenceV1 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Legacy graph extraction plus structured evidence from the same traversal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionArtifactV1 {
    pub result: ExtractionResult,
    pub imports: Vec<ExtractedImportEvidenceV1>,
}

impl ExtractionArtifactV1 {
    pub(crate) fn from_result(result: ExtractionResult) -> Self {
        Self {
            result,
            imports: Vec::new(),
        }
    }

    pub(crate) fn canonicalize_order(&mut self) {
        self.result.canonicalize_order();
        self.imports.sort();
    }
}

#[cfg(test)]
mod tests {
    use super::{ImportModuleKindV1, import_module_kind};

    #[test]
    fn module_kind_uses_the_importing_language_syntax() {
        for specifier in [
            "crate",
            "crate::target",
            "self",
            "self::target",
            "super",
            "super::target",
        ] {
            assert_eq!(
                import_module_kind("rust", specifier),
                Some(ImportModuleKindV1::ProjectRelative)
            );
        }
        assert_eq!(
            import_module_kind("typescript", "crate"),
            Some(ImportModuleKindV1::BareModule)
        );
        assert_eq!(
            import_module_kind("typescript", "../target"),
            Some(ImportModuleKindV1::ProjectRelative)
        );
    }
}
