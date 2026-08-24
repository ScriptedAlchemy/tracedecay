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
